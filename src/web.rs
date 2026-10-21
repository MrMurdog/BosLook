use crate::config::UserConfig;
use crate::db::Db;
use crate::telegram::Telegram;
use argon2::password_hash::{PasswordVerifier, phc::PasswordHash};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

const SESSION_COOKIE: &str = "boslook_session";

pub struct App {
    pub db: Mutex<Db>,
    pub live: broadcast::Sender<Telegram>,
    users: Vec<UserConfig>,
    /// Sitzungs-Token -> Benutzername
    sessions: Mutex<HashMap<String, String>>,
    secure_cookie: bool,
}

impl App {
    pub fn new(db: Db, users: Vec<UserConfig>, secure_cookie: bool) -> App {
        App {
            db: Mutex::new(db),
            live: broadcast::channel(256).0,
            users,
            sessions: Mutex::new(HashMap::new()),
            secure_cookie,
        }
    }

    fn session_user(&self, headers: &HeaderMap) -> Option<String> {
        let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
        let token = cookies
            .split(';')
            .filter_map(|c| c.trim().split_once('='))
            .find(|(k, _)| *k == SESSION_COOKIE)?
            .1;
        self.sessions.lock().unwrap().get(token).cloned()
    }
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(|| async { Html(include_str!("web/index.html")) }))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/telegrams", get(telegrams))
        .route("/api/live", get(live))
        .with_state(app)
}

#[derive(Deserialize)]
struct LoginRequest {
    user: String,
    password: String,
}

async fn login(State(app): State<Arc<App>>, Json(req): Json<LoginRequest>) -> Response {
    let Some(hash) = app.users.iter().find(|u| u.name == req.user).map(|u| u.password_hash.clone()) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    // Argon2 ist absichtlich langsam, darum nicht im async-Thread rechnen
    let ok = tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash)
            .is_ok_and(|h| argon2::Argon2::default().verify_password(req.password.as_bytes(), &h).is_ok())
    })
    .await
    .unwrap_or(false);
    if !ok {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let mut bytes = [0u8; 32];
    if getrandom::fill(&mut bytes).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    app.sessions.lock().unwrap().insert(token.clone(), req.user);
    let secure = if app.secure_cookie { "; Secure" } else { "" };
    let cookie = format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict{secure}");
    ([(header::SET_COOKIE, cookie)], StatusCode::NO_CONTENT).into_response()
}

async fn logout(State(app): State<Arc<App>>, headers: HeaderMap) -> impl IntoResponse {
    if let Some(cookies) = headers.get(header::COOKIE).and_then(|c| c.to_str().ok()) {
        let mut sessions = app.sessions.lock().unwrap();
        for (k, v) in cookies.split(';').filter_map(|c| c.trim().split_once('=')) {
            if k == SESSION_COOKIE {
                sessions.remove(v);
            }
        }
    }
    let cookie = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0");
    ([(header::SET_COOKIE, cookie)], StatusCode::NO_CONTENT)
}

async fn me(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    match app.session_user(&headers) {
        Some(user) => Json(serde_json::json!({ "user": user })).into_response(),
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

#[derive(Deserialize)]
struct Page {
    limit: Option<u32>,
    before: Option<i64>,
}

async fn telegrams(State(app): State<Arc<App>>, headers: HeaderMap, Query(page): Query<Page>) -> Response {
    if app.session_user(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let limit = page.limit.unwrap_or(100).min(1000);
    match app.db.lock().unwrap().latest(limit, page.before) {
        Ok(list) => Json(list).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn live(State(app): State<Arc<App>>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    if app.session_user(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let rx = app.live.subscribe();
    ws.on_upgrade(move |socket| send_live(socket, rx))
}

async fn send_live(mut socket: WebSocket, mut rx: broadcast::Receiver<Telegram>) {
    loop {
        match rx.recv().await {
            Ok(t) => {
                let json = serde_json::to_string(&t).unwrap();
                if socket.send(Message::Text(json.into())).await.is_err() {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

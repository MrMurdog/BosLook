use crate::config::{ClientConfig, UserConfig};
use crate::db::Db;
use crate::filter::Filters;
use crate::forward::{Ack, Envelope};
use crate::telegram::{Decoded, Telegram};
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
use tokio::sync::{broadcast, mpsc};

const SESSION_COOKIE: &str = "boslook_session";

pub struct App {
    pub db: Mutex<Db>,
    pub live: broadcast::Sender<Telegram>,
    users: Vec<UserConfig>,
    /// Namen der konfigurierten Eingänge, damit sie auch ohne Telegramme angezeigt werden
    inputs: Vec<String>,
    /// Sitzungs-Token -> Benutzername
    sessions: Mutex<HashMap<String, String>>,
    secure_cookie: bool,
    clients: Vec<ClientConfig>,
    /// Weg zur Speicherung für Telegramme von Clients
    ingest: mpsc::Sender<Telegram>,
    filters: Arc<Filters>,
    config_path: String,
}

pub struct Settings {
    pub users: Vec<UserConfig>,
    pub inputs: Vec<String>,
    pub secure_cookie: bool,
    pub clients: Vec<ClientConfig>,
    pub ingest: mpsc::Sender<Telegram>,
    pub filters: Arc<Filters>,
    pub config_path: String,
}

impl App {
    pub fn new(db: Db, s: Settings) -> App {
        App {
            db: Mutex::new(db),
            live: broadcast::channel(256).0,
            users: s.users,
            inputs: s.inputs,
            sessions: Mutex::new(HashMap::new()),
            secure_cookie: s.secure_cookie,
            clients: s.clients,
            ingest: s.ingest,
            filters: s.filters,
            config_path: s.config_path,
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
        .route("/icon.svg", get(|| async { ([(header::CONTENT_TYPE, "image/svg+xml")], include_str!("../images/icon.svg")) }))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/sources", get(sources))
        .route("/api/telegrams", get(telegrams))
        .route("/api/live", get(live))
        .route("/api/ingest", get(ingest))
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/restart", post(restart))
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
    if !verify(hash, req.password).await {
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

async fn verify(hash: String, secret: String) -> bool {
    // Argon2 ist absichtlich langsam, darum nicht im async-Thread rechnen
    tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash).is_ok_and(|h| argon2::Argon2::default().verify_password(secret.as_bytes(), &h).is_ok())
    })
    .await
    .unwrap_or(false)
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

async fn sources(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if app.session_user(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let stored = match app.db.lock().unwrap().sources() {
        Ok(list) => list,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let mut list = app.inputs.clone();
    list.extend(stored.into_iter().filter(|s| !app.inputs.contains(s)));
    Json(list).into_response()
}

#[derive(Deserialize)]
struct Page {
    source: String,
    limit: Option<u32>,
    before: Option<i64>,
}

async fn telegrams(State(app): State<Arc<App>>, headers: HeaderMap, Query(page): Query<Page>) -> Response {
    if app.session_user(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let limit = page.limit.unwrap_or(100).min(1000);
    match app.db.lock().unwrap().latest(&page.source, limit, page.before) {
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

/// Telegramme von einem Client annehmen (siehe `forward.rs`).
async fn ingest(State(app): State<Arc<App>>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let header = |name| headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    let name = header(header::HeaderName::from_static("x-boslook-client"));
    let token = header(header::AUTHORIZATION).strip_prefix("Bearer ").unwrap_or_default().to_string();
    let Some(hash) = app.clients.iter().find(|c| c.name == name).map(|c| c.token_hash.clone()) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if !verify(hash, token).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| receive(socket, app, name))
}

async fn receive(mut socket: WebSocket, app: Arc<App>, name: String) {
    eprintln!("Client '{name}' verbunden");
    while let Some(Ok(msg)) = socket.recv().await {
        let Message::Text(text) = msg else { continue };
        let Ok(Envelope { seq, telegram: t }) = serde_json::from_str(&text) else {
            eprintln!("Client '{name}': ungültige Nachricht");
            break;
        };
        let d = Decoded { kind: t.kind, address: t.address.clone(), sub: t.sub.clone(), message: t.message.clone() };
        if app.filters.allows(&d) && app.ingest.send(t).await.is_err() {
            break;
        }
        let ack = serde_json::to_string(&Ack { ack: seq }).unwrap();
        if socket.send(Message::Text(ack.into())).await.is_err() {
            break;
        }
    }
    eprintln!("Client '{name}' getrennt");
}

async fn get_config(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if app.session_user(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match std::fs::read_to_string(&app.config_path) {
        Ok(text) => text.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Neue Konfiguration prüfen und speichern. Wirksam wird sie erst nach einem Neustart.
async fn put_config(State(app): State<Arc<App>>, headers: HeaderMap, text: String) -> Response {
    if app.session_user(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let checked = crate::config::Config::parse(&text).and_then(|c| {
        if c.web.is_none() {
            anyhow::bail!("[web] fehlt, die Weboberfläche wäre danach nicht mehr erreichbar");
        }
        crate::serve::validate(&c)
    });
    if let Err(e) = checked {
        return (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response();
    }
    // Erst in eine temporäre Datei schreiben, damit die Konfiguration nie halb gespeichert ist
    let tmp = format!("{}.tmp", app.config_path);
    let result = std::fs::write(&tmp, &text)
        .and_then(|_| std::fs::metadata(&app.config_path))
        .and_then(|m| std::fs::set_permissions(&tmp, m.permissions()))
        .and_then(|_| std::fs::rename(&tmp, &app.config_path));
    match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn restart(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if app.session_user(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    eprintln!("Neustart über die Weboberfläche");
    tokio::spawn(async {
        // Antwort noch ausliefern lassen
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        crate::serve::restart();
    });
    StatusCode::ACCEPTED.into_response()
}

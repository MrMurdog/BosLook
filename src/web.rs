use crate::config::{ClientConfig, UserConfig};
use crate::db::Db;
use crate::descriptions::{self, Description, Search};
use crate::forward::{Ack, Envelope};
use crate::telegram::Telegram;
use argon2::password_hash::{PasswordVerifier, phc::PasswordHash};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Query, State};
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
    /// Alle Beschreibungen für die Suche im Editor; wird nach Änderungen neu geladen
    descriptions: Mutex<Option<Arc<Vec<Description>>>>,
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
    config_path: String,
}

pub struct Settings {
    pub users: Vec<UserConfig>,
    pub inputs: Vec<String>,
    pub secure_cookie: bool,
    pub clients: Vec<ClientConfig>,
    pub ingest: mpsc::Sender<Telegram>,
    pub config_path: String,
}

impl App {
    pub fn new(db: Db, s: Settings) -> App {
        App {
            db: Mutex::new(db),
            descriptions: Mutex::new(None),
            live: broadcast::channel(256).0,
            users: s.users,
            inputs: s.inputs,
            sessions: Mutex::new(HashMap::new()),
            secure_cookie: s.secure_cookie,
            clients: s.clients,
            ingest: s.ingest,
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

    fn descriptions(&self) -> anyhow::Result<Arc<Vec<Description>>> {
        let mut cache = self.descriptions.lock().unwrap();
        if cache.is_none() {
            *cache = Some(Arc::new(self.db.lock().unwrap().descriptions()?));
        }
        Ok(cache.clone().expect("geladen"))
    }

    fn write_descriptions(&self, clear: bool, delete: &[String], insert: &[Description]) -> anyhow::Result<()> {
        let delete: Vec<&str> = delete.iter().map(String::as_str).collect();
        let result = self.db.lock().unwrap().write_descriptions(clear, &delete, insert);
        // Zwischenspeicher mitändern statt neu zu laden (das dauert bei großen Adressbüchern)
        let mut cache = self.descriptions.lock().unwrap();
        match (&result, cache.as_mut()) {
            (Ok(()), Some(all)) => {
                let all = Arc::make_mut(all);
                let delete: std::collections::HashSet<&str> = delete.into_iter().collect();
                all.retain(|d| !clear && !delete.contains(d.address.as_str()));
                all.extend_from_slice(insert);
            }
            _ => *cache = None,
        }
        result
    }
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(|| async { Html(include_str!("web/index.html")) }))
        .route("/icon.svg", get(|| async { ([(header::CONTENT_TYPE, "image/svg+xml")], include_str!("../images/icon.svg")) }))
        .route("/banner.svg", get(|| async { ([(header::CONTENT_TYPE, "image/svg+xml")], include_str!("../images/Logo-Banner.svg")) }))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/sources", get(sources))
        .route("/api/telegrams", get(telegrams))
        .route("/api/live", get(live))
        .route("/api/ingest", get(ingest))
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/config/form", get(get_form).put(put_form))
        .route("/api/restart", post(restart))
        .route("/beschreibungen", get(|| async { Html(include_str!("web/descriptions.html")) }))
        .route("/api/descriptions", get(search_descriptions).put(put_description).delete(delete_description))
        // Große Adressbücher (z. B. 86 000 Einträge, etwa 5 MB)
        .route("/api/descriptions/import", post(import_descriptions).layer(DefaultBodyLimit::max(64 << 20)))
        .route("/api/descriptions/export", get(export_descriptions))
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
        // Filtern und Beschreibungen nachschlagen wie bei eigenen Eingängen (siehe serve.rs)
        if app.ingest.send(t).await.is_err() {
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
    save_config(&app, text)
}

/// Schema, Werte und Soundkarten für das Formular der Konfiguration
async fn get_form(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if app.session_user(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let path = app.config_path.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<serde_json::Value> {
        let schema = crate::form::server_schema();
        let values = crate::form::to_json(&std::fs::read_to_string(path)?, &schema)?;
        // Ohne Soundkarten (z. B. auf einem vServer) bleibt die Liste leer
        let devices: Vec<_> = crate::input::devices().unwrap_or_default().into_iter().map(|(id, label)| serde_json::json!({"id": id, "label": label})).collect();
        Ok(serde_json::json!({ "schema": schema, "values": values, "devices": devices }))
    })
    .await;
    match result {
        Ok(Ok(form)) => Json(form).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Formularwerte in die Konfiguration übernehmen (Kommentare bleiben erhalten) und speichern
async fn put_form(State(app): State<Arc<App>>, headers: HeaderMap, Json(form): Json<serde_json::Value>) -> Response {
    if app.session_user(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let path = app.config_path.clone();
    // Neue Passwörter werden gehasht, das dauert
    let text = tokio::task::spawn_blocking(move || crate::form::apply(&std::fs::read_to_string(path)?, &crate::form::server_schema(), &form)).await;
    match text {
        Ok(Ok(text)) => save_config(&app, text),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

fn save_config(app: &App, text: String) -> Response {
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

fn error(status: StatusCode, e: impl std::fmt::Display) -> Response {
    (status, format!("{e:#}")).into_response()
}

async fn search_descriptions(State(app): State<Arc<App>>, headers: HeaderMap, Query(search): Query<Search>) -> Response {
    if app.session_user(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match tokio::task::spawn_blocking(move || app.descriptions().map(|all| descriptions::search(&all, &search))).await {
        Ok(Ok(page)) => Json(page).into_response(),
        Ok(Err(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct Edit {
    /// Bisherige Adresse, fehlt bei neuen Einträgen
    old: Option<String>,
    #[serde(flatten)]
    description: Description,
}

async fn put_description(State(app): State<Arc<App>>, headers: HeaderMap, Json(edit): Json<Edit>) -> Response {
    if app.session_user(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let result = tokio::task::spawn_blocking(move || {
        let (delete, d) = descriptions::edit(&app.descriptions()?, edit.old.as_deref(), edit.description)?;
        app.write_descriptions(false, &delete, std::slice::from_ref(&d))?;
        anyhow::Ok(d)
    })
    .await;
    match result {
        // Mit der einheitlichen Schreibweise der Adresse zurück
        Ok(Ok(d)) => Json(d).into_response(),
        Ok(Err(e)) => error(StatusCode::BAD_REQUEST, e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct Address {
    address: String,
}

async fn delete_description(State(app): State<Arc<App>>, headers: HeaderMap, Query(q): Query<Address>) -> Response {
    if app.session_user(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match tokio::task::spawn_blocking(move || app.write_descriptions(false, &[q.address], &[])).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct ImportOptions {
    #[serde(default)]
    clear: bool,
}

/// CSV-Datei (eigenes Format oder BosMon) importieren
async fn import_descriptions(State(app): State<Arc<App>>, headers: HeaderMap, Query(o): Query<ImportOptions>, data: Bytes) -> Response {
    if app.session_user(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let result = tokio::task::spawn_blocking(move || {
        let import = descriptions::import(&data, &app.descriptions()?, o.clear);
        app.write_descriptions(o.clear, &import.delete, &import.insert)?;
        anyhow::Ok(import)
    })
    .await;
    match result {
        Ok(Ok(import)) => Json(import).into_response(),
        Ok(Err(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn export_descriptions(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if app.session_user(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match tokio::task::spawn_blocking(move || descriptions::export(&app.descriptions()?)).await {
        Ok(Ok(csv)) => (
            [(header::CONTENT_TYPE, "text/csv; charset=utf-8"), (header::CONTENT_DISPOSITION, "attachment; filename=\"beschreibungen.csv\"")],
            csv,
        )
            .into_response(),
        Ok(Err(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

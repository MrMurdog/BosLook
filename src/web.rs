use crate::config::{ClientConfig, UserConfig};
use crate::db::Db;
use crate::descriptions::{self, Description, Search};
use crate::forward::{Ack, Envelope, FromClient, FromServer, InputState};
use crate::plugin::Plugins;
use crate::telegram::{Decoded, Kind, Telegram};
use argon2::password_hash::{PasswordVerifier, phc::PasswordHash};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path as AxumPath, Query, RawQuery, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
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
    /// Ein neuer Einrichtungscode ändert das Token sofort, ohne Neustart
    clients: Mutex<Vec<ClientConfig>>,
    /// Verbindungen der Clients, nach Name
    client_state: Mutex<HashMap<String, ClientState>>,
    /// Meldet verbundenen Clients, dass die Konfiguration gespeichert wurde
    clients_changed: tokio::sync::watch::Sender<()>,
    /// Fingerabdruck des selbst signierten Zertifikats, für den Einrichtungscode
    fingerprint: Option<String>,
    /// Weg zur Speicherung für Telegramme von Clients
    ingest: mpsc::Sender<Telegram>,
    config_path: String,
    plugins: Arc<Plugins>,
}

pub struct Settings {
    pub users: Vec<UserConfig>,
    pub inputs: Vec<String>,
    pub secure_cookie: bool,
    pub clients: Vec<ClientConfig>,
    pub fingerprint: Option<String>,
    pub ingest: mpsc::Sender<Telegram>,
    pub config_path: String,
    pub plugins: Arc<Plugins>,
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
            clients: Mutex::new(s.clients),
            client_state: Mutex::new(HashMap::new()),
            clients_changed: tokio::sync::watch::Sender::new(()),
            fingerprint: s.fingerprint,
            ingest: s.ingest,
            config_path: s.config_path,
            plugins: s.plugins,
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

    /// Konfigurierte Eingänge, dann weitere Quellen aus der Datenbank
    fn sources(&self) -> anyhow::Result<Vec<String>> {
        let stored = self.db.lock().unwrap().sources()?;
        let mut list = self.inputs.clone();
        list.extend(stored.into_iter().filter(|s| !self.inputs.contains(s)));
        Ok(list)
    }

    /// Datenabfrage eines Plugins (`query`): `sources`, `telegrams` (wie `/api/telegrams`) oder
    /// `descriptions` (wie die Suche im Editor). Nur lesend.
    pub fn plugin_query(&self, q: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
        use anyhow::Context;
        Ok(match q["query"].as_str().unwrap_or_default() {
            "sources" => serde_json::to_value(self.sources()?)?,
            "telegrams" => {
                let source = q["source"].as_str().context("source fehlt")?;
                let limit = q["limit"].as_u64().unwrap_or(100).min(1000) as u32;
                serde_json::to_value(self.db.lock().unwrap().latest(source, limit, q["before"].as_i64())?)?
            }
            "descriptions" => {
                let search: Search = serde_json::from_value(q.clone())?;
                serde_json::to_value(descriptions::search(&self.descriptions()?, &search))?
            }
            other => anyhow::bail!("unbekannte Abfrage '{other}'"),
        })
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
        .route("/clients", get(|| async { Html(include_str!("web/clients.html")) }))
        .route("/api/clients", get(clients))
        .route("/api/clients/setup", post(setup_code))
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/config/form", get(get_form).put(put_form))
        .route("/api/restart", post(restart))
        .route("/api/rewrite/test", post(test_rewrite))
        .route("/plugins", get(|| async { Html(include_str!("web/plugins.html")) }))
        .route("/api/plugins", get(plugins))
        .route("/plugin.css", get(|| async { ([(header::CONTENT_TYPE, "text/css")], include_str!("web/plugin.css")) }))
        .route("/plugins/{name}", get(|AxumPath(name): AxumPath<String>| async move { Redirect::permanent(&format!("/plugins/{name}/")) }))
        .route("/plugins/{name}/", any(plugin_page))
        .route("/plugins/{name}/{*path}", any(plugin_page))
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
        Some(user) => Json(serde_json::json!({ "user": user, "version": env!("CARGO_PKG_VERSION") })).into_response(),
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

async fn sources(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if app.session_user(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match app.sources() {
        Ok(list) => Json(list).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
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

/// Verbindung eines Clients, für die Seite „Clients"
#[derive(Clone, Default, Serialize)]
struct ClientState {
    connected: bool,
    version: Option<String>,
    /// Verbunden seit bzw. getrennt seit
    since: Option<chrono::DateTime<chrono::Local>>,
    inputs: BTreeMap<String, InputState>,
}

/// Telegramme von einem Client annehmen (siehe `forward.rs`).
async fn ingest(State(app): State<Arc<App>>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let header = |name| headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    let name = header(header::HeaderName::from_static("x-boslook-client"));
    let token = header(header::AUTHORIZATION).strip_prefix("Bearer ").unwrap_or_default().to_string();
    // Ältere Clients schicken keine Version und verstehen keine Fernkonfiguration
    let version = Some(header(header::HeaderName::from_static("x-boslook-version"))).filter(|v| !v.is_empty());
    let Some((hash, config)) = app.clients.lock().unwrap().iter().find(|c| c.name == name).map(|c| (c.token_hash.clone(), remote_config(c))) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if !verify(hash, token).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let config = version.is_some().then_some(config);
    ws.on_upgrade(move |socket| receive(socket, app, name, version, config))
}

/// Eingänge für einen Client mit Fernkonfiguration als TOML, sonst leer
fn remote_config(c: &ClientConfig) -> String {
    #[derive(Serialize)]
    struct Remote<'a> {
        input: &'a [crate::config::InputConfig],
    }
    if !c.remote {
        return String::new();
    }
    let inputs = toml::to_string(&Remote { input: &c.inputs }).unwrap_or_default();
    format!("# Eingänge von Client '{}', vom Server (Fernkonfiguration). Änderungen hier werden überschrieben.\n{inputs}", c.name)
}

async fn receive(mut socket: WebSocket, app: Arc<App>, name: String, version: Option<String>, config: Option<String>) {
    eprintln!("Client '{name}' verbunden{}", version.as_ref().map(|v| format!(" (Version {v})")).unwrap_or_default());
    let state = ClientState { connected: true, version, since: Some(chrono::Local::now()), inputs: BTreeMap::new() };
    app.client_state.lock().unwrap().insert(name.clone(), state);
    let mut changed = app.clients_changed.subscribe();
    let mut sent = None;
    let mut next = config;
    loop {
        // Fernkonfiguration beim Verbinden und nach jedem Speichern, wenn sie sich geändert hat
        if let Some(config) = next.take().filter(|c| sent.as_ref() != Some(c)) {
            let text = serde_json::to_string(&FromServer::Config { config: config.clone() }).unwrap();
            if socket.send(Message::Text(text.into())).await.is_err() {
                break;
            }
            sent = Some(config);
        }
        let msg = tokio::select! {
            msg = socket.recv() => msg,
            Ok(()) = changed.changed(), if sent.is_some() => {
                next = app.clients.lock().unwrap().iter().find(|c| c.name == name).map(remote_config);
                continue;
            }
        };
        let Some(Ok(msg)) = msg else { break };
        let Message::Text(text) = msg else { continue };
        let Envelope { seq, telegram: t } = match serde_json::from_str(&text) {
            Ok(FromClient::Telegram(envelope)) => envelope,
            Ok(FromClient::Status { status }) => {
                if let Some(s) = app.client_state.lock().unwrap().get_mut(&name) {
                    s.inputs = status;
                }
                continue;
            }
            Err(_) => {
                eprintln!("Client '{name}': ungültige Nachricht");
                break;
            }
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
    if let Some(s) = app.client_state.lock().unwrap().get_mut(&name) {
        s.connected = false;
        s.since = Some(chrono::Local::now());
    }
    eprintln!("Client '{name}' getrennt");
}

/// Konfigurierte Clients mit ihrem Zustand
async fn clients(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if app.session_user(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let state = app.client_state.lock().unwrap();
    let list: Vec<_> = app
        .clients
        .lock()
        .unwrap()
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "token": !c.token_hash.is_empty(),
                "remote": c.remote,
                "inputs": c.inputs.iter().flat_map(crate::config::InputConfig::sources).collect::<Vec<_>>(),
                "state": state.get(&c.name).cloned().unwrap_or_default(),
            })
        })
        .collect();
    Json(list).into_response()
}

#[derive(Deserialize)]
struct SetupRequest {
    name: String,
    /// Adresse, unter der der Client den Server erreicht (aus dem Browser vorbelegt)
    server: String,
}

/// Neues Token für einen Client erzeugen, in der Konfiguration speichern und als
/// Einrichtungscode zurückgeben. Das alte Token wird damit ungültig.
async fn setup_code(State(app): State<Arc<App>>, headers: HeaderMap, Json(req): Json<SetupRequest>) -> Response {
    if app.session_user(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let server = req.server.trim().trim_end_matches('/').to_string();
    if !(server.starts_with("http://") || server.starts_with("https://")) {
        return error(StatusCode::BAD_REQUEST, "Die Adresse muss mit http:// oder https:// beginnen");
    }
    let mut bytes = [0u8; 24];
    if getrandom::fill(&mut bytes).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let (path, name, secret) = (app.config_path.clone(), req.name.clone(), token.clone());
    let text = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let hash = crate::form::hash(&secret)?;
        let mut doc: toml_edit::DocumentMut = std::fs::read_to_string(&path)?.parse()?;
        let client = doc
            .get_mut("client")
            .and_then(toml_edit::Item::as_array_of_tables_mut)
            .and_then(|list| list.iter_mut().find(|c| c.get("name").and_then(|n| n.as_str()) == Some(name.as_str())))
            .ok_or_else(|| anyhow::anyhow!("Client '{name}' steht nicht in der gespeicherten Konfiguration, bitte erst unter „Einstellungen“ anlegen und speichern"))?;
        client["token_hash"] = toml_edit::value(hash);
        Ok(doc.to_string())
    })
    .await;
    let text = match text {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => return error(StatusCode::BAD_REQUEST, format!("{e:#}")),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let saved = save_config(&app, text);
    if !saved.status().is_success() {
        return saved;
    }
    let fingerprint = server.starts_with("https://").then(|| app.fingerprint.clone()).flatten();
    let code = crate::setup::SetupCode { server, name: req.name, token, fingerprint };
    Json(serde_json::json!({ "code": code.encode() })).into_response()
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
        let schema = crate::form::server_schema(&crate::plugin::manifests(&crate::plugin::dir(&path)).0);
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
    let text = tokio::task::spawn_blocking(move || {
        let schema = crate::form::server_schema(&crate::plugin::manifests(&crate::plugin::dir(&path)).0);
        crate::form::apply(&std::fs::read_to_string(&path)?, &schema, &form)
    })
    .await;
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
        crate::serve::validate(&c, &crate::plugin::dir(&app.config_path))?;
        Ok(c)
    });
    let config = match checked {
        Ok(c) => c,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
    };
    // Erst in eine temporäre Datei schreiben, damit die Konfiguration nie halb gespeichert ist
    let tmp = format!("{}.tmp", app.config_path);
    let result = std::fs::write(&tmp, &text)
        .and_then(|_| std::fs::metadata(&app.config_path))
        .and_then(|m| std::fs::set_permissions(&tmp, m.permissions()))
        .and_then(|_| std::fs::rename(&tmp, &app.config_path));
    match result {
        Ok(()) => {
            // Clients (Tokens, Fernkonfiguration) gelten sofort, alles andere nach einem Neustart
            *app.clients.lock().unwrap() = config.clients;
            app.clients_changed.send_replace(());
            StatusCode::NO_CONTENT.into_response()
        }
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

#[derive(Deserialize)]
struct RewriteTest {
    /// Regeln aus dem Formular, noch nicht gespeichert
    rules: serde_json::Value,
    kind: Kind,
    address: String,
    #[serde(default)]
    sub: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    source: String,
}

/// Testfeld im Formular: Was machen die Bearbeitungsregeln aus diesem Telegramm?
async fn test_rewrite(State(app): State<Arc<App>>, headers: HeaderMap, Json(test): Json<RewriteTest>) -> Response {
    if app.session_user(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let result = tokio::task::spawn_blocking(move || {
        let rules = crate::rewrite::Rewrites::from_form(&test.rules)?;
        let mut address = test.address.trim().to_string();
        if test.kind == Kind::Pocsag && let Ok(ric) = address.parse::<u32>() {
            address = format!("{ric:07}");
        }
        let decoded = Decoded { kind: test.kind, address, sub: test.sub, message: test.message };
        let source = Some(test.source.trim()).filter(|s| !s.is_empty()).unwrap_or("Test");
        let mut t = Telegram::new(decoded, source, chrono::Local::now());
        let applied = rules.describe_and_apply(&app.db.lock().unwrap(), &mut t)?.join(", ");
        anyhow::Ok(serde_json::json!({ "telegram": t, "applied": applied }))
    })
    .await;
    match result {
        Ok(Ok(r)) => Json(r).into_response(),
        Ok(Err(e)) => error(StatusCode::BAD_REQUEST, e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Installierte Plugins mit Zustand und letzten Logzeilen
async fn plugins(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if app.session_user(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match tokio::task::spawn_blocking(move || app.plugins.status()).await {
        Ok(status) => Json(status).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Eigene Seite eines Plugins: Dateien aus seinem Ordner `web/`, Anfragen an `api/…` gehen an
/// das Plugin selbst. Nur für angemeldete Benutzer.
async fn plugin_page(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    AxumPath(params): AxumPath<HashMap<String, String>>,
    method: Method,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> Response {
    let Some(user) = app.session_user(&headers) else {
        return (StatusCode::UNAUTHORIZED, Html("<p>Bitte zuerst <a href=\"/\">in BosLook anmelden</a>.</p>")).into_response();
    };
    let Some((plugin, dir)) = app.plugins.get(&params["name"]).and_then(|p| p.web_dir().map(|d| (p, d))) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let path = params.get("path").map_or("", String::as_str);
    if let Some(api) = path.strip_prefix("api/").or((path == "api").then_some("")) {
        let request = serde_json::json!({
            "method": method.as_str(),
            "path": format!("/{api}"),
            "query": query.unwrap_or_default(),
            "body": String::from_utf8_lossy(&body),
            "user": user,
        });
        return match plugin.http(request).await {
            Ok(r) => plugin_response(&r),
            Err(e) => error(StatusCode::BAD_GATEWAY, format!("Plugin '{}': {e:#}", params["name"])),
        };
    }
    if method != Method::GET {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let file = if path.is_empty() || path.ends_with('/') { format!("{path}index.html") } else { path.to_string() };
    // Nur Dateien innerhalb von web/
    if std::path::Path::new(&file).components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match tokio::fs::read(dir.join(&file)).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, content_type(&file)), (header::CACHE_CONTROL, "no-cache")], bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Antwort des Plugins: `status` (Standard 200) und entweder `json` oder `body` mit `content_type`
fn plugin_response(r: &serde_json::Value) -> Response {
    let status = r["status"].as_u64().and_then(|s| StatusCode::from_u16(s as u16).ok()).unwrap_or(StatusCode::OK);
    let (content_type, body) = match r.get("json") {
        Some(json) => ("application/json".to_string(), json.to_string()),
        None => (
            r["content_type"].as_str().unwrap_or("text/plain; charset=utf-8").to_string(),
            r["body"].as_str().unwrap_or_default().to_string(),
        ),
    };
    (status, [(header::CONTENT_TYPE, content_type)], body).into_response()
}

fn content_type(file: &str) -> &'static str {
    match file.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn body(r: Response) -> (StatusCode, String, String) {
        let ct = r.headers()[header::CONTENT_TYPE].to_str().unwrap().to_string();
        let status = r.status();
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        (status, ct, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[test]
    fn remote_config_round_trip() {
        let config = crate::config::Config::parse(
            r#"
            [[client]]
            name = "Pi"
            token_hash = ""
            remote = true
            [[client.input]]
            name = "Funk"
            type = "rtl_fm"
            frequency = "173.255M"
            ppm = -3
            [[client.input]]
            name = "Karte"
            type = "soundcard"
            pocsag = "auto"
            [[client.input]]
            name = "Stick"
            type = "rtl_sdr"
            [[client.input.channel]]
            name = "Kreis"
            frequency = "173.255M"
            [[client.input.channel]]
            name = "FMS"
            frequency = "172.89M"
            "#,
        )
        .unwrap();
        let text = remote_config(&config.clients[0]);
        let remote = crate::forward::parse_remote(&text).unwrap();
        assert_eq!(remote.inputs.len(), 3, "{text}");
        assert_eq!(remote.inputs[2].sources(), ["Kreis", "FMS"]);
        assert!(matches!(&remote.inputs[0].source, crate::config::Source::RtlFm { ppm: -3, program: None, .. }));
        assert!(matches!(&remote.inputs[1].source, crate::config::Source::Soundcard { device: None }));
        assert_eq!(remote.inputs[1].pocsag, crate::decoder::pocsag::Format::Auto);
        // Ohne Fernkonfiguration nichts schicken
        let mut client = config.clients.into_iter().next().unwrap();
        client.remote = false;
        assert_eq!(remote_config(&client), "");
    }

    #[tokio::test]
    async fn plugin_responses() {
        let json = body(plugin_response(&serde_json::json!({"json": {"a": 1}}))).await;
        assert_eq!(json, (StatusCode::OK, "application/json".into(), "{\"a\":1}".into()));
        let text = body(plugin_response(&serde_json::json!({"status": 404, "body": "weg"}))).await;
        assert_eq!(text, (StatusCode::NOT_FOUND, "text/plain; charset=utf-8".into(), "weg".into()));
        let html = body(plugin_response(&serde_json::json!({"content_type": "text/html", "body": "<p>"}))).await;
        assert_eq!(html.1, "text/html");
        assert_eq!(content_type("web/app.js"), "text/javascript");
    }
}

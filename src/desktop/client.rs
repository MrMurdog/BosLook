//! Verbindung zum BosLook-Server: Anmeldung, Verlauf und Live-Telegramme wie in der Weboberfläche.

use crate::config::ServerConfig;
use crate::events::Events;
use anyhow::{Context, Result, bail};
use boslook::telegram::Telegram;
use futures_util::{SinkExt, StreamExt};
use reqwest::header::{COOKIE, SET_COOKIE};
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const RETRY: Duration = Duration::from_secs(5);
const PING: Duration = Duration::from_secs(20);
/// So viele Telegramme je Quelle werden beim Verbinden geladen
const HISTORY: &str = "200";

pub enum FromClient {
    Status { text: String, connected: bool },
    /// Neu verbunden: Quellen und Verlauf ersetzen die bisherige Anzeige
    Reset { sources: Vec<String>, history: Vec<Telegram> },
    Telegram(Telegram),
    ServerConfig(Result<String, String>),
    Saved(Result<String, String>),
}

pub enum ToClient {
    LoadServerConfig,
    SaveServerConfig { text: String, restart: bool },
}

/// Startet die Verbindung in einem eigenen Thread. Jede Nachricht an die Oberfläche weckt sie über `wake` auf.
pub fn spawn(
    config: ServerConfig,
    mut events: Events,
    tx: Sender<FromClient>,
    wake: impl Fn() + Send + 'static,
) -> mpsc::UnboundedSender<ToClient> {
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio");
        let send = |m: FromClient| {
            let _ = tx.send(m);
            wake();
        };
        runtime.block_on(async {
            loop {
                send(FromClient::Status { text: "verbinde …".into(), connected: false });
                let error = match session(&config, &mut events, &send, &mut cmd_rx).await {
                    Ok(()) => return,
                    Err(e) => e,
                };
                send(FromClient::Status { text: format!("getrennt: {error:#}"), connected: false });
                // Während der Pause kommen Befehle aus der Oberfläche nicht zum Server durch
                let pause = tokio::time::sleep(RETRY);
                tokio::pin!(pause);
                loop {
                    tokio::select! {
                        _ = &mut pause => break,
                        cmd = cmd_rx.recv() => match cmd {
                            Some(cmd) => send(not_connected(cmd)),
                            None => return,
                        },
                    }
                }
            }
        });
    });
    cmd_tx
}

fn not_connected(cmd: ToClient) -> FromClient {
    let e = Err("Nicht mit dem Server verbunden".to_string());
    match cmd {
        ToClient::LoadServerConfig => FromClient::ServerConfig(e),
        ToClient::SaveServerConfig { .. } => FromClient::Saved(e),
    }
}

async fn session(
    config: &ServerConfig,
    events: &mut Events,
    send: &impl Fn(FromClient),
    cmd_rx: &mut mpsc::UnboundedReceiver<ToClient>,
) -> Result<()> {
    let tls = boslook::forward::tls_config(config.cert.as_deref())?;
    let http = reqwest::Client::builder().tls_backend_preconfigured(tls.clone()).timeout(Duration::from_secs(20)).build()?;
    let base = config.url.trim_end_matches('/').to_string();

    let login = serde_json::json!({ "user": config.user, "password": config.password });
    let res = http.post(format!("{base}/api/login")).json(&login).send().await.context("Server nicht erreichbar")?;
    if res.status() == reqwest::StatusCode::UNAUTHORIZED {
        bail!("Benutzer oder Passwort falsch");
    }
    let res = res.error_for_status()?;
    let cookie = res
        .headers()
        .get(SET_COOKIE)
        .and_then(|c| c.to_str().ok())
        .and_then(|c| c.split(';').next())
        .context("Server hat keine Sitzung geliefert")?
        .to_string();

    let get = |path: &str| http.get(format!("{base}{path}")).header(COOKIE, &cookie);
    let sources: Vec<String> = get("/api/sources").send().await?.error_for_status()?.json().await?;
    let mut history = Vec::new();
    for source in &sources {
        let page: Vec<Telegram> =
            get("/api/telegrams").query(&[("source", source.as_str()), ("limit", HISTORY)]).send().await?.error_for_status()?.json().await?;
        history.extend(page);
    }

    let ws_url = format!("{base}/api/live").replacen("https://", "wss://", 1).replacen("http://", "ws://", 1);
    let mut request = ws_url.as_str().into_client_request()?;
    request.headers_mut().insert("cookie", cookie.parse()?);
    let connector = tokio_tungstenite::Connector::Rustls(Arc::new(tls));
    let (mut ws, _) = tokio_tungstenite::connect_async_tls_with_config(request, None, false, Some(connector)).await?;
    send(FromClient::Reset { sources, history });
    send(FromClient::Status { text: format!("verbunden mit {base}"), connected: true });

    let mut ping = tokio::time::interval(PING);
    let mut last_seen = Instant::now();
    loop {
        tokio::select! {
            msg = ws.next() => {
                last_seen = Instant::now();
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let t: Telegram = serde_json::from_str(&text)?;
                        events.handle(&t);
                        send(FromClient::Telegram(t));
                    }
                    Some(Ok(Message::Close(_))) | None => bail!("Verbindung vom Server beendet"),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.into()),
                }
            }
            _ = ping.tick() => {
                if last_seen.elapsed() > PING * 2 + Duration::from_secs(5) {
                    bail!("Server antwortet nicht");
                }
                ws.send(Message::Ping(Default::default())).await?;
            }
            cmd = cmd_rx.recv() => match cmd {
                None => return Ok(()),
                Some(ToClient::LoadServerConfig) => {
                    let result = async { get("/api/config").send().await?.error_for_status()?.text().await }.await;
                    send(FromClient::ServerConfig(result.map_err(|e| e.to_string())));
                }
                Some(ToClient::SaveServerConfig { text, restart }) => {
                    let result = save_server_config(&http, &base, &cookie, text, restart).await;
                    send(FromClient::Saved(result.map_err(|e| format!("{e:#}"))));
                }
            },
        }
    }
}

async fn save_server_config(http: &reqwest::Client, base: &str, cookie: &str, text: String, restart: bool) -> Result<String> {
    let res = http.put(format!("{base}/api/config")).header(COOKIE, cookie).body(text).send().await?;
    if !res.status().is_success() {
        bail!("Nicht gespeichert: {}", res.text().await?);
    }
    if !restart {
        return Ok("Gespeichert. Wirksam nach einem Neustart des Servers.".into());
    }
    http.post(format!("{base}/api/restart")).header(COOKIE, cookie).send().await?.error_for_status()?;
    Ok("Gespeichert, der Server startet neu. Die Verbindung wird danach neu aufgebaut.".into())
}

//! Verbindung zum BosLook-Server: Anmeldung, Verlauf und Live-Telegramme wie in der Weboberfläche.

use crate::config::ServerConfig;
use crate::events::Events;
use anyhow::{Context, Result, bail};
use boslook::descriptions::{Description, Page, Search};
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
    /// Neu verbunden: Quellen und Verlauf ersetzen die bisherige Anzeige; `pages` sind die
    /// Plugins mit eigener Seite
    Reset { sources: Vec<String>, history: Vec<Telegram>, pages: Vec<String> },
    Telegram(Telegram),
    /// Bessere Fassung eines angezeigten Telegramms (Doppelfilter des Servers), gleiche ID
    Updated(Telegram),
    ServerConfig(Result<String, String>),
    /// Schema, Werte und Soundkarten für das Formular
    ServerForm(Result<serde_json::Value, String>),
    Saved(Result<String, String>),
    /// Seite der Beschreibungen für den Editor
    Descriptions(Result<Page, String>),
    /// Gespeicherte Beschreibung (Zeile im Editor, Eintrag in einheitlicher Schreibweise)
    DescriptionSaved { row: u64, result: Result<Description, String> },
    /// Ergebnis von Löschen, Import und Export; danach die Seite neu laden
    DescriptionsChanged(Result<String, String>),
    /// Ergebnis des Testfelds der Bearbeitungsregeln
    RewriteTested(Result<serde_json::Value, String>),
    /// Telegramme eines Reiters gelöscht (Name des Reiters bzw. Fehler)
    SourceDeleted(Result<String, String>),
}

pub enum ToClient {
    LoadServerConfig,
    LoadServerForm,
    SaveServerConfig { text: String, restart: bool },
    SaveServerForm { values: serde_json::Value, restart: bool },
    SearchDescriptions(Search),
    SaveDescription { row: u64, old: Option<String>, description: Description },
    DeleteDescription(String),
    ImportDescriptions { path: std::path::PathBuf, clear: bool },
    ExportDescriptions(std::path::PathBuf),
    TestRewrite(serde_json::Value),
    /// Alle Telegramme eines Reiters löschen
    DeleteSource(String),
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
    let e = "Nicht mit dem Server verbunden".to_string();
    match cmd {
        ToClient::LoadServerConfig => FromClient::ServerConfig(Err(e)),
        ToClient::LoadServerForm => FromClient::ServerForm(Err(e)),
        ToClient::SaveServerConfig { .. } | ToClient::SaveServerForm { .. } => FromClient::Saved(Err(e)),
        ToClient::SearchDescriptions(_) => FromClient::Descriptions(Err(e)),
        ToClient::SaveDescription { row, .. } => FromClient::DescriptionSaved { row, result: Err(e) },
        ToClient::DeleteDescription(_) | ToClient::ImportDescriptions { .. } | ToClient::ExportDescriptions(_) => {
            FromClient::DescriptionsChanged(Err(e))
        }
        ToClient::TestRewrite(_) => FromClient::RewriteTested(Err(e)),
        ToClient::DeleteSource(_) => FromClient::SourceDeleted(Err(e)),
    }
}

async fn session(
    config: &ServerConfig,
    events: &mut Events,
    send: &impl Fn(FromClient),
    cmd_rx: &mut mpsc::UnboundedReceiver<ToClient>,
) -> Result<()> {
    let tls = boslook::forward::tls_config(config.cert.as_deref(), None)?;
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
    // Ältere Server ohne Plugins liefern hier einen Fehler, dann eben keine Seiten
    let plugins: serde_json::Value = match get("/api/plugins").send().await.and_then(|r| r.error_for_status()) {
        Ok(res) => res.json().await.unwrap_or_default(),
        Err(_) => serde_json::Value::Null,
    };
    let pages = plugins["plugins"].as_array().into_iter().flatten().filter(|p| p["page"] == true).filter_map(|p| p["name"].as_str().map(str::to_string)).collect();

    let ws_url = format!("{base}/api/live").replacen("https://", "wss://", 1).replacen("http://", "ws://", 1);
    let mut request = ws_url.as_str().into_client_request()?;
    request.headers_mut().insert("cookie", cookie.parse()?);
    let connector = tokio_tungstenite::Connector::Rustls(Arc::new(tls));
    let (mut ws, _) = tokio_tungstenite::connect_async_tls_with_config(request, None, false, Some(connector)).await?;
    // IDs steigen; eine schon bekannte ist eine bessere Fassung und löst keine Ereignisse aus
    let mut newest = history.iter().map(|t| t.id).max().unwrap_or(0);
    send(FromClient::Reset { sources, history, pages });
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
                        if t.id != 0 && t.id <= newest {
                            send(FromClient::Updated(t));
                            continue;
                        }
                        newest = newest.max(t.id);
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
                Some(ToClient::LoadServerForm) => {
                    let result = async { get("/api/config/form").send().await?.error_for_status()?.json().await }.await;
                    send(FromClient::ServerForm(result.map_err(|e| e.to_string())));
                }
                Some(ToClient::SaveServerConfig { text, restart }) => {
                    let request = http.put(format!("{base}/api/config")).body(text);
                    let result = save_server_config(&http, &base, &cookie, request, restart).await;
                    send(FromClient::Saved(result.map_err(|e| format!("{e:#}"))));
                }
                Some(ToClient::SaveServerForm { values, restart }) => {
                    let request = http.put(format!("{base}/api/config/form")).json(&values);
                    let result = save_server_config(&http, &base, &cookie, request, restart).await;
                    send(FromClient::Saved(result.map_err(|e| format!("{e:#}"))));
                }
                Some(ToClient::SearchDescriptions(search)) => {
                    let result = async { anyhow::Ok(checked(get("/api/descriptions").query(&search)).await?.json().await?) }.await;
                    send(FromClient::Descriptions(result.map_err(|e| format!("{e:#}"))));
                }
                Some(ToClient::SaveDescription { row, old, description }) => {
                    let body = serde_json::json!({ "old": old, "address": description.address, "ort": description.ort, "kurz": description.kurz, "lang": description.lang });
                    let request = http.put(format!("{base}/api/descriptions")).header(COOKIE, &cookie).json(&body);
                    let result = async { anyhow::Ok(checked(request).await?.json().await?) }.await;
                    send(FromClient::DescriptionSaved { row, result: result.map_err(|e| format!("{e:#}")) });
                }
                Some(ToClient::DeleteDescription(address)) => {
                    let request = http.delete(format!("{base}/api/descriptions")).header(COOKIE, &cookie).query(&[("address", &address)]);
                    let result = checked(request).await.map(|_| format!("{address} gelöscht"));
                    send(FromClient::DescriptionsChanged(result.map_err(|e| format!("{e:#}"))));
                }
                Some(ToClient::ImportDescriptions { path, clear }) => {
                    let result = async {
                        let data = std::fs::read(&path).with_context(|| path.display().to_string())?;
                        let request = http.post(format!("{base}/api/descriptions/import")).header(COOKIE, &cookie).query(&[("clear", clear)]).body(data);
                        let r: serde_json::Value = checked(request.timeout(Duration::from_secs(300))).await?.json().await?;
                        let errors: Vec<&str> = r["errors"].as_array().into_iter().flatten().filter_map(|e| e.as_str()).collect();
                        let mut text = format!("Import: {} neu, {} geändert, {} unverändert", r["new"], r["changed"], r["unchanged"]);
                        if !errors.is_empty() {
                            text += &format!(", {} Zeilen fehlerhaft:\n{}", errors.len(), errors.iter().take(20).copied().collect::<Vec<_>>().join("\n"));
                        }
                        anyhow::Ok(text)
                    }
                    .await;
                    send(FromClient::DescriptionsChanged(result.map_err(|e| format!("Import fehlgeschlagen: {e:#}"))));
                }
                Some(ToClient::ExportDescriptions(path)) => {
                    let result = async {
                        let csv = checked(get("/api/descriptions/export")).await?.bytes().await?;
                        std::fs::write(&path, csv).with_context(|| path.display().to_string())?;
                        anyhow::Ok(format!("Exportiert nach {}", path.display()))
                    }
                    .await;
                    send(FromClient::DescriptionsChanged(result.map_err(|e| format!("Export fehlgeschlagen: {e:#}"))));
                }
                Some(ToClient::TestRewrite(body)) => {
                    let request = http.post(format!("{base}/api/rewrite/test")).header(COOKIE, &cookie).json(&body);
                    let result = async { anyhow::Ok(checked(request).await?.json().await?) }.await;
                    send(FromClient::RewriteTested(result.map_err(|e| format!("{e:#}"))));
                }
                Some(ToClient::DeleteSource(source)) => {
                    let request = http.delete(format!("{base}/api/sources")).header(COOKIE, &cookie).query(&[("source", &source)]);
                    let result = checked(request).await.map(|_| source);
                    send(FromClient::SourceDeleted(result.map_err(|e| format!("{e:#}"))));
                }
            },
        }
    }
}

async fn save_server_config(http: &reqwest::Client, base: &str, cookie: &str, request: reqwest::RequestBuilder, restart: bool) -> Result<String> {
    let res = request.header(COOKIE, cookie).send().await?;
    if !res.status().is_success() {
        bail!("Nicht gespeichert: {}", res.text().await?);
    }
    if !restart {
        return Ok("Gespeichert. Wirksam nach einem Neustart des Servers.".into());
    }
    http.post(format!("{base}/api/restart")).header(COOKIE, cookie).send().await?.error_for_status()?;
    Ok("Gespeichert, der Server startet neu. Die Verbindung wird danach neu aufgebaut.".into())
}

/// Anfrage senden; bei einem Fehler die Meldung des Servers zurückgeben
async fn checked(request: reqwest::RequestBuilder) -> Result<reqwest::Response> {
    let res = request.send().await?;
    if !res.status().is_success() {
        bail!("{}", res.text().await?);
    }
    Ok(res)
}

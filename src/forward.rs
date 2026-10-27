//! Weiterleitung an einen BosLook-Server (`[forward]` in der Konfiguration).
//!
//! Protokoll über WebSocket (`/api/ingest`): Der Client schickt `{"seq": n, "telegram": {...}}`,
//! der Server bestätigt mit `{"ack": n}`. Unbestätigte Telegramme werden gepuffert und nach
//! einem Verbindungsabbruch erneut gesendet.
//!
//! Fernkonfiguration (ab 0.2.2): Der Client nennt seine Version im Header `x-boslook-version`.
//! Nur dann schickt der Server nach dem Verbindungsaufbau `{"config": "<TOML>"}` mit den
//! Eingängen des Clients (leer = keine Fernkonfiguration). Erst danach meldet der Client den
//! Zustand seiner Eingänge mit `{"status": {...}}`. So bleiben ältere Gegenstellen verträglich.
//!
//! Update (ab 0.3.0): `{"update": "<Version>"}` vom Server, nur an Clients ab 0.3.0. Der Client
//! lädt diese Version aus den GitHub-Releases und startet neu.

use crate::config::{ForwardConfig, RemoteConfig};
use crate::telegram::Telegram;
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime, pem::PemObject};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// So viele Telegramme werden höchstens gepuffert, solange der Server nicht erreichbar ist.
const MAX_QUEUE: usize = 10_000;
const RETRY: Duration = Duration::from_secs(5);
const PING: Duration = Duration::from_secs(20);

#[derive(Serialize, Deserialize)]
pub struct Envelope {
    pub seq: u64,
    pub telegram: Telegram,
}

#[derive(Serialize, Deserialize)]
pub struct Ack {
    pub ack: u64,
}

/// Nachrichten vom Server an den Client
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub enum FromServer {
    Ack(Ack),
    Config { config: String },
    Update { update: String },
}

/// Ab dieser Version verstehen Clients `{"update": …}`
pub const UPDATE_SINCE: &str = "0.3.0";

/// Nachrichten vom Client an den Server
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub enum FromClient {
    Telegram(Envelope),
    Status { status: BTreeMap<String, InputState> },
}

/// Zustand eines Eingangs, z. B. „Soundkarte mit 48000 Hz geöffnet" oder eine Fehlermeldung
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputState {
    pub ok: bool,
    pub message: String,
}

/// Zustand aller Eingänge, nach Name
pub type InputStatus = Arc<Mutex<BTreeMap<String, InputState>>>;

/// Datei, in der ein Client die Eingänge vom Server aufhebt (neben der Konfiguration)
pub fn remote_path(config_path: &str) -> PathBuf {
    Path::new(config_path).with_extension("remote.toml")
}

/// Fernkonfiguration des Clients: wo sie liegt und welche gerade läuft
pub struct Remote {
    pub path: PathBuf,
    /// Text der laufenden Fernkonfiguration, leer = eigene Eingänge
    pub running: String,
    pub status: InputStatus,
}

/// Prüft eine Fernkonfiguration (leer = keine)
pub fn parse_remote(text: &str) -> Result<RemoteConfig> {
    let remote: RemoteConfig = toml::from_str(text)?;
    crate::serve::validate_inputs(&remote.inputs)?;
    Ok(remote)
}

struct Queue {
    items: VecDeque<(u64, Telegram)>,
    next: u64,
}

impl Queue {
    fn push(&mut self, t: Telegram) {
        if self.items.len() == MAX_QUEUE {
            self.items.pop_front();
            eprintln!("Weiterleitung: Puffer voll, ältestes Telegramm verworfen");
        }
        self.next += 1;
        self.items.push_back((self.next, t));
    }
}

pub async fn run(config: ForwardConfig, mut rx: mpsc::Receiver<Telegram>, remote: Remote) {
    let mut queue = Queue { items: VecDeque::new(), next: 0 };
    loop {
        if let Err(e) = session(&config, &mut rx, &mut queue, &remote).await {
            // Manche Fehler wiederholen den Text ihrer Ursache, darum doppelte Teile weglassen
            let mut text = e.to_string();
            for cause in e.chain().skip(1).map(|c| c.to_string()) {
                if !text.contains(&cause) {
                    text = format!("{text}: {cause}");
                }
            }
            eprintln!("Weiterleitung an {}: {text}, neuer Versuch in {} s", config.server, RETRY.as_secs());
        }
        // Während der Pause weiter Telegramme annehmen
        let pause = tokio::time::sleep(RETRY);
        tokio::pin!(pause);
        loop {
            tokio::select! {
                _ = &mut pause => break,
                Some(t) = rx.recv() => queue.push(t),
            }
        }
    }
}

async fn session(config: &ForwardConfig, rx: &mut mpsc::Receiver<Telegram>, queue: &mut Queue, remote: &Remote) -> Result<()> {
    let url = format!("{}/api/ingest", config.server.trim_end_matches('/'))
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    let mut request = url.as_str().into_client_request()?;
    request.headers_mut().insert("x-boslook-client", config.name.parse()?);
    request.headers_mut().insert("authorization", format!("Bearer {}", config.token).parse()?);
    request.headers_mut().insert("x-boslook-version", env!("CARGO_PKG_VERSION").parse()?);
    let connector = tokio_tungstenite::Connector::Rustls(Arc::new(tls_config(config.cert.as_deref(), config.fingerprint.as_deref())?));
    let (mut ws, _) = tokio_tungstenite::connect_async_tls_with_config(request, None, false, Some(connector))
        .await
        .context("Verbindung fehlgeschlagen")?;
    eprintln!("Weiterleitung an {}: verbunden", config.server);

    for (seq, t) in &queue.items {
        ws.send(text(*seq, t)).await?;
    }
    let mut ping = tokio::time::interval(PING);
    let mut last_seen = Instant::now();
    // Zustand der Eingänge erst melden, wenn der Server Fernkonfiguration kennt
    let mut sent_status = None;
    let mut status_check = tokio::time::interval(Duration::from_secs(2));
    let mut restart = false;
    loop {
        // Neue Fernkonfiguration erst übernehmen, wenn alle Telegramme beim Server sind
        if restart && queue.items.is_empty() {
            ws.close(None).await.ok();
            eprintln!("Neue Fernkonfiguration bzw. Update, BosLook startet neu");
            crate::serve::restart();
        }
        tokio::select! {
            t = rx.recv() => {
                let Some(t) = t else { return Ok(()) };
                queue.push(t);
                let (seq, t) = queue.items.back().unwrap();
                ws.send(text(*seq, t)).await?;
            }
            msg = ws.next() => {
                last_seen = Instant::now();
                match msg {
                    Some(Ok(Message::Text(text))) => match serde_json::from_str(&text)? {
                        FromServer::Ack(ack) => {
                            while queue.items.front().is_some_and(|(seq, _)| *seq <= ack.ack) {
                                queue.items.pop_front();
                            }
                        }
                        FromServer::Config { config } => {
                            restart |= apply_remote(remote, &config);
                            sent_status.get_or_insert_with(BTreeMap::new);
                            status_check.reset_immediately();
                        }
                        FromServer::Update { update } if update != crate::update::CURRENT => {
                            eprintln!("Server verlangt Update auf {update}");
                            let version = update.clone();
                            let result = tokio::task::spawn_blocking(move || crate::update::install(&crate::update::release(&version)?, "boslook")).await?;
                            let state = match result {
                                Ok(()) => {
                                    restart = true;
                                    InputState { ok: true, message: format!("{update} eingespielt, Neustart folgt") }
                                }
                                Err(e) => InputState { ok: false, message: format!("{update}: {e:#}") },
                            };
                            eprintln!("Update: {}", state.message);
                            remote.status.lock().unwrap().insert("Update".into(), state);
                        }
                        FromServer::Update { .. } => {}
                    },
                    Some(Ok(Message::Close(_))) | None => bail!("Verbindung vom Server beendet"),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.into()),
                }
            }
            _ = status_check.tick() => {
                let Some(sent) = &mut sent_status else { continue };
                let status = remote.status.lock().unwrap().clone();
                if status != *sent {
                    ws.send(Message::Text(serde_json::to_string(&FromClient::Status { status: status.clone() })?.into())).await?;
                    *sent = status;
                }
            }
            _ = ping.tick() => {
                // Ohne Antwort auf zwei Pings gilt die Verbindung als tot
                if last_seen.elapsed() > PING * 2 + Duration::from_secs(5) {
                    bail!("Server antwortet nicht");
                }
                ws.send(Message::Ping(Default::default())).await?;
            }
        }
    }
}

/// Fernkonfiguration vom Server speichern; `true`, wenn sie sich gegenüber der laufenden geändert
/// hat und BosLook neu starten muss
fn apply_remote(remote: &Remote, config: &str) -> bool {
    if config == remote.running {
        return false;
    }
    let saved = if config.is_empty() {
        match std::fs::remove_file(&remote.path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    } else {
        parse_remote(config).and_then(|_| Ok(std::fs::write(&remote.path, config)?))
    };
    if let Err(e) = saved {
        let message = format!("Fernkonfiguration nicht übernommen: {e:#}");
        eprintln!("{message}");
        remote.status.lock().unwrap().insert("Fernkonfiguration".into(), InputState { ok: false, message });
        return false;
    }
    true
}

fn text(seq: u64, telegram: &Telegram) -> Message {
    let envelope = Envelope { seq, telegram: telegram.clone() };
    Message::Text(serde_json::to_string(&envelope).unwrap().into())
}

/// SHA-256-Fingerabdruck eines Zertifikats, z. B. `AB:CD:…`
pub fn fingerprint(cert: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(cert).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

/// Fingerabdruck der Zertifikatsdatei (PEM)
pub fn fingerprint_file(path: &str) -> Result<String> {
    Ok(fingerprint(CertificateDer::from_pem_file(path).with_context(|| format!("Zertifikat {path}"))?.as_ref()))
}

/// TLS für Verbindungen zu einem BosLook-Server: mit `cert` (Datei) oder `fingerprint` wird genau
/// dieses Zertifikat akzeptiert, sonst wird über die üblichen Zertifizierungsstellen geprüft.
pub fn tls_config(cert: Option<&str>, fingerprint: Option<&str>) -> Result<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone()).with_safe_default_protocol_versions()?;
    let pinned = match (cert, fingerprint) {
        (Some(path), _) => Some(fingerprint_file(path)?),
        (None, Some(f)) => Some(f.to_string()),
        (None, None) => None,
    };
    Ok(match pinned {
        Some(fingerprint) => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Pinned { fingerprint: normalize(&fingerprint), provider }))
            .with_no_client_auth(),
        None => {
            let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
            builder.with_root_certificates(roots).with_no_client_auth()
        }
    })
}

/// Fingerabdruck ohne Doppelpunkte und in Großbuchstaben, zum Vergleichen
fn normalize(fingerprint: &str) -> String {
    fingerprint.chars().filter(char::is_ascii_hexdigit).collect::<String>().to_uppercase()
}

/// Akzeptiert genau das hinterlegte (z. B. selbst signierte) Server-Zertifikat, unabhängig vom Hostnamen.
#[derive(Debug)]
struct Pinned {
    /// SHA-256 des Zertifikats, siehe [`normalize`]
    fingerprint: String,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if normalize(&fingerprint(end_entity.as_ref())) == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("Server-Zertifikat stimmt nicht mit cert bzw. fingerprint überein".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

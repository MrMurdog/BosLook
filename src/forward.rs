//! Weiterleitung an einen BosLook-Server (`[forward]` in der Konfiguration).
//!
//! Protokoll über WebSocket (`/api/ingest`): Der Client schickt `{"seq": n, "telegram": {...}}`,
//! der Server bestätigt mit `{"ack": n}`. Unbestätigte Telegramme werden gepuffert und nach
//! einem Verbindungsabbruch erneut gesendet.

use crate::config::ForwardConfig;
use crate::telegram::Telegram;
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime, pem::PemObject};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
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

pub async fn run(config: ForwardConfig, mut rx: mpsc::Receiver<Telegram>) {
    let mut queue = Queue { items: VecDeque::new(), next: 0 };
    loop {
        if let Err(e) = session(&config, &mut rx, &mut queue).await {
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

async fn session(config: &ForwardConfig, rx: &mut mpsc::Receiver<Telegram>, queue: &mut Queue) -> Result<()> {
    let url = format!("{}/api/ingest", config.server.trim_end_matches('/'))
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    let mut request = url.as_str().into_client_request()?;
    request.headers_mut().insert("x-boslook-client", config.name.parse()?);
    request.headers_mut().insert("authorization", format!("Bearer {}", config.token).parse()?);
    let connector = tokio_tungstenite::Connector::Rustls(Arc::new(tls_config(config)?));
    let (mut ws, _) = tokio_tungstenite::connect_async_tls_with_config(request, None, false, Some(connector))
        .await
        .context("Verbindung fehlgeschlagen")?;
    eprintln!("Weiterleitung an {}: verbunden", config.server);

    for (seq, t) in &queue.items {
        ws.send(text(*seq, t)).await?;
    }
    let mut ping = tokio::time::interval(PING);
    let mut last_seen = Instant::now();
    loop {
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
                    Some(Ok(Message::Text(text))) => {
                        let ack: Ack = serde_json::from_str(&text)?;
                        while queue.items.front().is_some_and(|(seq, _)| *seq <= ack.ack) {
                            queue.items.pop_front();
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => bail!("Verbindung vom Server beendet"),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.into()),
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

fn text(seq: u64, telegram: &Telegram) -> Message {
    let envelope = Envelope { seq, telegram: telegram.clone() };
    Message::Text(serde_json::to_string(&envelope).unwrap().into())
}

fn tls_config(config: &ForwardConfig) -> Result<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone()).with_safe_default_protocol_versions()?;
    Ok(match &config.cert {
        Some(path) => {
            let cert = CertificateDer::from_pem_file(path).with_context(|| format!("Zertifikat {path}"))?;
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(Pinned { cert, provider }))
                .with_no_client_auth()
        }
        None => {
            let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
            builder.with_root_certificates(roots).with_no_client_auth()
        }
    })
}

/// Akzeptiert genau das hinterlegte (z. B. selbst signierte) Server-Zertifikat, unabhängig vom Hostnamen.
#[derive(Debug)]
struct Pinned {
    cert: CertificateDer<'static>,
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
        if end_entity.as_ref() == self.cert.as_ref() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("Server-Zertifikat stimmt nicht mit forward.cert überein".into()))
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

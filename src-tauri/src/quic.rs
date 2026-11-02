//! QUIC transport (beta, `udp_port`): mirrors the TCP file + signaling
//! servers over UDP. Same JSON-line protocol; streams are dispatched by
//! content: `{"kind", ...}` is a signal frame, `{"name", ...}` a file offer.
//!
//! Nothing here changes TCP behaviour: when `udp_enabled` is off this
//! module is never used (the server doesn't even bind).

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, BufReader};

const ALPN: &[u8] = b"safedrop/1";
/// One JSON line per stream. A chat message may carry up to 10 images of up
/// to 700 KB each, so 1 MB was too small and such messages were dropped.
pub(crate) const MAX_LINE: usize = 12 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const STREAM_TIMEOUT: Duration = Duration::from_secs(5);

fn cert_paths(app_dir: &std::path::Path) -> (PathBuf, PathBuf) {
    (app_dir.join("quic_cert.der"), app_dir.join("quic_key.der"))
}

/// Load our self-signed QUIC cert, generating it once on first use.
pub(crate) fn load_or_gen_cert(app_dir: &std::path::Path) -> Result<(Vec<u8>, Vec<u8>), String> {
    let (cert_path, key_path) = cert_paths(app_dir);
    if let (Ok(c), Ok(k)) = (std::fs::read(&cert_path), std::fs::read(&key_path)) {
        if !c.is_empty() && !k.is_empty() {
            return Ok((c, k));
        }
    }
    let key = rcgen::generate_simple_self_signed(vec!["safedrop".into()])
        .map_err(|e| e.to_string())?;
    let cert_der = key.cert.der().to_vec();
    let key_der = key.key_pair.serialize_der();
    let _ = std::fs::create_dir_all(app_dir);
    std::fs::write(&cert_path, &cert_der).map_err(|e| e.to_string())?;
    std::fs::write(&key_path, &key_der).map_err(|e| e.to_string())?;
    Ok((cert_der, key_der))
}

pub(crate) fn ring_provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn server_crypto(app_dir: &std::path::Path) -> Result<quinn::crypto::rustls::QuicServerConfig, String> {
    let (cert_der, key_der) = load_or_gen_cert(app_dir)?;
    let cert = rustls::pki_types::CertificateDer::from(cert_der);
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(key_der.into());
    let mut inner = rustls::ServerConfig::builder_with_provider(ring_provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .map_err(|e| e.to_string())?;
    inner.alpn_protocols = vec![ALPN.to_vec()];
    quinn::crypto::rustls::QuicServerConfig::try_from(inner).map_err(|e| e.to_string())
}

/// Accept-anything verifier: our network is private (Radmin/LAN),
/// peers are identified by Radmin IP, not by certificate.
#[derive(Debug)]
pub(crate) struct SkipVerify;

impl rustls::client::danger::ServerCertVerifier for SkipVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        use rustls::SignatureScheme::*;
        vec![
            ECDSA_NISTP256_SHA256,
            ECDSA_NISTP384_SHA384,
            ECDSA_NISTP521_SHA512,
            ED25519,
            RSA_PSS_SHA256,
            RSA_PSS_SHA384,
            RSA_PSS_SHA512,
            RSA_PKCS1_SHA256,
        ]
    }
}

fn client_crypto() -> Result<quinn::crypto::rustls::QuicClientConfig, String> {
    let mut inner = rustls::ClientConfig::builder_with_provider(ring_provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| e.to_string())?
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    inner
        .dangerous()
        .set_certificate_verifier(Arc::new(SkipVerify));
    inner.alpn_protocols = vec![ALPN.to_vec()];
    quinn::crypto::rustls::QuicClientConfig::try_from(inner).map_err(|e| e.to_string())
}

static CLIENT_EP: tokio::sync::OnceCell<quinn::Endpoint> = tokio::sync::OnceCell::const_new();

/// Shared outbound QUIC endpoint (one UDP socket for all client streams).
pub async fn client_endpoint() -> Result<quinn::Endpoint, String> {
    CLIENT_EP
        .get_or_try_init(|| async {
            let mut ep = quinn::Endpoint::client("0.0.0.0:0".parse().unwrap())
                .map_err(|e| e.to_string())?;
            ep.set_default_client_config(quinn::ClientConfig::new(Arc::new(client_crypto()?)));
            Ok::<_, String>(ep)
        })
        .await
        .map(|ep| ep.clone())
        .map_err(|e| e.to_string())
}

async fn connect(ip: &str, port: u16) -> Result<quinn::Connection, String> {
    let ep = client_endpoint().await?;
    let addr: SocketAddr = format!("{ip}:{port}").parse().map_err(|e| format!("bad addr: {e}"))?;
    let connecting = ep
        .connect(addr, "safedrop")
        .map_err(|e| e.to_string())?;
    tokio::time::timeout(CONNECT_TIMEOUT, connecting)
        .await
        .map_err(|_| "quic timeout".to_string())?
        .map_err(|e| e.to_string())
}

async fn read_line_capped(
    reader: &mut (impl AsyncBufRead + Unpin),
    cap: usize,
) -> Result<Vec<u8>, String> {
    read_line_limited(reader, cap, STREAM_TIMEOUT.max(Duration::from_secs((cap / 200_000) as u64))).await
}

/// Reads one `\n`-terminated line, never buffering more than `cap + 1` bytes
/// and never waiting longer than `wait`: a peer that streams bytes without a
/// newline, or stalls mid-line, can't exhaust memory or hang the task.
pub(crate) async fn read_line_limited(
    reader: &mut (impl AsyncBufRead + Unpin),
    cap: usize,
    wait: Duration,
) -> Result<Vec<u8>, String> {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    tokio::time::timeout(wait, (&mut *reader).take(cap as u64 + 1).read_until(b'\n', &mut buf))
        .await
        .map_err(|_| "read timeout".to_string())?
        .map_err(|e| e.to_string())?;
    if buf.is_empty() {
        return Err("empty frame".into());
    }
    if buf.len() > cap {
        return Err("frame too big".into());
    }
    Ok(buf)
}

/// QUIC file + signaling server. Mirrors the TCP servers: same protocol,
/// same handlers, only the transport differs.
pub async fn run_quic_server(app: AppHandle, port: u16) {
    let app_dir: PathBuf = app
        .state::<crate::state::AppState>()
        .app_dir
        .clone();
    let crypto = match server_crypto(&app_dir) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SafeDrop QUIC crypto failed: {e}");
            return;
        }
    };
    let addr: SocketAddr = format!("0.0.0.0:{port}").parse().unwrap();
    let endpoint = match quinn::Endpoint::server(
        quinn::ServerConfig::with_crypto(Arc::new(crypto)),
        addr,
    ) {
        Ok(ep) => ep,
        Err(e) => {
            eprintln!("SafeDrop QUIC bind {addr} failed: {e}");
            return;
        }
    };
    println!("SafeDrop QUIC on {addr}");
    while let Some(incoming) = endpoint.accept().await {
        let app2 = app.clone();
        tokio::spawn(async move {
            let conn = match incoming.await {
                Ok(c) => c,
                Err(_) => return,
            };
            let ip = conn.remote_address().ip().to_string();
            loop {
                let (send, recv) = match conn.accept_bi().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let app3 = app2.clone();
                let ip3 = ip.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_quic_stream(app3, send, recv, ip3).await {
                        eprintln!("quic stream error: {e}");
                    }
                });
            }
        });
    }
}

async fn handle_quic_stream(
    app: AppHandle,
    mut send: quinn::SendStream,
    recv: quinn::RecvStream,
    ip: String,
) -> Result<(), String> {
    let mut reader = BufReader::new(recv);
    let buf = read_line_capped(&mut reader, MAX_LINE).await?;
    let v: serde_json::Value = serde_json::from_slice(&buf).map_err(|e| e.to_string())?;
    let r = if v.get("kind").is_some() {
        // signal frame
        let frame: crate::signal::Frame =
            serde_json::from_value(v).map_err(|e| e.to_string())?;
        crate::signal::process_frame(&app, frame, ip, &mut send).await
    } else {
        // file offer
        let offer: crate::transfer::Offer =
            serde_json::from_value(v).map_err(|e| e.to_string())?;
        let accepted = crate::transfer::authorize_offer(&app, &offer, &ip).await;
        if offer.confirm {
            let reply = serde_json::json!({"kind":"file-offer-result","accepted":accepted});
            let mut line = serde_json::to_vec(&reply).map_err(|e| e.to_string())?;
            line.push(b'\n');
            send.write_all(&line).await.map_err(|e| e.to_string())?;
        }
        if !accepted { let _ = send.finish(); return Ok(()); }
        let drop_dir: String = app
            .state::<crate::state::AppState>()
            .settings
            .lock()
            .await
            .loaddrop_dir
            .clone();
        crate::transfer::receive_stream(app, offer, &mut reader, ip, drop_dir, None)
            .await
            .map_err(|e| e.to_string())
    };
    // graceful FIN so the client knows we consumed everything
    let _ = send.finish();
    r
}

/// Wait for the peer's FIN: proves it consumed everything we sent.
/// Without this the connection may close before the peer reads (data loss).
async fn wait_peer_done(recv: &mut quinn::RecvStream, secs: u64) -> Result<(), String> {
    let mut tmp = [0u8; 64];
    loop {
        let r = tokio::time::timeout(Duration::from_secs(secs), recv.read(&mut tmp))
            .await
            .map_err(|_| "quic peer did not confirm".to_string())?
            .map_err(|e| e.to_string())?;
        match r {
            // clean FIN (or empty read): peer is done
            None | Some(0) => return Ok(()),
            // stray bytes: keep draining until FIN
            Some(_) => continue,
        }
    }
}

/// Send one signal frame over QUIC (mirrors signal::send_frame).
pub async fn send_frame_quic(
    ip: &str,
    port: u16,
    value: serde_json::Value,
    wait_reply: bool,
) -> Result<Option<serde_json::Value>, String> {
    let conn = connect(ip, port).await?;
    let (mut send, mut recv) = tokio::time::timeout(STREAM_TIMEOUT, conn.open_bi())
        .await
        .map_err(|_| "quic timeout".to_string())?
        .map_err(|e| e.to_string())?;
    let mut out = serde_json::to_string(&value).map_err(|e| e.to_string())?;
    out.push('\n');
    send.write_all(out.as_bytes()).await.map_err(|e| e.to_string())?;
    let _ = send.finish();
    if !wait_reply {
        wait_peer_done(&mut recv, 5).await?;
        return Ok(None);
    }
    let mut reader = BufReader::new(&mut recv);
    let buf = read_line_capped(&mut reader, MAX_LINE).await?;
    Ok(serde_json::from_slice::<serde_json::Value>(&buf).ok())
}

/// Dual-stack send: TCP first (5s budget), QUIC fallback when beta is on.
/// With beta off this behaves exactly like plain TCP (same timeouts).
pub async fn send_frame_dual(
    app: &AppHandle,
    ip: &str,
    sig_port: u16,
    value: serde_json::Value,
    wait_reply: bool,
) -> Result<Option<serde_json::Value>, String> {
    let (udp_on, udp_port) = {
        match app.state::<crate::state::AppState>().settings.try_lock() {
            Ok(s) => (s.udp_enabled, s.udp_port),
            Err(_) => (false, 47833),
        }
    };
    if !udp_on {
        return crate::signal::send_frame(ip, sig_port, value, wait_reply).await;
    }
    match tokio::time::timeout(
        Duration::from_secs(5),
        crate::signal::send_frame(ip, sig_port, value.clone(), wait_reply),
    )
    .await
    {
        Ok(Ok(r)) if !(wait_reply && r.is_none()) => Ok(r),
        _ => send_frame_quic(ip, udp_port, value, wait_reply).await,
    }
}

/// Dual-stack file send: TCP first (5s budget), QUIC fallback when beta is on.
pub async fn send_file_dual(
    app: &AppHandle,
    peer_ip: String,
    tcp_port: u16,
    file_path: String,
    device_name: String,
    kind: String,
    require_confirmation: bool,
    version: String,
) -> Result<String, String> {
    // QUIC fallback on a whole-file operation was unsafe: the 5s wrapper
    // cancelled a valid TCP upload mid-stream, then tried UDP even when the
    // remote peer had no QUIC listener. File data therefore always uses TCP.
    crate::transfer::send_file_to(
        app.clone(), peer_ip, tcp_port, file_path, device_name, kind, require_confirmation, version,
    ).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// QUIC plumbing smoke test: cert gen + endpoint bind + handshake +
    /// bidi echo over loopback. No Tauri state involved.
    #[tokio::test]
    async fn quic_loopback_echo() {
        let dir = std::env::temp_dir().join("safedrop-quic-test");
        let _ = std::fs::create_dir_all(&dir);
        let crypto = server_crypto(&dir).expect("server crypto");
        // second call must reuse the stored cert files
        let (c1, k1) = load_or_gen_cert(&dir).expect("cert reload");
        assert!(!c1.is_empty() && !k1.is_empty());

        let server_ep =
            quinn::Endpoint::server(quinn::ServerConfig::with_crypto(Arc::new(crypto)), "127.0.0.1:0".parse().unwrap())
                .expect("server bind");
        let port = server_ep.local_addr().unwrap().port();

        let server = tokio::spawn(async move {
            let incoming = server_ep.accept().await.expect("no incoming");
            let conn = incoming.await.expect("handshake");
            let (mut send, mut recv) = conn.accept_bi().await.expect("no stream");
            let mut buf = vec![0u8; 5];
            use tokio::io::AsyncReadExt;
            recv.read_exact(&mut buf).await.expect("read");
            assert_eq!(&buf, b"ping\n");
            send.write_all(b"pong\n").await.expect("write");
            let _ = send.finish();
            // keep the connection alive a beat so the client drains first
            tokio::time::sleep(Duration::from_millis(500)).await;
        });

        let conn = connect("127.0.0.1", port).await.expect("client connect");
        let (mut send, mut recv) = conn.open_bi().await.expect("open_bi");
        send.write_all(b"ping\n").await.expect("write");
        let _ = send.finish();
        let mut reader = BufReader::new(&mut recv);
        let mut line = String::new();
        use tokio::io::AsyncBufReadExt;
        reader.read_line(&mut line).await.expect("read line");
        assert_eq!(line, "pong\n");

        server.await.expect("server task");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Client roundtrip against a raw QUIC server speaking our JSON-line
    /// protocol: fire-and-forget (EOF handshake) + request/reply.
    #[tokio::test]
    async fn quic_frame_roundtrip() {
        let dir = std::env::temp_dir().join("safedrop-quic-test2");
        let _ = std::fs::create_dir_all(&dir);
        let crypto = server_crypto(&dir).expect("server crypto");
        let server_ep =
            quinn::Endpoint::server(quinn::ServerConfig::with_crypto(Arc::new(crypto)), "127.0.0.1:0".parse().unwrap())
                .expect("server bind");
        let port = server_ep.local_addr().unwrap().port();

        let server = tokio::spawn(async move {
            // mirror run_quic_server: accept loop + per-connection tasks.
            // the test opens exactly 2 connections to this server
            // (the dead-port probe goes to port 1), so accept exactly 2.
            for _ in 0..2 {
                let incoming = server_ep.accept().await.expect("no incoming");
                let conn = incoming.await.expect("handshake");
                tokio::spawn(async move {
                    loop {
                        let (mut send, mut recv) = match conn.accept_bi().await {
                            Ok(s) => s,
                            Err(_) => break,
                        };
                        let mut reader = BufReader::new(&mut recv);
                        let mut line = String::new();
                        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
                        if reader.read_line(&mut line).await.is_err() {
                            break;
                        }
                        if line.contains("hello") {
                            let _ = send.write_all(b"{\"kind\":\"hello-reply\"}\n").await;
                        }
                        let _ = send.finish();
                    }
                });
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        });

        // fire-and-forget must return Ok(None) after server FIN
        let r = send_frame_quic(
            "127.0.0.1",
            port,
            serde_json::json!({"kind": "ping"}),
            false,
        )
        .await
        .expect("fire-and-forget");
        assert!(r.is_none());

        // request/reply must return the parsed reply
        let r = send_frame_quic(
            "127.0.0.1",
            port,
            serde_json::json!({"kind": "hello"}),
            true,
        )
        .await
        .expect("request/reply");
        assert_eq!(r.unwrap().get("kind").and_then(|k| k.as_str()), Some("hello-reply"));

        // dead port must fail (not hang): proves fallback triggers on this path
        let bad = send_frame_quic("127.0.0.1", 1, serde_json::json!({"kind": "ping"}), false).await;
        assert!(bad.is_err());

        server.await.expect("server task");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod line_tests {
    use super::read_line_limited;
    use std::time::Duration;

    #[tokio::test]
    async fn line_reads_are_capped() {
        let mut ok: &[u8] = b"{\"kind\":\"x\"}\nrest";
        let line = read_line_limited(&mut ok, 100, Duration::from_secs(1)).await.unwrap();
        assert_eq!(line, b"{\"kind\":\"x\"}\n");
        // no newline within the cap: rejected after reading cap+1 bytes, the rest stays unread
        let big = vec![b'a'; 10_000];
        let mut r: &[u8] = &big;
        assert!(read_line_limited(&mut r, 100, Duration::from_secs(1)).await.is_err());
        assert_eq!(r.len(), 10_000 - 101);
    }
}

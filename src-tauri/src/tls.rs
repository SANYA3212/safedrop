//! Шифрование TCP-соединений (файлы 47831 и сигналы 47832), с 0.3.26.
//!
//! Сервер смотрит на первый байт: TLS-рукопожатие начинается с 0x16, старый
//! клиент шлёт JSON (`{`). Поэтому один порт принимает и новых, и старых.
//! Клиент шифрует, только если контакт сообщил версию 0.3.26+ (в hello); если
//! рукопожатие не удалось (контакт откатился на старую версию), соединение
//! повторяется без шифрования, а контакт перестаёт считаться «шифрующим».
//!
//! Ключи — тот же самоподписанный сертификат, что у QUIC. Сертификат не
//! проверяется: это защита от подслушивания по дороге (Radmin-ретранслятор,
//! чужая Wi-Fi), а не от подмены собеседника.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
/// Поток соединения: обычный TCP или TLS поверх него.
pub type Stream = Box<dyn Io>;

const TLS_SINCE: (u32, u32, u32) = (0, 3, 26);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_NAME: &str = "safedrop";

fn peers() -> &'static Mutex<HashSet<String>> {
    static P: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(HashSet::new()))
}

fn parse_ver(v: &str) -> Option<(u32, u32, u32)> {
    let mut it = v.trim().split('.').map(|p| p.parse::<u32>().ok());
    Some((it.next()??, it.next()??, it.next().flatten().unwrap_or(0)))
}

/// Запомнить версию контакта (из hello / peers.json): шифровать ли с ним.
pub fn note_peer_version(ip: &str, version: &str) {
    let ok = parse_ver(version).map(|v| v >= TLS_SINCE).unwrap_or(false);
    if let Ok(mut set) = peers().lock() {
        if ok { set.insert(ip.to_string()); } else { set.remove(ip); }
    }
}

pub fn peer_ok(ip: &str) -> bool {
    peers().lock().map(|s| s.contains(ip)).unwrap_or(false)
}

fn forget(ip: &str) {
    if let Ok(mut s) = peers().lock() { s.remove(ip); }
}

fn server_config() -> &'static OnceLock<Option<Arc<rustls::ServerConfig>>> {
    static S: OnceLock<Option<Arc<rustls::ServerConfig>>> = OnceLock::new();
    &S
}

/// Подготовить серверный TLS (сертификат из папки приложения). Без него сервер
/// принимает только незашифрованные соединения, как раньше.
pub fn init(app_dir: &Path) {
    server_config().get_or_init(|| {
        let (cert_der, key_der) = crate::quic::load_or_gen_cert(app_dir).ok()?;
        let cert = rustls::pki_types::CertificateDer::from(cert_der);
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(key_der.into());
        let cfg = rustls::ServerConfig::builder_with_provider(crate::quic::ring_provider())
            .with_protocol_versions(&[&rustls::version::TLS13]).ok()?
            .with_no_client_auth()
            .with_single_cert(vec![cert], key).ok()?;
        Some(Arc::new(cfg))
    });
}

fn client_config() -> Option<Arc<rustls::ClientConfig>> {
    static C: OnceLock<Option<Arc<rustls::ClientConfig>>> = OnceLock::new();
    C.get_or_init(|| {
        let cfg = rustls::ClientConfig::builder_with_provider(crate::quic::ring_provider())
            .with_protocol_versions(&[&rustls::version::TLS13]).ok()?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(crate::quic::SkipVerify))
            .with_no_client_auth();
        Some(Arc::new(cfg))
    }).clone()
}

/// Входящее соединение: TLS, если клиент начал рукопожатие, иначе как есть.
/// Второе значение — было ли соединение зашифровано.
pub async fn accept(sock: TcpStream) -> std::io::Result<(Stream, bool)> {
    let mut first = [0u8; 1];
    // зонды скана подключаются и молчат — не ждём их вечно
    let peeked = tokio::time::timeout(HANDSHAKE_TIMEOUT, sock.peek(&mut first)).await;
    let is_tls = matches!(peeked, Ok(Ok(1))) && first[0] == 0x16;
    if is_tls {
        if let Some(Some(cfg)) = server_config().get() {
            let acceptor = tokio_rustls::TlsAcceptor::from(cfg.clone());
            let tls = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(sock))
                .await
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "TLS handshake timeout"))??;
            return Ok((Box::new(tls), true));
        }
    }
    Ok((Box::new(sock), false))
}

async fn tcp(ip: &str, port: u16, timeout: Duration) -> Result<TcpStream, String> {
    let s = tokio::time::timeout(timeout, TcpStream::connect(format!("{ip}:{port}")))
        .await
        .map_err(|_| format!("тайм-аут подключения к {ip}:{port}"))?
        .map_err(|e| format!("connect {ip}:{port}: {e}"))?;
    let _ = s.set_nodelay(true);
    Ok(s)
}

/// Исходящее соединение: зашифрованное, если контакт это умеет.
pub async fn connect(ip: &str, port: u16, timeout: Duration) -> Result<Stream, String> {
    let sock = tcp(ip, port, timeout).await?;
    if !peer_ok(ip) {
        return Ok(Box::new(sock));
    }
    if let (Some(cfg), Ok(name)) = (client_config(), rustls::pki_types::ServerName::try_from(SERVER_NAME)) {
        let connector = tokio_rustls::TlsConnector::from(cfg);
        if let Ok(Ok(tls)) = tokio::time::timeout(HANDSHAKE_TIMEOUT, connector.connect(name.to_owned(), sock)).await {
            return Ok(Box::new(tls));
        }
    }
    // контакт не ответил на рукопожатие (старая версия) — как раньше, без шифрования
    forget(ip);
    Ok(Box::new(tcp(ip, port, timeout).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[test]
    fn version_gate() {
        note_peer_version("10.0.0.1", "0.3.26");
        note_peer_version("10.0.0.2", "0.3.25");
        note_peer_version("10.0.0.3", "");
        assert!(peer_ok("10.0.0.1"));
        assert!(!peer_ok("10.0.0.2"));
        assert!(!peer_ok("10.0.0.3"));
        note_peer_version("10.0.0.1", "0.3.20");
        assert!(!peer_ok("10.0.0.1"));
    }

    /// Сервер на одном порту принимает и TLS-клиента, и старого (без TLS).
    #[tokio::test]
    async fn one_port_serves_tls_and_plain() {
        let dir = std::env::temp_dir().join(format!("safedrop-tls-test-{}", std::process::id()));
        init(&dir);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            for _ in 0..2 {
                let (sock, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let (mut s, enc) = accept(sock).await.unwrap();
                    let mut line = String::new();
                    BufReader::new(&mut s).read_line(&mut line).await.unwrap();
                    let reply = format!("{}|{}", if enc { "tls" } else { "plain" }, line);
                    s.write_all(reply.as_bytes()).await.unwrap();
                    s.flush().await.unwrap();
                    let _ = s.shutdown().await;
                });
            }
        });
        // новый контакт — зашифровано
        note_peer_version("127.0.0.1", "0.3.26");
        let mut s = connect("127.0.0.1", port, Duration::from_secs(5)).await.unwrap();
        s.write_all(b"hello\n").await.unwrap();
        s.flush().await.unwrap();
        let mut got = String::new();
        BufReader::new(&mut s).read_line(&mut got).await.unwrap();
        assert_eq!(got, "tls|hello\n");
        // старый контакт — как раньше
        note_peer_version("127.0.0.1", "0.3.20");
        let mut s = connect("127.0.0.1", port, Duration::from_secs(5)).await.unwrap();
        s.write_all(b"hi\n").await.unwrap();
        s.flush().await.unwrap();
        let mut got = String::new();
        BufReader::new(&mut s).read_line(&mut got).await.unwrap();
        assert_eq!(got, "plain|hi\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

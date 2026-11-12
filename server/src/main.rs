//! SafeDrop Server — ретранслятор для режима «через сервер».
//!
//! Приложения держат с сервером управляющее TLS-соединение (ключ устройства
//! = ID), видят друг друга в списке и открывают каналы: сервер сводит два
//! TCP-соединения и пересылает байты, не расшифровывая их (внутри — свой
//! взаимный TLS устройств). Звонки идут через встроенный TURN (`turnsrv`).
//! Описание протокола — в `proto.rs`.

mod proto;
mod turnsrv;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use proto::*;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, Mutex, Notify, Semaphore};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Сколько ждать второй конец канала.
const PAIR_TIMEOUT: Duration = Duration::from_secs(20);
/// Канал без единого байта в обе стороны столько времени закрывается.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// Новые данные TURN — раньше, чем истекут старые.
const TURN_REFRESH: Duration = Duration::from_secs(12 * 3600);
const MAX_PENDING_PER_CLIENT: usize = 128;
const MAX_PENDING: usize = 8192;

// ---------- настройки ----------

struct Config {
    listen: SocketAddr,
    data_dir: PathBuf,
    password: String,
    turn_port: u16,
    relay_min: u16,
    relay_max: u16,
    public_ip: Option<Ipv4Addr>,
    max_clients: usize,
    max_conns: usize,
}

const HELP: &str = "SafeDrop Server — ретранслятор для режима «через сервер»

Использование: safedrop-server [параметры]

  --listen АДРЕС:ПОРТ   где принимать приложения (по умолчанию 0.0.0.0:47840)
  --data ПАПКА          где хранить ключ сервера (по умолчанию /var/lib/safedrop-server,
                        если она есть, иначе ./safedrop-server-data)
  --password ПАРОЛЬ     пароль для входа (или переменная SAFEDROP_SERVER_PASSWORD,
                        или --password-file ФАЙЛ); без пароля войти может любой
  --password-file ФАЙЛ  прочитать пароль из файла (первая строка)
  --public-ip IP        белый IPv4 сервера для звонков (по умолчанию — адрес
                        интерфейса, через который сервер выходит в интернет)
  --turn-port ПОРТ      UDP-порт TURN для звонков (по умолчанию 3478, 0 — выключить)
  --relay-ports A-B     внутренние UDP-порты TURN (по умолчанию 49160-49400;
                        снаружи не нужны, открывать их не надо)
  --max-clients N       сколько устройств может быть подключено (по умолчанию 500)
  --fingerprint         показать отпечаток ключа сервера и выйти
  --version             версия
  --help                эта справка

Порты, которые нужно открыть в файрволе: TCP 47840 и UDP 3478 (или те, что
заданы в --listen и --turn-port; можно один номер для обоих).";

fn parse_args() -> Result<(Config, bool), String> {
    let mut cfg = Config {
        listen: SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT)),
        data_dir: PathBuf::new(),
        password: std::env::var("SAFEDROP_SERVER_PASSWORD").unwrap_or_default(),
        turn_port: DEFAULT_TURN_PORT,
        relay_min: 49160,
        relay_max: 49400,
        public_ip: None,
        max_clients: 500,
        max_conns: 4096,
    };
    let mut print_fp = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = |name: &str| args.next().ok_or_else(|| format!("{name}: нужно значение"));
        match a.as_str() {
            "--listen" => cfg.listen = val("--listen")?.parse().map_err(|e| format!("--listen: {e}"))?,
            "--data" => cfg.data_dir = PathBuf::from(val("--data")?),
            "--password" => cfg.password = val("--password")?,
            "--password-file" => {
                let p = val("--password-file")?;
                let s = std::fs::read_to_string(&p).map_err(|e| format!("{p}: {e}"))?;
                cfg.password = s.lines().next().unwrap_or("").trim().to_string();
            }
            "--public-ip" => cfg.public_ip = Some(val("--public-ip")?.parse().map_err(|e| format!("--public-ip: {e}"))?),
            "--turn-port" => cfg.turn_port = val("--turn-port")?.parse().map_err(|e| format!("--turn-port: {e}"))?,
            "--relay-ports" => {
                let v = val("--relay-ports")?;
                let (a, b) = v.split_once('-').ok_or("--relay-ports: формат A-B")?;
                cfg.relay_min = a.trim().parse().map_err(|e| format!("--relay-ports: {e}"))?;
                cfg.relay_max = b.trim().parse().map_err(|e| format!("--relay-ports: {e}"))?;
                if cfg.relay_min == 0 || cfg.relay_max < cfg.relay_min {
                    return Err("--relay-ports: неверный диапазон".into());
                }
            }
            "--max-clients" => cfg.max_clients = val("--max-clients")?.parse().map_err(|e| format!("--max-clients: {e}"))?,
            "--fingerprint" => print_fp = true,
            "--version" | "-V" => {
                println!("safedrop-server {VERSION}");
                std::process::exit(0);
            }
            "--help" | "-h" => {
                println!("{HELP}");
                std::process::exit(0);
            }
            other => return Err(format!("неизвестный параметр {other} (см. --help)")),
        }
    }
    if cfg.data_dir.as_os_str().is_empty() {
        let sys = PathBuf::from("/var/lib/safedrop-server");
        cfg.data_dir = if sys.is_dir() { sys } else { PathBuf::from("safedrop-server-data") };
    }
    Ok((cfg, print_fp))
}

// ---------- случайные числа ----------

pub(crate) fn random_bytes<const N: usize>() -> [u8; N] {
    use ring::rand::SecureRandom;
    let mut b = [0u8; N];
    ring::rand::SystemRandom::new().fill(&mut b).expect("system random");
    b
}

pub(crate) fn random_hex(n: usize) -> String {
    (0..n).map(|_| format!("{:02x}", random_bytes::<1>()[0])).collect()
}

pub(crate) fn random_u32() -> u32 {
    u32::from_le_bytes(random_bytes::<4>())
}

// ---------- ключ сервера и TLS ----------

fn load_or_create_key(dir: &Path) -> Result<(Vec<u8>, Vec<u8>), String> {
    let cert_p = dir.join("server-cert.der");
    let key_p = dir.join("server-key.der");
    if let (Ok(c), Ok(k)) = (std::fs::read(&cert_p), std::fs::read(&key_p)) {
        return Ok((c, k));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let ck = rcgen::generate_simple_self_signed(vec!["safedrop-server".to_string()]).map_err(|e| e.to_string())?;
    let cert = ck.cert.der().to_vec();
    let key = ck.key_pair.serialize_der();
    write_private(&key_p, &key)?;
    std::fs::write(&cert_p, &cert).map_err(|e| format!("{}: {e}", cert_p.display()))?;
    Ok((cert, key))
}

fn write_private(p: &Path, data: &[u8]) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(p)
            .map_err(|e| format!("{}: {e}", p.display()))?;
        f.write_all(data).map_err(|e| format!("{}: {e}", p.display()))
    }
    #[cfg(not(unix))]
    {
        std::fs::write(p, data).map_err(|e| format!("{}: {e}", p.display()))
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Ключ клиента: любой самоподписанный, но с настоящей проверкой подписи
/// рукопожатия — клиент доказывает, что владеет ключом своего ID.
#[derive(Debug)]
struct DeviceKey(Arc<rustls::crypto::CryptoProvider>);

impl rustls::server::danger::ClientCertVerifier for DeviceKey {
    fn offer_client_auth(&self) -> bool {
        true
    }
    fn client_auth_mandatory(&self) -> bool {
        true
    }
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }
    fn verify_client_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        Ok(rustls::server::danger::ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(rustls::PeerIncompatible::Tls12NotOffered))
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn tls_config(cert: Vec<u8>, key: Vec<u8>) -> Result<Arc<rustls::ServerConfig>, String> {
    let p = provider();
    let mut cfg = rustls::ServerConfig::builder_with_provider(p.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| e.to_string())?
        .with_client_cert_verifier(Arc::new(DeviceKey(p)))
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(cert)],
            rustls::pki_types::PrivateKeyDer::Pkcs8(key.into()),
        )
        .map_err(|e| e.to_string())?;
    cfg.send_tls13_tickets = 0;
    Ok(Arc::new(cfg))
}

// ---------- состояние ----------

struct Client {
    conn: u64,
    tx: mpsc::Sender<String>,
    kick: Arc<Notify>,
    name: String,
    ver: String,
}

struct Pending {
    from: String,
    at: Instant,
    /// первый подключившийся конец ждёт здесь второй
    waiter: Option<oneshot::Sender<TcpStream>>,
    joined: bool,
}

struct Hub {
    clients: Mutex<HashMap<String, Client>>,
    pending: Mutex<HashMap<String, Pending>>,
    next_conn: AtomicU64,
    password: String,
    max_clients: usize,
    turn: Option<turnsrv::Turn>,
}

fn line<T: serde::Serialize>(m: &T) -> String {
    let mut s = serde_json::to_string(m).unwrap_or_default();
    s.push('\n');
    s
}

fn short(id: &str) -> &str {
    &id[..id.len().min(8)]
}

fn same_secret(a: &str, b: &str) -> bool {
    use sha2::{Digest, Sha256};
    let (x, y) = (Sha256::digest(a.as_bytes()), Sha256::digest(b.as_bytes()));
    x.iter().zip(y.iter()).fold(0u8, |acc, (p, q)| acc | (p ^ q)) == 0
}

impl Hub {
    fn turn_info(&self) -> Option<TurnInfo> {
        let t = self.turn.as_ref()?;
        let (user, cred, expires) = t.credentials()?;
        Some(TurnInfo { port: t.port, user, cred, expires })
    }

    async fn broadcast_peers(&self) {
        let g = self.clients.lock().await;
        let all: Vec<PeerInfo> = g
            .iter()
            .map(|(id, c)| PeerInfo { id: id.clone(), name: c.name.clone(), ver: c.ver.clone() })
            .collect();
        for (id, c) in g.iter() {
            let list: Vec<PeerInfo> = all.iter().filter(|p| p.id != *id).cloned().collect();
            let _ = c.tx.try_send(line(&ToClient::Peers { list }));
        }
    }

    async fn send_to(&self, id: &str, m: &ToClient) {
        if let Some(c) = self.clients.lock().await.get(id) {
            let _ = c.tx.try_send(line(m));
        }
    }

    /// Открыть канал from → to. Ошибка — текст для `Fail`.
    async fn open(&self, from: &str, to: &str, port: &str, tok: &str) -> Result<(), String> {
        if !valid_id(to) || !valid_port(port) || !valid_token(tok) {
            return Err("неверный запрос".into());
        }
        if to == from {
            return Err("это же устройство".into());
        }
        let target = self.clients.lock().await.get(to).map(|c| c.tx.clone());
        let Some(target) = target else { return Err("не в сети".into()) };
        {
            let mut p = self.pending.lock().await;
            if p.len() >= MAX_PENDING {
                return Err("сервер занят".into());
            }
            if p.values().filter(|e| e.from == from).count() >= MAX_PENDING_PER_CLIENT {
                return Err("слишком много каналов сразу".into());
            }
            if p.contains_key(tok) {
                return Err("токен занят".into());
            }
            p.insert(tok.to_string(), Pending { from: from.into(), at: Instant::now(), waiter: None, joined: false });
        }
        let msg = line(&ToClient::Incoming { from: from.into(), port: port.into(), tok: tok.into() });
        if target.try_send(msg).is_err() {
            self.pending.lock().await.remove(tok);
            return Err("адресат не успевает".into());
        }
        Ok(())
    }

    async fn sweep(&self) {
        let mut p = self.pending.lock().await;
        p.retain(|_, e| e.joined || e.at.elapsed() < PAIR_TIMEOUT + Duration::from_secs(10));
    }
}

// ---------- соединения ----------

async fn read_line_limited<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R, max: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let buf = r.fill_buf().await.map_err(|e| e.to_string())?;
        if buf.is_empty() {
            return if out.is_empty() { Err("закрыто".into()) } else { Ok(out) };
        }
        if let Some(i) = buf.iter().position(|b| *b == b'\n') {
            out.extend_from_slice(&buf[..i]);
            r.consume(i + 1);
            return Ok(out);
        }
        let n = buf.len();
        out.extend_from_slice(buf);
        r.consume(n);
        if out.len() > max {
            return Err("слишком длинная строка".into());
        }
    }
}

async fn handle_conn(hub: Arc<Hub>, tls: tokio_rustls::TlsAcceptor, sock: TcpStream, addr: SocketAddr) {
    let _ = sock.set_nodelay(true);
    let mut first = [0u8; 1];
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, sock.peek(&mut first)).await {
        Ok(Ok(1)) => {}
        _ => return,
    }
    match first[0] {
        0x16 => {
            let s = match tokio::time::timeout(HANDSHAKE_TIMEOUT, tls.accept(sock)).await {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    eprintln!("{addr}: TLS: {e}");
                    return;
                }
                Err(_) => return,
            };
            let id = s.get_ref().1.peer_certificates().and_then(|c| c.first()).map(|c| fp_of(c));
            let Some(id) = id else { return };
            control(hub, s, id, addr).await;
        }
        b if b == DATA_MAGIC[0] => data(hub, sock).await,
        _ => {}
    }
}

async fn control(hub: Arc<Hub>, s: tokio_rustls::server::TlsStream<TcpStream>, id: String, addr: SocketAddr) {
    let (r, mut w) = tokio::io::split(s);
    let mut reader = BufReader::new(r);
    let hello = match tokio::time::timeout(HANDSHAKE_TIMEOUT, read_line_limited(&mut reader, MAX_CTL_LINE)).await {
        Ok(Ok(l)) => serde_json::from_slice::<ToServer>(&l).ok(),
        _ => None,
    };
    let Some(ToServer::Hello { pass, name, ver, .. }) = hello else { return };
    let deny = |msg: &str| line(&ToClient::Deny { msg: msg.into() });
    if !hub.password.is_empty() && !same_secret(&pass, &hub.password) {
        eprintln!("{addr}: {} — неверный пароль", short(&id));
        // медленнее перебирать пароли
        tokio::time::sleep(Duration::from_secs(2)).await;
        let _ = w.write_all(deny("неверный пароль сервера").as_bytes()).await;
        let _ = w.shutdown().await;
        return;
    }
    let (tx, mut rx) = mpsc::channel::<String>(512);
    let kick = Arc::new(Notify::new());
    let conn = hub.next_conn.fetch_add(1, Ordering::Relaxed);
    {
        let mut g = hub.clients.lock().await;
        if g.len() >= hub.max_clients && !g.contains_key(&id) {
            drop(g);
            let _ = w.write_all(deny("сервер переполнен").as_bytes()).await;
            let _ = w.shutdown().await;
            return;
        }
        let client = Client { conn, tx: tx.clone(), kick: kick.clone(), name: clean_name(&name), ver: clean_name(&ver) };
        if let Some(old) = g.insert(id.clone(), client) {
            // то же устройство подключилось заново (сменилась сеть): старое закрыть
            old.kick.notify_one();
        }
    }
    eprintln!("{addr}: + {} «{}» {}", short(&id), clean_name(&name), clean_name(&ver));
    let welcome = ToClient::Welcome { id: id.clone(), server_ver: VERSION.into(), turn: hub.turn_info() };
    let _ = tx.try_send(line(&welcome));
    hub.broadcast_peers().await;

    let wkick = kick.clone();
    let writer = tokio::spawn(async move {
        while let Some(l) = rx.recv().await {
            let ok = tokio::time::timeout(Duration::from_secs(30), async {
                w.write_all(l.as_bytes()).await?;
                w.flush().await
            })
            .await;
            if !matches!(ok, Ok(Ok(()))) {
                break;
            }
        }
        wkick.notify_one();
        let _ = w.shutdown().await;
    });

    let mut turn_at = Instant::now();
    // смена имени рассылает список всем: не чаще раза в пару секунд
    let mut name_at: Option<Instant> = None;
    loop {
        let got = tokio::select! {
            r = tokio::time::timeout(Duration::from_secs(PING_SECS * 3), read_line_limited(&mut reader, MAX_CTL_LINE)) => r,
            _ = kick.notified() => break,
        };
        let Ok(Ok(l)) = got else { break };
        let Ok(msg) = serde_json::from_slice::<ToServer>(&l) else { continue };
        match msg {
            ToServer::Open { to, port, tok } => {
                if let Err(e) = hub.open(&id, &to, &port, &tok).await {
                    let _ = tx.try_send(line(&ToClient::Fail { tok, msg: e }));
                }
            }
            ToServer::Name { name } => {
                if name_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
                    continue;
                }
                name_at = Some(Instant::now());
                let changed = {
                    let mut g = hub.clients.lock().await;
                    match g.get_mut(&id) {
                        Some(c) if c.conn == conn && c.name != clean_name(&name) => {
                            c.name = clean_name(&name);
                            true
                        }
                        _ => false,
                    }
                };
                if changed {
                    hub.broadcast_peers().await;
                }
            }
            ToServer::Ping => {
                let _ = tx.try_send(line(&ToClient::Pong));
                if turn_at.elapsed() >= TURN_REFRESH {
                    turn_at = Instant::now();
                    if let Some(turn) = hub.turn_info() {
                        let _ = tx.try_send(line(&ToClient::Turn { turn }));
                    }
                }
            }
            ToServer::Hello { .. } | ToServer::Unknown => {}
        }
    }
    drop(tx);
    writer.abort();
    let removed = {
        let mut g = hub.clients.lock().await;
        if g.get(&id).map(|c| c.conn) == Some(conn) {
            g.remove(&id);
            true
        } else {
            false
        }
    };
    if removed {
        eprintln!("{addr}: - {}", short(&id));
        hub.broadcast_peers().await;
    }
}

async fn data(hub: Arc<Hub>, mut sock: TcpStream) {
    let mut head = [0u8; 4 + TOKEN_LEN];
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, sock.read_exact(&mut head)).await {
        Ok(Ok(_)) => {}
        _ => return,
    }
    if &head[..4] != DATA_MAGIC {
        return;
    }
    let Ok(tok) = std::str::from_utf8(&head[4..]).map(str::to_string) else { return };
    if !valid_token(&tok) {
        return;
    }
    // клиент подключается каналом сразу после `open`: запрос мог ещё не дойти
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut p = hub.pending.lock().await;
        if let Some(e) = p.get_mut(&tok) {
            if let Some(w) = e.waiter.take() {
                p.remove(&tok);
                drop(p);
                let _ = w.send(sock);
                return;
            }
            if e.joined {
                return; // третий с тем же токеном
            }
            let (tx, rx) = oneshot::channel();
            e.waiter = Some(tx);
            e.joined = true;
            let from = e.from.clone();
            drop(p);
            match tokio::time::timeout(PAIR_TIMEOUT, rx).await {
                Ok(Ok(other)) => pump(sock, other).await,
                _ => {
                    hub.pending.lock().await.remove(&tok);
                    hub.send_to(&from, &ToClient::Fail { tok, msg: "собеседник не ответил".into() }).await;
                }
            }
            return;
        }
        drop(p);
        if Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Переслать байты между двумя концами канала (полузакрытие сохраняется:
/// TLS внутри канала на нём завершает соединение).
async fn pump(a: TcpStream, b: TcpStream) {
    let _ = b.set_nodelay(true);
    let last = Arc::new(AtomicU64::new(now_ms()));
    let (ar, aw) = a.into_split();
    let (br, bw) = b.into_split();
    tokio::join!(half(ar, bw, last.clone()), half(br, aw, last));
}

async fn half(mut r: tokio::net::tcp::OwnedReadHalf, mut w: tokio::net::tcp::OwnedWriteHalf, last: Arc<AtomicU64>) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match tokio::time::timeout(Duration::from_secs(30), r.read(&mut buf)).await {
            Ok(Ok(0)) | Ok(Err(_)) => break,
            Ok(Ok(n)) => {
                last.store(now_ms(), Ordering::Relaxed);
                match tokio::time::timeout(IDLE_TIMEOUT, w.write_all(&buf[..n])).await {
                    Ok(Ok(())) => {}
                    _ => break,
                }
            }
            Err(_) => {
                // тишина в эту сторону — нормально (файл идёт в другую);
                // закрываем, только если молчат обе стороны
                if now_ms().saturating_sub(last.load(Ordering::Relaxed)) >= IDLE_TIMEOUT.as_millis() as u64 {
                    break;
                }
            }
        }
    }
    let _ = w.shutdown().await;
}

fn outbound_ipv4() -> Option<Ipv4Addr> {
    // без отправки пакетов: только выбор маршрута ядром
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(v4) if !v4.is_unspecified() => Some(v4),
        _ => None,
    }
}

#[tokio::main]
async fn main() {
    let (cfg, print_fp) = match parse_args() {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let (cert, key) = match load_or_create_key(&cfg.data_dir) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("ключ сервера: {e}");
            std::process::exit(1);
        }
    };
    let fp = fp_of(&cert);
    if print_fp {
        println!("{fp}");
        return;
    }
    let tls = match tls_config(cert, key) {
        Ok(c) => tokio_rustls::TlsAcceptor::from(c),
        Err(e) => {
            eprintln!("TLS: {e}");
            std::process::exit(1);
        }
    };
    let turn = if cfg.turn_port == 0 {
        None
    } else {
        let ip = cfg.public_ip.or_else(outbound_ipv4).unwrap_or(Ipv4Addr::LOCALHOST);
        match turnsrv::start(cfg.turn_port, ip, cfg.relay_min, cfg.relay_max).await {
            Ok(t) => {
                eprintln!("TURN: UDP {} (потоки {}-{}), адрес для звонков {ip}", cfg.turn_port, cfg.relay_min, cfg.relay_max);
                Some(t)
            }
            Err(e) => {
                eprintln!("{e} — звонки через сервер работать не будут");
                None
            }
        }
    };
    let hub = Arc::new(Hub {
        clients: Mutex::new(HashMap::new()),
        pending: Mutex::new(HashMap::new()),
        next_conn: AtomicU64::new(1),
        password: cfg.password.clone(),
        max_clients: cfg.max_clients,
        turn,
    });
    let listener = match TcpListener::bind(cfg.listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{}: {e}", cfg.listen);
            std::process::exit(1);
        }
    };
    eprintln!("SafeDrop Server {VERSION} на {}", cfg.listen);
    eprintln!("ключ сервера: {fp}");
    eprintln!("данные: {}", cfg.data_dir.display());
    if cfg.password.is_empty() {
        eprintln!("ВНИМАНИЕ: пароль не задан — подключиться может любой, кто знает адрес");
    }
    {
        let hub = hub.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                hub.sweep().await;
            }
        });
    }
    let slots = Arc::new(Semaphore::new(cfg.max_conns));
    loop {
        let (sock, addr) = match listener.accept().await {
            Ok(x) => x,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else { continue };
        let (hub, tls) = (hub.clone(), tls.clone());
        tokio::spawn(async move {
            let _permit = permit;
            handle_conn(hub, tls, sock, addr).await;
        });
    }
}

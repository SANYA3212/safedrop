//! Режим «через сервер»: вместо прямых соединений по LAN/Radmin всё идёт
//! через SafeDrop Server (папка `server/`, протокол — `server/src/proto.rs`).
//!
//! Контакт на сервере — адрес `sd-<ID>`, где ID — отпечаток ключа его
//! устройства. Такой адрес живёт везде, где раньше был IP (контакты, чаты,
//! группы), а соединение к нему открывается каналом через сервер. Внутри
//! канала — тот же взаимный TLS, что и напрямую, и ключ собеседника обязан
//! совпасть с ID из адреса (`tls::connect`/`check_inbound`): сервер видит
//! только, кто с кем соединяется, но не содержимое, и подменить собеседника
//! не может. Звонки идут через TURN этого же сервера (`ice`).
//!
//! Ключ сервера запоминается при первом подключении (как ключи контактов);
//! другой ключ — соединение не устанавливается, пока пользователь не
//! подтвердит новый во вкладке «Сервер».

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Notify, Semaphore};

use crate::state::{AppState, Peer};

#[path = "../../server/src/proto.rs"]
mod proto;
use proto::{PeerInfo, ToClient, ToServer, TurnInfo};

pub const PREFIX: &str = "sd-";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// входящих каналов одновременно (как у прямых серверов сигналов и файлов)
const SIG_SLOTS: usize = 128;
const FILE_SLOTS: usize = 32;

pub const MODE_P2P: &str = "p2p";
pub const MODE_SERVER: &str = "server";

/// Что пользователь выбрал во вкладке «Сервер» (`server.json`).
#[derive(serde::Serialize, serde::Deserialize, Clone, Default, Debug, PartialEq)]
pub struct ServerConf {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub addr: String,
    #[serde(default)]
    pub pass: String,
    /// запомненный ключ сервера
    #[serde(default)]
    pub fp: String,
}

/// Состояние для интерфейса (событие `safedrop-server`).
#[derive(serde::Serialize, Clone, Default, Debug)]
pub struct Status {
    pub mode: String,
    pub addr: String,
    pub pass: String,
    pub fp: String,
    /// пришёл другой ключ сервера (ждёт подтверждения)
    pub pending_fp: String,
    /// off | connecting | online | error
    pub state: String,
    pub error: String,
    pub my_id: String,
    pub server_ver: String,
    pub peers: Vec<PeerInfo>,
    pub turn: bool,
    pub rtt_ms: Option<u64>,
}

#[derive(Default)]
struct Shared {
    conf: ServerConf,
    path: Option<PathBuf>,
    state: String,
    error: String,
    pending_fp: String,
    server_ver: String,
    host: String,
    port: u16,
    peers: HashMap<String, PeerInfo>,
    turn: Option<TurnInfo>,
    tx: Option<mpsc::Sender<String>>,
    rtt_ms: Option<u64>,
    /// пинг до сервера у других устройств (из `Rtts`)
    peer_rtt: HashMap<String, u64>,
    /// сервер умеет раздавать пинги (новая версия); у старого — только свой
    rtts_known: bool,
    sig_port: u16,
    file_port: u16,
}

fn shared() -> &'static Mutex<Shared> {
    static S: OnceLock<Mutex<Shared>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Shared::default()))
}

fn lock() -> std::sync::MutexGuard<'static, Shared> {
    shared().lock().unwrap_or_else(|e| e.into_inner())
}

static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Разбудить цикл подключения (сменились настройки).
fn kick() -> &'static Notify {
    static K: OnceLock<Notify> = OnceLock::new();
    K.get_or_init(Notify::new)
}

fn slots(file: bool) -> Arc<Semaphore> {
    static S: OnceLock<(Arc<Semaphore>, Arc<Semaphore>)> = OnceLock::new();
    let s = S.get_or_init(|| (Arc::new(Semaphore::new(SIG_SLOTS)), Arc::new(Semaphore::new(FILE_SLOTS))));
    if file { s.1.clone() } else { s.0.clone() }
}

// ---------- адреса ----------

/// Адрес контакта через сервер (`sd-` + 64 hex).
pub fn is_relay(addr: &str) -> bool {
    id_of(addr).is_some()
}

pub fn id_of(addr: &str) -> Option<&str> {
    addr.strip_prefix(PREFIX).filter(|id| proto::valid_id(id))
}

pub fn addr_of(id: &str) -> String {
    format!("{PREFIX}{id}")
}

/// Включён ли режим «через сервер» (прямые соединения при этом не используются).
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Свой адрес в режиме сервера (по нему себя узнают в группах).
pub fn my_addr() -> Option<String> {
    if !active() {
        return None;
    }
    let fp = crate::tls::my_fp();
    (!fp.is_empty()).then(|| addr_of(&fp))
}

/// Пинг до собеседника через сервер: путь туда и обратно идёт через сервер,
/// значит, это свой пинг до сервера плюс его. Пока замера нет — `None`
/// (а не выдуманная цифра). Старый сервер пинги не раздаёт — тогда свой.
fn peer_ping(g: &Shared, id: &str) -> Option<u64> {
    let mine = g.rtt_ms?;
    if !g.rtts_known {
        return Some(mine.max(1));
    }
    g.peer_rtt.get(id).map(|theirs| (mine + theirs).max(1))
}

/// «В сети ли» контакт через сервер (вместо TCP-пинга); число — пинг до него,
/// 0 — в сети, но пинг ещё не замерен.
pub fn ping(addr: &str) -> Option<u64> {
    let id = id_of(addr)?;
    let g = lock();
    (g.state == "online" && g.peers.contains_key(id)).then(|| peer_ping(&g, id).unwrap_or(0))
}

/// Разобрать «хост[:порт]» (IPv6 — в квадратных скобках).
fn split_addr(s: &str) -> Result<(String, u16), String> {
    let s = s.trim();
    let bad = || "адрес сервера: IP или имя, можно с портом (1.2.3.4:47840)".to_string();
    if s.is_empty() {
        return Err(bad());
    }
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let (h, tail) = rest.split_once(']').ok_or_else(bad)?;
        let port = match tail.strip_prefix(':') {
            Some(p) => p.parse::<u16>().map_err(|_| bad())?,
            None if tail.is_empty() => proto::DEFAULT_PORT,
            None => return Err(bad()),
        };
        (h.to_string(), port)
    } else {
        match s.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') => (h.to_string(), p.parse::<u16>().map_err(|_| bad())?),
            Some(_) => (s.to_string(), proto::DEFAULT_PORT), // голый IPv6
            None => (s.to_string(), proto::DEFAULT_PORT),
        }
    };
    let ok = !host.is_empty()
        && host.len() <= 253
        && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == ':');
    if !ok || port == 0 {
        return Err(bad());
    }
    Ok((host, port))
}

fn url_host(host: &str) -> String {
    if host.contains(':') { format!("[{host}]") } else { host.to_string() }
}

// ---------- настройки ----------

fn save(g: &Shared) {
    if let (Some(path), Ok(data)) = (&g.path, serde_json::to_string_pretty(&g.conf)) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, data).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

pub fn init(app_dir: &Path) {
    let path = app_dir.join("server.json");
    let conf = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<ServerConf>(&s).ok())
        .unwrap_or_default();
    ACTIVE.store(conf.mode == MODE_SERVER, Ordering::Relaxed);
    let mut g = lock();
    g.conf = conf;
    g.path = Some(path);
    g.state = "off".into();
}

pub fn status() -> Status {
    let g = lock();
    let mut peers: Vec<PeerInfo> = g.peers.values().cloned().collect();
    peers.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
    Status {
        mode: if g.conf.mode == MODE_SERVER { MODE_SERVER.into() } else { MODE_P2P.into() },
        addr: g.conf.addr.clone(),
        pass: g.conf.pass.clone(),
        fp: g.conf.fp.clone(),
        pending_fp: g.pending_fp.clone(),
        state: if g.state.is_empty() { "off".into() } else { g.state.clone() },
        error: g.error.clone(),
        my_id: crate::tls::my_fp(),
        server_ver: g.server_ver.clone(),
        peers,
        turn: g.turn.is_some(),
        rtt_ms: g.rtt_ms,
    }
}

fn emit_status(app: &AppHandle) {
    let _ = app.emit("safedrop-server", status());
}

/// Сохранить выбор пользователя и переподключиться.
pub fn configure(app: &AppHandle, mode: &str, addr: &str, pass: &str) -> Result<Status, String> {
    let mode = if mode == MODE_SERVER { MODE_SERVER } else { MODE_P2P };
    let addr = addr.trim().to_string();
    // в P2P адрес только хранится: опечатка в нём не мешает вернуться на P2P
    if mode == MODE_SERVER {
        split_addr(&addr)?;
    }
    if addr.chars().count() > 300 || pass.chars().count() > 256 {
        return Err("адрес или пароль слишком длинный".into());
    }
    {
        let mut g = lock();
        if g.conf.addr != addr {
            // другой сервер — другой ключ
            g.conf.fp.clear();
            g.pending_fp.clear();
        }
        g.conf.mode = mode.into();
        g.conf.addr = addr;
        g.conf.pass = pass.to_string();
        save(&g);
    }
    ACTIVE.store(mode == MODE_SERVER, Ordering::Relaxed);
    kick().notify_waiters();
    emit_status(app);
    Ok(status())
}

/// Пользователь подтвердил новый ключ сервера.
pub fn trust_key(app: &AppHandle, expected: &str) -> Result<(), String> {
    {
        let mut g = lock();
        if g.pending_fp.is_empty() {
            return Err("новый ключ сервера не приходил".into());
        }
        if g.pending_fp != expected {
            return Err("пришёл ещё один ключ — проверьте заново".into());
        }
        g.conf.fp = std::mem::take(&mut g.pending_fp);
        save(&g);
    }
    kick().notify_waiters();
    emit_status(app);
    Ok(())
}

/// «Сбросить всё»: забыть сервер.
pub fn reset() {
    ACTIVE.store(false, Ordering::Relaxed);
    let mut g = lock();
    g.conf = ServerConf::default();
    g.pending_fp.clear();
    if let Some(p) = &g.path {
        let _ = std::fs::remove_file(p);
    }
    drop(g);
    kick().notify_waiters();
}

/// Как WebRTC соединяться в текущем режиме (для `rtcConfig` в app.js).
pub fn ice() -> serde_json::Value {
    let g = lock();
    if g.conf.mode != MODE_SERVER {
        return serde_json::json!({ "mode": MODE_P2P });
    }
    match (&g.turn, g.state.as_str()) {
        (Some(t), "online") => serde_json::json!({
            "mode": MODE_SERVER,
            "urls": [format!("turn:{}:{}?transport=udp", url_host(&g.host), t.port)],
            "username": t.user,
            "credential": t.cred,
        }),
        _ => serde_json::json!({ "mode": MODE_SERVER }),
    }
}

/// Ник сменился — показать его остальным на сервере.
pub fn send_name(name: &str) {
    let tx = lock().tx.clone();
    if let Some(tx) = tx {
        let _ = tx.try_send(line(&ToServer::Name { name: proto::clean_name(name) }));
    }
}

fn line<T: serde::Serialize>(m: &T) -> String {
    let mut s = serde_json::to_string(m).unwrap_or_default();
    s.push('\n');
    s
}

// ---------- каналы ----------

async fn data_conn(host: &str, port: u16, tok: &str) -> Result<TcpStream, String> {
    let mut s = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(|_| "сервер не отвечает".to_string())?
        .map_err(|e| format!("сервер {host}:{port}: {e}"))?;
    let _ = s.set_nodelay(true);
    let mut head = Vec::with_capacity(4 + proto::TOKEN_LEN);
    head.extend_from_slice(proto::DATA_MAGIC);
    head.extend_from_slice(tok.as_bytes());
    s.write_all(&head).await.map_err(|e| e.to_string())?;
    Ok(s)
}

/// Открыть канал к контакту `addr` на его порт `port` (сигналы или файлы).
/// Возвращает «сырой» поток: TLS с собеседником поверх него ставит `tls::connect`.
pub async fn open(addr: &str, port: u16) -> Result<TcpStream, String> {
    let id = id_of(addr).ok_or("неверный адрес контакта")?.to_string();
    let (tx, host, sport, kind) = {
        let g = lock();
        if g.conf.mode != MODE_SERVER {
            return Err("режим «через сервер» выключен".into());
        }
        if g.state != "online" {
            return Err("нет связи с сервером SafeDrop".into());
        }
        let kind = if port == g.sig_port {
            proto::PORT_SIG
        } else if port == g.file_port {
            proto::PORT_FILE
        } else {
            return Err(format!("порт {port} через сервер не поддерживается"));
        };
        if !g.peers.contains_key(&id) {
            return Err("контакт сейчас не подключён к серверу".into());
        }
        (g.tx.clone().ok_or("нет связи с сервером SafeDrop")?, g.host.clone(), g.port, kind)
    };
    let tok = uuid::Uuid::new_v4().simple().to_string();
    tx.send(line(&ToServer::Open { to: id, port: kind.into(), tok: tok.clone() }))
        .await
        .map_err(|_| "связь с сервером прервалась".to_string())?;
    data_conn(&host, sport, &tok).await
}

/// Кто-то открыл канал к нам: подключиться и обслужить как обычное входящее.
async fn incoming(app: AppHandle, host: String, port: u16, from: String, kind: String, tok: String) {
    let file = kind == proto::PORT_FILE;
    let Ok(_permit) = slots(file).try_acquire_owned() else { return };
    let r: Result<(), String> = async {
        let sock = data_conn(&host, port, &tok).await?;
        let (stream, auth) = crate::tls::accept_relayed(sock).await.map_err(|e| e.to_string())?;
        let ip = addr_of(&from);
        crate::tls::check_inbound(&ip, &auth)?;
        if file {
            crate::transfer::serve_relayed(app, stream, ip).await
        } else {
            crate::signal::serve_frame(app, stream, ip).await
        }
    }
    .await;
    if let Err(e) = r {
        eprintln!("relay {kind}: {e}");
    }
}

// ---------- соединение с сервером ----------

fn set_state(app: &AppHandle, state: &str, error: &str) {
    {
        let mut g = lock();
        g.state = state.into();
        g.error = error.into();
    }
    emit_status(app);
}

/// Почему сессия с сервером закончилась.
enum Stop {
    /// связь: переподключаться с нарастающей паузой
    Retry(String),
    /// сервер отказал (пароль) или сменил ключ: ждать, пока пользователь
    /// не поменяет настройки, — повторять то же самое бессмысленно
    Wait(String),
}

impl From<String> for Stop {
    fn from(e: String) -> Self {
        Stop::Retry(e)
    }
}

impl From<&str> for Stop {
    fn from(e: &str) -> Self {
        Stop::Retry(e.to_string())
    }
}

/// Запустить фоновое подключение (один раз при старте).
pub fn start(app: AppHandle, sig_port: u16, file_port: u16) {
    {
        let mut g = lock();
        g.sig_port = sig_port;
        g.file_port = file_port;
    }
    tauri::async_runtime::spawn(async move {
        let mut backoff = 2u64;
        loop {
            // подписка до чтения настроек: изменение между ними не потеряется
            let woke = kick().notified();
            tokio::pin!(woke);
            woke.as_mut().enable();
            let conf = lock().conf.clone();
            if conf.mode != MODE_SERVER || conf.addr.trim().is_empty() {
                set_state(&app, "off", if conf.mode == MODE_SERVER { "не указан адрес сервера" } else { "" });
                woke.await;
                continue;
            }
            match session(&app, &conf, woke.as_mut()).await {
                Ok(()) => backoff = 2,
                Err(Stop::Retry(e)) => {
                    set_state(&app, "error", &e);
                    eprintln!("SafeDrop server: {e}");
                    let wait = Duration::from_secs(backoff);
                    backoff = (backoff * 2).min(60);
                    let _ = tokio::time::timeout(wait, woke).await;
                }
                Err(Stop::Wait(e)) => {
                    set_state(&app, "error", &e);
                    eprintln!("SafeDrop server: {e}");
                    backoff = 2;
                    let _ = tokio::time::timeout(Duration::from_secs(600), woke).await;
                }
            }
        }
    });
}

async fn session(
    app: &AppHandle,
    conf: &ServerConf,
    mut woke: std::pin::Pin<&mut tokio::sync::futures::Notified<'_>>,
) -> Result<(), Stop> {
    let (host, port) = split_addr(&conf.addr)?;
    // причина прошлой неудачи остаётся видна, пока идёт новая попытка
    lock().state = "connecting".into();
    emit_status(app);
    let sock = tokio::select! {
        r = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), port))) => r
            .map_err(|_| format!("сервер {host}:{port} не отвечает"))?
            .map_err(|e| format!("не удалось подключиться к {host}:{port}: {e}"))?,
        _ = woke.as_mut() => return Ok(()),
    };
    let _ = sock.set_nodelay(true);
    let cfg = crate::tls::client_config().ok_or("нет ключа устройства")?;
    let name = rustls::pki_types::ServerName::try_from("safedrop").map_err(|e| e.to_string())?;
    let tls = tokio::time::timeout(CONNECT_TIMEOUT, tokio_rustls::TlsConnector::from(cfg).connect(name, sock))
        .await
        .map_err(|_| "сервер не завершил TLS-рукопожатие".to_string())?
        .map_err(|e| format!("TLS с сервером: {e}"))?;
    let fp = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|c| c.first())
        .map(|c| crate::tls::fp_of(c))
        .ok_or("сервер не показал ключ")?;
    {
        let mut g = lock();
        if g.conf.fp.is_empty() {
            g.conf.fp = fp.clone();
            save(&g);
        } else if g.conf.fp != fp {
            g.pending_fp = fp;
            return Err(Stop::Wait("Ключ сервера изменился. Если сервер переустанавливали — подтвердите новый ключ; иначе это может быть подмена".into()));
        }
        g.pending_fp.clear();
    }
    let (r, mut w) = tokio::io::split(tls);
    let nick = {
        let st: tauri::State<'_, AppState> = app.state();
        let s = st.settings.lock().await;
        if s.profile.nickname.is_empty() { s.device_name.clone() } else { s.profile.nickname.clone() }
    };
    let hello = ToServer::Hello { proto: proto::PROTO, pass: conf.pass.clone(), name: proto::clean_name(&nick), ver: crate::app_version() };
    w.write_all(line(&hello).as_bytes()).await.map_err(|e| e.to_string())?;
    w.flush().await.map_err(|e| e.to_string())?;

    // чтение — отдельной задачей: строка, прочитанная наполовину, не теряется
    // при срабатывании таймера в select!
    let (ltx, mut lrx) = mpsc::channel::<Result<Vec<u8>, String>>(64);
    let reader = tokio::spawn(async move {
        let mut br = BufReader::new(r);
        loop {
            let got = crate::quic::read_line_limited(&mut br, proto::MAX_CTL_LINE, Duration::from_secs(proto::PING_SECS * 3)).await;
            let stop = got.is_err();
            if ltx.send(got).await.is_err() || stop {
                break;
            }
        }
    });
    let (tx, mut rx) = mpsc::channel::<String>(256);
    let writer = tokio::spawn(async move {
        while let Some(l) = rx.recv().await {
            let ok = tokio::time::timeout(Duration::from_secs(20), async {
                w.write_all(l.as_bytes()).await?;
                w.flush().await
            })
            .await;
            if !matches!(ok, Ok(Ok(()))) {
                break;
            }
        }
        let _ = w.shutdown().await;
    });
    {
        let mut g = lock();
        g.tx = Some(tx.clone());
        g.host = host.clone();
        g.port = port;
    }
    // первый ping — сразу (первый тик interval мгновенный): пинг виден через
    // долю секунды после подключения, а не через PING_SECS
    let mut ping = tokio::time::interval(Duration::from_secs(proto::PING_SECS));
    let mut ping_at: Option<Instant> = None;
    let result: Result<(), Stop> = loop {
        tokio::select! {
            _ = woke.as_mut() => break Ok(()),
            _ = ping.tick() => {
                ping_at = Some(Instant::now());
                if tx.try_send(line(&ToServer::Ping)).is_err() {
                    break Err("связь с сервером потеряна".into());
                }
            }
            got = lrx.recv() => {
                let l = match got {
                    Some(Ok(l)) => l,
                    _ => break Err("связь с сервером потеряна".into()),
                };
                let Ok(msg) = serde_json::from_slice::<ToClient>(&l) else { continue };
                match msg {
                    ToClient::Welcome { id, server_ver, turn } => {
                        if id != crate::tls::my_fp() {
                            break Err("сервер назвал чужой ID устройства".into());
                        }
                        {
                            let mut g = lock();
                            g.server_ver = proto::clean_name(&server_ver);
                            g.turn = turn.filter(|t| t.port != 0);
                        }
                        set_state(app, "online", "");
                    }
                    ToClient::Deny { msg } => break Err(Stop::Wait(format!("Сервер отказал: {}", proto::clean_name(&msg)))),
                    ToClient::Peers { list } => {
                        let mine = crate::tls::my_fp();
                        {
                            let mut g = lock();
                            g.peers = list
                                .into_iter()
                                .filter(|p| proto::valid_id(&p.id) && p.id != mine)
                                .take(2000)
                                .map(|p| (p.id.clone(), PeerInfo { id: p.id, name: proto::clean_name(&p.name), ver: proto::clean_name(&p.ver) }))
                                .collect();
                        }
                        emit_status(app);
                        apply_presence(app).await;
                    }
                    ToClient::Incoming { from, port: kind, tok } => {
                        if proto::valid_id(&from) && proto::valid_port(&kind) && proto::valid_token(&tok) {
                            tokio::spawn(incoming(app.clone(), host.clone(), port, from, kind, tok));
                        }
                    }
                    ToClient::Turn { turn } => {
                        lock().turn = (turn.port != 0).then_some(turn);
                        emit_status(app);
                    }
                    ToClient::Pong => {
                        if let Some(t) = ping_at.take() {
                            let ms = t.elapsed().as_millis() as u64;
                            lock().rtt_ms = Some(ms);
                            emit_status(app);
                            // сообщить свой пинг: сервер ответит пингами остальных
                            // 0 у сервера значит «неизвестен»: сервер рядом — это 1 мс
                            let ms = ms.clamp(1, proto::MAX_RTT_MS as u64) as u32;
                            let _ = tx.try_send(line(&ToServer::Rtt { ms }));
                        }
                    }
                    ToClient::Rtts { list } => {
                        {
                            let mut g = lock();
                            g.rtts_known = true;
                            g.peer_rtt = list
                                .into_iter()
                                .filter(|r| proto::valid_id(&r.id) && r.ms <= proto::MAX_RTT_MS)
                                .take(2000)
                                .map(|r| (r.id, r.ms as u64))
                                .collect();
                        }
                        apply_presence(app).await;
                    }
                    ToClient::Fail { .. } | ToClient::Unknown => {}
                }
            }
        }
    };
    reader.abort();
    writer.abort();
    {
        let mut g = lock();
        g.tx = None;
        g.peers.clear();
        g.turn = None;
        g.rtt_ms = None;
        g.peer_rtt.clear();
        g.rtts_known = false;
        g.state = "off".into();
    }
    emit_status(app);
    result
}

/// Список устройств на сервере → контакты: новые добавить, пришедшим в
/// сеть — запросить профиль. Ушедших помечает «не в сети» общий
/// heartbeat (`signal::dead_online`, через `ping`), как и в LAN.
pub async fn apply_presence(app: &AppHandle) {
    let (online, list, pings) = {
        let g = lock();
        let pings: HashMap<String, Option<u64>> = g.peers.keys().map(|id| (id.clone(), peer_ping(&g, id))).collect();
        (g.state == "online", g.peers.values().cloned().collect::<Vec<_>>(), pings)
    };
    if !online {
        return;
    }
    let state: tauri::State<'_, AppState> = app.state();
    let mine = crate::tls::my_fp();
    let mut fresh = Vec::new();
    {
        let mut g = state.peers.lock().await;
        let now = chrono::Local::now().format("%H:%M:%S").to_string();
        for info in &list {
            if info.id == mine {
                continue;
            }
            let ip = addr_of(&info.id);
            let rtt = pings.get(&info.id).copied().flatten();
            // этот компьютер может уже быть контактом по LAN/Radmin (тот же ключ)
            let owner = crate::ident::attach(&mut g, &ip).filter(|key| g.contains_key(key));
            match owner {
                Some(key) => {
                    if let Some(p) = g.get_mut(&key) {
                        if !p.online {
                            fresh.push(p.ip.clone());
                        }
                        p.online = true;
                        p.ping_ms = rtt;
                        p.last_seen = Some(now.clone());
                    }
                }
                None => {
                    let name = if info.name.is_empty() { ip.clone() } else { info.name.clone() };
                    g.insert(
                        ip.clone(),
                        Peer {
                            ip: ip.clone(),
                            name,
                            nickname: info.name.clone(),
                            avatar: String::new(),
                            custom_avatar: String::new(),
                            trusted: false,
                            online: true,
                            supports_file_confirm: false,
                            supports_chat_ack: false,
                            supports_watch: false,
                            ping_ms: rtt,
                            last_seen: Some(now.clone()),
                            phash: String::new(),
                            state: "idle".into(),
                            detail: String::new(),
                            ips: vec![ip.clone()],
                            version: info.ver.clone(),
                        },
                    );
                    fresh.push(ip);
                }
            }
        }
    }
    crate::settings::persist_peers(&state);
    let peers: Vec<Peer> = state.peers.lock().await.values().cloned().collect();
    let _ = app.emit("safedrop-peers", peers);
    for ip in fresh {
        let a = app.clone();
        tokio::spawn(async move { crate::signal::announce_to(&a, &ip).await });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_addresses() {
        let id = "ab".repeat(32);
        let a = addr_of(&id);
        assert!(is_relay(&a));
        assert_eq!(id_of(&a), Some(id.as_str()));
        assert!(!is_relay("26.1.2.3"));
        assert!(!is_relay("sd-xyz"));
        assert!(!is_relay(&format!("sd-{}", "AB".repeat(32))));
    }

    #[test]
    fn server_address_forms() {
        assert_eq!(split_addr("1.2.3.4").unwrap(), ("1.2.3.4".into(), 47840));
        assert_eq!(split_addr(" 1.2.3.4:5000 ").unwrap(), ("1.2.3.4".into(), 5000));
        assert_eq!(split_addr("relay.example.org").unwrap(), ("relay.example.org".into(), 47840));
        assert_eq!(split_addr("[2001:db8::1]:47841").unwrap(), ("2001:db8::1".into(), 47841));
        assert_eq!(split_addr("2001:db8::1").unwrap(), ("2001:db8::1".into(), 47840));
        assert!(split_addr("").is_err());
        assert!(split_addr("1.2.3.4:0").is_err());
        assert!(split_addr("1.2.3.4:http").is_err());
        assert!(split_addr("a b").is_err());
        assert!(split_addr("evil/path").is_err());
        assert_eq!(url_host("2001:db8::1"), "[2001:db8::1]");
    }
}

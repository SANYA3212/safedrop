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
pub(crate) mod proto;
pub(crate) use proto::DevInfo;
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
    /// это устройство внутри аккаунта (у ПК и телефона с одним ключом — разные)
    #[serde(default)]
    pub dev: String,
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
    /// другие свои устройства в сети (тот же аккаунт)
    pub siblings: Vec<DevInfo>,
    /// сервер умеет несколько устройств на аккаунт и вход по коду
    pub multi: bool,
    /// ключ этого устройства пришёл с другого своего (вход по коду)
    pub linked: bool,
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
    multi: bool,
    /// свои другие устройства в сети
    siblings: Vec<DevInfo>,
    /// звонок с контактом идёт с этим его устройством (ID → dev, когда)
    call_pins: HashMap<String, (String, Instant)>,
    /// свои исходящие звонки, на которые ещё не ответили (ID → приглашение)
    out_calls: HashMap<String, OutCall>,
    /// ждём новое устройство по коду (`pair::start`)
    pair: Option<crate::pair::Waiting>,
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

// ---------- устройства аккаунта ----------
//
// У одного аккаунта (ключа) может быть несколько устройств: ПК и телефон.
// Сообщения, файлы и звонок контакту рассылаются на все его устройства
// (`plan`), ответы в звонке — тому устройству, с которым звонок идёт
// (`pin_call`). Свои другие устройства — `siblings` (синхронизация — `own`).

tokio::task_local! {
    /// к какому устройству контакта открывать каналы (`to_dev`)
    static TARGET_DEV: String;
    /// с какого устройства контакта пришёл обрабатываемый кадр
    static FROM_DEV: String;
}

/// Выполнить отправку к одному устройству контакта.
pub async fn to_dev<F: std::future::Future>(dev: String, f: F) -> F::Output {
    TARGET_DEV.scope(dev, f).await
}

/// То же, но только если устройство известно (иначе — как обычно).
pub async fn maybe_to_dev<F: std::future::Future>(dev: Option<String>, f: F) -> F::Output {
    match dev {
        Some(d) => TARGET_DEV.scope(d, f).await,
        None => f.await,
    }
}

fn target_dev() -> Option<String> {
    TARGET_DEV.try_with(|d| d.clone()).ok()
}

/// С какого устройства контакта пришёл текущий кадр (через сервер).
pub fn from_dev() -> Option<String> {
    FROM_DEV.try_with(|d| d.clone()).ok()
}

/// Кадры звонка: после ответа идут только устройству, с которым звонок.
const CALL_KINDS: &[&str] = &["call-offer", "call-answer", "ice", "call-hangup", "screen-offer", "screen-answer", "screen-ice", "screen-stop"];
/// Звонок дольше этого без единого кадра — не тот же звонок.
const PIN_FOR: Duration = Duration::from_secs(12 * 3600);

/// Куда слать кадр `kind` контакту `addr`.
pub enum Plan {
    /// как раньше: одно соединение (устройство выберет сервер или `to_dev`)
    Default,
    One(String),
    All(Vec<String>),
}

pub fn plan(addr: &str, kind: &str) -> Plan {
    if target_dev().is_some() {
        return Plan::Default;
    }
    let Some(id) = id_of(addr) else { return Plan::Default };
    let mut g = lock();
    if !g.multi {
        return Plan::Default;
    }
    let devs: Vec<String> = g.peers.get(id).map(|p| p.devs.iter().map(|d| d.dev.clone()).collect()).unwrap_or_default();
    if CALL_KINDS.contains(&kind) {
        // новый звонок — снова всем устройствам; ответ — тому, кто звонит
        if kind == "call-offer" {
            g.call_pins.remove(id);
        } else if let Some((dev, at)) = g.call_pins.get(id).cloned() {
            if at.elapsed() < PIN_FOR && devs.contains(&dev) {
                if kind == "call-hangup" {
                    g.call_pins.remove(id);
                } else if let Some(p) = g.call_pins.get_mut(id) {
                    p.1 = Instant::now();
                }
                return Plan::One(dev);
            }
            g.call_pins.remove(id);
        }
    }
    if devs.len() > 1 { Plan::All(devs) } else { Plan::Default }
}

/// Исходящий звонок, пока не ответили: его приглашение и кандидаты ICE. Если у
/// вызываемого появится новое устройство (телефон разбудил сторож, и SafeDrop
/// на нём запустился), приглашение уходит и туда.
pub(crate) struct OutCall {
    offer: serde_json::Value,
    ice: Vec<serde_json::Value>,
    at: Instant,
    devs: Vec<String>,
}

/// Звонок ждёт ответа не дольше этого.
const OUT_CALL_FOR: Duration = Duration::from_secs(70);

/// Кадр звонка уходит контакту `addr` (из `signal::send_frame`): запомнить
/// приглашение и разбудить сторожей его телефона (`Ring`).
pub fn note_out_frame(addr: &str, kind: &str, value: &serde_json::Value) {
    // повтор приглашения новому устройству (`resend_out_calls`) — не новый звонок
    if target_dev().is_some() {
        return;
    }
    let Some(id) = id_of(addr) else { return };
    let ring = {
        let mut g = lock();
        if !g.multi {
            return;
        }
        match kind {
            "call-offer" => {
                let devs = g.peers.get(id).map(|p| p.devs.iter().map(|d| d.dev.clone()).collect()).unwrap_or_default();
                g.out_calls.insert(id.to_string(), OutCall { offer: value.clone(), ice: Vec::new(), at: Instant::now(), devs });
                g.out_calls.retain(|_, c| c.at.elapsed() < OUT_CALL_FOR);
                Some("")
            }
            "ice" => {
                if let Some(c) = g.out_calls.get_mut(id).filter(|c| c.ice.len() < 64) {
                    c.ice.push(value.clone());
                }
                None
            }
            // сбросил, пока не ответили: сторож покажет «пропущенный»
            "call-hangup" => g.out_calls.remove(id).filter(|c| c.at.elapsed() < OUT_CALL_FOR).map(|_| proto::RING_STOP),
            _ => None,
        }
    };
    if let Some(state) = ring {
        let _ = ctl_send(&ToServer::Ring { to: id.to_string(), state: state.into() });
    }
}

/// На звонок ответили: больше никому его не досылать.
pub fn note_answered(addr: &str) {
    if let Some(id) = id_of(addr) {
        lock().out_calls.remove(id);
    }
}

/// Список устройств обновился: тем, кто подключился во время звонка, —
/// приглашение и уже отправленные кандидаты.
fn resend_out_calls(app: &AppHandle) {
    let jobs: Vec<(String, String, serde_json::Value, Vec<serde_json::Value>, u16)> = {
        let mut g = lock();
        let sig_port = g.sig_port;
        g.out_calls.retain(|_, c| c.at.elapsed() < OUT_CALL_FOR);
        let mut jobs = Vec::new();
        let peers = g.peers.clone();
        for (id, c) in g.out_calls.iter_mut() {
            let Some(p) = peers.get(id) else { continue };
            for d in &p.devs {
                if !c.devs.contains(&d.dev) {
                    c.devs.push(d.dev.clone());
                    jobs.push((addr_of(id), d.dev.clone(), c.offer.clone(), c.ice.clone(), sig_port));
                }
            }
        }
        jobs
    };
    for (addr, dev, offer, ice, port) in jobs {
        let app = app.clone();
        tokio::spawn(async move {
            let _ = app;
            to_dev(dev, async move {
                if crate::signal::send_frame(&addr, port, offer, false).await.is_ok() {
                    for v in ice {
                        let _ = crate::signal::send_frame(&addr, port, v, false).await;
                    }
                }
            })
            .await;
        });
    }
}

/// Вызываемый отклонил звонок со сторожа (SafeDrop на телефоне был закрыт).
async fn ring_declined(app: &AppHandle, from: &str) {
    let had = lock().out_calls.remove(from).is_some();
    if !had {
        return;
    }
    let addr = addr_of(from);
    unpin_call(&addr);
    let _ = app.emit("safedrop-hangup", serde_json::json!({ "ip": addr, "from": "" }));
    // его другие устройства (ПК) ещё звонят — отбой и им
    let port = lock().sig_port;
    let nick = {
        let st: tauri::State<'_, AppState> = app.state();
        let s = st.settings.lock().await;
        if s.profile.nickname.is_empty() { s.device_name.clone() } else { s.profile.nickname.clone() }
    };
    let v = serde_json::json!({ "kind": "call-hangup", "from": nick });
    let _ = crate::signal::send_frame(&addr, port, v, false).await;
}

/// Звонок с контактом идёт с устройством, с которого пришёл текущий кадр.
pub fn pin_call(addr: &str) {
    let (Some(id), Some(dev)) = (id_of(addr), from_dev()) else { return };
    lock().call_pins.insert(id.to_string(), (dev, Instant::now()));
}

/// Кадр пришёл не с того устройства контакта, с которым идёт звонок.
pub fn other_call_dev(addr: &str) -> bool {
    let (Some(id), Some(dev)) = (id_of(addr), from_dev()) else { return false };
    let g = lock();
    matches!(g.call_pins.get(id), Some((pinned, at)) if *pinned != dev && at.elapsed() < PIN_FOR)
}

pub fn unpin_call(addr: &str) {
    if let Some(id) = id_of(addr) {
        lock().call_pins.remove(id);
    }
}

/// Ожидание нового устройства по коду (`pair`).
pub(crate) fn with_pair<R>(f: impl FnOnce(&mut Option<crate::pair::Waiting>) -> R) -> R {
    f(&mut lock().pair)
}

/// Отправить сообщение серверу по текущему соединению.
pub(crate) fn ctl_send(m: &ToServer) -> Result<(), String> {
    let tx = lock().tx.clone().ok_or("нет связи с сервером SafeDrop")?;
    tx.try_send(line(m)).map_err(|_| "связь с сервером занята — попробуйте ещё раз".to_string())
}

/// Сервер на связи и умеет несколько устройств на аккаунт.
pub(crate) fn multi_online() -> bool {
    let g = lock();
    g.state == "online" && g.multi
}

pub(crate) fn conf() -> ServerConf {
    lock().conf.clone()
}

/// Записать настройки сервера целиком (новое устройство вошло по коду).
pub(crate) fn set_conf(app: &AppHandle, conf: ServerConf) {
    ACTIVE.store(conf.mode == MODE_SERVER, Ordering::Relaxed);
    {
        let mut g = lock();
        g.conf = conf;
        g.pending_fp.clear();
        save(&g);
    }
    kick().notify_waiters();
    emit_status(app);
}

pub(crate) fn split_addr_pub(s: &str) -> Result<(String, u16), String> {
    split_addr(s)
}

/// Свои другие устройства в сети.
pub fn siblings() -> Vec<DevInfo> {
    let g = lock();
    if g.state == "online" { g.siblings.clone() } else { Vec::new() }
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
    let mut conf = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<ServerConf>(&s).ok())
        .unwrap_or_default();
    ACTIVE.store(conf.mode == MODE_SERVER, Ordering::Relaxed);
    let fresh_dev = conf.dev.is_empty() || !proto::valid_dev(&conf.dev);
    if fresh_dev {
        conf.dev = new_dev();
    }
    let mut g = lock();
    g.conf = conf;
    g.path = Some(path);
    g.state = "off".into();
    if fresh_dev && g.conf.mode == MODE_SERVER {
        save(&g);
    }
}

fn new_dev() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_string()
}

/// Это устройство внутри аккаунта.
pub fn my_dev() -> String {
    let mut g = lock();
    if g.conf.dev.is_empty() {
        g.conf.dev = new_dev();
    }
    g.conf.dev.clone()
}

fn my_form() -> &'static str {
    if cfg!(target_os = "android") { proto::FORM_PHONE } else { proto::FORM_DESKTOP }
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
        siblings: g.siblings.clone(),
        multi: g.multi,
        linked: g.path.as_ref().and_then(|p| p.parent()).is_some_and(|d| d.join(LINKED_MARK).is_file()),
    }
}

/// Файл-отметка: ключ получен входом по коду (`pair::install`).
pub(crate) const LINKED_MARK: &str = "account-linked";

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
        if g.conf.dev.is_empty() {
            g.conf.dev = new_dev();
        }
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
    let dev = target_dev().unwrap_or_default();
    open_raw(&tx, &host, sport, &id, kind, &dev).await
}

/// Канал к устройству `dev` аккаунта `id` (к своему — тоже, `own`).
async fn open_raw(tx: &mpsc::Sender<String>, host: &str, sport: u16, id: &str, kind: &str, dev: &str) -> Result<TcpStream, String> {
    let tok = uuid::Uuid::new_v4().simple().to_string();
    tx.send(line(&ToServer::Open { to: id.to_string(), port: kind.into(), tok: tok.clone(), dev: dev.to_string() }))
        .await
        .map_err(|_| "связь с сервером прервалась".to_string())?;
    data_conn(host, sport, &tok).await
}

/// Канал к своему другому устройству: TLS, и ответить обязан свой же ключ.
/// Запомненные ключи контактов при этом не трогаются.
pub async fn own_connect(dev: &str) -> Result<crate::tls::Stream, String> {
    let mine = crate::tls::my_fp();
    let (tx, host, sport) = {
        let g = lock();
        if g.conf.mode != MODE_SERVER || g.state != "online" {
            return Err("нет связи с сервером SafeDrop".into());
        }
        if !g.siblings.iter().any(|d| d.dev == dev) {
            return Err("своё устройство не в сети".into());
        }
        (g.tx.clone().ok_or("нет связи с сервером SafeDrop")?, g.host.clone(), g.port)
    };
    let sock = tokio::time::timeout(CONNECT_TIMEOUT, open_raw(&tx, &host, sport, &mine, proto::PORT_SIG, dev))
        .await
        .map_err(|_| "тайм-аут канала к своему устройству".to_string())??;
    let cfg = crate::tls::client_config().ok_or("нет ключа устройства")?;
    let name = rustls::pki_types::ServerName::try_from("safedrop").map_err(|e| e.to_string())?;
    let tls = tokio::time::timeout(CONNECT_TIMEOUT, tokio_rustls::TlsConnector::from(cfg).connect(name, sock))
        .await
        .map_err(|_| "своё устройство не завершило TLS".to_string())?
        .map_err(|e| format!("TLS со своим устройством: {e}"))?;
    let fp = tls.get_ref().1.peer_certificates().and_then(|c| c.first()).map(|c| crate::tls::fp_of(c));
    if mine.is_empty() || fp.as_deref() != Some(mine.as_str()) {
        return Err("ответило не своё устройство (другой ключ)".into());
    }
    Ok(Box::new(tls))
}

/// Канал сопряжения к тому, кто ждёт код (вход нового устройства).
pub(crate) async fn data_conn_pub(host: &str, port: u16, tok: &str) -> Result<TcpStream, String> {
    data_conn(host, port, tok).await
}

/// Кто-то открыл канал к нам: подключиться и обслужить как обычное входящее.
async fn incoming(app: AppHandle, host: String, port: u16, from: String, kind: String, tok: String, dev: String) {
    let file = kind == proto::PORT_FILE;
    let Ok(_permit) = slots(file).try_acquire_owned() else { return };
    let r: Result<(), String> = async {
        let sock = data_conn(&host, port, &tok).await?;
        let (stream, auth) = crate::tls::accept_relayed(sock).await.map_err(|e| e.to_string())?;
        if from == crate::tls::my_fp() {
            // своё другое устройство: ключ — свой же, кадры — синхронизация
            if auth.fp.as_deref() != Some(from.as_str()) || file {
                return Err("канал от своего ID: не тот ключ".into());
            }
            return crate::own::serve(app, stream, dev).await;
        }
        let ip = addr_of(&from);
        crate::tls::check_inbound(&ip, &auth)?;
        FROM_DEV
            .scope(dev, async move {
                if file {
                    crate::transfer::serve_relayed(app, stream, ip).await
                } else {
                    crate::signal::serve_frame(app, stream, ip).await
                }
            })
            .await
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
    let hello = ToServer::Hello {
        proto: proto::PROTO,
        pass: conf.pass.clone(),
        name: proto::clean_name(&nick),
        ver: crate::app_version(),
        dev: my_dev(),
        form: my_form().into(),
        quiet: false,
        watch: false,
    };
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
                    ToClient::Welcome { id, server_ver, turn, multi } => {
                        if id != crate::tls::my_fp() {
                            break Err("сервер назвал чужой ID устройства".into());
                        }
                        {
                            let mut g = lock();
                            g.server_ver = proto::clean_name(&server_ver);
                            g.turn = turn.filter(|t| t.port != 0);
                            g.multi = multi;
                        }
                        set_state(app, "online", "");
                    }
                    ToClient::Deny { msg } => break Err(Stop::Wait(format!("Сервер отказал: {}", proto::clean_name(&msg)))),
                    ToClient::Peers { list } => {
                        let mine = crate::tls::my_fp();
                        let me_dev = my_dev();
                        let new_sibling = {
                            let mut g = lock();
                            let clean = |d: &DevInfo| DevInfo { dev: d.dev.clone(), form: proto::clean_form(&d.form), ver: proto::clean_name(&d.ver) };
                            let siblings: Vec<DevInfo> = list
                                .iter()
                                .find(|p| p.id == mine)
                                .map(|p| {
                                    p.devs.iter()
                                        .filter(|d| !d.dev.is_empty() && d.dev != me_dev && proto::valid_dev(&d.dev))
                                        .take(proto::MAX_DEVS)
                                        .map(clean)
                                        .collect()
                                })
                                .unwrap_or_default();
                            let fresh = siblings.iter().any(|d| !g.siblings.iter().any(|o| o.dev == d.dev));
                            g.siblings = siblings;
                            g.peers = list
                                .into_iter()
                                .filter(|p| proto::valid_id(&p.id) && p.id != mine)
                                .take(2000)
                                .map(|p| {
                                    let devs = p.devs.iter().filter(|d| proto::valid_dev(&d.dev)).take(proto::MAX_DEVS).map(clean).collect();
                                    (p.id.clone(), PeerInfo { id: p.id, name: proto::clean_name(&p.name), ver: proto::clean_name(&p.ver), devs })
                                })
                                .collect();
                            fresh
                        };
                        emit_status(app);
                        apply_presence(app).await;
                        resend_out_calls(app);
                        if new_sibling {
                            crate::own::siblings_online(app);
                        }
                    }
                    ToClient::Incoming { from, port: kind, tok, dev } => {
                        if !proto::valid_id(&from) || !proto::valid_token(&tok) || !proto::valid_dev(&dev) {
                            continue;
                        }
                        if kind == proto::PORT_PAIR {
                            // только если сами ждём новое устройство по коду
                            if crate::pair::waiting() {
                                tokio::spawn(crate::pair::host(app.clone(), host.clone(), port, from, tok));
                            }
                        } else if proto::valid_port(&kind) {
                            tokio::spawn(incoming(app.clone(), host.clone(), port, from, kind, tok, dev));
                        }
                    }
                    ToClient::Ring { from, state, .. } => {
                        if state == proto::RING_DECLINE && proto::valid_id(&from) {
                            let a = app.clone();
                            tokio::spawn(async move { ring_declined(&a, &from).await });
                        }
                    }
                    ToClient::PairFail { slot, msg } => {
                        crate::pair::failed(app, &slot, &proto::clean_name(&msg));
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
    let had_pair = {
        let mut g = lock();
        g.tx = None;
        g.peers.clear();
        g.siblings.clear();
        g.call_pins.clear();
        g.out_calls.clear();
        let had_pair = g.pair.take().is_some();
        g.turn = None;
        g.rtt_ms = None;
        g.peer_rtt.clear();
        g.rtts_known = false;
        g.state = "off".into();
        had_pair
    };
    // сервер забыл код вместе с соединением
    if had_pair {
        let _ = app.emit("safedrop-pair", serde_json::json!({"state": "error", "msg": "связь с сервером прервалась — получите новый код"}));
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

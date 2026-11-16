//! Свои устройства: один аккаунт (один ключ) на ПК и телефоне, как в Telegram.
//!
//! Устройства одного аккаунта видят друг друга на сервере (`relay::siblings`)
//! и держат одинаковыми чаты через сервер (`sd-…`) и группы, профиль, кто что
//! прочитал. Каналы между ними — тот же TLS через сервер, и ответить обязан
//! свой же ключ (`relay::own_connect`), поэтому сервер подсунуть кадры не может.
//!
//! - Живые изменения: после каждого сохранения чатов (`changed`) строки,
//!   которые появились или поменялись, уходят другим своим устройствам, а
//!   удалённые — «надгробиями» (`Gone`), чтобы удалённое не вернулось.
//! - Догонялка: устройство, которое было выключено, при встрече шлёт
//!   `own-hello` со списком своих сообщений (id и версия), в ответ получает
//!   недостающее и изменившееся. Медиафайлы недостающих сообщений
//!   запрашиваются отдельно (`own-want` → `own-file`).
//! - Звонок: ответили или отклонили на одном устройстве — на остальных он
//!   перестаёт звонить (`own-call`).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};

use crate::state::{AppState, ChatMsg, ChatReaction, Group, GroupSyncState};

/// Предел одной строки кадра (список известных сообщений бывает большим).
const MAX_LINE: usize = 16 * 1024 * 1024;
/// Пачка сообщений в одном кадре (картинки — внутри сообщений).
const BATCH_BYTES: usize = 4 * 1024 * 1024;
/// В `own-hello`: сколько последних id каждого чата перечислить, у скольких из
/// них указать версию, и всего id в кадре.
const KNOWN_PER_CHAT: usize = 2000;
const HASHED_PER_CHAT: usize = 300;
const KNOWN_TOTAL: usize = 60_000;
/// В ответ на `own-hello`: из скольких последних сообщений чата выбирать.
const OFFER_PER_CHAT: usize = 500;
const GONE_MAX: usize = 5000;
const GONE_KEEP_MS: i64 = 60 * 24 * 3600 * 1000;
const MAX_FILE: u64 = 500 * 1024 * 1024;
const WANT_MAX: usize = 50;
/// Догонялка повторяется, пока устройства в сети (живые изменения идут сразу).
const HELLO_EVERY: Duration = Duration::from_secs(30 * 60);

/// Удалённое сообщение (`id`) или весь чат (`id` пусто: всё не новее `at`).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Gone {
    pub chat: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct OwnProfile {
    #[serde(default)]
    nickname: String,
    #[serde(default)]
    avatar: String,
    #[serde(default)]
    at: i64,
}

/// `own-sync.json`: надгробия и когда профиль меняли здесь.
#[derive(Serialize, Deserialize, Default)]
struct Store {
    #[serde(default)]
    gone: Vec<Gone>,
    #[serde(default)]
    profile_at: i64,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "kind")]
enum OwnFrame {
    #[serde(rename = "own-hello")]
    Hello {
        /// чат → id сообщений; у последних — «id:версия»
        #[serde(default)]
        known: HashMap<String, Vec<String>>,
        #[serde(default)]
        groups: Vec<Group>,
        #[serde(default)]
        group_sync: GroupSyncState,
        #[serde(default)]
        gone: Vec<Gone>,
        #[serde(default)]
        profile: Option<OwnProfile>,
    },
    #[serde(rename = "own-msgs")]
    Msgs {
        #[serde(default)]
        msgs: Vec<ChatMsg>,
        #[serde(default)]
        gone: Vec<Gone>,
        /// живое изменение (реакции берутся как есть), а не догонялка
        #[serde(default)]
        live: bool,
    },
    #[serde(rename = "own-groups")]
    Groups {
        #[serde(default)]
        groups: Vec<Group>,
        #[serde(default)]
        group_sync: GroupSyncState,
    },
    #[serde(rename = "own-profile")]
    Profile { profile: OwnProfile },
    #[serde(rename = "own-call")]
    Call {
        peer: String,
        #[serde(default)]
        what: String,
    },
    #[serde(rename = "own-want")]
    Want { names: Vec<String> },
    /// после строки — `size` байт файла
    #[serde(rename = "own-file")]
    File { name: String, size: u64 },
    #[serde(other)]
    Unknown,
}

// ---------- состояние ----------

struct Own {
    app: Option<AppHandle>,
    store: Store,
    /// что уже видели другие свои устройства: чат → id → версия
    snap: Option<HashMap<String, HashMap<String, String>>>,
    groups_ver: String,
}

fn own() -> &'static Mutex<Own> {
    static S: OnceLock<Mutex<Own>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Own { app: None, store: Store::default(), snap: None, groups_ver: String::new() }))
}

fn lock() -> std::sync::MutexGuard<'static, Own> {
    own().lock().unwrap_or_else(|e| e.into_inner())
}

/// После «Сбросить всё» до перезапуска ничего не рассылаем: очистка чатов
/// не должна стереть историю на других своих устройствах.
static PAUSED: AtomicBool = AtomicBool::new(false);

fn wake() -> &'static tokio::sync::Notify {
    static N: OnceLock<tokio::sync::Notify> = OnceLock::new();
    N.get_or_init(tokio::sync::Notify::new)
}

fn store_path(app: &AppHandle) -> std::path::PathBuf {
    app.state::<AppState>().app_dir.join("own-sync.json")
}

fn save_store(app: &AppHandle) {
    let data = serde_json::to_vec(&lock().store);
    if let Ok(data) = data {
        let path = store_path(app);
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, data).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// Чаты, которые держим одинаковыми: через сервер и группы. Чаты с адресами
/// LAN/Radmin остаются на своём компьютере.
fn synced(chat: &str) -> bool {
    crate::relay::is_relay(chat) || chat.strip_prefix("g:").is_some_and(|g| !g.is_empty() && g.len() <= 100)
}

/// Один и тот же друг на ПК может быть записан под адресом LAN/Radmin (так
/// было до режима сервера), а на телефоне — под `sd-<ID>`. Между своими
/// устройствами чаты и участники групп ходят в «сетевом» виде (`sd-…`,
/// `g:…`), а у себя — под тем ключом, под которым контакт записан здесь.
#[derive(Default)]
struct KeyMap {
    to_wire: HashMap<String, String>,
    to_local: HashMap<String, String>,
}

impl KeyMap {
    async fn load(state: &AppState) -> KeyMap {
        let peers = state.peers.lock().await;
        let mut km = KeyMap::default();
        for p in peers.values() {
            let relay = std::iter::once(&p.ip).chain(p.ips.iter()).find(|a| crate::relay::is_relay(a));
            if let Some(r) = relay {
                km.to_wire.insert(p.ip.clone(), r.clone());
                km.to_local.insert(r.clone(), p.ip.clone());
            }
        }
        km
    }

    /// Ключ чата или адрес для своих устройств; None — чат только этого ПК.
    fn wire(&self, local: &str) -> Option<String> {
        if local.starts_with("g:") {
            return synced(local).then(|| local.to_string());
        }
        if let Some(w) = self.to_wire.get(local) {
            return Some(w.clone());
        }
        crate::relay::is_relay(local).then(|| local.to_string())
    }

    fn local(&self, wire: &str) -> String {
        self.to_local.get(wire).cloned().unwrap_or_else(|| wire.to_string())
    }

    fn wire_list(&self, list: &[String]) -> Vec<String> {
        list.iter().filter_map(|a| self.wire(a)).collect()
    }

    fn local_list(&self, list: &[String]) -> Vec<String> {
        list.iter().filter(|a| synced(a)).map(|a| self.local(a)).collect()
    }

    /// Сообщение для своего устройства: чат и адреса — в сетевом виде.
    fn msg_out(&self, m: &ChatMsg, wire_chat: &str) -> ChatMsg {
        let mut x = m.clone();
        x.peer = wire_chat.to_string();
        x.delivered_to = self.wire_list(&m.delivered_to);
        x.from_ip = self.wire(&m.from_ip).unwrap_or_default();
        x
    }

    fn group_out(&self, g: &Group) -> Group {
        let mut x = g.clone();
        x.members = self.wire_list(&g.members);
        x
    }

    fn group_in(&self, g: &mut Group) {
        g.members = self.local_list(&g.members);
    }

    fn sync_map(&self, s: &GroupSyncState, f: &dyn Fn(&[String]) -> Vec<String>, one: &dyn Fn(&str) -> Option<String>) -> GroupSyncState {
        let mut out = s.clone();
        for d in out.deleted.iter_mut() {
            d.members = f(&d.members);
        }
        out.departures.retain_mut(|d| match one(&d.member_ip) {
            Some(a) => {
                d.member_ip = a;
                d.members = f(&d.members);
                true
            }
            None => false,
        });
        out.rejoins.retain_mut(|r| match one(&r.member_ip) {
            Some(a) => {
                r.member_ip = a;
                r.members = f(&r.members);
                true
            }
            None => false,
        });
        out
    }

    fn sync_out(&self, s: &GroupSyncState) -> GroupSyncState {
        self.sync_map(s, &|l| self.wire_list(l), &|a| self.wire(a))
    }

    fn sync_in(&self, s: &GroupSyncState) -> GroupSyncState {
        self.sync_map(s, &|l| self.local_list(l), &|a| synced(a).then(|| self.local(a)))
    }
}

/// Сетевой ключ чата (для `is_gone` из истории от контакта).
pub async fn wire_key(state: &AppState, local: &str) -> Option<String> {
    KeyMap::load(state).await.wire(local)
}

/// Версия сообщения: что может меняться после отправки.
fn version(m: &ChatMsg) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(m.edited_at.to_le_bytes());
    h.update(m.text.len().to_le_bytes());
    h.update([m.read as u8]);
    h.update(m.status.as_bytes());
    h.update(m.delivered_to.len().to_le_bytes());
    let mut rs: Vec<String> = m.reactions.iter().map(|r| {
        let mut by = r.by.clone();
        by.sort();
        format!("{}={}", r.emoji, by.join(","))
    }).collect();
    rs.sort();
    h.update(rs.join(";").as_bytes());
    h.finalize().iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn snapshot(chats: &HashMap<String, Vec<ChatMsg>>, km: &KeyMap) -> HashMap<String, HashMap<String, String>> {
    chats
        .iter()
        .filter(|(k, _)| km.wire(k).is_some())
        .map(|(k, rows)| (k.clone(), rows.iter().filter(|m| !m.id.is_empty()).map(|m| (m.id.clone(), version(m))).collect()))
        .collect()
}

fn groups_version(groups: &[Group], sync: &GroupSyncState) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(serde_json::to_vec(groups).unwrap_or_default());
    h.update(serde_json::to_vec(sync).unwrap_or_default());
    h.finalize().iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Запустить: загрузить надгробия, запомнить, что уже есть (рассылаются только
/// изменения после запуска), и ждать изменений.
pub fn start(app: AppHandle) {
    let path = store_path(&app);
    let store = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str::<Store>(&s).ok()).unwrap_or_default();
    {
        let mut g = lock();
        g.app = Some(app.clone());
        g.store = store;
    }
    tauri::async_runtime::spawn(async move {
        {
            let state = app.state::<AppState>();
            let km = KeyMap::load(&state).await;
            let snap = snapshot(&*state.chats.lock().await, &km);
            let groups = state.groups.lock().await.clone();
            let sync = state.group_sync.lock().await.clone();
            let gv = groups_version(&groups, &sync);
            let mut g = lock();
            g.snap = Some(snap);
            g.groups_ver = gv;
        }
        let mut hello_at = std::time::Instant::now();
        loop {
            let _ = tokio::time::timeout(Duration::from_secs(60), wake().notified()).await;
            // пачка изменений подряд — одна рассылка
            tokio::time::sleep(Duration::from_millis(600)).await;
            if PAUSED.load(Ordering::Relaxed) {
                continue;
            }
            diff_and_push(&app).await;
            if hello_at.elapsed() >= HELLO_EVERY {
                hello_at = std::time::Instant::now();
                for d in crate::relay::siblings() {
                    let a = app.clone();
                    tokio::spawn(async move { send_hello(&a, &d.dev).await });
                }
            }
        }
    });
}

/// Чаты или группы сохранены: проверить, что поменялось.
pub fn changed() {
    wake().notify_one();
}

/// «Сбросить всё»: забыть надгробия и не рассылать очистку.
pub fn reset(app: &AppHandle) {
    PAUSED.store(true, Ordering::Relaxed);
    {
        let mut g = lock();
        g.store = Store::default();
        g.snap = None;
    }
    let _ = std::fs::remove_file(store_path(app));
}

/// Вход в аккаунт по коду: синхронизация — с пустого места. Вызывать под
/// блокировкой чатов, сразу после их очистки: прежние чаты этого устройства
/// не дают надгробий, прежние надгробия забыты.
pub fn start_fresh(app: &AppHandle) {
    {
        let mut g = lock();
        g.snap = Some(HashMap::new());
        g.store = Store::default();
        g.groups_ver.clear();
    }
    let _ = std::fs::remove_file(store_path(app));
    PAUSED.store(false, Ordering::Relaxed);
}

/// Удалено ли сообщение на каком-то из своих устройств (чтобы история от
/// контакта его не вернула). `chat` — сетевой ключ (`wire_key`).
pub fn is_gone(chat: &str, id: &str, sent_at: i64) -> bool {
    if !synced(chat) {
        return false;
    }
    gone_in(&lock().store.gone, chat, id, sent_at)
}

fn gone_in(list: &[Gone], chat: &str, id: &str, sent_at: i64) -> bool {
    list.iter().any(|x| x.chat == chat && ((!x.id.is_empty() && x.id == id) || (x.id.is_empty() && sent_at <= x.at)))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn add_gone(list: &mut Vec<Gone>, new: &[Gone]) -> bool {
    let mut changed = false;
    for x in new {
        if !synced(&x.chat) || x.id.len() > 100 {
            continue;
        }
        if x.id.is_empty() {
            if let Some(e) = list.iter_mut().find(|e| e.chat == x.chat && e.id.is_empty()) {
                if x.at > e.at {
                    e.at = x.at;
                    changed = true;
                }
                continue;
            }
        } else if list.iter().any(|e| e.chat == x.chat && e.id == x.id) {
            continue;
        }
        list.push(x.clone());
        changed = true;
    }
    let old = now_ms() - GONE_KEEP_MS;
    let before = list.len();
    list.retain(|e| e.at >= old);
    if list.len() > GONE_MAX {
        let extra = list.len() - GONE_MAX;
        list.drain(..extra);
    }
    changed || list.len() != before
}

// ---------- живые изменения ----------

async fn diff_and_push(app: &AppHandle) {
    let state = app.state::<AppState>();
    let km = KeyMap::load(&state).await;
    let (rows, gone) = {
        let chats = state.chats.lock().await;
        let mut g = lock();
        let Some(snap) = g.snap.as_mut() else { return };
        let mut rows: Vec<ChatMsg> = Vec::new();
        let mut gone: Vec<Gone> = Vec::new();
        let now = now_ms();
        for (chat, list) in chats.iter() {
            let Some(wire) = km.wire(chat) else { continue };
            let seen = snap.entry(chat.clone()).or_default();
            let mut alive: HashSet<&str> = HashSet::with_capacity(list.len());
            for m in list.iter().filter(|m| !m.id.is_empty()) {
                alive.insert(m.id.as_str());
                let v = version(m);
                if seen.get(&m.id) != Some(&v) {
                    seen.insert(m.id.clone(), v);
                    rows.push(km.msg_out(m, &wire));
                }
            }
            let removed: Vec<String> = seen.keys().filter(|id| !alive.contains(id.as_str())).cloned().collect();
            if !removed.is_empty() {
                if list.is_empty() {
                    gone.push(Gone { chat: wire.clone(), id: String::new(), at: now });
                } else {
                    gone.extend(removed.iter().map(|id| Gone { chat: wire.clone(), id: id.clone(), at: now }));
                }
                for id in removed {
                    seen.remove(&id);
                }
            }
        }
        // чат удалён целиком (контакт удалили)
        let dropped: Vec<String> = snap.keys().filter(|k| !chats.contains_key(*k)).cloned().collect();
        for chat in dropped {
            let had = snap.remove(&chat).is_some_and(|ids| !ids.is_empty());
            // ключ мог смениться (контакт склеен с другим адресом) — это не удаление
            if had && !chats.contains_key(&km.local(&km.wire(&chat).unwrap_or_default())) {
                if let Some(wire) = km.wire(&chat) {
                    gone.push(Gone { chat: wire, id: String::new(), at: now });
                }
            }
        }
        if !gone.is_empty() {
            add_gone(&mut g.store.gone, &gone);
        }
        (rows, gone)
    };
    if !gone.is_empty() {
        save_store(app);
    }
    let groups_frame = {
        let groups = state.groups.lock().await.clone();
        let sync = state.group_sync.lock().await.clone();
        let v = groups_version(&groups, &sync);
        let mut g = lock();
        if g.groups_ver != v {
            g.groups_ver = v;
            Some(OwnFrame::Groups { groups: groups.iter().map(|x| km.group_out(x)).collect(), group_sync: km.sync_out(&sync) })
        } else {
            None
        }
    };
    let siblings = crate::relay::siblings();
    if siblings.is_empty() {
        return; // встретятся — догонят через own-hello
    }
    for d in siblings {
        if let Some(f) = &groups_frame {
            let _ = send(&d.dev, f).await;
        }
        if !rows.is_empty() || !gone.is_empty() {
            send_msgs(&d.dev, rows.clone(), gone.clone(), true).await;
        }
    }
}

// ---------- отправка ----------

async fn send(dev: &str, frame: &OwnFrame) -> Result<(), String> {
    let mut line = serde_json::to_vec(frame).map_err(|e| e.to_string())?;
    if line.len() > MAX_LINE {
        return Err("кадр слишком большой".into());
    }
    line.push(b'\n');
    let mut s = crate::relay::own_connect(dev).await?;
    s.write_all(&line).await.map_err(|e| e.to_string())?;
    s.flush().await.map_err(|e| e.to_string())?;
    crate::tls::finish(s).await;
    Ok(())
}

async fn send_msgs(dev: &str, msgs: Vec<ChatMsg>, gone: Vec<Gone>, live: bool) {
    let mut batch: Vec<ChatMsg> = Vec::new();
    let mut size = 0usize;
    let mut gone = Some(gone);
    for m in msgs {
        let len = serde_json::to_vec(&m).map(|v| v.len()).unwrap_or(usize::MAX / 4) + 1;
        if len > BATCH_BYTES {
            continue;
        }
        if !batch.is_empty() && size + len > BATCH_BYTES {
            let f = OwnFrame::Msgs { msgs: std::mem::take(&mut batch), gone: gone.take().unwrap_or_default(), live };
            if send(dev, &f).await.is_err() {
                return;
            }
            size = 0;
        }
        size += len;
        batch.push(m);
    }
    let gone = gone.take().unwrap_or_default();
    if !batch.is_empty() || !gone.is_empty() {
        let _ = send(dev, &OwnFrame::Msgs { msgs: batch, gone, live }).await;
    }
}

/// Своё другое устройство появилось в сети: обменяться тем, что пропустили.
pub fn siblings_online(app: &AppHandle) {
    if PAUSED.load(Ordering::Relaxed) {
        return;
    }
    for d in crate::relay::siblings() {
        let a = app.clone();
        tokio::spawn(async move { send_hello(&a, &d.dev).await });
    }
}

async fn send_hello(app: &AppHandle, dev: &str) {
    if PAUSED.load(Ordering::Relaxed) {
        return;
    }
    let state = app.state::<AppState>();
    let km = KeyMap::load(&state).await;
    let known = {
        let chats = state.chats.lock().await;
        let mut known: HashMap<String, Vec<String>> = HashMap::new();
        let mut total = 0usize;
        for (chat, rows) in chats.iter() {
            let Some(wire) = km.wire(chat) else { continue };
            let mut ids = Vec::new();
            for (i, m) in rows.iter().rev().filter(|m| !m.id.is_empty()).take(KNOWN_PER_CHAT).enumerate() {
                ids.push(if i < HASHED_PER_CHAT { format!("{}:{}", m.id, version(m)) } else { m.id.clone() });
            }
            total += ids.len();
            known.entry(wire).or_default().extend(ids);
            if total >= KNOWN_TOTAL {
                break;
            }
        }
        known
    };
    let groups: Vec<Group> = state.groups.lock().await.iter().map(|g| km.group_out(g)).collect();
    let group_sync = km.sync_out(&state.group_sync.lock().await.clone());
    let (gone, profile_at) = {
        let g = lock();
        (g.store.gone.clone(), g.store.profile_at)
    };
    let profile = {
        let s = state.settings.lock().await;
        (profile_at > 0).then(|| OwnProfile { nickname: s.profile.nickname.clone(), avatar: s.profile.avatar.clone(), at: profile_at })
    };
    let _ = send(dev, &OwnFrame::Hello { known, groups, group_sync, gone, profile }).await;
}

/// Ответил или отклонил звонок здесь: остальным своим устройствам перестать звонить.
pub fn call_elsewhere(peer: &str, what: &str) {
    let siblings = crate::relay::siblings();
    if siblings.is_empty() {
        return;
    }
    let f = OwnFrame::Call { peer: peer.to_string(), what: what.to_string() };
    let Ok(v) = serde_json::to_vec(&f) else { return };
    for d in siblings {
        let v = v.clone();
        tokio::spawn(async move {
            if let Ok(f) = serde_json::from_slice::<OwnFrame>(&v) {
                let _ = send(&d.dev, &f).await;
            }
        });
    }
}

/// Профиль поменяли здесь: он новее, чем на других своих устройствах.
pub async fn profile_changed(app: &AppHandle) {
    let at = now_ms();
    lock().store.profile_at = at;
    save_store(app);
    let state = app.state::<AppState>();
    let profile = {
        let s = state.settings.lock().await;
        OwnProfile { nickname: s.profile.nickname.clone(), avatar: s.profile.avatar.clone(), at }
    };
    for d in crate::relay::siblings() {
        let p = profile.clone();
        tokio::spawn(async move { let _ = send(&d.dev, &OwnFrame::Profile { profile: p }).await; });
    }
}

// ---------- приём ----------

/// Входящий канал от своего другого устройства (ключ уже проверен).
pub async fn serve(app: AppHandle, sock: crate::tls::Stream, dev: String) -> Result<(), String> {
    let mut reader = BufReader::new(sock);
    let line = crate::quic::read_line_limited(&mut reader, MAX_LINE, Duration::from_secs(120)).await?;
    if line.is_empty() {
        return Ok(());
    }
    let frame: OwnFrame = serde_json::from_slice(&line).map_err(|e| e.to_string())?;
    match frame {
        OwnFrame::Hello { known, groups, group_sync, gone, profile } => {
            merge_groups(&app, groups, group_sync).await;
            apply_gone(&app, &gone).await;
            if let Some(p) = profile {
                apply_profile(&app, p).await;
            }
            let (msgs, my_gone) = offer(&app, &known).await;
            send_msgs(&dev, msgs, my_gone, false).await;
        }
        OwnFrame::Msgs { msgs, gone, live } => {
            apply_gone(&app, &gone).await;
            let want = merge_msgs(&app, msgs, live).await;
            if !want.is_empty() {
                let _ = send(&dev, &OwnFrame::Want { names: want }).await;
            }
        }
        OwnFrame::Groups { groups, group_sync } => merge_groups(&app, groups, group_sync).await,
        OwnFrame::Profile { profile } => apply_profile(&app, profile).await,
        OwnFrame::Call { peer, what } => {
            if crate::relay::is_relay(&peer) && (what == "call-answer" || what == "call-hangup") {
                let _ = app.emit("safedrop-call-elsewhere", serde_json::json!({"ip": peer, "what": what}));
            }
        }
        OwnFrame::Want { names } => {
            let a = app.clone();
            tokio::spawn(async move { send_files(&a, &dev, names).await });
        }
        OwnFrame::File { name, size } => receive_file(&app, &mut reader, &name, size).await?,
        OwnFrame::Unknown => {}
    }
    Ok(())
}

/// Что отдать устройству, приславшему `known`: чего у него нет и что у него старее.
async fn offer(app: &AppHandle, known: &HashMap<String, Vec<String>>) -> (Vec<ChatMsg>, Vec<Gone>) {
    let state = app.state::<AppState>();
    let km = KeyMap::load(&state).await;
    let chats = state.chats.lock().await;
    let mut out = Vec::new();
    for (chat, rows) in chats.iter() {
        let Some(wire) = km.wire(chat) else { continue };
        let theirs: HashMap<&str, Option<&str>> = known
            .get(&wire)
            .map(|ids| ids.iter().map(|x| match x.split_once(':') { Some((id, v)) => (id, Some(v)), None => (x.as_str(), None) }).collect())
            .unwrap_or_default();
        for m in rows.iter().rev().filter(|m| !m.id.is_empty()).take(OFFER_PER_CHAT) {
            match theirs.get(m.id.as_str()) {
                None => out.push(km.msg_out(m, &wire)),
                Some(Some(v)) if *v != version(m) => out.push(km.msg_out(m, &wire)),
                _ => {}
            }
        }
    }
    out.sort_by(|a, b| a.sent_at.cmp(&b.sent_at).then_with(|| a.id.cmp(&b.id)));
    drop(chats);
    let gone = lock().store.gone.clone();
    (out, gone)
}

/// Имя файла без папок (путь мог прийти с Windows на телефон и наоборот).
fn base_name(p: &str) -> &str {
    p.rsplit(['/', '\\']).next().unwrap_or("")
}

fn safe_file_name(name: &str) -> Option<String> {
    let n = crate::transfer::sanitize(base_name(name));
    (!n.is_empty() && n != "." && n != ".." && n.chars().count() <= 200).then_some(n)
}

fn rank(status: &str) -> u8 {
    match status {
        "delivered" => 2,
        "sent" => 1,
        _ => 0,
    }
}

fn union_reactions(a: &mut Vec<ChatReaction>, b: &[ChatReaction]) {
    for r in b {
        match a.iter_mut().find(|x| x.emoji == r.emoji) {
            Some(x) => {
                for who in &r.by {
                    if !x.by.contains(who) {
                        x.by.push(who.clone());
                    }
                }
            }
            None => a.push(r.clone()),
        }
    }
}

/// Привести сообщение со своего устройства к местным правилам (как историю от контакта).
fn clean(mut m: ChatMsg, loaddrop: &str) -> Option<ChatMsg> {
    if m.id.is_empty() || m.id.len() > 100 || !synced(&m.peer) {
        return None;
    }
    m.text = m.text.chars().take(2000).collect();
    m.from_nick = m.from_nick.chars().take(64).collect();
    m.images.retain(|s| s.len() <= 700_000 && crate::signal::is_data_image(s));
    m.images.truncate(10);
    if m.images.is_empty() && m.image.len() <= 700_000 && crate::signal::is_data_image(&m.image) {
        m.images.push(m.image.clone());
    }
    m.image = m.images.first().cloned().unwrap_or_default();
    m.forwarded_from = m.forwarded_from.trim().chars().take(64).collect();
    m.reply_to = m.reply_to.chars().take(100).collect();
    m.reactions.truncate(50);
    for r in m.reactions.iter_mut() {
        r.emoji = r.emoji.chars().take(16).collect();
        r.by.truncate(100);
    }
    m.delivered_to.truncate(100);
    m.status = match m.status.as_str() {
        "sent" | "delivered" => m.status,
        _ => String::new(),
    };
    m.audio = match safe_file_name(&m.audio) {
        Some(n) if !m.audio.is_empty() => std::path::Path::new(loaddrop).join(n).to_string_lossy().to_string(),
        _ => String::new(),
    };
    Some(m)
}

/// Слить сообщения со своего устройства. Возвращает имена медиафайлов,
/// которых здесь нет (их запросить).
async fn merge_msgs(app: &AppHandle, msgs: Vec<ChatMsg>, live: bool) -> Vec<String> {
    if msgs.is_empty() {
        return Vec::new();
    }
    let state = app.state::<AppState>();
    let loaddrop = state.settings.lock().await.loaddrop_dir.clone();
    let km = KeyMap::load(&state).await;
    let mut changed: HashSet<String> = HashSet::new();
    let mut want: Vec<String> = Vec::new();
    {
        let mut chats = state.chats.lock().await;
        let mut g = lock();
        for m in msgs {
            let Some(mut m) = clean(m, &loaddrop) else { continue };
            if gone_in(&g.store.gone, &m.peer, &m.id, m.sent_at) {
                continue;
            }
            // в местный вид: под каким ключом этот контакт записан здесь
            m.peer = km.local(&m.peer);
            m.delivered_to = km.local_list(&m.delivered_to);
            m.from_ip = if synced(&m.from_ip) { km.local(&m.from_ip) } else { String::new() };
            if !m.audio.is_empty() && !std::path::Path::new(&m.audio).is_file() && want.len() < WANT_MAX {
                if let Some(n) = safe_file_name(&m.audio) {
                    if !want.contains(&n) {
                        want.push(n);
                    }
                }
            }
            let rows = chats.entry(m.peer.clone()).or_default();
            let merged = match rows.iter_mut().find(|x| x.id == m.id) {
                Some(x) => {
                    let before = version(x);
                    if m.edited_at > x.edited_at {
                        x.text = m.text.clone();
                        x.edited_at = m.edited_at;
                    }
                    if live {
                        x.reactions = m.reactions.clone();
                    } else {
                        union_reactions(&mut x.reactions, &m.reactions);
                    }
                    x.read |= m.read;
                    if rank(&m.status) > rank(&x.status) {
                        x.status = m.status.clone();
                    }
                    for d in &m.delivered_to {
                        if !x.delivered_to.contains(d) {
                            x.delivered_to.push(d.clone());
                        }
                    }
                    if x.audio.is_empty() && !m.audio.is_empty() {
                        x.audio = m.audio.clone();
                    }
                    (before != version(x)).then(|| x.clone())
                }
                None => {
                    rows.push(m.clone());
                    rows.sort_by(|a, b| a.sent_at.cmp(&b.sent_at).then_with(|| a.id.cmp(&b.id)));
                    Some(m.clone())
                }
            };
            if let Some(x) = merged {
                // это уже видели: обратно не слать (если не поменялось после слияния)
                if let Some(snap) = g.snap.as_mut() {
                    snap.entry(x.peer.clone()).or_default().insert(x.id.clone(), version(&x));
                }
                changed.insert(x.peer.clone());
            }
        }
    }
    if !changed.is_empty() {
        crate::settings::save_chats_soon(&state);
        for peer in changed {
            let _ = app.emit("safedrop-chat-changed", serde_json::json!({"peer": peer}));
        }
    }
    want
}

async fn apply_gone(app: &AppHandle, gone: &[Gone]) {
    if gone.is_empty() {
        return;
    }
    let fresh = {
        let mut g = lock();
        add_gone(&mut g.store.gone, gone)
    };
    let state = app.state::<AppState>();
    let km = KeyMap::load(&state).await;
    let mut changed = HashSet::new();
    {
        let mut chats = state.chats.lock().await;
        let mut g = lock();
        for x in gone.iter().filter(|x| synced(&x.chat)) {
            let local = km.local(&x.chat);
            if let Some(rows) = chats.get_mut(&local) {
                let before = rows.len();
                if x.id.is_empty() {
                    rows.retain(|m| m.sent_at > x.at);
                } else {
                    rows.retain(|m| m.id != x.id);
                }
                if rows.len() != before {
                    changed.insert(local.clone());
                }
            }
            // удаление уже здесь: своё надгробие для него не нужно
            if let Some(snap) = g.snap.as_mut().and_then(|s| s.get_mut(&local)) {
                if x.id.is_empty() {
                    let left: HashSet<String> = chats.get(&local).map(|r| r.iter().map(|m| m.id.clone()).collect()).unwrap_or_default();
                    snap.retain(|id, _| left.contains(id));
                } else {
                    snap.remove(&x.id);
                }
            }
        }
    }
    if fresh {
        save_store(app);
    }
    if !changed.is_empty() {
        crate::settings::save_chats_soon(&state);
        for peer in changed {
            let _ = app.emit("safedrop-chat-changed", serde_json::json!({"peer": peer}));
        }
    }
}

/// Группы со своего устройства: недостающие добавить, участников объединить,
/// имя и аватар — у кого новее. Удаления и выходы — общим `merge_group_sync`.
async fn merge_groups(app: &AppHandle, groups: Vec<Group>, group_sync: GroupSyncState) {
    let state = app.state::<AppState>();
    let km = KeyMap::load(&state).await;
    crate::signal::merge_group_sync(app, km.sync_in(&group_sync), None).await;
    let sync = state.group_sync.lock().await.clone();
    let me = crate::relay::my_addr().unwrap_or_default();
    let mut changed = false;
    {
        let mut gs = state.groups.lock().await;
        for mut ng in groups.into_iter().take(500) {
            if ng.id.is_empty() || ng.id.len() > 100 {
                continue;
            }
            if sync.deleted.iter().any(|d| d.id == ng.id) || sync.departures.iter().any(|d| d.group_id == ng.id && d.member_ip == me) {
                continue;
            }
            ng.name = ng.name.chars().take(64).collect();
            ng.members.retain(|m| m.len() <= 100);
            ng.members.truncate(200);
            km.group_in(&mut ng);
            if !crate::signal::is_data_image(&ng.avatar) || ng.avatar.len() > 300_000 {
                ng.avatar.clear();
            }
            match gs.iter_mut().find(|g| g.id == ng.id) {
                Some(g) => {
                    for m in &ng.members {
                        if !g.members.contains(m) {
                            g.members.push(m.clone());
                            changed = true;
                        }
                    }
                    if ng.updated_at > g.updated_at {
                        g.name = ng.name.clone();
                        g.avatar = ng.avatar.clone();
                        g.updated_at = ng.updated_at;
                        changed = true;
                    }
                }
                None => {
                    gs.push(ng);
                    changed = true;
                }
            }
        }
    }
    if changed {
        crate::signal::persist_groups(&state).await;
        let gs: Vec<Group> = state.groups.lock().await.clone();
        let _ = app.emit("safedrop-groups", gs);
    }
    // то, что пришло, — уже общее: не рассылать обратно
    let groups = state.groups.lock().await.clone();
    let sync = state.group_sync.lock().await.clone();
    let v = groups_version(&groups, &sync);
    lock().groups_ver = v;
}

async fn apply_profile(app: &AppHandle, p: OwnProfile) {
    let newer = {
        let g = lock();
        p.at > g.store.profile_at
    };
    if !newer {
        return;
    }
    let nickname: String = p.nickname.trim().chars().take(32).collect();
    let avatar = if p.avatar.is_empty() || (p.avatar.len() <= 300_000 && crate::signal::is_data_image(&p.avatar)) { p.avatar } else { String::new() };
    lock().store.profile_at = p.at;
    save_store(app);
    let state = app.state::<AppState>();
    let profile = crate::state::Profile { nickname, avatar };
    let shown = {
        let mut s = state.settings.lock().await;
        s.profile = profile.clone();
        if profile.nickname.is_empty() { s.device_name.clone() } else { profile.nickname.clone() }
    };
    crate::settings::persist_settings(&state);
    let _ = app.emit("safedrop-profile", profile);
    crate::relay::send_name(&shown);
}

/// Отдать запрошенные медиафайлы: только те, на которые ссылаются свои чаты.
async fn send_files(app: &AppHandle, dev: &str, names: Vec<String>) {
    let state = app.state::<AppState>();
    let paths: Vec<(String, std::path::PathBuf)> = {
        let chats = state.chats.lock().await;
        let mut out = Vec::new();
        for n in names.iter().take(WANT_MAX) {
            let Some(n) = safe_file_name(n) else { continue };
            let found = chats
                .values()
                .flat_map(|rows| rows.iter())
                .find(|m| !m.audio.is_empty() && safe_file_name(&m.audio).as_deref() == Some(n.as_str()) && std::path::Path::new(&m.audio).is_file())
                .map(|m| std::path::PathBuf::from(&m.audio));
            if let Some(p) = found {
                out.push((n, p));
            }
        }
        out
    };
    for (name, path) in paths {
        if send_file(dev, &name, &path).await.is_err() {
            break;
        }
    }
}

async fn send_file(dev: &str, name: &str, path: &std::path::Path) -> Result<(), String> {
    let mut f = tokio::fs::File::open(path).await.map_err(|e| e.to_string())?;
    let size = f.metadata().await.map_err(|e| e.to_string())?.len();
    if size > MAX_FILE {
        return Err("файл слишком большой".into());
    }
    let mut line = serde_json::to_vec(&OwnFrame::File { name: name.to_string(), size }).map_err(|e| e.to_string())?;
    line.push(b'\n');
    let mut s = crate::relay::own_connect(dev).await?;
    s.write_all(&line).await.map_err(|e| e.to_string())?;
    let sent = tokio::io::copy(&mut f, &mut s).await.map_err(|e| e.to_string())?;
    if sent != size {
        return Err("файл изменился во время отправки".into());
    }
    s.flush().await.map_err(|e| e.to_string())?;
    crate::tls::finish(s).await;
    Ok(())
}

async fn receive_file(app: &AppHandle, reader: &mut BufReader<crate::tls::Stream>, name: &str, size: u64) -> Result<(), String> {
    let name = safe_file_name(name).ok_or("неверное имя файла")?;
    if size > MAX_FILE {
        return Err("файл слишком большой".into());
    }
    let state = app.state::<AppState>();
    let dir = state.settings.lock().await.loaddrop_dir.clone();
    let dest = std::path::Path::new(&dir).join(&name);
    // берём только файлы, на которые ссылаются сообщения здесь
    let wanted = {
        let chats = state.chats.lock().await;
        let dest_s = dest.to_string_lossy().to_string();
        chats.values().flatten().any(|m| m.audio == dest_s)
    };
    if !wanted || dest.exists() {
        return Ok(());
    }
    let _ = tokio::fs::create_dir_all(&dir).await;
    let part = dest.with_extension(format!("{}.own-part", dest.extension().and_then(|e| e.to_str()).unwrap_or("")));
    let mut out = tokio::fs::File::create(&part).await.map_err(|e| e.to_string())?;
    let mut limited = reader.take(size);
    let got = tokio::time::timeout(Duration::from_secs(1800), tokio::io::copy(&mut limited, &mut out))
        .await
        .map_err(|_| "файл шёл слишком долго".to_string())?
        .map_err(|e| e.to_string());
    let _ = out.flush().await;
    drop(out);
    match got {
        Ok(n) if n == size => {
            if dest.exists() {
                let _ = tokio::fs::remove_file(&part).await;
            } else {
                tokio::fs::rename(&part, &dest).await.map_err(|e| e.to_string())?;
            }
        }
        _ => {
            let _ = tokio::fs::remove_file(&part).await;
            return Err("файл пришёл не целиком".into());
        }
    }
    let dest_s = dest.to_string_lossy().to_string();
    let peers: HashSet<String> = state.chats.lock().await.iter().filter(|(_, rows)| rows.iter().any(|m| m.audio == dest_s)).map(|(k, _)| k.clone()).collect();
    for peer in peers {
        let _ = app.emit("safedrop-chat-changed", serde_json::json!({"peer": peer}));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, peer: &str) -> ChatMsg {
        ChatMsg {
            id: id.into(), peer: peer.into(), from_me: true, from_nick: "я".into(), text: "привет".into(), ts: String::new(),
            sent_at: 10, image: String::new(), images: vec![], audio: String::new(), forwarded_from: String::new(),
            status: "sent".into(), delivered_to: vec![], read: false, reply_to: String::new(), edited_at: 0, reactions: vec![], from_ip: String::new(),
        }
    }

    #[test]
    fn only_server_and_group_chats_are_synced() {
        assert!(synced(&format!("sd-{}", "a".repeat(64))));
        assert!(synced("g:abc"));
        assert!(!synced("26.1.2.3"));
        assert!(!synced("g:"));
    }

    #[test]
    fn version_changes_with_edits_reactions_and_reads() {
        let a = msg("1", "g:x");
        let mut b = a.clone();
        assert_eq!(version(&a), version(&b));
        b.read = true;
        assert_ne!(version(&a), version(&b));
        let mut c = a.clone();
        c.reactions.push(ChatReaction { emoji: "👍".into(), by: vec!["я".into()] });
        assert_ne!(version(&a), version(&c));
        // порядок тех, кто поставил реакцию, не важен
        let mut d = a.clone();
        d.reactions.push(ChatReaction { emoji: "👍".into(), by: vec!["а".into(), "б".into()] });
        let mut e = a.clone();
        e.reactions.push(ChatReaction { emoji: "👍".into(), by: vec!["б".into(), "а".into()] });
        assert_eq!(version(&d), version(&e));
    }

    #[test]
    fn tombstones_merge_and_cover_whole_chat() {
        let mut list = Vec::new();
        let now = now_ms();
        assert!(add_gone(&mut list, &[Gone { chat: "g:x".into(), id: "1".into(), at: now }]));
        assert!(!add_gone(&mut list, &[Gone { chat: "g:x".into(), id: "1".into(), at: now }]), "повтор не добавляется");
        assert!(!add_gone(&mut list, &[Gone { chat: "10.0.0.1".into(), id: "2".into(), at: now }]), "чаты LAN не трогаем");
        assert!(add_gone(&mut list, &[Gone { chat: "g:x".into(), id: String::new(), at: now - 5 }]));
        assert!(add_gone(&mut list, &[Gone { chat: "g:x".into(), id: String::new(), at: now }]), "очистка чата — по последней");
        assert_eq!(list.iter().filter(|g| g.id.is_empty()).count(), 1);
        // старые забываются
        add_gone(&mut list, &[Gone { chat: "g:y".into(), id: "3".into(), at: now - GONE_KEEP_MS - 1000 }]);
        assert!(!list.iter().any(|g| g.chat == "g:y"));
    }

    #[test]
    fn foreign_rows_are_cleaned() {
        let mut m = msg("1", "g:x");
        m.audio = "C:\\Users\\me\\Downloads\\LoadDrop\\safedrop-voice-1.webm".into();
        m.images = vec!["javascript:alert(1)".into()];
        m.status = "weird".into();
        let c = clean(m, "/data/LoadDrop").unwrap();
        assert!(c.audio.ends_with("safedrop-voice-1.webm"));
        assert!(c.audio.starts_with("/data/LoadDrop"));
        assert!(c.images.is_empty());
        assert_eq!(c.status, "");
        assert!(clean(msg("1", "10.0.0.1"), "/d").is_none(), "чат LAN не принимаем");
        assert!(clean(msg("", "g:x"), "/d").is_none());
        let mut evil = msg("2", "g:x");
        evil.audio = "../../etc/passwd".into();
        let c = clean(evil, "/d").unwrap();
        // разделитель папок свой у каждой системы (на Windows — «\»)
        assert_eq!(c.audio, std::path::Path::new("/d").join("passwd").to_string_lossy());
    }

    #[test]
    fn frames_roundtrip_and_unknown_tolerated() {
        let f = OwnFrame::Call { peer: "sd-x".into(), what: "call-answer".into() };
        let s = serde_json::to_string(&f).unwrap();
        assert!(s.contains("\"kind\":\"own-call\""));
        assert!(matches!(serde_json::from_str::<OwnFrame>(r#"{"kind":"own-future"}"#).unwrap(), OwnFrame::Unknown));
        assert!(matches!(serde_json::from_str::<OwnFrame>(r#"{"kind":"own-msgs"}"#).unwrap(), OwnFrame::Msgs { live: false, .. }));
    }
}

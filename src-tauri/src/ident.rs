//! Один компьютер — один контакт. Адрес контакта (IP в LAN, Radmin, другой
//! VPN, `sd-<ID>` через сервер) — это лишь способ до него достучаться, а кто
//! он — определяет ключ устройства: отпечаток TLS-сертификата, проверенный при
//! рукопожатии (`tls::pin_of`), а у адресов через сервер он и есть ID.
//!
//! Адреса с одним ключом склеиваются в один контакт: первичный `Peer.ip`
//! остаётся ключом контакта и чата, остальные адреса живут в `Peer.ips`.
//! Отправка идёт по тому адресу из `ips`, который сейчас отвечает быстрее
//! (`route`), а входящее с любого адреса попадает в один контакт (`canon`).
//! Подделать склейку нельзя: ключ берётся только из проверенного TLS.

use crate::state::{AppState, CallRecord, ChatMsg, Peer};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Адрес считается живым столько после последнего ответа.
const ALIVE_FOR: Duration = Duration::from_secs(120);

struct Routes {
    /// любой адрес → первичный адрес контакта
    owner: HashMap<String, String>,
    /// первичный адрес → все адреса контакта (первичный первым)
    addrs: HashMap<String, Vec<String>>,
    /// адрес → (пинг, когда ответил)
    alive: HashMap<String, (u64, Instant)>,
}

fn routes() -> &'static Mutex<Routes> {
    static R: std::sync::OnceLock<Mutex<Routes>> = std::sync::OnceLock::new();
    R.get_or_init(|| Mutex::new(Routes { owner: HashMap::new(), addrs: HashMap::new(), alive: HashMap::new() }))
}

/// Все адреса контакта: первичный и остальные, без повторов.
pub fn addrs_of(p: &Peer) -> Vec<String> {
    let mut out = vec![p.ip.clone()];
    for a in &p.ips {
        if !a.is_empty() && !out.contains(a) {
            out.push(a.clone());
        }
    }
    out
}

/// Обновить таблицу адресов (вызывается при каждом сохранении контактов).
pub fn sync(peers: &HashMap<String, Peer>) {
    let Ok(mut r) = routes().lock() else { return };
    r.owner.clear();
    r.addrs.clear();
    for p in peers.values() {
        let list = addrs_of(p);
        for a in &list {
            r.owner.insert(a.clone(), p.ip.clone());
        }
        r.addrs.insert(p.ip.clone(), list);
    }
}

/// Адрес ответил (скан, пинг, удачное соединение).
pub fn note_alive(addr: &str, ping_ms: u64) {
    if let Ok(mut r) = routes().lock() {
        r.alive.insert(addr.to_string(), (ping_ms, Instant::now()));
    }
}

/// Адрес не ответил.
pub fn note_dead(addr: &str) {
    if let Ok(mut r) = routes().lock() {
        r.alive.remove(addr);
    }
}

/// Куда подключаться, чтобы достучаться до контакта с адресом `addr`: его
/// адреса в порядке предпочтения. В режиме «через сервер» — только адреса
/// через сервер, иначе — только прямые; сначала недавно ответившие (по пингу),
/// потом сам `addr`, потом остальные.
pub fn route(addr: &str) -> Vec<String> {
    let relay_mode = crate::relay::active();
    let (all, alive) = {
        let Ok(r) = routes().lock() else { return vec![addr.to_string()] };
        let all = r
            .owner
            .get(addr)
            .and_then(|o| r.addrs.get(o))
            .cloned()
            .unwrap_or_else(|| vec![addr.to_string()]);
        let alive: HashMap<String, u64> = all
            .iter()
            .filter_map(|a| r.alive.get(a).filter(|(_, at)| at.elapsed() < ALIVE_FOR).map(|(p, _)| (a.clone(), *p)))
            .collect();
        (all, alive)
    };
    let all = same_key_only(addr, &all, key_of);
    order(addr, &all, relay_mode, |a| alive.get(a).copied())
}

/// Запасные адреса — только с тем же запомненным ключом, что у контакта:
/// адрес без ключа (ключ забыли, сбросили) не получает право говорить от
/// имени контакта, и соединение с ним не обойдёт проверку ключа.
/// Сам `addr` остаётся всегда — это прежнее поведение для него.
fn same_key_only(addr: &str, all: &[String], key: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let Some(fp) = key(addr).or_else(|| all.iter().find_map(|a| key(a))) else {
        return vec![addr.to_string()];
    };
    all.iter()
        .filter(|a| *a == addr || key(a).as_deref() == Some(fp.as_str()))
        .cloned()
        .collect()
}

/// Ключ, который устройство за адресом само предъявило нам при входящем
/// соединении (взаимный TLS), или ID адреса через сервер. Только такой
/// ключ даёт право считать входящее с адреса — контактом.
fn verified_key(addr: &str) -> Option<String> {
    if let Some(id) = crate::relay::id_of(addr) {
        return Some(id.to_string());
    }
    crate::tls::pin_of(addr).filter(|p| p.mutual && !p.fp.is_empty()).map(|p| p.fp)
}

fn order(addr: &str, all: &[String], relay_mode: bool, alive: impl Fn(&str) -> Option<u64>) -> Vec<String> {
    let usable: Vec<&String> = all.iter().filter(|a| crate::relay::is_relay(a) == relay_mode).collect();
    if usable.is_empty() {
        // нет адреса для этого режима: пусть соединение само скажет почему
        return vec![addr.to_string()];
    }
    let mut live: Vec<(u64, &String)> = usable.iter().filter_map(|a| alive(a).map(|p| (p, *a))).collect();
    live.sort_by_key(|(p, _)| *p);
    let mut out: Vec<String> = live.into_iter().map(|(_, a)| a.clone()).collect();
    if usable.iter().any(|a| *a == addr) && !out.iter().any(|a| a == addr) {
        out.push(addr.to_string());
    }
    for a in usable {
        if !out.contains(a) {
            out.push(a.clone());
        }
    }
    out
}

/// Ключ устройства за адресом, если он известен и проверен.
pub fn key_of(addr: &str) -> Option<String> {
    if let Some(id) = crate::relay::id_of(addr) {
        return Some(id.to_string());
    }
    crate::tls::pin_of(addr).map(|p| p.fp).filter(|fp| !fp.is_empty())
}

/// Ключ контакта: по любому из его адресов.
fn key_of_peer(p: &Peer, key: &impl Fn(&str) -> Option<String>) -> Option<String> {
    addrs_of(p).iter().find_map(|a| key(a))
}

/// Чей это адрес: первичный адрес контакта, у которого этот адрес уже есть
/// или у которого тот же ключ устройства.
fn owner_in(peers: &HashMap<String, Peer>, addr: &str, key: &impl Fn(&str) -> Option<String>) -> Option<String> {
    if peers.contains_key(addr) {
        return Some(addr.to_string());
    }
    if let Some(p) = peers.values().find(|p| p.ips.iter().any(|a| a == addr)) {
        return Some(p.ip.clone());
    }
    let fp = key(addr)?;
    let mut owners: Vec<&Peer> = peers.values().filter(|p| key_of_peer(p, key).as_deref() == Some(fp.as_str())).collect();
    owners.sort_by(|a, b| a.ip.cmp(&b.ip));
    owners.first().map(|p| p.ip.clone())
}

/// Первичный адрес контакта для входящего с `addr`. Чужой адрес считается
/// контактом только по ключу, который устройство на нём само предъявило
/// (`verified_key`): одного членства в списке адресов мало — иначе кто-то
/// без ключа на старом адресе получил бы доверие контакта. Новый адрес с
/// ключом известного контакта сразу приклеивается к нему.
pub async fn canon(state: &AppState, addr: &str) -> String {
    let mut g = state.peers.lock().await;
    if g.contains_key(addr) {
        return addr.to_string();
    }
    let Some(fp) = verified_key(addr) else { return addr.to_string() };
    let mut owners: Vec<String> = g
        .values()
        .filter(|p| key_of_peer(p, &key_of).as_deref() == Some(fp.as_str()))
        .map(|p| p.ip.clone())
        .collect();
    owners.sort();
    let Some(owner) = owners.into_iter().next() else { return addr.to_string() };
    let added = match g.get_mut(&owner) {
        Some(p) if !p.ips.iter().any(|a| a == addr) => {
            if !p.ips.contains(&p.ip) {
                p.ips.insert(0, p.ip.clone());
            }
            p.ips.push(addr.to_string());
            true
        }
        _ => false,
    };
    drop(g);
    if added {
        crate::settings::persist_peers(state);
    }
    owner
}

/// Чей это адрес — для своих исходящих (мы сами подключились и проверили
/// ключ): ничего не приклеивает и прав входящим не даёт.
pub async fn owner(state: &AppState, addr: &str) -> String {
    let g = state.peers.lock().await;
    owner_in(&g, addr, &key_of).unwrap_or_else(|| addr.to_string())
}

/// Найти контакт для адреса и приклеить адрес к нему (для мест, которые уже
/// держат блокировку контактов: скан, список устройств сервера).
pub fn attach(peers: &mut HashMap<String, Peer>, addr: &str) -> Option<String> {
    let owner = owner_in(peers, addr, &key_of)?;
    if let Some(p) = peers.get_mut(&owner) {
        if !p.ips.contains(&p.ip) {
            p.ips.insert(0, p.ip.clone());
        }
        if !p.ips.iter().any(|a| a == addr) {
            p.ips.push(addr.to_string());
        }
    }
    Some(owner)
}

/// Склеить контакты с одним ключом устройства. Возвращает пары
/// «бывший первичный адрес → первичный адрес контакта, в который он вошёл».
/// `weight` — сколько у адреса сообщений: первичным становится контакт со
/// звёздочкой, а среди равных — с самым большим чатом (меньше переносить).
pub fn merge_peers(
    peers: &mut HashMap<String, Peer>,
    key: impl Fn(&str) -> Option<String>,
    weight: impl Fn(&str) -> usize,
) -> Vec<(String, String)> {
    let mut by_key: HashMap<String, Vec<String>> = HashMap::new();
    for p in peers.values() {
        if let Some(fp) = key_of_peer(p, &key) {
            by_key.entry(fp).or_default().push(p.ip.clone());
        }
    }
    let mut moved = Vec::new();
    let mut groups: Vec<Vec<String>> = by_key.into_values().filter(|v| v.len() > 1).collect();
    groups.sort();
    for mut ips in groups {
        ips.sort_by(|a, b| {
            let (pa, pb) = (&peers[a], &peers[b]);
            pb.trusted
                .cmp(&pa.trusted)
                .then_with(|| weight(b).cmp(&weight(a)))
                .then_with(|| crate::relay::is_relay(a).cmp(&crate::relay::is_relay(b)))
                .then_with(|| a.cmp(b))
        });
        let primary = ips[0].clone();
        for other in ips.into_iter().skip(1) {
            let Some(o) = peers.remove(&other) else { continue };
            if let Some(p) = peers.get_mut(&primary) {
                absorb(p, o);
                moved.push((other, primary.clone()));
            }
        }
    }
    moved
}

/// Влить контакт `o` в `p` (это одно устройство).
fn absorb(p: &mut Peer, o: Peer) {
    let mut all = addrs_of(p);
    for a in addrs_of(&o) {
        if !all.contains(&a) {
            all.push(a);
        }
    }
    p.ips = all;
    // своё имя для контакта: если у первичного его не было (имя = адрес)
    let named = |x: &Peer| !x.name.is_empty() && x.name != x.ip;
    if !named(p) && named(&o) {
        p.name = o.name.clone();
    }
    if p.custom_avatar.is_empty() {
        p.custom_avatar = o.custom_avatar.clone();
    }
    // профиль — от того, кто сейчас в сети (он свежее)
    if (o.online && !p.online) || p.nickname.is_empty() {
        if !o.nickname.is_empty() {
            p.nickname = o.nickname.clone();
            p.avatar = o.avatar.clone();
            p.phash = o.phash.clone();
        }
        if !o.version.is_empty() {
            p.version = o.version.clone();
        }
        if o.online {
            p.state = o.state.clone();
            p.detail = o.detail.clone();
        }
    }
    p.trusted |= o.trusted;
    p.supports_file_confirm |= o.supports_file_confirm;
    p.supports_chat_ack |= o.supports_chat_ack;
    p.supports_watch |= o.supports_watch;
    if o.online {
        p.ping_ms = match (p.online, p.ping_ms, o.ping_ms) {
            (true, Some(a), Some(b)) => Some(a.min(b)),
            (true, a, b) => a.or(b),
            (false, _, b) => b,
        };
        p.online = true;
    }
    if o.last_seen > p.last_seen {
        p.last_seen = o.last_seen;
    }
}

/// Перенести чаты склеенных адресов в чат контакта.
pub fn merge_chats(chats: &mut HashMap<String, Vec<ChatMsg>>, moved: &[(String, String)]) -> bool {
    let mut changed = false;
    for (from, to) in moved {
        let Some(mut rows) = chats.remove(from) else { continue };
        changed = true;
        for m in rows.iter_mut() {
            m.peer = to.clone();
        }
        let dst = chats.entry(to.clone()).or_default();
        let mut seen: HashSet<String> = dst.iter().map(|m| m.id.clone()).collect();
        dst.extend(rows.into_iter().filter(|m| seen.insert(m.id.clone())));
        dst.sort_by(|a, b| a.sent_at.cmp(&b.sent_at).then_with(|| a.id.cmp(&b.id)));
    }
    changed
}

/// История звонков: адреса склеенных контактов → адрес контакта.
pub fn merge_calls(calls: &mut [CallRecord], moved: &[(String, String)]) -> bool {
    let map: HashMap<&str, &str> = moved.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let mut changed = false;
    for c in calls.iter_mut() {
        if let Some(to) = map.get(c.peer.as_str()) {
            c.peer = to.to_string();
            changed = true;
        }
        for p in c.participants.iter_mut() {
            if let Some(to) = map.get(p.as_str()) {
                *p = to.to_string();
                changed = true;
            }
        }
    }
    changed
}

/// Перед первой склейкой — копии файлов как были (на случай отката).
fn backup(state: &AppState) {
    for path in [state.peers_path(), state.chats_path(), state.call_history_path()] {
        let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string()) else { continue };
        let copy = path.with_file_name(format!("{name}.before-merge.json"));
        if path.exists() && !copy.exists() {
            let _ = std::fs::copy(&path, &copy);
        }
    }
}

fn chat_weight(chats: &HashMap<String, Vec<ChatMsg>>) -> impl Fn(&str) -> usize + '_ {
    move |ip| chats.get(ip).map_or(0, |v| v.len())
}

fn save_calls(state: &AppState, calls: &[CallRecord]) {
    if let Ok(data) = serde_json::to_vec_pretty(calls) {
        let _ = std::fs::write(state.call_history_path(), data);
    }
}

/// При запуске (до окна): склеить накопившиеся дубли.
pub fn merge_at_start(state: &AppState) {
    let (Ok(mut peers), Ok(mut chats), Ok(mut calls)) =
        (state.peers.try_lock(), state.chats.try_lock(), state.call_history.try_lock())
    else {
        return;
    };
    let moved = merge_peers(&mut peers, key_of, chat_weight(&chats));
    sync(&peers);
    if moved.is_empty() {
        return;
    }
    backup(state);
    merge_chats(&mut chats, &moved);
    if merge_calls(&mut calls, &moved) {
        save_calls(state, &calls);
    }
    drop((peers, chats, calls));
    crate::settings::persist_peers(state);
    crate::settings::save_chats_now(state);
    eprintln!("SafeDrop: склеены контакты одного устройства: {moved:?}");
}

/// Во время работы (после скана, знакомства): склеить новые дубли и сказать
/// интерфейсу, какой чат куда переехал.
pub async fn merge_now(app: &tauri::AppHandle) {
    use tauri::{Emitter, Manager};
    let state = app.state::<AppState>();
    let moved = {
        let mut peers = state.peers.lock().await;
        let chats = state.chats.lock().await;
        merge_peers(&mut peers, key_of, chat_weight(&chats))
    };
    if moved.is_empty() {
        return;
    }
    backup(&state);
    if merge_chats(&mut *state.chats.lock().await, &moved) {
        crate::settings::save_chats_soon(&state);
    }
    {
        let mut calls = state.call_history.lock().await;
        if merge_calls(&mut calls, &moved) {
            save_calls(&state, &calls);
        }
    }
    crate::settings::persist_peers(&state);
    let map: HashMap<&str, &str> = moved.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let _ = app.emit("safedrop-contacts-merged", map);
    let peers: Vec<Peer> = state.peers.lock().await.values().cloned().collect();
    let _ = app.emit("safedrop-peers", peers);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(ip: &str) -> Peer {
        Peer {
            ip: ip.into(), name: ip.into(), nickname: String::new(), avatar: String::new(),
            custom_avatar: String::new(), trusted: false, supports_file_confirm: false,
            supports_chat_ack: false, supports_watch: false, version: String::new(), online: false,
            ping_ms: None, last_seen: None, phash: String::new(), state: "idle".into(),
            detail: String::new(), ips: vec![ip.into()],
        }
    }

    fn msg(id: &str, peer: &str, at: i64) -> ChatMsg {
        serde_json::from_value(serde_json::json!({
            "id": id, "peer": peer, "from_me": false, "from_nick": "", "text": id, "ts": "", "sent_at": at
        }))
        .unwrap()
    }

    const K: &str = "aaaa";

    fn keys(addr: &str) -> Option<String> {
        match addr {
            "10.0.0.1" | "26.0.0.1" => Some(K.into()),
            a if a == format!("sd-{}", "b".repeat(64)) => Some("b".repeat(64)),
            _ => None,
        }
    }

    #[test]
    fn same_key_becomes_one_contact_with_one_chat() {
        let mut peers: HashMap<String, Peer> = HashMap::new();
        let mut a = peer("10.0.0.1");
        a.name = "Друг".into();
        let mut b = peer("26.0.0.1");
        b.trusted = true;
        b.online = true;
        b.ping_ms = Some(30);
        b.nickname = "Hazyy".into();
        peers.insert(a.ip.clone(), a);
        peers.insert(b.ip.clone(), b);
        peers.insert("10.0.0.9".into(), peer("10.0.0.9")); // без ключа — не трогаем

        let mut chats: HashMap<String, Vec<ChatMsg>> = HashMap::new();
        chats.insert("10.0.0.1".into(), vec![msg("m1", "10.0.0.1", 1), msg("m3", "10.0.0.1", 3)]);
        chats.insert("26.0.0.1".into(), vec![msg("m2", "26.0.0.1", 2), msg("m3", "26.0.0.1", 3)]);

        let moved = merge_peers(&mut peers, keys, |ip| chats.get(ip).map_or(0, |v| v.len()));
        // первичный — со звёздочкой, хоть чат у них одинаковый
        assert_eq!(moved, vec![("10.0.0.1".to_string(), "26.0.0.1".to_string())]);
        assert_eq!(peers.len(), 2);
        let p = &peers["26.0.0.1"];
        assert_eq!(p.ips, vec!["26.0.0.1".to_string(), "10.0.0.1".to_string()]);
        assert_eq!(p.name, "Друг");
        assert!(p.trusted && p.online);
        assert_eq!(p.nickname, "Hazyy");

        assert!(merge_chats(&mut chats, &moved));
        assert_eq!(chats.len(), 1);
        let ids: Vec<&str> = chats["26.0.0.1"].iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["m1", "m2", "m3"]);
        assert!(chats["26.0.0.1"].iter().all(|m| m.peer == "26.0.0.1"));

        // повторная склейка ничего не меняет
        assert!(merge_peers(&mut peers, keys, |_| 0).is_empty());
    }

    #[test]
    fn biggest_chat_wins_and_calls_follow() {
        let mut peers: HashMap<String, Peer> = HashMap::new();
        for ip in ["10.0.0.1", "26.0.0.1"] {
            peers.insert(ip.into(), peer(ip));
        }
        let moved = merge_peers(&mut peers, keys, |ip| if ip == "10.0.0.1" { 5 } else { 1 });
        assert_eq!(moved, vec![("26.0.0.1".to_string(), "10.0.0.1".to_string())]);
        let mut calls: Vec<CallRecord> = vec![serde_json::from_value(serde_json::json!({
            "id": "c", "startedAt": 0, "durationSecs": 0, "direction": "incoming", "status": "completed",
            "peer": "26.0.0.1", "participants": ["26.0.0.1", "10.0.0.7"]
        }))
        .unwrap()];
        assert!(merge_calls(&mut calls, &moved));
        assert_eq!(calls[0].peer, "10.0.0.1");
        assert_eq!(calls[0].participants, vec!["10.0.0.1".to_string(), "10.0.0.7".to_string()]);
    }

    #[test]
    fn relay_address_joins_by_its_id() {
        let relay = format!("sd-{}", "b".repeat(64));
        let mut peers: HashMap<String, Peer> = HashMap::new();
        peers.insert("192.168.1.5".into(), peer("192.168.1.5"));
        peers.insert(relay.clone(), peer(&relay));
        let key = |a: &str| if a == "192.168.1.5" { Some("b".repeat(64)) } else { keys(a) };
        let moved = merge_peers(&mut peers, key, |_| 0);
        // при равенстве первичным остаётся прямой адрес
        assert_eq!(moved, vec![(relay.clone(), "192.168.1.5".to_string())]);
        assert_eq!(peers["192.168.1.5"].ips, vec!["192.168.1.5".to_string(), relay.clone()]);
        assert_eq!(owner_in(&peers, &relay, &key).as_deref(), Some("192.168.1.5"));
    }

    #[test]
    fn backup_addresses_need_the_same_key() {
        let all = vec!["10.0.0.1".to_string(), "26.0.0.1".to_string(), "10.9.9.9".to_string()];
        // 10.9.9.9 в списке, но ключа у него нет (забыли) — не запасной адрес
        assert_eq!(same_key_only("10.0.0.1", &all, keys), vec!["10.0.0.1", "26.0.0.1"]);
        let other = |a: &str| if a == "10.9.9.9" { Some("zzzz".to_string()) } else { keys(a) };
        assert_eq!(same_key_only("10.0.0.1", &all, other), vec!["10.0.0.1", "26.0.0.1"]);
        // у контакта вообще нет ключа — только сам адрес
        assert_eq!(same_key_only("10.0.0.9", &["10.0.0.9".to_string(), "10.0.0.8".to_string()], keys), vec!["10.0.0.9"]);
    }

    #[test]
    fn route_prefers_live_address_of_the_right_mode() {
        let relay = format!("sd-{}", "c".repeat(64));
        let all = vec!["10.0.0.1".to_string(), "26.0.0.1".to_string(), relay.clone()];
        let alive = |a: &str| match a { "26.0.0.1" => Some(40), _ => None };
        assert_eq!(order("10.0.0.1", &all, false, alive), vec!["26.0.0.1", "10.0.0.1"]);
        assert_eq!(order("10.0.0.1", &all, true, alive), vec![relay.clone()]);
        assert_eq!(order("10.0.0.9", &["10.0.0.9".to_string()], true, |_| None), vec!["10.0.0.9"]);
    }
}

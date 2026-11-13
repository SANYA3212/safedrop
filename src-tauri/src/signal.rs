//! Signaling server: profiles sync + chat + WebRTC call signaling.
//! Port = sig_port (default 47832). Protocol: one JSON object per TCP connection,
//! terminated by newline. Large avatar dataURLs (~200KB) are OK.

use crate::state::{AppState, ChatMsg, Group, GroupSyncState, Peer};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio::{
    io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GroupRef {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default)]
    pub initiator: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
pub(crate) enum Frame {
    #[serde(rename = "hello")]
    Hello { nickname: String, avatar: String, device: String, #[serde(default)] file_confirm: bool, #[serde(default)] chat_ack: bool, #[serde(default)] watch: bool, #[serde(default)] version: String, #[serde(default)] group_sync: GroupSyncState },
    #[serde(rename = "hello-reply")]
    HelloReply { nickname: String, avatar: String, device: String, #[serde(default)] file_confirm: bool, #[serde(default)] chat_ack: bool, #[serde(default)] watch: bool, #[serde(default)] version: String, #[serde(default)] group_sync: GroupSyncState },
    #[serde(rename = "group-sync")]
    GroupSync { #[serde(default)] state: GroupSyncState },
    #[serde(rename = "chat")]
    Chat {
        from: String,
        nickname: String,
        text: String,
        ts: String,
        #[serde(default)]
        id: String,
        #[serde(default)]
        sent_at: i64,
        #[serde(default)]
        image: String,
        #[serde(default)]
        images: Vec<String>,
        #[serde(default)]
        audio: String,
        #[serde(default)]
        to_group: Option<String>,
        #[serde(default)]
        group_name: String,
        #[serde(default)]
        members: Vec<String>,
        #[serde(default)]
        reply_to: String,
        #[serde(default)]
        forwarded_from: String,
    },
    #[serde(rename = "chat-sync-request")]
    ChatSyncRequest { #[serde(default)] known_ids: Vec<String> },
    #[serde(rename = "chat-sync")]
    ChatSync { messages: Vec<ChatMsg> },
    #[serde(rename = "chat-ack")]
    ChatAck { ids: Vec<String> },
    #[serde(rename = "chat-read")]
    ChatRead { from: String, id: String },
    #[serde(rename = "call-offer")]
    CallOffer { from: String, sdp: String, #[serde(default)] group: Option<GroupRef> },
    #[serde(rename = "call-answer")]
    CallAnswer { from: String, sdp: String },
    #[serde(rename = "ice")]
    Ice { from: String, candidate: String },
    #[serde(rename = "call-hangup")]
    Hangup { from: String },
    #[serde(rename = "screen-offer")]
    ScreenOffer { from: String, sdp: String, share_id: String, #[serde(default)] label: String, #[serde(default)] with_audio: bool, #[serde(default)] call_id: String },
    #[serde(rename = "screen-answer")]
    ScreenAnswer { from: String, share_id: String, sdp: String },
    #[serde(rename = "screen-ice")]
    ScreenIce { from: String, share_id: String, candidate: String },
    #[serde(rename = "screen-stop")]
    ScreenStop { from: String, share_id: String, #[serde(default)] request: bool },
    #[serde(rename = "profile-hash")]
    ProfileHash { hash: String, device: String },
    #[serde(rename = "presence")]
    Presence { state: String, #[serde(default)] detail: String },
    #[serde(rename = "chat-delete")]
    ChatDelete { from: String, ids: Vec<String>, all: bool },
    #[serde(rename = "chat-edit")]
    ChatEdit { from: String, id: String, text: String, #[serde(default)] edited_at: i64 },
    #[serde(rename = "chat-react")]
    ChatReact { from: String, nickname: String, id: String, emoji: String, #[serde(default)] remove: bool },
    #[serde(rename = "chat-typing")]
    ChatTyping { from: String, nickname: String, #[serde(default)] to_group: Option<String> },
    #[serde(rename = "yt-sync")]
    YtSync { from: String, #[serde(default)] ip: String, #[serde(default)] nickname: String, #[serde(default)] action: String, #[serde(default)] video: String, #[serde(default)] pos: f64, #[serde(default)] paused: bool, #[serde(default)] at: i64, #[serde(default)] seq: i64, #[serde(default)] title: String, #[serde(default)] by: String, #[serde(default)] watching: bool, #[serde(default)] vertical: bool, #[serde(default)] queue: serde_json::Value, #[serde(default)] qv: i64, #[serde(default, rename = "loop")] repeat: String, #[serde(default)] lv: i64, #[serde(default)] to_group: Option<String> },
    /// «Листать вместе»: участник открыл страницу в окне браузера.
    #[serde(rename = "web-sync")]
    WebSync { from: String, #[serde(default)] action: String, #[serde(default)] url: String, #[serde(default)] title: String, #[serde(default)] at: i64, #[serde(default)] media: String, #[serde(default)] t: f64, #[serde(default)] x: f64, #[serde(default)] y: f64, #[serde(default)] h: f64, #[serde(default)] v: i64, #[serde(default)] seq: i64, #[serde(default)] to_group: Option<String> },
    #[serde(other)]
    Unknown,
}

/// Hash of a profile (nick+avatar) for cheap change detection.
pub fn profile_hash(nick: &str, avatar: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(nick.as_bytes());
    h.update([0u8]);
    h.update(avatar.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

const MAX_AVATAR: usize = 300_000; // chars

/// Peers' avatars and chat images are rendered as <img src>, so accept only a
/// plain base64 image data URL — anything else could break out of the attribute.
pub fn is_data_image(s: &str) -> bool {
    let body = ["data:image/png;base64,", "data:image/jpeg;base64,", "data:image/webp;base64,", "data:image/gif;base64,"]
        .iter()
        .find_map(|p| s.strip_prefix(p));
    match body {
        Some(b) => b.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/' || c == b'='),
        None => false,
    }
}

fn trim_avatar(s: &str) -> String {
    if s.len() > MAX_AVATAR || !is_data_image(s) {
        String::new()
    } else {
        s.to_string()
    }
}

/// Sender clocks can be wrong or in another time zone: never trust timestamps
/// from the future. Old clients only send `ts` (HH:MM), keep it as a fallback.
fn normalize_sent_at(sent_at: i64) -> i64 {
    if sent_at <= 0 { 0 } else { sent_at.min(chrono::Utc::now().timestamp_millis()).max(1) }
}

const SIGNAL_SLOTS: usize = 128;

pub async fn run_signal_server(app: AppHandle, port: u16) {
    let addr = format!("0.0.0.0:{port}");
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("SafeDrop signal bind {addr} failed: {e}");
            return;
        }
    };
    println!("SafeDrop signal on {addr}");
    // Кадр живёт секунды; столько одновременных соединений с запасом хватает
    // на группы и звонки, а зависшие или зацикленные клиенты не съедят память.
    let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(SIGNAL_SLOTS));
    loop {
        let (sock, peer_addr) = match listener.accept().await {
            Ok(x) => x,
            Err(_) => continue,
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            eprintln!("signal: занято {SIGNAL_SLOTS} соединений, {peer_addr} отклонён");
            continue; // сокет закрывается здесь
        };
        let app2 = app.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let ip = peer_addr.ip().to_string();
            if let Err(e) = handle_frame(app2, sock, ip).await {
                eprintln!("signal error: {e}");
            }
        });
    }
}

async fn handle_frame(app: AppHandle, sock: TcpStream, ip: String) -> Result<(), String> {
    let (sock, auth) = match crate::tls::accept(sock).await {
        Ok(x) => x,
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()), // tcp_ping
        Err(e) => return Err(e.to_string()),
    };
    // контакт с запомненным ключом: чужой ключ или соединение без ключа не разбираем
    crate::tls::check_inbound(&ip, &auth)?;
    serve_frame(app, sock, ip).await
}

/// Один кадр с уже установленного соединения (напрямую или через сервер).
pub(crate) async fn serve_frame(app: AppHandle, mut sock: crate::tls::Stream, ip: String) -> Result<(), String> {
    let mut reader = BufReader::new(&mut sock);
    // one JSON line per connection, size- and time-limited
    let buf = crate::quic::read_line_limited(&mut reader, crate::quic::MAX_LINE, std::time::Duration::from_secs(60)).await?;
    if buf.is_empty() {
        return Ok(());
    }
    if buf.len() > crate::quic::MAX_LINE {
        return Err("frame too big".into());
    }
    let frame: Frame = serde_json::from_slice(&buf).map_err(|e| e.to_string())?;
    drop(reader);
    let r = process_frame(&app, frame, ip, &mut sock).await;
    // TLS держит ответ в буфере до flush; shutdown корректно закрывает сессию
    let _ = sock.flush().await;
    let _ = sock.shutdown().await;
    r
}

async fn persist_group_sync(state: &AppState) {
    let sync = state.group_sync.lock().await.clone();
    let _ = std::fs::create_dir_all(&state.app_dir);
    if let Ok(data) = serde_json::to_string_pretty(&sync) {
        let _ = std::fs::write(state.group_sync_path(), data);
    }
}

/// Who sent a frame: every address of that contact (LAN, Radmin, ...) and its
/// nickname from hello. An unknown host gets just the connection IP.
async fn sender_identity(state: &AppState, ip: &str) -> (Vec<String>, String) {
    let peers = state.peers.lock().await;
    let mut addrs = vec![ip.to_string()];
    let mut nick = peers.get(ip).map(|p| p.nickname.trim().to_string()).unwrap_or_default();
    for p in peers.values().filter(|p| p.ip == ip || p.ips.iter().any(|x| x == ip)) {
        addrs.push(p.ip.clone());
        addrs.extend(p.ips.iter().cloned());
        if nick.is_empty() { nick = p.nickname.trim().to_string(); }
    }
    addrs.retain(|a| !a.is_empty());
    addrs.sort();
    addrs.dedup();
    (addrs, nick)
}

/// Chats a peer may edit/react in: its direct chat (stored under any of its
/// addresses) and the groups it is a member of. For groups the value lists
/// the nicknames of the other members (messages carry no sender IP).
async fn sender_chats(state: &AppState, addrs: &[String]) -> Vec<(String, Vec<String>)> {
    let groups: Vec<Group> = state.groups.lock().await.clone();
    let peers = state.peers.lock().await;
    let mut out: Vec<(String, Vec<String>)> = addrs.iter().map(|a| (a.clone(), Vec::new())).collect();
    for g in groups.iter().filter(|g| g.members.iter().any(|m| addrs.contains(m))) {
        let others = g.members.iter()
            .filter(|m| !addrs.contains(m))
            .filter_map(|m| peers.get(m).or_else(|| peers.values().find(|p| p.ips.iter().any(|x| x == m))))
            .map(|p| p.nickname.trim().to_string())
            .filter(|n| !n.is_empty())
            .collect();
        out.push((format!("g:{}", g.id), others));
    }
    out
}

/// Group id seen anywhere locally (group list or sync records).
fn group_known(gid: &str, groups: &[Group], sync: &GroupSyncState) -> bool {
    groups.iter().any(|g| g.id == gid)
        || sync.deleted.iter().any(|d| d.id == gid)
        || sync.departures.iter().any(|d| d.group_id == gid)
        || sync.rejoins.iter().any(|r| r.group_id == gid)
        || sync.metas.iter().any(|m| m.id == gid)
}

/// The source is (or was) a member of the group as far as we know.
fn source_in_group(gid: &str, src: &[String], groups: &[Group], sync: &GroupSyncState) -> bool {
    let has = |list: &Vec<String>| list.iter().any(|m| src.contains(m));
    groups.iter().any(|g| g.id == gid && has(&g.members))
        || sync.deleted.iter().any(|d| d.id == gid && has(&d.members))
        || sync.departures.iter().any(|d| d.group_id == gid && (src.contains(&d.member_ip) || has(&d.members)))
        || sync.rejoins.iter().any(|r| r.group_id == gid && (src.contains(&r.member_ip) || has(&r.members)))
}

/// Merge group deletions, departures and re-joins, then relay newly learned
/// records to the other participants so offline peers converge as well.
/// Records from a remote host (`source_ip`) are accepted only from members of
/// that group; local commands (`None`) are trusted.
pub async fn merge_group_sync(app: &AppHandle, incoming: GroupSyncState, source_ip: Option<&str>) {
    let state: tauri::State<'_, AppState> = app.state();
    let mut forwarded = GroupSyncState::default();
    let src_addrs: Option<Vec<String>> = match source_ip {
        Some(ip) => Some(sender_identity(&state, ip).await.0),
        None => None,
    };
    let local_groups: Vec<Group> = if src_addrs.is_some() { state.groups.lock().await.clone() } else { Vec::new() };
    {
        let mut local = state.group_sync.lock().await;
        // checked against the live records: a rejoin accepted earlier in this
        // batch makes its member trusted for the records that follow
        let trusted = |gid: &str, local: &GroupSyncState| match &src_addrs {
            None => true,
            Some(src) => source_in_group(gid, src, &local_groups, local),
        };
        let unknown = |gid: &str, local: &GroupSyncState| !group_known(gid, &local_groups, local);
        let is_source = |ip: &str| src_addrs.as_ref().map_or(false, |src| src.iter().any(|a| a == ip));
        for mut deletion in incoming.deleted {
            if deletion.id.is_empty() { continue; }
            // unknown group: nothing to delete, keep the record so it propagates
            if !trusted(&deletion.id, &local) && !unknown(&deletion.id, &local) { continue; }
            deletion.members.sort();
            deletion.members.dedup();
            if let Some(current) = local.deleted.iter_mut().find(|x| x.id == deletion.id) {
                let old = current.clone();
                current.deleted_at = current.deleted_at.max(deletion.deleted_at);
                current.members.extend(deletion.members);
                current.members.sort();
                current.members.dedup();
                if *current != old { forwarded.deleted.push(current.clone()); }
            } else {
                local.deleted.push(deletion.clone());
                forwarded.deleted.push(deletion);
            }
        }
        for mut departure in incoming.departures {
            if departure.group_id.is_empty() || departure.member_ip.is_empty() { continue; }
            // a member leaves itself, or a member removes someone
            if !is_source(&departure.member_ip) && !trusted(&departure.group_id, &local) { continue; }
            if local.rejoins.iter().any(|r| r.group_id == departure.group_id && r.member_ip == departure.member_ip && r.joined_at >= departure.left_at) { continue; }
            departure.members.sort();
            departure.members.dedup();
            if let Some(current) = local.departures.iter_mut().find(|x| x.group_id == departure.group_id && x.member_ip == departure.member_ip) {
                let old = current.clone();
                current.left_at = current.left_at.max(departure.left_at);
                current.members.extend(departure.members);
                current.members.push(current.member_ip.clone());
                current.members.sort();
                current.members.dedup();
                if *current != old { forwarded.departures.push(current.clone()); }
            } else {
                departure.members.push(departure.member_ip.clone());
                departure.members.sort();
                departure.members.dedup();
                local.departures.push(departure.clone());
                forwarded.departures.push(departure);
            }
        }
        for mut rejoin in incoming.rejoins {
            if rejoin.group_id.is_empty() || rejoin.member_ip.is_empty() { continue; }
            // added by a member, own rejoin, or an invite to a group we never saw
            if !is_source(&rejoin.member_ip) && !trusted(&rejoin.group_id, &local) && !unknown(&rejoin.group_id, &local) { continue; }
            if local.departures.iter().any(|d| d.group_id == rejoin.group_id && d.member_ip == rejoin.member_ip && d.left_at >= rejoin.joined_at) { continue; }
            let rejoin_group_id = rejoin.group_id.clone();
            let rejoin_member_ip = rejoin.member_ip.clone();
            rejoin.members.push(rejoin.member_ip.clone());
            rejoin.members.sort();
            rejoin.members.dedup();
            if let Some(current) = local.rejoins.iter_mut().find(|x| x.group_id == rejoin.group_id && x.member_ip == rejoin.member_ip) {
                let old = current.clone();
                if rejoin.joined_at >= current.joined_at {
                    current.joined_at = rejoin.joined_at;
                    if !rejoin.name.is_empty() { current.name = rejoin.name; }
                }
                current.members.extend(rejoin.members);
                current.members.sort();
                current.members.dedup();
                if *current != old { forwarded.rejoins.push(current.clone()); }
            } else {
                local.rejoins.push(rejoin.clone());
                forwarded.rejoins.push(rejoin);
            }
            local.departures.retain(|d| !(d.group_id == rejoin_group_id && d.member_ip == rejoin_member_ip));
        }
        for mut meta in incoming.metas {
            if meta.id.is_empty() { continue; }
            // rename/avatar only from members (or for a group new to us)
            if !trusted(&meta.id, &local) && !unknown(&meta.id, &local) { continue; }
            meta.avatar = trim_avatar(&meta.avatar);
            meta.name = meta.name.chars().take(40).collect();
            if let Some(current) = local.metas.iter_mut().find(|x| x.id == meta.id) {
                if meta.updated_at >= current.updated_at {
                    let old = current.clone();
                    if !meta.name.is_empty() { current.name = meta.name; }
                    current.avatar = meta.avatar;
                    current.updated_at = current.updated_at.max(meta.updated_at);
                    if *current != old { forwarded.metas.push(current.clone()); }
                }
            } else {
                local.metas.push(meta.clone());
                forwarded.metas.push(meta);
            }
        }
    }
    if forwarded.deleted.is_empty() && forwarded.departures.is_empty() && forwarded.rejoins.is_empty() && forwarded.metas.is_empty() { return; }
    persist_group_sync(&state).await;

    let local_ips: std::collections::HashSet<String> = crate::discovery::my_ips().into_iter().collect();
    let deleted_ids: std::collections::HashSet<String> = forwarded.deleted.iter().map(|d| d.id.clone()).collect();
    let all_deleted: std::collections::HashSet<String> = state.group_sync.lock().await.deleted.iter().map(|d| d.id.clone()).collect();
    let (all_departures, all_rejoins) = {
        let sync = state.group_sync.lock().await;
        (sync.departures.clone(), sync.rejoins.clone())
    };
    let local_departures: std::collections::HashSet<String> = all_departures.iter()
        .filter(|d| local_ips.contains(&d.member_ip))
        .map(|d| d.group_id.clone()).collect();

    let all_metas = state.group_sync.lock().await.metas.clone();
    let groups_changed = {
        let mut groups = state.groups.lock().await;
        let old = groups.clone();
        for meta in &all_metas {
            if let Some(group) = groups.iter_mut().find(|g| g.id == meta.id) {
                if meta.updated_at >= group.updated_at {
                    if !meta.name.is_empty() { group.name = meta.name.clone(); }
                    group.avatar = meta.avatar.clone();
                    group.updated_at = meta.updated_at;
                }
            }
        }
        groups.retain(|g| !all_deleted.contains(&g.id) && !local_departures.contains(&g.id));
        for rejoin in &all_rejoins {
            if all_deleted.contains(&rejoin.group_id) { continue; }
            if all_departures.iter().any(|d| d.group_id == rejoin.group_id && d.member_ip == rejoin.member_ip && d.left_at >= rejoin.joined_at) { continue; }
            if local_ips.contains(&rejoin.member_ip) && !groups.iter().any(|g| g.id == rejoin.group_id) {
                groups.push(Group { id: rejoin.group_id.clone(), name: rejoin.name.clone(), members: rejoin.members.clone(), avatar: String::new(), updated_at: 0 });
            }
            if let Some(group) = groups.iter_mut().find(|g| g.id == rejoin.group_id) {
                group.members.extend(rejoin.members.iter().cloned());
                group.members.sort();
                group.members.dedup();
            }
        }
        for group in groups.iter_mut() {
            group.members.retain(|member| !all_departures.iter().any(|d| d.group_id == group.id && d.member_ip == *member));
        }
        *groups != old
    };
    let chats_changed = {
        let mut chats = state.chats.lock().await;
        let old_len = chats.len();
        for id in deleted_ids.iter().chain(local_departures.iter()) {
            chats.remove(&format!("g:{id}"));
        }
        chats.len() != old_len
    };
    if groups_changed { persist_groups(&state).await; }
    if chats_changed { persist_chats(&state).await; }
    let groups: Vec<Group> = state.groups.lock().await.clone();
    let _ = app.emit("safedrop-groups", groups);
    for id in deleted_ids.iter().chain(local_departures.iter()) {
        let _ = app.emit("safedrop-chat-changed", serde_json::json!({"peer": format!("g:{id}")}));
    }

    let mut targets = std::collections::HashSet::new();
    for deletion in &forwarded.deleted { targets.extend(deletion.members.iter().cloned()); }
    for departure in &forwarded.departures { targets.extend(departure.members.iter().cloned()); }
    for rejoin in &forwarded.rejoins {
        targets.extend(rejoin.members.iter().cloned());
        targets.insert(rejoin.member_ip.clone());
    }
    if !forwarded.metas.is_empty() {
        let groups_for_metas: Vec<Group> = state.groups.lock().await.iter()
            .filter(|g| forwarded.metas.iter().any(|m| m.id == g.id))
            .cloned()
            .collect();
        for group in groups_for_metas { targets.extend(group.members.iter().cloned()); }
    }
    targets.retain(|ip| !ip.is_empty() && !local_ips.contains(ip) && source_ip != Some(ip.as_str()));
    if targets.is_empty() { return; }
    let port = state.settings.lock().await.sig_port;
    let value = serde_json::json!({"kind":"group-sync", "state":forwarded});
    for ip in targets {
        let app = app.clone();
        let value = value.clone();
        tokio::spawn(async move { let _ = crate::quic::send_frame_dual(&app, &ip, port, value, false).await; });
    }
}

/// Shared frame processing for TCP and QUIC transports.
/// `writer` carries replies (e.g. hello-reply) back on the same stream.
pub(crate) async fn process_frame(
    app: &AppHandle,
    frame: Frame,
    ip: String,
    writer: &mut (impl AsyncWrite + Unpin),
) -> Result<(), String> {
    let state: tauri::State<'_, AppState> = app.state();
    // один компьютер — один контакт: с какого бы из его адресов ни пришло
    // (ключ проверен TLS ещё до разбора кадра), это его чат и его профиль
    let ip = crate::ident::canon(&state, &ip).await;
    let now = chrono::Local::now().format("%H:%M").to_string();

    match frame {
        Frame::Hello { nickname, avatar, device, file_confirm, chat_ack, watch, version, group_sync } => {
            merge_group_sync(app, group_sync, Some(&ip)).await;
            let avatar = trim_avatar(&avatar);
            {
                let mut g = state.peers.lock().await;
                let e = g.entry(ip.clone()).or_insert(Peer {
                    ip: ip.clone(), name: device.clone(), nickname: String::new(),
                    avatar: String::new(), custom_avatar: String::new(),
                    trusted: false, online: true, ping_ms: None, last_seen: None,
                    supports_file_confirm: file_confirm, supports_chat_ack: chat_ack, supports_watch: watch,
                    phash: String::new(), state: "idle".into(), detail: String::new(),
                    ips: vec![ip.clone()],
                    version: String::new(),
                });
                e.nickname = nickname.clone();
                e.avatar = avatar.clone();
                e.phash = profile_hash(&nickname, &avatar);
                e.supports_file_confirm = file_confirm;
                e.supports_chat_ack = chat_ack;
                e.supports_watch = watch;
                e.version = version.clone();
                crate::tls::note_peer_version(&ip, &version);
                e.online = true;
                if !e.ips.contains(&ip) { e.ips.push(ip.clone()); }
                e.last_seen = Some(chrono::Local::now().format("%H:%M:%S").to_string());
                if e.name.is_empty() || e.name == e.ip {
                    e.name = if device.is_empty() { nickname.clone() } else { device.clone() };
                }
            }
            crate::settings::persist_peers(&state);
            let peers: Vec<Peer> = state.peers.lock().await.values().cloned().collect();
            let _ = app.emit("safedrop-peers", peers);
            // reply with our profile so the caller learns us too
            let (nn, av, dev) = {
                let s = state.settings.lock().await;
                (
                    if s.profile.nickname.is_empty() { s.device_name.clone() } else { s.profile.nickname.clone() },
                    s.profile.avatar.clone(),
                    s.device_name.clone(),
                )
            };
            let group_sync = state.group_sync.lock().await.clone();
            let reply = serde_json::json!({"kind":"hello-reply","nickname":nn,"avatar":av,"device":dev,"file_confirm":true,"chat_ack":true,"watch":true,"version":crate::app_version(),"group_sync":group_sync});
            let mut out = serde_json::to_string(&reply).unwrap_or_default();
            out.push('\n');
            // write back on same stream
            let _ = writer.write_all(out.as_bytes()).await;
            let _ = writer.flush().await;
            let app2 = app.clone();
            let ip2 = ip.clone();
            tokio::spawn(async move { flush_pending_for_peer(&app2, &ip2, true).await; });
        }
        Frame::HelloReply { nickname, avatar, device, file_confirm, chat_ack, watch: _, version: _, group_sync } => {
            // only happens on client side reading; ignore on server
            let _ = (nickname, avatar, device, file_confirm, chat_ack);
            merge_group_sync(app, group_sync, Some(&ip)).await;
        }
        Frame::GroupSync { state: group_sync } => {
            let local_ips: std::collections::HashSet<String> = crate::discovery::my_ips().into_iter().collect();
            let restored_groups: Vec<String> = group_sync.rejoins.iter()
                .filter(|r| local_ips.contains(&r.member_ip))
                .map(|r| r.group_id.clone())
                .collect();
            merge_group_sync(app, group_sync, Some(&ip)).await;
            if !restored_groups.is_empty() {
                let chats = state.chats.lock().await;
                let mut known_ids = Vec::new();
                for group_id in restored_groups {
                    if let Some(rows) = chats.get(&format!("g:{group_id}")) {
                        known_ids.extend(rows.iter().rev().take(500).map(|m| m.id.clone()));
                    }
                }
                drop(chats);
                known_ids.sort();
                known_ids.dedup();
                let port = state.settings.lock().await.sig_port;
                let request = serde_json::json!({"kind":"chat-sync-request", "known_ids": known_ids});
                let _ = crate::quic::send_frame_dual(app, &ip, port, request, false).await;
            }
        }
        Frame::Chat { from: _, nickname, text, ts, id, sent_at, image, images, audio, to_group, group_name, mut members, reply_to, forwarded_from } => {
            let text = text.chars().take(2000).collect::<String>();
            let mut images: Vec<String> = images.into_iter()
                .filter(|s| s.len() <= 700_000 && is_data_image(s))
                .take(10)
                .collect();
            if images.is_empty() && image.len() <= 700_000 && is_data_image(&image) {
                images.push(image);
            }
            let image = images.first().cloned().unwrap_or_default();
            let sent_at = if sent_at <= 0 {
                chrono::Utc::now().timestamp_millis()
            } else {
                normalize_sent_at(sent_at)
            };
            let audio = if audio.is_empty() {
                String::new()
            } else {
                let dir = state.settings.lock().await.loaddrop_dir.clone();
                local_audio_path(&dir, &audio)
            };
            // group message: sync group definition, store under g:<id>
            let (key, title) = if let Some(gid) = to_group {
                let key = format!("g:{gid}");
                let my_ips: std::collections::HashSet<String> = crate::discovery::my_ips().into_iter().collect();
                let sync = state.group_sync.lock().await.clone();
                if sync.deleted.iter().any(|d| d.id == gid)
                    || sync.departures.iter().any(|d| d.group_id == gid && my_ips.contains(&d.member_ip))
                {
                    return Ok(());
                }
                members.retain(|m| !my_ips.contains(m) && !sync.departures.iter().any(|d| d.group_id == gid && d.member_ip == *m));
                let meta = sync.metas.iter().find(|m| m.id == gid).cloned();
                {
                    let mut gs = state.groups.lock().await;
                    if let Some(g) = gs.iter_mut().find(|x| x.id == gid) {
                        if !group_name.is_empty() {
                            g.name = group_name.clone();
                        }
                        if let Some(meta) = &meta {
                            if meta.updated_at >= g.updated_at {
                                if !meta.name.is_empty() { g.name = meta.name.clone(); }
                                g.avatar = meta.avatar.clone();
                                g.updated_at = meta.updated_at;
                            }
                        }
                        for m in &members {
                            if !g.members.contains(m) {
                                g.members.push(m.clone());
                            }
                        }
                        if !g.members.contains(&ip) {
                            g.members.push(ip.clone());
                        }
                    } else {
                        let mut ms = members.clone();
                        if !ms.contains(&ip) {
                            ms.push(ip.clone());
                        }
                        let name = if let Some(meta) = &meta {
                            if !meta.name.is_empty() { meta.name.clone() }
                            else if group_name.is_empty() { "Группа".into() } else { group_name.clone() }
                        } else if group_name.is_empty() { "Группа".into() } else { group_name.clone() };
                        gs.push(Group {
                            id: gid.clone(),
                            name,
                            members: ms,
                            avatar: meta.as_ref().map(|m| m.avatar.clone()).unwrap_or_default(),
                            updated_at: meta.as_ref().map(|m| m.updated_at).unwrap_or(0),
                        });
                    }
                }
                persist_groups(&state).await;
                let gs: Vec<Group> = state.groups.lock().await.clone();
                let _ = app.emit("safedrop-groups", gs);
                let t = if group_name.is_empty() { "Группа".to_string() } else { group_name.clone() };
                (key, format!("{t} • {nickname}"))
            } else {
                (ip.clone(), nickname.clone())
            };
            let msg = ChatMsg {
                id: if id.is_empty() { uuid::Uuid::new_v4().to_string() } else { id },
                peer: key.clone(),
                from_me: false,
                from_nick: nickname.clone(),
                text: text.clone(),
                ts: if ts.is_empty() { now } else { ts },
                sent_at,
                image,
                images,
                audio,
                forwarded_from: forwarded_from.trim().chars().take(64).collect(),
                status: String::new(),
                delivered_to: Vec::new(),
                read: false,
                reply_to: reply_to.clone(),
                edited_at: 0,
                reactions: Vec::new(),
                // кадр пришёл напрямую от автора: адрес соединения (у контакта с
                // запомненным ключом он подтверждён ключом) — автор сообщения
                from_ip: ip.clone(),
            };
            let is_new = {
                let mut c = state.chats.lock().await;
                let rows = c.entry(key.clone()).or_default();
                let fresh = !rows.iter().any(|m| m.id == msg.id);
                if fresh {
                    rows.push(msg.clone());
                    rows.sort_by_key(|m| m.sent_at);
                }
                fresh
            };
            let ack_id = msg.id.clone();
            if !is_new {
                // A resend of a message we already have: just confirm it again.
                let app2 = app.clone();
                let ip2 = ip.clone();
                tokio::spawn(async move { send_chat_ack(&app2, &ip2, vec![ack_id]).await; });
                return Ok(());
            }
            persist_chats(&state).await;
            let _ = app.emit("safedrop-chat", msg.clone());
            // native Windows toast: avatar + name + text
            let av = state.peers.lock().await.get(&ip).map(|p| p.display_avatar());
            let icon = av.and_then(|a| avatar_icon_path(&state.app_dir, &ip, &a));
            let body = if !text.is_empty() {
                text.chars().take(120).collect()
            } else if !msg.audio.is_empty() {
                let ext = std::path::Path::new(&msg.audio).extension()
                    .and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                if matches!(ext.as_str(), "mp4" | "m4v" | "mov" | "mkv") { "🎬 видео".to_string() } else { "🎵 аудио".to_string() }
            } else {
                "📷 картинка".to_string()
            };
            notify(&title, &body, icon);
            // acknowledge only to peers that understand chat-ack (0.3.12+); time uses sent_at (0.3.13+)
            let app2 = app.clone();
            let ip2 = ip.clone();
            tokio::spawn(async move { send_chat_ack(&app2, &ip2, vec![ack_id]).await; });
        }
        Frame::ChatSyncRequest { known_ids } => {
            let app2 = app.clone();
            let peer_ip = ip.clone();
            tokio::spawn(async move { send_chat_sync(&app2, &peer_ip, known_ids).await; });
        }
        Frame::ChatSync { messages } => {
            let (my_nick, loaddrop_dir) = {
                let s = state.settings.lock().await;
                let nick = if s.profile.nickname.is_empty() { s.device_name.clone() } else { s.profile.nickname.clone() };
                (nick, s.loaddrop_dir.clone())
            };
            let group_ids: std::collections::HashSet<String> = state.groups.lock().await.iter()
                .filter(|g| g.members.contains(&ip))
                .map(|g| format!("g:{}", g.id)).collect();
            let mut changed = std::collections::HashSet::new();
            let mut ack_ids = Vec::new();
            {
                let mut chats = state.chats.lock().await;
                for mut msg in messages {
                    msg.sent_at = normalize_sent_at(msg.sent_at);
                    msg.edited_at = normalize_sent_at(msg.edited_at);
                    msg.text = msg.text.chars().take(2000).collect();
                    // same rules as a live chat frame: only plain image data URLs
                    msg.images.retain(|s| s.len() <= 700_000 && is_data_image(s));
                    msg.images.truncate(10);
                    if msg.images.is_empty() && msg.image.len() <= 700_000 && is_data_image(&msg.image) {
                        msg.images.push(msg.image.clone());
                    }
                    msg.image = msg.images.first().cloned().unwrap_or_default();
                    let is_group = msg.peer.starts_with("g:");
                    if is_group {
                        if !group_ids.contains(&msg.peer) { continue; }
                        msg.from_me = msg.from_nick == my_nick;
                    } else {
                        // Direct chat keys are local IPs; flip the remote copy's perspective.
                        msg.peer = ip.clone();
                        msg.from_me = !msg.from_me;
                    }
                    // Delivery state is local to the sender; never inherit it from sync.
                    msg.status.clear();
                    msg.delivered_to.clear();
                    // история пришла от другого участника: кто автор, по ней не проверить
                    msg.from_ip.clear();
                    msg.read = false;
                    msg.forwarded_from = msg.forwarded_from.trim().chars().take(64).collect();
                    // Remote absolute paths are meaningless here: resolve audio from
                    // LoadDrop (for restored own messages too, never a peer's path).
                    if !msg.audio.is_empty() {
                        msg.audio = local_audio_path(&loaddrop_dir, &msg.audio);
                    }
                    let rows = chats.entry(msg.peer.clone()).or_default();
                    // Message IDs are the identity. Comparing only minute-level
                    // timestamps and body text dropped legitimate repeated messages.
                    let duplicate = if msg.id.is_empty() {
                        rows.iter().any(|m| m.sent_at == msg.sent_at &&
                            m.from_me == msg.from_me && m.from_nick == msg.from_nick &&
                            m.text == msg.text && m.images == msg.images && m.audio == msg.audio)
                    } else {
                        rows.iter().any(|m| m.id == msg.id)
                    };
                    if !duplicate {
                        if !msg.id.is_empty() { ack_ids.push(msg.id.clone()); }
                        changed.insert(msg.peer.clone());
                        rows.push(msg);
                        rows.sort_by(|a, b| a.sent_at.cmp(&b.sent_at).then_with(|| a.id.cmp(&b.id)));
                    }
                }
            }
            if !changed.is_empty() {
                persist_chats(&state).await;
                for peer in changed { let _ = app.emit("safedrop-chat-changed", serde_json::json!({"peer": peer})); }
            }
            if !ack_ids.is_empty() {
                let app2 = app.clone();
                let ip2 = ip.clone();
                tokio::spawn(async move { send_chat_ack(&app2, &ip2, ack_ids).await; });
            }
        }
        Frame::ChatAck { ids } => {
            if ids.is_empty() { return Ok(()); }
            let mut changed = std::collections::HashSet::new();
            {
                let mut chats = state.chats.lock().await;
                for (key, rows) in chats.iter_mut() {
                    for m in rows.iter_mut() {
                        if !m.from_me || !ids.contains(&m.id) { continue; }
                        // repeated acks are idempotent: the same ip is stored once
                        if !m.delivered_to.iter().any(|x| x == &ip) {
                            m.delivered_to.push(ip.clone());
                        }
                        // Direct chat: one recipient. Group: two ticks once the
                        // message reached at least one member (other members may
                        // be offline; their acks still extend delivered_to).
                        m.status = if m.delivered_to.is_empty() { "sent".into() } else { "delivered".into() };
                        changed.insert(key.clone());
                    }
                }
            }
            if !changed.is_empty() {
                persist_chats(&state).await;
                let ack_ids: Vec<String> = ids.clone();
                for peer in changed.clone() { let _ = app.emit("safedrop-chat-changed", serde_json::json!({"peer": peer})); let _ = app.emit("safedrop-chat-ack", serde_json::json!({"peer": peer, "ids": ack_ids})); }
            }
        }
        Frame::ChatRead { from: _, id } => {
            let (addrs, _) = sender_identity(&state, &ip).await;
            let receipts_enabled = state.settings.lock().await.read_receipts;
            if !receipts_enabled { return Ok(()); }
            let mut changed = Vec::new();
            {
                let mut chats = state.chats.lock().await;
                // «прочитано до этого сообщения»: открыв чат, собеседник видел
                // и всё, что пришло раньше (старые версии отмечали только одно)
                for addr in addrs {
                    if let Some(rows) = chats.get_mut(&addr) {
                        let Some(upto) = rows.iter().find(|m| m.id == id && m.from_me).map(|m| m.sent_at) else { continue };
                        let mut any = false;
                        // только дошедшие: недоставленное раньше прочитанным не станет
                        for msg in rows.iter_mut().filter(|m| m.from_me && !m.read && m.sent_at <= upto && (m.id == id || m.status == "delivered")) {
                            msg.read = true;
                            any = true;
                        }
                        if any { changed.push(addr.clone()); }
                    }
                }
            }
            if !changed.is_empty() {
                persist_chats(&state).await;
                for peer in changed {
                    let _ = app.emit("safedrop-chat-changed", serde_json::json!({"peer": peer}));
                }
            }
        }
        Frame::CallOffer { from, sdp, group } => {
            let _ = app.emit("safedrop-call-offer", serde_json::json!({"from": from, "ip": ip, "sdp": sdp, "group": group}));
            let av = state.peers.lock().await.get(&ip).map(|p| p.display_avatar());
            let icon = av.and_then(|a| avatar_icon_path(&state.app_dir, &ip, &a));
            let title = match &group {
                Some(g) if !g.name.is_empty() => format!("📞 Групповой звонок: {}", g.name),
                _ => "📞 Входящий звонок".to_string(),
            };
            // The WebView plays the user's selected looping ringtone for calls.
            // Keep the Windows toast silent so it does not overlap that ringtone.
            notify_silent(&title, &format!("{from} ({ip})"), icon);
        }
        Frame::CallAnswer { from, sdp } => {
            let _ = app.emit("safedrop-call-answer", serde_json::json!({"from": from, "ip": ip, "sdp": sdp}));
        }
        Frame::Ice { from, candidate } => {
            let _ = app.emit("safedrop-ice", serde_json::json!({"from": from, "ip": ip, "candidate": candidate}));
        }
        Frame::Hangup { from } => {
            let _ = app.emit("safedrop-hangup", serde_json::json!({"from": from, "ip": ip}));
        }
        Frame::ScreenOffer { from, sdp, share_id, label, with_audio, call_id } => {
            let _ = app.emit("safedrop-screen-offer", serde_json::json!({
                "from": from, "ip": ip, "sdp": sdp,
                "share_id": share_id, "label": label, "with_audio": with_audio, "call_id": call_id,
            }));
        }
        Frame::ScreenAnswer { from, share_id, sdp } => {
            let _ = app.emit("safedrop-screen-answer", serde_json::json!({
                "from": from, "ip": ip, "share_id": share_id, "sdp": sdp,
            }));
        }
        Frame::ScreenIce { from, share_id, candidate } => {
            let _ = app.emit("safedrop-screen-ice", serde_json::json!({
                "from": from, "ip": ip, "share_id": share_id, "candidate": candidate,
            }));
        }
        Frame::ScreenStop { from, share_id, request } => {
            let _ = app.emit("safedrop-screen-stop", serde_json::json!({
                "from": from, "ip": ip, "share_id": share_id, "request": request,
            }));
        }
        Frame::ProfileHash { hash, device } => {
            // lightweight heartbeat: if peer profile changed -> full sync
            let changed = {
                let mut g = state.peers.lock().await;
                let e = g.entry(ip.clone()).or_insert(Peer {
                    ip: ip.clone(), name: device.clone(), nickname: String::new(),
                    avatar: String::new(), custom_avatar: String::new(),
                    trusted: false, online: true, ping_ms: None, last_seen: None,
                    supports_file_confirm: false, supports_chat_ack: false, supports_watch: false,
                    phash: String::new(), state: "idle".into(), detail: String::new(),
                    ips: vec![ip.clone()],
                    version: String::new(),
                });
                e.online = true;
                e.last_seen = Some(chrono::Local::now().format("%H:%M:%S").to_string());
                if e.phash != hash {
                    e.phash = hash;
                    true
                } else {
                    false
                }
            };
            if changed {
                crate::settings::persist_peers(&state);
                let peers: Vec<Peer> = state.peers.lock().await.values().cloned().collect();
                let _ = app.emit("safedrop-peers", peers);
                // pull their full profile (their reply syncs us back too)
                let app2 = app.clone();
                let ip2 = ip.clone();
                tokio::spawn(async move { announce_to(&app2, &ip2).await; });
            }
        }
        Frame::Presence { state: st, detail } => {
            let st = if ["idle", "incall", "sharing", "incall+sharing"].contains(&st.as_str()) {
                st
            } else {
                "idle".to_string()
            };
            let detail = detail.chars().take(60).collect::<String>();
            let changed = {
                let mut g = state.peers.lock().await;
                if let Some(p) = g.get_mut(&ip) {
                    if p.state != st || p.detail != detail {
                        p.state = st;
                        p.detail = detail;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            };
            if changed {
                crate::settings::persist_peers(&state);
                let peers: Vec<Peer> = state.peers.lock().await.values().cloned().collect();
                let _ = app.emit("safedrop-peers", peers);
            }
        }
        Frame::ChatDelete { from: _, ids, all } => {            {
                let mut c = state.chats.lock().await;
                if let Some(v) = c.get_mut(&ip) {
                    if all {
                        v.clear();
                    } else {
                        v.retain(|m| !ids.contains(&m.id));
                    }
                }
            }
            persist_chats(&state).await;
            let _ = app.emit("safedrop-chat-changed", serde_json::json!({"peer": ip}));
        }
        Frame::ChatEdit { from, id, text, edited_at } => {
            // idempotent: applied by message id; text at most 2000 chars;
            // only the author of the message may edit it
            let text = text.trim().chars().take(2000).collect::<String>();
            // legacy clients send no time: treat the edit as made now
            let edited_at = if edited_at <= 0 { chrono::Utc::now().timestamp_millis() } else { normalize_sent_at(edited_at) };
            let (addrs, known_nick) = sender_identity(&state, &ip).await;
            // nickname from hello; the frame's own `from` only for unknown hosts
            let author = if known_nick.is_empty() { from.trim().to_string() } else { known_nick };
            let allowed = sender_chats(&state, &addrs).await;
            let mut updated: Option<String> = None;
            {
                let mut c = state.chats.lock().await;
                for (key, others) in &allowed {
                    let Some(rows) = c.get_mut(key) else { continue };
                    let Some(m) = rows.iter_mut().find(|m| m.id == id && !m.from_me) else { continue };
                    // direct chat: every message not ours is theirs (survives a
                    // rename); group: by the address the message came from, and
                    // for messages restored from history (no address) by nickname,
                    // not one another member holds
                    let theirs = !key.starts_with("g:") || if m.from_ip.is_empty() {
                        m.from_nick == author && !others.contains(&author)
                    } else {
                        addrs.contains(&m.from_ip)
                    };
                    // a stale resend must not undo a newer edit
                    if theirs && edited_at >= m.edited_at && (m.text != text || m.edited_at < edited_at) {
                        m.text = text.clone();
                        m.edited_at = edited_at;
                        updated = Some(key.clone());
                    }
                    break;
                }
            }
            if let Some(peer) = updated {
                persist_chats(&state).await;
                let _ = app.emit("safedrop-chat-changed", serde_json::json!({"peer": peer}));
            }
        }
        Frame::ChatReact { from, nickname, id, emoji, remove } => {
            if emoji.is_empty() || emoji.chars().count() > 8 { return Ok(()); }
            let (addrs, known_nick) = sender_identity(&state, &ip).await;
            // reactions are keyed by nickname: a peer adds/removes only its own
            // (nickname from hello; the frame's one only for unknown hosts)
            let who = if !known_nick.is_empty() {
                known_nick
            } else if nickname.trim().is_empty() {
                from.trim().to_string()
            } else {
                nickname.trim().to_string()
            };
            let allowed = sender_chats(&state, &addrs).await;
            let mut updated: Option<String> = None;
            {
                let mut c = state.chats.lock().await;
                for (key, _) in &allowed {
                    let Some(rows) = c.get_mut(key) else { continue };
                    if let Some(m) = rows.iter_mut().find(|m| m.id == id) {
                        let who = who.clone();
                        let entry = m.reactions.iter_mut().find(|r| r.emoji == emoji);
                        match (entry, remove) {
                            (Some(r), false) => {
                                if !r.by.iter().any(|b| b == &who) {
                                    r.by.push(who);
                                    updated = Some(key.clone());
                                }
                            }
                            (Some(r), true) => {
                                let before = r.by.len();
                                r.by.retain(|b| b != &who);
                                r.by.retain(|b| !b.is_empty());
                                if r.by.len() != before {
                                    if r.by.is_empty() {
                                        m.reactions.retain(|r| !(r.emoji == emoji && r.by.is_empty()));
                                    }
                                    updated = Some(key.clone());
                                }
                            }
                            (None, false) if !who.is_empty() => {
                                m.reactions.push(crate::state::ChatReaction { emoji: emoji.clone(), by: vec![who] });
                                updated = Some(key.clone());
                            }
                            _ => {}
                        }
                        break;
                    }
                }
            }
            if let Some(peer) = updated {
                persist_chats(&state).await;
                let _ = app.emit("safedrop-chat-changed", serde_json::json!({"peer": peer}));
            }
        }
        Frame::ChatTyping { from, nickname, to_group } => {
            // momentary indicator, never persisted or recorded in chat history
            let _ = app.emit("safedrop-typing", serde_json::json!({
                "ip": ip, "from": from, "nickname": nickname, "group": to_group,
            }));
        }

        Frame::YtSync { from, nickname, action, video, pos, paused, at, seq, title, by, watching, vertical, queue, qv, repeat, lv, .. } => {
            let nm = if nickname.is_empty() { from.clone() } else { nickname.clone() };
            let payload = serde_json::json!({
                "kind":"yt-sync",
                "from": nm,
                "ip": ip,
                "action": action,
                "video": video,
                "pos": pos,
                "paused": paused,
                "at": at,
                "seq": seq,
                "title": title,
                "by": by,
                "watching": watching,
                "vertical": vertical,
                "queue": queue,
                "qv": qv,
                "loop": repeat,
                "lv": lv,
            });
            let _ = app.emit("safedrop-watch", serde_json::json!({"ip": ip, "payload": payload}));
        }

        Frame::WebSync { from, action, url, title, at, media, t, x, y, h, v, seq, .. } => {
            // «Листать вместе»: только главному окну, решение о переходе — там
            let payload = serde_json::json!({
                "kind": "web-sync", "from": from, "ip": ip,
                "action": action, "url": url, "title": title, "at": at,
                "media": media, "t": t,
                "x": x, "y": y, "h": h, "v": v, "seq": seq,
            });
            let _ = app.emit_to("main", "safedrop-web", payload);
        }
        Frame::Unknown => { }
    }
    Ok(())
}

fn local_audio_path(dir: &str, remote: &str) -> String {
    let name = std::path::Path::new(remote)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if name.is_empty() {
        return String::new();
    }
    std::path::Path::new(dir)
        .join(crate::transfer::sanitize(name))
        .to_string_lossy()
        .to_string()
}

async fn persist_chats(state: &AppState) {
    crate::settings::save_chats_soon(state);
}

pub async fn persist_groups(state: &AppState) {
    let g = state.groups.lock().await;
    let _ = std::fs::create_dir_all(&state.app_dir);
    if let Ok(data) = serde_json::to_string_pretty(&*g) {
        let _ = std::fs::write(state.groups_path(), data);
    }
}

async fn send_chat_sync(app: &AppHandle, peer_ip: &str, known_ids: Vec<String>) {
    let state: tauri::State<'_, AppState> = app.state();
    let known: std::collections::HashSet<String> = known_ids.into_iter().collect();
    let group_ids: std::collections::HashSet<String> = state.groups.lock().await.iter()
        .filter(|g| g.members.contains(&peer_ip.to_string()))
        .map(|g| format!("g:{}", g.id)).collect();
    let messages = {
        let chats = state.chats.lock().await;
        let mut out = Vec::new();
        // The requester lists only its last 500 ids per chat. Offering rows
        // older than that re-sent the whole history on every hello (~30 s), so
        // look only at our own last 400 rows: they overlap the requester's window.
        if let Some(rows) = chats.get(peer_ip) {
            out.extend(rows.iter().rev().take(400).filter(|m| !known.contains(&m.id)).cloned());
        }
        for key in group_ids {
            if let Some(rows) = chats.get(&key) {
                out.extend(rows.iter().rev().take(400).filter(|m| !known.contains(&m.id)).cloned());
            }
        }
        out.sort_by(|a, b| a.sent_at.cmp(&b.sent_at).then_with(|| a.id.cmp(&b.id)));
        out
    };
    if messages.is_empty() { return; }
    let port = state.settings.lock().await.sig_port;
    // Older peers accept frames up to 1 MB, so keep batches below that. Each
    // message is measured once instead of re-serializing the growing batch.
    let mut batch = Vec::new();
    let mut batch_len = 0usize;
    for msg in messages {
        let len = serde_json::to_vec(&msg).map(|v| v.len()).unwrap_or(usize::MAX / 2) + 1;
        if !batch.is_empty() && batch_len + len > 850_000 {
            let payload = serde_json::json!({"kind":"chat-sync", "messages": std::mem::take(&mut batch)});
            let _ = crate::quic::send_frame_dual(app, peer_ip, port, payload, false).await;
            batch_len = 0;
        }
        batch.push(msg);
        batch_len += len;
    }
    if !batch.is_empty() {
        let payload = serde_json::json!({"kind":"chat-sync", "messages": batch});
        let _ = crate::quic::send_frame_dual(app, peer_ip, port, payload, false).await;
    }
}

/// Save avatar dataURL to a temp file for toast icons. Returns file path.
/// Only png/jpeg are used (toast may reject webp/gif and drop the whole toast).
pub fn avatar_icon_path(app_dir: &std::path::Path, ip: &str, avatar_dataurl: &str) -> Option<String> {
    use base64::Engine;
    let (head, b64) = avatar_dataurl.split_once(',')?;
    let ext = if head.contains("image/png") {
        "png"
    } else if head.contains("image/jpeg") || head.contains("image/jpg") {
        "jpg"
    } else {
        return None;
    };
    if b64.len() > 500_000 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    if bytes.len() < 100 {
        return None;
    }
    let dir = app_dir.join("avatars");
    let _ = std::fs::create_dir_all(&dir);
    let safe: String = ip.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
    let p = dir.join(format!("{safe}.{ext}"));
    std::fs::write(&p, &bytes).ok()?;
    Some(p.to_string_lossy().to_string())
}

/// Native Windows toast notification (branded SafeDrop via Start Menu shortcut).
pub fn notify(title: &str, body: &str, icon: Option<String>) {
    notify_impl(title, body, icon);
}

pub fn notify_silent(title: &str, body: &str, icon: Option<String>) {
    notify_impl(title, body, icon);
}

fn notify_impl(title: &str, body: &str, icon: Option<String>) {
    let mut n = notify_rust::Notification::new();
    n.summary(title);
    n.body(body);
    if let Some(ic) = icon {
        n.icon(&ic);
    } else {
        n.auto_icon();
    }
    #[cfg(windows)]
    n.app_id("com.safedrop.app");
    #[cfg(not(windows))]
    n.appname("SafeDrop");
    // Keep Windows toasts silent: the frontend plays the user's selected
    // notification sound, including while the window is minimized.
    if let Err(e) = n.show() {
        eprintln!("SafeDrop notify failed: {e:?}");
    }
}

/// Send one frame to peer. If wait_reply, read hello-reply (up to 5s),
/// otherwise flush and return immediately (chat/call/ice must not wait).
pub async fn send_frame(ip: &str, port: u16, value: serde_json::Value, wait_reply: bool) -> Result<Option<serde_json::Value>, String> {
    let mut s = crate::tls::connect(ip, port, std::time::Duration::from_secs(13)).await?;
    let mut out = serde_json::to_string(&value).map_err(|e| e.to_string())?;
    out.push('\n');
    s.write_all(out.as_bytes()).await.map_err(|e| e.to_string())?;
    s.flush().await.map_err(|e| e.to_string())?;
    if !wait_reply {
        // не просто shutdown: см. tls::finish — иначе большие кадры (картинки)
        // обрывались. Дочитываем в фоне: вызывающий (и 5-секундный бюджет
        // send_frame_dual перед QUIC) ждать получателя не должен
        tokio::spawn(crate::tls::finish(s));
        return Ok(None);
    }
    let mut reader = BufReader::new(&mut s);
    let mut buf = Vec::new();
    let r = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        reader.read_until(b'\n', &mut buf),
    )
    .await;
    match r {
        Ok(Ok(n)) if n > 0 => {
            Ok(serde_json::from_slice::<serde_json::Value>(&buf).ok())
        }
        _ => Ok(None),
    }
}

/// Send a delivery acknowledgement for stored chat ids. Only peers that
/// advertised `chat_ack` get the new frame, so older clients stay compatible.
async fn send_chat_ack(app: &AppHandle, peer_ip: &str, ids: Vec<String>) {
    let state: tauri::State<'_, AppState> = app.state();
    // Gate on the peer's advertised support, but do not lose acks on a fresh
    // contact: a scanned peer has no profile hash yet, so its flags are still
    // defaults. Only skip when we positively know it is an older client.
    let capable = match state.peers.lock().await.get(peer_ip) {
        Some(p) => p.supports_chat_ack || p.phash.is_empty(),
        None => true,
    };
    if !capable {
        return;
    }
    let port = state.settings.lock().await.sig_port;
    let v = serde_json::json!({"kind":"chat-ack","ids":ids});
    let _ = crate::quic::send_frame_dual(app, peer_ip, port, v, false).await;
}

/// Re-send outgoing messages that were stored while the peer was offline.
/// Recipients deduplicate by message id, and acks move the row to "delivered".
/// File name of a stored audio path for the wire (remote peers resolve it
/// inside their own LoadDrop directory).
fn audio_name(path: &str) -> String {
    if path.is_empty() { return String::new(); }
    std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

/// Edits/reactions re-sent on hello: only this recent, at most this many per hello.
const EXTRAS_WINDOW_MS: i64 = 7 * 24 * 3600 * 1000;
const EXTRAS_MAX: usize = 30;

/// Edits/reactions already re-sent in this session (peer ip -> marks): every
/// hello (~30 s) used to re-send all of them again.
/// Аудиофайлы, уже отправленные при досылке: (собеседник, путь).
fn audio_sent() -> &'static std::sync::Mutex<std::collections::HashSet<(String, String)>> {
    static S: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<(String, String)>>> = std::sync::OnceLock::new();
    S.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

fn extras_sent() -> &'static std::sync::Mutex<std::collections::HashMap<String, std::collections::HashSet<String>>> {
    static SENT: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, std::collections::HashSet<String>>>> = std::sync::OnceLock::new();
    SENT.get_or_init(Default::default)
}

/// Re-send chat messages that are still "sent" to online peers. Called
/// periodically as well as on hello: a lost ack must not leave one tick
/// forever. Extras (edits/reactions) are only flushed on hello to avoid
/// needless traffic.
async fn flush_pending_for_peer(app: &AppHandle, peer_ip: &str, include_extras: bool) {
    let state: tauri::State<'_, AppState> = app.state();
    let (port, nick) = {
        let s = state.settings.lock().await;
        (
            s.sig_port,
            if s.profile.nickname.is_empty() { s.device_name.clone() } else { s.profile.nickname.clone() },
        )
    };
    let groups: Vec<(String, String, Vec<String>)> = state.groups.lock().await.iter()
        .map(|g| (format!("g:{}", g.id), g.name.clone(), g.members.clone()))
        .collect();
    let mut jobs: Vec<serde_json::Value> = Vec::new();
    // аудио недоставленных сообщений (id сообщения → файл): сначала файл,
    // иначе у получателя имя без файла
    let mut audio_of: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut extras: Vec<(String, serde_json::Value)> = Vec::new();
    {
        let chats = state.chats.lock().await;
        for (key, rows) in chats.iter() {
            let is_direct = key.as_str() == peer_ip;
            let group = groups.iter().find(|(k, _, members)| k == key && members.iter().any(|m| m == peer_ip));
            if !is_direct && group.is_none() { continue; }
            if is_direct {
                for m in rows.iter().rev().filter(|m| m.from_me && m.status == "sent").take(100) {
                    if !m.audio.is_empty() { audio_of.insert(m.id.clone(), m.audio.clone()); }
                    jobs.push(serde_json::json!({
                        "kind": "chat", "id": m.id, "sent_at": m.sent_at, "from": nick, "nickname": nick,
                        "text": m.text, "ts": m.ts, "image": m.image, "images": m.images,
                        "audio": audio_name(&m.audio), "reply_to": m.reply_to, "forwarded_from": m.forwarded_from,
                    }));
                }
            } else if let Some((gid, gname, members)) = group {
                for m in rows.iter().rev().filter(|m| m.from_me && m.status == "sent").take(100) {
                    jobs.push(serde_json::json!({
                        "kind": "chat", "id": m.id, "sent_at": m.sent_at, "from": nick, "nickname": nick,
                        "text": m.text, "ts": m.ts, "image": m.image, "images": m.images,
                        "audio": audio_name(&m.audio), "reply_to": m.reply_to, "forwarded_from": m.forwarded_from,
                        "to_group": gid.trim_start_matches("g:"), "group_name": gname, "members": members,
                    }));
                }
            }
            // offline catch-up for edits and own reactions (idempotent on the
            // receiving side: keyed by message id / (emoji, nickname) pair).
            // Only recent ones, each at most once per session and peer.
            if include_extras && (is_direct || group.is_some()) {
                let since = chrono::Utc::now().timestamp_millis() - EXTRAS_WINDOW_MS;
                let mut sent = extras_sent().lock().unwrap_or_else(|e| e.into_inner());
                let done = sent.entry(peer_ip.to_string()).or_default();
                if done.len() > 20_000 { done.clear(); }
                for m in rows.iter().rev().take(200) {
                    if extras.len() >= EXTRAS_MAX { break; }
                    if m.from_me && m.edited_at > since {
                        let mark = format!("e|{key}|{}|{}", m.id, m.edited_at);
                        if done.insert(mark.clone()) {
                            extras.push((mark, serde_json::json!({
                                "kind": "chat-edit", "from": nick, "id": m.id,
                                "text": m.text, "edited_at": m.edited_at,
                            })));
                        }
                    }
                    // reactions carry no time: use the message's age instead
                    if m.sent_at <= since { continue; }
                    for r in &m.reactions {
                        if extras.len() >= EXTRAS_MAX { break; }
                        if r.by.iter().any(|b| b == &nick) {
                            let mark = format!("r|{key}|{}|{}", m.id, r.emoji);
                            if done.insert(mark.clone()) {
                                extras.push((mark, serde_json::json!({
                                    "kind": "chat-react", "from": nick, "nickname": nick,
                                    "id": m.id, "emoji": r.emoji, "remove": false,
                                })));
                            }
                        }
                    }
                }
            }
        }
    }
    audio_of.retain(|_, p| std::path::Path::new(p).is_file());
    if audio_of.is_empty() {
        for v in jobs {
            let app2 = app.clone();
            let target = peer_ip.to_string();
            tokio::spawn(async move {
                let _ = crate::quic::send_frame_dual(&app2, &target, port, v, false).await;
            });
        }
    } else {
        let (tcp_port, device) = {
            let s = state.settings.lock().await;
            (s.tcp_port, s.device_name.clone())
        };
        let confirm = state.peers.lock().await.get(peer_ip).map(|p| p.supports_file_confirm).unwrap_or(false);
        let app2 = app.clone();
        let target = peer_ip.to_string();
        tokio::spawn(async move {
            for v in jobs {
                let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                if let Some(path) = audio_of.get(&id) {
                    // файл уже ушёл этому собеседнику (в этом запуске) или уходит
                    // сейчас из другой досылки — второй раз не шлём
                    let mark = (target.clone(), path.clone());
                    let fresh = audio_sent().lock().map(|mut s| s.insert(mark.clone())).unwrap_or(false);
                    if fresh {
                        let sent = crate::quic::send_file_dual(&app2, target.clone(), tcp_port, path.clone(), device.clone(), "file".into(), confirm, String::new(), String::new()).await;
                        if sent.is_err() {
                            // файл не ушёл — и сообщение пока не шлём: иначе его
                            // подтвердят, и досылать будет нечего
                            if let Ok(mut s) = audio_sent().lock() { s.remove(&mark); }
                            continue;
                        }
                    }
                }
                let _ = crate::quic::send_frame_dual(&app2, &target, port, v, false).await;
            }
        });
    }
    if extras.is_empty() { return; }
    // one connection after another instead of a burst of parallel ones;
    // stop at the first failure and let the next hello retry the rest
    let app2 = app.clone();
    let target = peer_ip.to_string();
    tokio::spawn(async move {
        for i in 0..extras.len() {
            let v = extras[i].1.clone();
            if crate::quic::send_frame_dual(&app2, &target, port, v, false).await.is_err() {
                let mut sent = extras_sent().lock().unwrap_or_else(|e| e.into_inner());
                if let Some(done) = sent.get_mut(&target) {
                    for (mark, _) in &extras[i..] { done.remove(mark); }
                }
                break;
            }
        }
    });
}

/// Periodically re-send still-unacknowledged messages to online peers. Only
/// peers that advertise chat-ack participate, so older clients are not spammed.
pub async fn retry_pending(app: &AppHandle) {
    let state: tauri::State<'_, AppState> = app.state();
    let peers: Vec<String> = state.peers.lock().await.values()
        .filter(|p| p.online && p.supports_chat_ack)
        .map(|p| p.ip.clone())
        .collect();
    for ip in peers {
        flush_pending_for_peer(app, &ip, false).await;
    }
}

/// Announce our profile to one peer; updates peer entry from reply.
pub async fn announce_to(app: &AppHandle, ip: &str) {
    let state: tauri::State<'_, AppState> = app.state();
    let (port, nn, av, dev, udp_on, udp_port) = {
        let s = state.settings.lock().await;
        (
            s.sig_port,
            if s.profile.nickname.is_empty() { s.device_name.clone() } else { s.profile.nickname.clone() },
            s.profile.avatar.clone(),
            s.device_name.clone(),
            s.udp_enabled,
            s.udp_port,
        )
    };
    let group_sync = state.group_sync.lock().await.clone();
    let v = serde_json::json!({"kind":"hello","nickname":nn,"avatar":av,"device":dev,"file_confirm":true,"chat_ack":true,"watch":true,"version":crate::app_version(),"group_sync":group_sync});
    let reply = match send_frame(ip, port, v.clone(), true).await {
        Ok(Some(r)) => Some(r),
        _ if udp_on => crate::quic::send_frame_quic(ip, udp_port, v, true).await.ok().flatten(),
        _ => None,
    };
    // после рукопожатия ключ этого адреса известен: если это новый адрес
    // знакомого устройства — склеить, и дальше работать с адресом контакта
    if reply.is_some() {
        crate::ident::merge_now(app).await;
    }
    let owner = crate::ident::owner(&state, ip).await;
    let ip: &str = &owner;
    match reply {
        Some(reply) => {
            if reply.get("kind").and_then(|k| k.as_str()) == Some("hello-reply") {
                let remote_group_sync = reply.get("group_sync").cloned()
                    .and_then(|value| serde_json::from_value::<GroupSyncState>(value).ok())
                    .unwrap_or_default();
                merge_group_sync(app, remote_group_sync, Some(ip)).await;
                let nn = reply.get("nickname").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let av = reply.get("avatar").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let dev = reply.get("device").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let file_confirm = reply.get("file_confirm").and_then(|x| x.as_bool()).unwrap_or(false);
                let chat_ack = reply.get("chat_ack").and_then(|x| x.as_bool()).unwrap_or(false);
                let watch = reply.get("watch").and_then(|x| x.as_bool()).unwrap_or(false);
                let version = reply.get("version").and_then(|x| x.as_str()).unwrap_or("").to_string();
                {
                    let mut g = state.peers.lock().await;
                    if let Some(p) = g.get_mut(ip) {
                        let av2 = trim_avatar(&av);
                        p.phash = profile_hash(&nn, &av2);
                        p.nickname = nn.clone();
                        p.avatar = av2;
                        p.supports_file_confirm = file_confirm;
                        p.supports_chat_ack = chat_ack;
                        p.supports_watch = watch;
                        p.version = version.clone();
                        crate::tls::note_peer_version(ip, &version);
                        if p.name.is_empty() || p.name == p.ip {
                            p.name = if dev.is_empty() { nn } else { dev };
                        }
                    }
                }
                crate::settings::persist_peers(&state);
                let peers: Vec<Peer> = state.peers.lock().await.values().cloned().collect();
                let _ = app.emit("safedrop-peers", peers);
                let sync_now = {
                    let mut sent = state.chat_sync_at.lock().await;
                    let due = sent.get(ip).map(|t| t.elapsed() >= std::time::Duration::from_secs(30)).unwrap_or(true);
                    if due { sent.insert(ip.to_string(), std::time::Instant::now()); }
                    due
                };
                if sync_now {
                    let groups = state.groups.lock().await;
                    let group_keys: std::collections::HashSet<String> = groups.iter()
                        .filter(|g| g.members.iter().any(|m| m == ip))
                        .map(|g| format!("g:{}", g.id)).collect();
                    drop(groups);
                    let chats = state.chats.lock().await;
                    let mut known_ids = Vec::new();
                    for key in std::iter::once(ip.to_string()).chain(group_keys.into_iter()) {
                        if let Some(rows) = chats.get(&key) {
                            known_ids.extend(rows.iter().rev().take(500).map(|m| m.id.clone()));
                        }
                    }
                    known_ids.sort();
                    known_ids.dedup();
                    if known_ids.len() > 8000 {
                        known_ids.drain(..known_ids.len() - 8000);
                    }
                    drop(chats);
                    let request = serde_json::json!({"kind":"chat-sync-request", "known_ids": known_ids});
                    let _ = crate::quic::send_frame_dual(app, ip, port, request, false).await;
                }
                let app2 = app.clone();
                let ip2 = ip.to_string();
                tokio::spawn(async move { flush_pending_for_peer(&app2, &ip2, true).await; });
            }
        }
        _ => {}
    }
}

/// Quick liveness check of currently-online peers (cheap: only known online).
/// Marks dead ones offline. Returns their ips.
pub async fn dead_online(state: &AppState, port: u16) -> Vec<String> {
    // A contact may answer on any of its addresses (LAN, Radmin, ...): the scan
    // marks it online through any of them, so it is dead only when none answers.
    let online: Vec<(String, Vec<String>)> = state
        .peers
        .lock()
        .await
        .values()
        .filter(|p| p.online)
        .map(|p| {
            let mut addrs = vec![p.ip.clone()];
            addrs.extend(p.ips.iter().filter(|x| **x != p.ip).cloned());
            (p.ip.clone(), addrs)
        })
        .collect();
    let mut dead = Vec::new();
    for (ip, addrs) in online {
        let mut alive = false;
        for a in &addrs {
            if crate::discovery::tcp_ping(a, port, 1200).await.is_some() {
                alive = true;
                break;
            }
        }
        if alive {
            state.unreachable_since.lock().await.remove(&ip);
            continue;
        }
        let expired = {
            let mut since = state.unreachable_since.lock().await;
            let first = since.entry(ip.clone()).or_insert_with(std::time::Instant::now);
            first.elapsed() >= std::time::Duration::from_secs(5)
        };
        if expired {
            state.unreachable_since.lock().await.remove(&ip);
            dead.push(ip);
        }
    }
    if !dead.is_empty() {
        let mut g = state.peers.lock().await;
        for ip in &dead {
            if let Some(p) = g.get_mut(ip) {
                p.online = false;
                p.ping_ms = None;
                p.state = "idle".into();
                p.detail = String::new();
            }
        }
    }
    dead
}

/// Lightweight heartbeat: send our profile hash to all known peers.
/// Peers whose stored hash differs pull the full profile.
pub async fn broadcast_hash(app: &AppHandle) {
    let state: tauri::State<'_, AppState> = app.state();
    let (port, hash, dev, ips) = {
        let s = state.settings.lock().await;
        let nn = if s.profile.nickname.is_empty() {
            s.device_name.clone()
        } else {
            s.profile.nickname.clone()
        };
        let h = profile_hash(&nn, &s.profile.avatar);
        let g = state.peers.lock().await;
        (s.sig_port, h, s.device_name.clone(), g.keys().cloned().collect::<Vec<_>>())
    };
    for ip in ips {
        let v = serde_json::json!({"kind":"profile-hash","hash":hash,"device":dev});
        let _ = send_frame(&ip, port, v, false).await;
    }
}

/// Push our profile to all known peers (fire-and-forget, parallel).
pub async fn announce_all(app: AppHandle) {
    let state: tauri::State<'_, AppState> = app.state();
    let x = state.peers.lock().await.keys().cloned().collect::<Vec<String>>();
    drop(state);
    for ip in x {
        let a = app.clone();
        let ip2 = ip.clone();
        tokio::spawn(async move { announce_to(&a, &ip2).await; });
    }
}

#[cfg(test)]
mod tests {
    use super::is_data_image;

    #[test]
    fn only_plain_image_data_urls_pass() {
        assert!(is_data_image("data:image/png;base64,iVBORw0KGgo="));
        assert!(is_data_image("data:image/jpeg;base64,/9j/4AAQ+/="));
        assert!(!is_data_image(""));
        assert!(!is_data_image("x\" onerror=\"alert(1)"));
        assert!(!is_data_image("data:image/png;base64,abc\" onerror=\"x"));
        assert!(!is_data_image("data:image/svg+xml;base64,PHN2Zz4="));
        assert!(!is_data_image("https://example.com/a.png"));
    }
}

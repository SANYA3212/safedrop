#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod discovery;
mod peers_cmd;
mod quic;
mod settings;
mod signal;
mod state;
mod tls;
mod transfer;
mod updsig;
mod ytbrowse;

use state::{AppState, CallRecord, ChatMsg, Group, Peer, PendingSend, Profile, Settings, TransferInfo};
use std::sync::Arc;
use tauri::{Emitter, Manager, WindowEvent};
use tauri::webview::{PermissionKind, PermissionResponse};

#[tauri::command]
async fn get_settings(state: tauri::State<'_, AppState>) -> Result<Settings, String> {
    Ok(state.settings.lock().await.clone())
}

#[tauri::command]
async fn save_settings(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    settings: Settings,
) -> Result<(), String> {
    if settings.loaddrop_dir.trim().is_empty() {
        return Err("LoadDrop path is empty".into());
    }
    let settings = {
        let mut g = state.settings.lock().await;
        // profile and LoadDrop are changed by their own commands; the page keeps
        // the copy it loaded at start, so never let it write them back
        let mut s = settings;
        s.profile = g.profile.clone();
        s.loaddrop_dir = g.loaddrop_dir.clone();
        *g = s.clone();
        s
    };
    settings::persist_settings(&state);
    let _ = settings::apply_autostart(settings.autostart);
    let _ = app.emit("safedrop-settings", settings);
    Ok(())
}

// ---------- profile ----------

#[tauri::command]
async fn get_profile(state: tauri::State<'_, AppState>) -> Result<Profile, String> {
    Ok(state.settings.lock().await.profile.clone())
}

#[tauri::command]
async fn save_profile(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    nickname: String,
    avatar: String,
) -> Result<Profile, String> {
    let nickname = nickname.trim().chars().take(32).collect::<String>();
    if avatar.len() > 300_000 {
        return Err("Avatar too big (max ~300KB dataURL, choose smaller image)".into());
    }
    if !avatar.is_empty()
        && !(avatar.starts_with("data:image/png;base64,")
            || avatar.starts_with("data:image/jpeg;base64,")
            || avatar.starts_with("data:image/webp;base64,")
            || avatar.starts_with("data:image/gif;base64,"))
    {
        return Err("Avatar must be dataURL image".into());
    }
    let p = Profile { nickname, avatar };
    {
        state.settings.lock().await.profile = p.clone();
    }
    settings::persist_settings(&state);
    let _ = app.emit("safedrop-profile", p.clone());
    // push to everyone
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        signal::announce_all(app2).await;
    });
    Ok(p)
}

/// Local override avatar for a peer (you assign to anyone).
#[tauri::command]
async fn set_peer_avatar(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    ip: String,
    avatar: String,
) -> Result<(), String> {
    if avatar.len() > 300_000 {
        return Err("Avatar too big".into());
    }
    {
        let mut g = state.peers.lock().await;
        if let Some(p) = g.get_mut(&ip) {
            p.custom_avatar = avatar;
        } else {
            return Err("peer not found".into());
        }
    }
    settings::persist_peers(&state);
    let peers: Vec<Peer> = state.peers.lock().await.values().cloned().collect();
    let _ = app.emit("safedrop-peers", peers);
    Ok(())
}

#[tauri::command]
async fn get_my_ips() -> Result<Vec<String>, String> {
    Ok(discovery::my_ips())
}

#[tauri::command]
async fn get_my_adapters() -> Result<Vec<discovery::AdapterInfo>, String> {
    Ok(discovery::my_adapters())
}

#[tauri::command]
async fn get_peers(state: tauri::State<'_, AppState>) -> Result<Vec<Peer>, String> {
    let g = state.peers.lock().await;
    let mut v: Vec<Peer> = g.values().cloned().collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(v)
}

#[tauri::command]
async fn add_peer(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    ip: String,
    name: String,
) -> Result<Peer, String> {
    let ip = ip.trim().to_string();
    if ip.is_empty() { return Err("empty ip".into()); }
    let mut g = state.peers.lock().await;
    if let Some(existing) = g.get_mut(&ip) {
        if !name.trim().is_empty() { existing.name = name.trim().to_string(); }
        let p = existing.clone();
        drop(g);
        settings::persist_peers(&state);
        return Ok(p);
    }
    let p = Peer {
        ip: ip.clone(),
        name: if name.trim().is_empty() { ip.clone() } else { name.trim().to_string() },
        nickname: String::new(), avatar: String::new(), custom_avatar: String::new(),
        trusted: true, online: false, ping_ms: None, last_seen: None,
        supports_file_confirm: false, supports_chat_ack: false, supports_watch: false,
        phash: String::new(), state: "idle".into(), detail: String::new(),
        ips: vec![ip.clone()],
        version: String::new(),
    };
    g.insert(ip.clone(), p.clone());
    drop(g);
    settings::persist_peers(&state);
    // сразу запросить профиль у нового пира (ник/ава подтянутся сами)
    let app2 = app.clone();
    let ip2 = ip.clone();
    tauri::async_runtime::spawn(async move {
        signal::announce_to(&app2, &ip2).await;
    });
    Ok(p)
}

#[tauri::command]
async fn rename_peer(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    ip: String,
    name: String,
) -> Result<(), String> {
    let mut g = state.peers.lock().await;
    if let Some(p) = g.get_mut(&ip) {
        p.name = name;
        drop(g);
        settings::persist_peers(&state);
        let peers: Vec<Peer> = state.peers.lock().await.values().cloned().collect();
        let _ = app.emit("safedrop-peers", peers);
        Ok(())
    } else { Err("peer not found".into()) }
}

#[tauri::command]
async fn remove_peer(state: tauri::State<'_, AppState>, ip: String) -> Result<(), String> {
    state.peers.lock().await.remove(&ip);
    settings::persist_peers(&state);
    Ok(())
}

#[tauri::command]
async fn toggle_trust(state: tauri::State<'_, AppState>, ip: String) -> Result<bool, String> {
    let mut g = state.peers.lock().await;
    if let Some(p) = g.get_mut(&ip) {
        p.trusted = !p.trusted;
        let v = p.trusted;
        drop(g);
        settings::persist_peers(&state);
        Ok(v)
    } else { Err("peer not found".into()) }
}

#[tauri::command]
async fn ping_peer(ip: String, port: u16) -> Result<Option<u64>, String> {
    Ok(discovery::tcp_ping(&ip, port, 1200).await)
}

fn adapter_key(a: &discovery::AdapterInfo) -> String {
    format!("{}|{}", a.name, a.ip)
}

async fn refresh_live(app: &tauri::AppHandle, state: &tauri::State<'_, AppState>) -> Vec<Peer> {
    // adapters for the scan: user filter (settings.scan_adapters, empty = all),
    // signature stored so loop-level code can notice network changes too
    let ads_all = discovery::my_adapters();
    let sig = discovery::adapter_signature(&ads_all);
    let changed = if let Ok(mut last) = state.last_adapter_sig.lock() {
        let diff = *last != sig && !last.is_empty();
        *last = sig;
        diff
    } else {
        false
    };
    if changed {
        // network changed (VPN connected / Wi-Fi switched): re-run scan immediately
        let _ = app.emit("safedrop-scan-progress", serde_json::json!({ "done": 0, "total": 1 }));
    }
    // one scan at a time: the adapter watcher and the periodic loop must not
    // run two sweeps in parallel
    if state.scanning.compare_exchange(
        false, true,
        std::sync::atomic::Ordering::SeqCst,
        std::sync::atomic::Ordering::SeqCst,
    ).is_err() {
        return state.peers.lock().await.values().cloned().collect();
    }
    let result = refresh_live_inner(app, state).await;
    state.scanning.store(false, std::sync::atomic::Ordering::SeqCst);
    result
}

async fn refresh_live_inner(app: &tauri::AppHandle, state: &tauri::State<'_, AppState>) -> Vec<Peer> {
    // adapters for the scan: user filter (settings.scan_adapters, empty = all),
    // signature stored so loop-level code can notice network changes too
    let ads_all = discovery::my_adapters();
    let sig = discovery::adapter_signature(&ads_all);
    if let Ok(mut last) = state.last_adapter_sig.lock() {
        *last = sig;
    }
    let ads = {
        let s = state.settings.lock().await;
        let mut list = ads_all;
        if !s.scan_adapters.is_empty() {
            let allowed: std::collections::HashSet<String> =
                s.scan_adapters.iter().cloned().collect();
            list.retain(|a| allowed.contains(&adapter_key(a)));
        }
        list
    };
    let (port, manual, known): (u16, Vec<String>, Vec<String>) = {
        let s = state.settings.lock().await;
        let mut known = Vec::new();
        for p in state.peers.lock().await.values() {
            known.push(p.ip.clone());
            known.extend(p.ips.iter().cloned());
        }
        (s.tcp_port, s.scan_manual.clone(), known)
    };
    let targets = discovery::scan_targets(&ads, &known, &manual);
    let live = discovery::scan_subnet_with_progress(port, targets, Some(app.clone())).await;
    // свои адреса: сервер слушает 0.0.0.0, поэтому скан находит нас самих
    // на каждом адаптере (пинг 0-2мс) — себя в контакты не добавляем
    let mine: std::collections::HashSet<String> =
        discovery::my_ips().into_iter().collect();
    let live: Vec<(String, u64)> = live
        .into_iter()
        .filter(|(ip, _)| !mine.contains(ip))
        .collect();
    {
        let mut g = state.peers.lock().await;
        // зачищаем ранее пролезшие свои адреса
        g.retain(|ip, _| !mine.contains(ip));
        let now = chrono::Local::now().format("%H:%M:%S").to_string();
        for p in g.values_mut() { p.online = false; p.ping_ms = None; }
        for (ip, ping) in &live {
            // dedup: контакт ищется по первичному IP И по любому из его адресов
            let key = if g.contains_key(ip) {
                ip.clone()
            } else {
                match g.values().find(|p| p.ips.iter().any(|x| x == ip)) {
                    Some(p) => p.ip.clone(),
                    None => String::new(),
                }
            };
            if !key.is_empty() {
                if let Some(p) = g.get_mut(&key) {
                    p.online = true;
                    p.ping_ms = Some(*ping);
                    p.last_seen = Some(now.clone());
                    if !p.ips.contains(ip) { p.ips.push(ip.clone()); }
                }
            } else {
                g.insert(ip.clone(), Peer {
                    ip: ip.clone(), name: ip.clone(), nickname: String::new(),
                    avatar: String::new(), custom_avatar: String::new(),
                    trusted: false, online: true,
                    supports_file_confirm: false, supports_chat_ack: false, supports_watch: false,
                    ping_ms: Some(*ping), last_seen: Some(now.clone()),
                    phash: String::new(), state: "idle".into(), detail: String::new(),
                    ips: vec![ip.clone()],
                    version: String::new(),
                });
            }
        }
    }
    settings::persist_peers(&state);
    let peers: Vec<Peer> = state.peers.lock().await.values().cloned().collect();
    let _ = app.emit("safedrop-peers", peers.clone());
    // pull profiles from live hosts
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        for (ip, _) in live {
            let a = app2.clone();
            tokio::spawn(async move { signal::announce_to(&a, &ip).await; });
        }
    });
    peers
}

#[tauri::command]
async fn scan_network(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<Peer>, String> {
    Ok(refresh_live(&app, &state).await)
}

// ---------- chat ----------

#[tauri::command]
async fn get_chat(state: tauri::State<'_, AppState>, peer_ip: String) -> Result<Vec<ChatMsg>, String> {
    let g = state.chats.lock().await;
    Ok(g.get(&peer_ip).cloned().unwrap_or_default())
}

fn audio_ext_ok(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "mp3" | "wav" | "ogg" | "m4a" | "flac"))
        .unwrap_or(false)
}

fn audio_file_name(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

#[tauri::command]
async fn send_chat(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    peer_ip: String,
    text: String,
    image: Option<String>,
    images: Option<Vec<String>>,
    audio: Option<String>,
    reply_to: Option<String>,
) -> Result<ChatMsg, String> {
    let text = text.trim().chars().take(2000).collect::<String>();
    let mut images: Vec<String> = images.unwrap_or_default().into_iter()
        .filter(|s| !s.is_empty() && s.len() <= 700_000)
        .take(10)
        .collect();
    let image = image.unwrap_or_default();
    if images.is_empty() && !image.is_empty() && image.len() <= 700_000 {
        images.push(image);
    }
    let image = images.first().cloned().unwrap_or_default();
    let audio = audio.unwrap_or_default();
    if !audio.is_empty() && !audio_ext_ok(&audio) {
        return Err("unsupported audio".into());
    }
    if text.is_empty() && images.is_empty() && audio.is_empty() {
        return Err("empty".into());
    }
    let (sig_port, nick) = {
        let s = state.settings.lock().await;
        (s.sig_port, if s.profile.nickname.is_empty() { s.device_name.clone() } else { s.profile.nickname.clone() })
    };
    let sent_at = chrono::Utc::now().timestamp_millis();
    // Kept only for older clients; 0.3.13+ renders sent_at with the local clock.
    let ts = chrono::Local::now().format("%H:%M").to_string();
    let reply_to = reply_to.unwrap_or_default();
    let msg = ChatMsg {
        id: uuid::Uuid::new_v4().to_string(),
        peer: peer_ip.clone(),
        from_me: true,
        from_nick: nick.clone(),
        text: text.clone(),
        ts: ts.clone(),
        sent_at,
        image: image.clone(),
        images: images.clone(),
        audio: audio.clone(),
        status: "sent".into(),
        delivered_to: Vec::new(),
        reply_to,
        edited_at: 0,
        reactions: Vec::new(),
    };
    {
        let mut chats = state.chats.lock().await;
        let rows = chats.entry(peer_ip.clone()).or_default();
        rows.push(msg.clone());
        rows.sort_by(|a, b| a.sent_at.cmp(&b.sent_at).then_with(|| a.id.cmp(&b.id)));
    }
    settings::save_chats_soon(&state);
    let _ = app.emit("safedrop-chat", msg.clone());
    // Deliver in the background: an offline peer must not block the composer.
    // The message stays in chats.json with status "sent" and is flushed when
    // the peer comes online; the receiver acknowledges it with "chat-ack".
    let v = serde_json::json!({"kind":"chat","id":msg.id,"sent_at":sent_at,"from":nick,"nickname":nick,"text":text,"ts":ts,"image":image,"images":images,"audio":audio_file_name(&audio),"reply_to":msg.reply_to});
    let app2 = app.clone();
    let peer = peer_ip.clone();
    tokio::spawn(async move {
        let _ = quic::send_frame_dual(&app2, &peer, sig_port, v, false).await;
    });
    Ok(msg)
}

// ---------- chat delete / clear ----------

fn persist_chats(state: &tauri::State<'_, AppState>) {
    settings::save_chats_soon(state);
}

#[tauri::command]
async fn delete_msgs(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    peer_ip: String,
    ids: Vec<String>,
    for_peer: bool,
) -> Result<(), String> {
    {
        let mut c = state.chats.lock().await;
        if let Some(v) = c.get_mut(&peer_ip) {
            v.retain(|m| !ids.contains(&m.id));
        }
    }
    persist_chats(&state);
    let _ = app.emit("safedrop-chat-changed", serde_json::json!({"peer": peer_ip}));
    if for_peer {
        let (port, nick) = {
            let s = state.settings.lock().await;
            (s.sig_port, if s.profile.nickname.is_empty() { s.device_name.clone() } else { s.profile.nickname.clone() })
        };
        let v = serde_json::json!({"kind":"chat-delete","from":nick,"ids":ids,"all":false});
        quic::send_frame_dual(&app, &peer_ip, port, v, false).await?;
    }
    Ok(())
}

#[tauri::command]
async fn clear_chat(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    peer_ip: String,
    for_peer: bool,
) -> Result<(), String> {
    {
        let mut c = state.chats.lock().await;
        if let Some(v) = c.get_mut(&peer_ip) {
            v.clear();
        }
    }
    persist_chats(&state);
    let _ = app.emit("safedrop-chat-changed", serde_json::json!({"peer": peer_ip}));
    if for_peer {
        let (port, nick) = {
            let s = state.settings.lock().await;
            (s.sig_port, if s.profile.nickname.is_empty() { s.device_name.clone() } else { s.profile.nickname.clone() })
        };
        let v = serde_json::json!({"kind":"chat-delete","from":nick,"ids":[],"all":true});
        quic::send_frame_dual(&app, &peer_ip, port, v, false).await?;
    }
    Ok(())
}

/// Open a Windows Settings page (ms-settings:...).
fn open_ms_settings(page: &str) -> Result<(), String> {
    if cfg!(not(windows)) {
        return Err("Эта настройка есть только в Windows — открой параметры своей системы".into());
    }
    std::process::Command::new("cmd")
        .args(["/C", "start", "", page])
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Open Windows microphone privacy settings page.
#[tauri::command]
async fn open_mic_settings() -> Result<(), String> {
    open_ms_settings("ms-settings:privacy-microphone")
}

// ---------- groups ----------

#[tauri::command]
async fn get_groups(state: tauri::State<'_, AppState>) -> Result<Vec<Group>, String> {
    Ok(state.groups.lock().await.clone())
}

#[tauri::command]
async fn create_group(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    name: String,
    members: Vec<String>,
) -> Result<Group, String> {
    let name = name.trim().chars().take(40).collect::<String>();
    if name.is_empty() {
        return Err("empty name".into());
    }
    let g = Group {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        members: members.into_iter().collect::<std::collections::HashSet<_>>().into_iter().collect(),
        avatar: String::new(),
        updated_at: chrono::Utc::now().timestamp_millis(),
    };
    {
        state.groups.lock().await.push(g.clone());
    }
    signal::persist_groups(&state).await;
    let gs: Vec<Group> = state.groups.lock().await.clone();
    let _ = app.emit("safedrop-groups", gs);
    signal::merge_group_sync(&app, state::GroupSyncState {
        deleted: Vec::new(), departures: Vec::new(), rejoins: Vec::new(),
        metas: vec![state::GroupMeta { id: g.id.clone(), name: g.name.clone(), avatar: g.avatar.clone(), updated_at: g.updated_at }],
    }, None).await;
    Ok(g)
}

#[tauri::command]
async fn rename_group(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    id: String,
    name: String,
) -> Result<(), String> {
    let meta = {
        let mut gs = state.groups.lock().await;
        if let Some(g) = gs.iter_mut().find(|x| x.id == id) {
            g.name = name.trim().chars().take(40).collect();
            g.updated_at = chrono::Utc::now().timestamp_millis();
            Some(state::GroupMeta { id: g.id.clone(), name: g.name.clone(), avatar: g.avatar.clone(), updated_at: g.updated_at })
        } else {
            None
        }
    };
    let meta = meta.ok_or("group not found")?;
    signal::persist_groups(&state).await;
    let gs: Vec<Group> = state.groups.lock().await.clone();
    let _ = app.emit("safedrop-groups", gs);
    signal::merge_group_sync(&app, state::GroupSyncState {
        deleted: Vec::new(), departures: Vec::new(), rejoins: Vec::new(), metas: vec![meta],
    }, None).await;
    Ok(())
}

#[tauri::command]
async fn set_group_avatar(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    id: String,
    avatar: String,
) -> Result<(), String> {
    let avatar = if avatar.len() > 300_000 { String::new() } else { avatar };
    let meta = {
        let mut gs = state.groups.lock().await;
        if let Some(g) = gs.iter_mut().find(|x| x.id == id) {
            g.avatar = avatar;
            g.updated_at = chrono::Utc::now().timestamp_millis();
            Some(state::GroupMeta { id: g.id.clone(), name: g.name.clone(), avatar: g.avatar.clone(), updated_at: g.updated_at })
        } else {
            None
        }
    };
    let meta = meta.ok_or("group not found")?;
    signal::persist_groups(&state).await;
    let gs: Vec<Group> = state.groups.lock().await.clone();
    let _ = app.emit("safedrop-groups", gs);
    signal::merge_group_sync(&app, state::GroupSyncState {
        deleted: Vec::new(), departures: Vec::new(), rejoins: Vec::new(), metas: vec![meta],
    }, None).await;
    Ok(())
}

#[tauri::command]
async fn delete_group(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let group = state.groups.lock().await.iter().find(|g| g.id == id).cloned().ok_or("group not found")?;
    signal::merge_group_sync(&app, state::GroupSyncState {
        deleted: vec![state::GroupDeletion { id, deleted_at: chrono::Utc::now().timestamp_millis(), members: group.members }],
        departures: Vec::new(),
        rejoins: Vec::new(),
        metas: Vec::new(),
    }, None).await;
    Ok(())
}

#[tauri::command]
async fn leave_group(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let group = state.groups.lock().await.iter().find(|g| g.id == id).cloned().ok_or("group not found")?;
    // Peers know us by whichever address they reached us on, so leave with all
    // of our addresses that are in the group (or all of them if none is).
    let mine = discovery::my_ips();
    let mut leave_ips: Vec<String> = mine.iter().filter(|ip| group.members.contains(ip)).cloned().collect();
    if leave_ips.is_empty() { leave_ips = mine; }
    if leave_ips.is_empty() { return Err("локальный IP не найден".into()); }
    let left_at = chrono::Utc::now().timestamp_millis();
    signal::merge_group_sync(&app, state::GroupSyncState {
        deleted: Vec::new(),
        departures: leave_ips.into_iter().map(|member_ip| state::GroupDeparture {
            group_id: id.clone(),
            member_ip,
            left_at,
            members: group.members.clone(),
        }).collect(),
        rejoins: Vec::new(),
        metas: Vec::new(),
    }, None).await;
    Ok(())
}

#[tauri::command]
async fn group_add_member(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    id: String,
    ip: String,
) -> Result<(), String> {
    let group = {
        let mut gs = state.groups.lock().await;
        if let Some(g) = gs.iter_mut().find(|x| x.id == id) {
            if !g.members.contains(&ip) {
                g.members.push(ip.clone());
            }
            g.clone()
        } else {
            return Err("group not found".into());
        }
    };
    signal::merge_group_sync(&app, state::GroupSyncState {
        deleted: Vec::new(),
        departures: Vec::new(),
        rejoins: vec![state::GroupRejoin {
            group_id: id,
            member_ip: ip,
            joined_at: chrono::Utc::now().timestamp_millis(),
            name: group.name,
            members: group.members,
        }],
        metas: Vec::new(),
    }, None).await;
    Ok(())
}

#[tauri::command]
async fn group_remove_member(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    id: String,
    ip: String,
) -> Result<(), String> {
    let group = state.groups.lock().await.iter().find(|x| x.id == id).cloned().ok_or("group not found")?;
    // A plain local edit is undone by the next message that carries the old
    // member list; a departure is remembered and sent to every member.
    signal::merge_group_sync(&app, state::GroupSyncState {
        deleted: Vec::new(),
        departures: vec![state::GroupDeparture {
            group_id: id,
            member_ip: ip,
            left_at: chrono::Utc::now().timestamp_millis(),
            members: group.members,
        }],
        rejoins: Vec::new(),
        metas: Vec::new(),
    }, None).await;
    Ok(())
}

#[tauri::command]
async fn send_group_chat(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    group_id: String,
    text: String,
    image: Option<String>,
    images: Option<Vec<String>>,
    audio: Option<String>,
    reply_to: Option<String>,
) -> Result<ChatMsg, String> {
    let text = text.trim().chars().take(2000).collect::<String>();
    let mut images: Vec<String> = images.unwrap_or_default().into_iter()
        .filter(|s| !s.is_empty() && s.len() <= 700_000)
        .take(10)
        .collect();
    let image = image.unwrap_or_default();
    if images.is_empty() && !image.is_empty() && image.len() <= 700_000 {
        images.push(image);
    }
    let image = images.first().cloned().unwrap_or_default();
    let audio = audio.unwrap_or_default();
    if !audio.is_empty() && !audio_ext_ok(&audio) {
        return Err("unsupported audio".into());
    }
    if text.is_empty() && images.is_empty() && audio.is_empty() {
        return Err("empty".into());
    }
    let (sig_port, nick, group) = {
        let s = state.settings.lock().await;
        let g = state.groups.lock().await;
        let group = g.iter().find(|x| x.id == group_id).cloned();
        (
            s.sig_port,
            if s.profile.nickname.is_empty() { s.device_name.clone() } else { s.profile.nickname.clone() },
            group,
        )
    };
    let group = group.ok_or("group not found")?;
    let sent_at = chrono::Utc::now().timestamp_millis();
    // Kept only for older clients; 0.3.13+ renders sent_at with the local clock.
    let ts = chrono::Local::now().format("%H:%M").to_string();
    let reply_to = reply_to.unwrap_or_default();
    let key = format!("g:{}", group.id);
    let msg = ChatMsg {
        id: uuid::Uuid::new_v4().to_string(),
        peer: key.clone(),
        from_me: true,
        from_nick: nick.clone(),
        text: text.clone(),
        ts: ts.clone(),
        sent_at,
        image: image.clone(),
        images: images.clone(),
        audio: audio.clone(),
        status: "sent".into(),
        delivered_to: Vec::new(),
        reply_to,
        edited_at: 0,
        reactions: Vec::new(),
    };
    {
        let mut chats = state.chats.lock().await;
        let rows = chats.entry(key).or_default();
        rows.push(msg.clone());
        rows.sort_by(|a, b| a.sent_at.cmp(&b.sent_at).then_with(|| a.id.cmp(&b.id)));
    }
    settings::save_chats_soon(&state);
    let _ = app.emit("safedrop-chat", msg.clone());
    // Fan out in parallel; unreachable members keep the message queued ("sent")
    // until the next hello and are deduplicated by id on the receiving side.
    for m in &without_me(&group.members) {
        let v = serde_json::json!({
            "kind": "chat", "id": msg.id, "sent_at": sent_at, "from": nick, "nickname": nick, "text": text, "ts": ts,
            "image": image, "images": images, "audio": audio_file_name(&audio), "to_group": group.id, "group_name": group.name, "members": group.members, "reply_to": msg.reply_to,
        });
        let app2 = app.clone();
        let target = m.clone();
        tokio::spawn(async move {
            let _ = quic::send_frame_dual(&app2, &target, sig_port, v, false).await;
        });
    }
    Ok(msg)
}

// ---------- chat edit / reactions / typing ----------

/// Group member lists may contain our own addresses; don't send to ourselves.
fn without_me(members: &[String]) -> Vec<String> {
    let mine = discovery::my_ips();
    members.iter().filter(|m| !mine.contains(m)).cloned().collect()
}

fn my_nick(settings: &crate::state::Settings) -> String {
    if settings.profile.nickname.is_empty() {
        settings.device_name.clone()
    } else {
        settings.profile.nickname.clone()
    }
}

#[tauri::command]
async fn chat_edit(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    peer_ip: String,
    id: String,
    text: String,
) -> Result<(), String> {
    let text = text.trim().chars().take(2000).collect::<String>();
    let (nick, port) = {
        let s = state.settings.lock().await;
        (my_nick(&s), s.sig_port)
    };
    let edited_at = chrono::Utc::now().timestamp_millis();
    {
        let mut c = state.chats.lock().await;
        if let Some(rows) = c.get_mut(&peer_ip) {
            let m = rows.iter_mut().find(|m| m.id == id && m.from_me).ok_or("message not found")?;
            m.text = text.clone();
            m.edited_at = edited_at;
        }
    }
    settings::save_chats_soon(&state);
    let _ = app.emit("safedrop-chat-changed", serde_json::json!({"peer": peer_ip}));
    // fan-out to the direct peer or to every group member
    let members: Option<Vec<String>> = if let Some(gid) = peer_ip.strip_prefix("g:") {
        let g = state.groups.lock().await;
        g.iter().find(|x| x.id == gid).map(|x| without_me(&x.members))
    } else {
        None
    };
    let v = serde_json::json!({"kind":"chat-edit","from":nick,"id":id,"text":text,"edited_at":edited_at});
    match members {
        Some(ms) => {
            for target in ms {
                let app2 = app.clone();
                let v = v.clone();
                tokio::spawn(async move {
                    let _ = quic::send_frame_dual(&app2, &target, port, v, false).await;
                });
            }
        }
        None => {
            let app2 = app.clone();
            let target = peer_ip;
            tokio::spawn(async move {
                let _ = quic::send_frame_dual(&app2, &target, port, v, false).await;
            });
        }
    }
    Ok(())
}

#[tauri::command]
async fn chat_react(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    peer_ip: String,
    id: String,
    emoji: String,
    remove: bool,
) -> Result<(), String> {
    if emoji.chars().count() > 8 {
        return Err("emoji too long".into());
    }
    let (nick, port) = {
        let s = state.settings.lock().await;
        (my_nick(&s), s.sig_port)
    };
    {
        let mut c = state.chats.lock().await;
        if let Some(rows) = c.get_mut(&peer_ip) {
            if let Some(m) = rows.iter_mut().find(|m| m.id == id) {
                let entry = m.reactions.iter_mut().find(|r| r.emoji == emoji);
                match entry {
                    Some(r) => {
                        if remove {
                            r.by.retain(|b| b != &nick);
                            m.reactions.retain(|r| !r.by.is_empty());
                        } else if !r.by.iter().any(|b| b == &nick) {
                            r.by.push(nick.clone());
                        }
                    }
                    None if !remove => {
                        m.reactions.push(crate::state::ChatReaction { emoji: emoji.clone(), by: vec![nick.clone()] });
                    }
                    _ => {}
                }
            }
        }
    }
    settings::save_chats_soon(&state);
    let _ = app.emit("safedrop-chat-changed", serde_json::json!({"peer": peer_ip}));
    let members: Option<Vec<String>> = if let Some(gid) = peer_ip.strip_prefix("g:") {
        let g = state.groups.lock().await;
        g.iter().find(|x| x.id == gid).map(|x| without_me(&x.members))
    } else {
        None
    };
    let v = serde_json::json!({"kind":"chat-react","from":nick,"nickname":nick,"id":id,"emoji":emoji,"remove":remove});
    match members {
        Some(ms) => {
            for target in ms {
                let app2 = app.clone();
                let v = v.clone();
                tokio::spawn(async move {
                    let _ = quic::send_frame_dual(&app2, &target, port, v, false).await;
                });
            }
        }
        None => {
            let app2 = app.clone();
            let target = peer_ip;
            tokio::spawn(async move {
                let _ = quic::send_frame_dual(&app2, &target, port, v, false).await;
            });
        }
    }
    Ok(())
}

#[tauri::command]
async fn chat_typing(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    peer_ip: String,
) -> Result<(), String> {
    // not stored anywhere: momentary signal only; toggleable in settings
    let show = state.settings.lock().await.show_typing;
    if !show {
        return Ok(());
    }
    let (nick, port) = {
        let s = state.settings.lock().await;
        (my_nick(&s), s.sig_port)
    };
    let (members, to_group): (Option<Vec<String>>, Option<String>) = if let Some(gid) = peer_ip.strip_prefix("g:") {
        let g = state.groups.lock().await;
        let found = g.iter().find(|x| x.id == gid).map(|x| without_me(&x.members));
        (found, Some(gid.to_string()))
    } else {
        (None, None)
    };
    let v = serde_json::json!({"kind":"chat-typing","from":nick,"nickname":nick,"to_group":to_group});
    match members {
        Some(ms) => {
            for target in ms {
                let app2 = app.clone();
                let v = v.clone();
                tokio::spawn(async move {
                    let _ = quic::send_frame_dual(&app2, &target, port, v, false).await;
                });
            }
        }
        None => {
            let app2 = app.clone();
            let target = peer_ip;
            tokio::spawn(async move {
                let _ = quic::send_frame_dual(&app2, &target, port, v, false).await;
            });
        }
    }
    Ok(())
}

// ---------- call signaling relay ----------

#[tauri::command]
async fn sig_send(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    peer_ip: String,
    payload: serde_json::Value,
) -> Result<(), String> {
    let port = state.settings.lock().await.sig_port;
    quic::send_frame_dual(&app, &peer_ip, port, payload, false).await?;
    Ok(())
}

#[tauri::command]
async fn send_file(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    peer_ip: String,
    file_path: String,
) -> Result<String, String> {
    let (port, device) = {
        let s = state.settings.lock().await;
        (s.tcp_port, s.device_name.clone())
    };
    let confirm = state.peers.lock().await.get(&peer_ip).map(|p| p.supports_file_confirm).unwrap_or(false);
    quic::send_file_dual(&app, peer_ip, port, file_path, device, "file".into(), confirm, String::new(), String::new()).await
}

/// Send an installer to a peer as an update offer.
#[tauri::command]
async fn send_update(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    peer_ip: String,
    file_path: String,
) -> Result<String, String> {
    let extension = std::path::Path::new(&file_path)
        .extension().and_then(|s| s.to_str()).unwrap_or("");
    if !extension.eq_ignore_ascii_case("exe") {
        return Err("Выбери установщик SafeDrop с расширением .exe".into());
    }
    // получатели (0.3.31+) ставят только подписанный установщик — без подписи не шлём
    let path = std::path::Path::new(&file_path);
    let sig = std::fs::read_to_string(updsig::sig_path(path))
        .map_err(|_| "У установщика нет подписи (.sig рядом с .exe) — собери его через build-release.bat".to_string())?;
    let ver = updsig::verify_file(path).map_err(|e| format!("Установщик не прошёл проверку подписи: {e}"))?;
    let (port, device) = {
        let s = state.settings.lock().await;
        (s.tcp_port, s.device_name.clone())
    };
    let id = quic::send_file_dual(&app, peer_ip, port, file_path, device, "update".into(), false, ver.clone(), sig).await?;
    Ok(format!("{id}:{ver}"))
}

/// Apply a downloaded update: run the installer exe directly.
#[tauri::command]
async fn apply_update(state: tauri::State<'_, AppState>, path: String, version: String) -> Result<(), String> {
    if !std::path::Path::new(&path).exists() {
        return Err("файл обновления не найден (уже удалён?)".into());
    }
    // перед запуском — ещё раз подпись: файл на диске могли подменить после приёма.
    // Версия — из подписи, а не из того, что прислал отправитель.
    let _ = version;
    let version = updsig::verify_file(std::path::Path::new(&path)).map_err(|e| format!("обновление не установлено: {e}"))?;
    if transfer::cmp_ver(&version, &app_version()) == std::cmp::Ordering::Less {
        return Err(format!("это старая версия ({version}) — откат не ставим"));
    }
    if cfg!(not(windows)) {
        return Err("Обновление через SafeDrop работает только в Windows — поставь новую версию пакетом".into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().map(|p| p.to_path_buf()).ok_or("no exe dir")?;
    let script_path = std::env::temp_dir().join("safedrop-update.ps1");
    let quote_ps = |value: &str| format!("'{}'", value.replace('\'', "''"));
    let script = format!(r#"
$ErrorActionPreference = 'Stop'
$installer = {installer}
$installDir = {install_dir}
$appExe = Join-Path $installDir 'safedrop.exe'
$parentPid = {parent_pid}
$version = {version}
$log = Join-Path $env:TEMP 'safedrop-update.log'
function Write-Log([string]$message) {{ Add-Content -LiteralPath $log -Value "$(Get-Date -Format s) $message" }}
try {{
  Write-Log 'Waiting for SafeDrop to exit'
  $deadline = (Get-Date).AddSeconds(30)
  while (Get-Process -Id $parentPid -ErrorAction SilentlyContinue) {{
    if ((Get-Date) -gt $deadline) {{ throw 'SafeDrop process did not exit in 30 seconds' }}
    Start-Sleep -Milliseconds 200
  }}
  # Close any second SafeDrop instance that could still lock installed files.
  Get-Process -Name 'safedrop' -ErrorAction SilentlyContinue | ForEach-Object {{ Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }}
  $deadline = (Get-Date).AddSeconds(30)
  while (Get-Process -Name 'safedrop' -ErrorAction SilentlyContinue) {{
    if ((Get-Date) -gt $deadline) {{ throw 'SafeDrop process is still running' }}
    Start-Sleep -Milliseconds 200
  }}
  Write-Log 'Starting installer'
  $process = Start-Process -FilePath $installer -ArgumentList @('/SILENT', "/DIR=`"$installDir`"") -Wait -PassThru
  if ($process.ExitCode -ne 0) {{ throw "Installer exited with code $($process.ExitCode)" }}
  if (-not (Test-Path -LiteralPath $appExe)) {{ throw 'Installed executable was not found' }}
  Set-Content -LiteralPath (Join-Path $installDir 'version.txt') -Value $version -Encoding ascii
  Write-Log "Installed version $version"
  Start-Process -FilePath $appExe
  Write-Log 'SafeDrop restarted'
}} catch {{
  Write-Log "UPDATE ERROR: $($_.Exception.Message)"
}} finally {{
  Remove-Item -LiteralPath $PSCommandPath -Force -ErrorAction SilentlyContinue
}}
"#,
        installer = quote_ps(&path),
        install_dir = quote_ps(&dir.to_string_lossy()),
        parent_pid = std::process::id(),
        version = quote_ps(&version),
    );
    std::fs::write(&script_path, script).map_err(|e| e.to_string())?;
    std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"])
        .arg(&script_path)
        .spawn()
        .map_err(|e| e.to_string())?;
    settings::save_chats_now(&state); // история пишется с задержкой
    std::thread::sleep(std::time::Duration::from_millis(250));
    std::process::exit(0);
}

/// Linux: notifications are named by the desktop file, nothing to set up.
#[cfg(not(windows))]
fn ensure_notif_shortcut() {}

/// Ensure Start Menu shortcut with our AppUserModelID so Windows toasts
/// show as SafeDrop (not PowerShell). Idempotent, per-user, no admin.
#[cfg(windows)]
fn ensure_notif_shortcut() {
    // NOTE: plain thread (NOT tokio::spawn_blocking): setup() runs before
    // the Tokio runtime context exists, which would panic.
    let _ = std::thread::spawn(|| -> Result<(), String> {
        use windows::{
            core::{Interface, GUID, HSTRING},
            Win32::{
                Foundation::PROPERTYKEY,
                System::{
                    Com::{
                        CoCreateInstance, CoInitializeEx, CoTaskMemAlloc,
                        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, IPersistFile,
                        StructuredStorage::{PROPVARIANT, PropVariantClear},
                    },
                    Variant::VT_LPWSTR,
                },
                UI::Shell::{
                    IShellLinkW,
                    PropertiesSystem::IPropertyStore,
                },
            },
        };
        // CLSID_ShellLink = {00021401-0000-0000-C000-000000000046}
        const CLSID_SHELLLINK: GUID = GUID::from_u128(0x0002140100000000C000000000000046);
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let dir = exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            let lnk = dirs::config_dir()
                .map(|p| {
                    p.join("Microsoft")
                        .join("Windows")
                        .join("Start Menu")
                        .join("Programs")
                        .join("SafeDrop.lnk")
                })
                .ok_or("no config dir")?;
            if let Some(parent) = lnk.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let link: IShellLinkW =
                CoCreateInstance(&CLSID_SHELLLINK, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| format!("{e:?}"))?;
            link.SetPath(&HSTRING::from(exe.to_string_lossy().as_ref()))
                .map_err(|e| format!("{e:?}"))?;
            link.SetWorkingDirectory(&HSTRING::from(dir.to_string_lossy().as_ref()))
                .map_err(|e| format!("{e:?}"))?;
            link.SetDescription(&HSTRING::from("SafeDrop — P2P обмен файлами"))
                .map_err(|e| format!("{e:?}"))?;
            link.SetIconLocation(&HSTRING::from(exe.to_string_lossy().as_ref()), 0)
                .map_err(|e| format!("{e:?}"))?;
            // PKEY_AppUserModel_ID = {9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3},5
            let key = PROPERTYKEY {
                fmtid: GUID::from_u128(0x9F4C28559F794B39A8D0E1D42DE1D5F3),
                pid: 5,
            };
            let idw: Vec<u16> = "com.safedrop.app".encode_utf16().chain(Some(0)).collect();
            let mem = CoTaskMemAlloc(idw.len() * 2) as *mut u16;
            if mem.is_null() {
                return Err("oom".into());
            }
            std::ptr::copy_nonoverlapping(idw.as_ptr(), mem, idw.len());
            // PROPVARIANT assembled bottom-up by value (unions can't be
            // field-assigned through ManuallyDrop auto-deref).
            use windows::Win32::System::Com::StructuredStorage::{
                PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
            };
            let inner = PROPVARIANT_0_0 {
                vt: VT_LPWSTR,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    pwszVal: windows::core::PWSTR(mem),
                },
            };
            let mut pv = PROPVARIANT {
                Anonymous: PROPVARIANT_0 {
                    Anonymous: std::mem::ManuallyDrop::new(inner),
                },
            };
            let store: IPropertyStore = link.cast().map_err(|e| format!("{e:?}"))?;
            let r = store.SetValue(&key, &pv).map_err(|e| format!("{e:?}"));
            let _ = PropVariantClear(&mut pv);
            r?;
            store.Commit().map_err(|e| format!("{e:?}"))?;
            let persist: IPersistFile = link.cast().map_err(|e| format!("{e:?}"))?;
            persist
                .Save(&HSTRING::from(lnk.to_string_lossy().as_ref()), true)
                .map_err(|e| format!("{e:?}"))?;
            Ok(())
        }
    });
}

/// Open an http(s) link in the default browser (validated).
#[tauri::command]
async fn open_url(url: String) -> Result<(), String> {
    let ok = (url.starts_with("https://") || url.starts_with("http://"))
        && url.len() < 2000
        && !url.contains([' ', '\n', '\r', '"', '\'', '<', '>']);
    if !ok {
        return Err("bad url".into());
    }
    open::that(url).map_err(|e| e.to_string())
}

/// Open Windows notification settings page.
#[tauri::command]
async fn open_notif_settings() -> Result<(), String> {
    open_ms_settings("ms-settings:notifications")
}

// ---------- native dialogs (real paths for Tauri WebView) ----------

#[tauri::command]
async fn pick_files() -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("Выбери файлы для отправки")
            .pick_files()
            .map(|v| v.into_iter().map(|p| p.to_string_lossy().to_string()).collect())
            .unwrap_or_default()
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pick_image() -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("Выбери картинку")
            .add_filter("image", &["png", "jpg", "jpeg", "webp", "gif", "bmp"])
            .pick_file()
            .map(|p| p.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pick_folder() -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("Выбери папку")
            .pick_folder()
            .map(|p| p.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pick_audio() -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("Выбери мелодию звонка")
            .add_filter("audio", &["mp3", "wav", "ogg", "m4a", "flac"])
            .pick_file()
            .map(|p| p.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| e.to_string())
}

/// Read image file (<=8MB) and return dataURL. Used for avatars/backgrounds.
#[tauri::command]
async fn read_image_dataurl(path: String) -> Result<String, String> {
    let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    if meta.len() > 8 * 1024 * 1024 {
        return Err("image too big (max 8MB)".into());
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let ext = std::path::Path::new(&path)
        .extension().and_then(|s| s.to_str()).unwrap_or("")
        .to_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        _ => "image/jpeg",
    };
    // base64 without extra deps
    const ALPH: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 4);
    for ch in bytes.chunks(3) {
        let b0 = ch[0] as u32;
        let b1 = *ch.get(1).unwrap_or(&0) as u32;
        let b2 = *ch.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPH[((n >> 18) & 63) as usize] as char);
        out.push(ALPH[((n >> 12) & 63) as usize] as char);
        out.push(if ch.len() > 1 { ALPH[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if ch.len() > 2 { ALPH[(n & 63) as usize] as char } else { '=' });
    }
    Ok(format!("data:{mime};base64,{out}"))
}

/// Ensure LoadDrop exists, return its path (used at startup + before receives).
#[tauri::command]
async fn get_loaddrop(state: tauri::State<'_, AppState>) -> Result<String, String> {
    let dir = state.settings.lock().await.loaddrop_dir.clone();
    if dir.trim().is_empty() {
        return Err("LoadDrop path is empty".into());
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("can't create LoadDrop: {e}"))?;
    Ok(dir)
}

/// DEBUG: what the scanner sees (no rescan — current state only).
#[tauri::command]
async fn scan_debug(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
    let my = discovery::my_ips();
    let radmin: Vec<String> = my.iter().filter(|ip| ip.starts_with("26.")).cloned().collect();
    let nets = discovery::scan_nets();
    let (tcp, sig, udp_on, udp_port) = {
        let s = state.settings.lock().await;
        (s.tcp_port, s.sig_port, s.udp_enabled, s.udp_port)
    };
    let online: Vec<serde_json::Value> = state
        .peers.lock().await.values()
        .filter(|p| p.online)
        .map(|p| serde_json::json!({"ip": p.ip, "ping": p.ping_ms}))
        .collect();
    let total = state.peers.lock().await.len();
    Ok(serde_json::json!({
        "my_ips": my, "radmin_ips": radmin, "scan_nets": nets,
        "tcp_port": tcp, "sig_port": sig,
        "udp_enabled": udp_on, "udp_port": udp_port,
        "contacts_total": total, "online": online,
    }))
}

/// DEBUG: wipe all contacts (peers.json cleared too).
#[tauri::command]
async fn clear_peers(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    state.peers.lock().await.clear();
    settings::persist_peers(&state);
    let _ = app.emit("safedrop-peers", Vec::<Peer>::new());
    Ok(())
}

/// FULL RESET: wipe settings/peers/history/chats, remove autostart +
/// explorer menu, back to defaults. Frontend reloads afterwards.
#[tauri::command]
async fn factory_reset(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    for f in [
        state.settings_path(),
        state.peers_path(),
        state.history_path(),
        state.chats_path(),
        state.groups_path(),
        state.group_sync_path(),
        state.call_history_path(),
    ] {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir_all(state.app_dir.join("avatars"));
    if cfg!(windows) {
        let _ = reg(&["delete", ctx_base(), "/f"]);
        let _ = reg(&["delete", &clsid_key(), "/f"]);
    }
    let _ = settings::apply_autostart(false);
    // ключи контактов забываем вместе с контактами; свой ключ остаётся,
    // чтобы друзьям не пришлось заново подтверждать этот ПК
    tls::clear_pins();
    let fresh = Settings::default();
    let _ = std::fs::create_dir_all(&fresh.loaddrop_dir);
    *state.settings.lock().await = fresh;
    state.peers.lock().await.clear();
    state.transfers.lock().await.clear();
    state.call_history.lock().await.clear();
    state.chats.lock().await.clear();
    state.groups.lock().await.clear();
    *state.group_sync.lock().await = state::GroupSyncState::default();
    settings::persist_settings(&state);
    let _ = app.emit("safedrop-peers", Vec::<Peer>::new());
    let _ = app.emit("safedrop-reset", ());
    Ok(())
}

/// Add Windows Firewall inbound rules for SafeDrop ports.
/// Needs admin rights — otherwise returns manual instructions.
#[tauri::command]
async fn open_firewall(state: tauri::State<'_, AppState>) -> Result<String, String> {
    let (tcp, sig, udp_on, udp_port) = {
        let s = state.settings.lock().await;
        (s.tcp_port, s.sig_port, s.udp_enabled, s.udp_port)
    };
    if cfg!(not(windows)) {
        let udp = if udp_on { format!(" и {udp_port}/udp") } else { String::new() };
        return Err(format!(
            "На Linux открой порты сам: {tcp}/tcp, {sig}/tcp{udp} (например, sudo ufw allow {tcp}/tcp)"
        ));
    }
    let out = tokio::task::spawn_blocking(move || {
        let mut log = String::new();
        let mut rules: Vec<(&str, u16, &str)> = vec![
            ("SafeDrop files", tcp, "TCP"),
            ("SafeDrop signal", sig, "TCP"),
        ];
        if udp_on {
            rules.push(("SafeDrop QUIC", udp_port, "UDP"));
        }
        for (name, port, proto) in rules {
            let r = std::process::Command::new("netsh")
                .args([
                    "advfirewall", "firewall", "add", "rule",
                    &format!("name={name}"), "dir=in", "action=allow",
                    &format!("protocol={proto}"), &format!("localport={port}"),
                ])
                .output();
            match r {
                Ok(o) if o.status.success() => {
                    log.push_str(&format!("OK {name} {proto}:{port}\n"));
                }
                Ok(o) => {
                    return Err(format!(
                        "netsh: {} — запусти приложение от имени администратора",
                        String::from_utf8_lossy(&o.stderr).trim()
                    ));
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(log)
    })
    .await
    .map_err(|e| e.to_string())?;
    out
}

fn exe_quoted() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(format!("\"{}\"", exe.display()))
}

fn reg(args: &[&str]) -> Result<String, String> {
    let o = std::process::Command::new("reg")
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

fn ctx_base() -> &'static str {
    "HKCU\\Software\\Classes\\*\\shell\\SafeDrop"
}

/// COM CLSID of safedrop-shell.dll (must match src-shell).
const SHELL_CLSID: &str = "{7D2E9F4A-3B1C-4D5E-8F60-9A1B2C3D4E5F}";

fn clsid_key() -> String {
    format!("HKCU\\Software\\Classes\\CLSID\\{SHELL_CLSID}")
}

fn shell_dll_path(app: &tauri::AppHandle) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // Installed builds keep the bundled extension under the app resource folder.
    let resource = app.path().resource_dir().map_err(|e| e.to_string())?.join("shell").join(format!("safedrop-shell-{}.dll", env!("CARGO_PKG_VERSION")));
    if resource.exists() {
        return Ok(resource.to_string_lossy().to_string());
    }
    let adjacent = exe
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("safedrop-shell.dll");
    if adjacent.exists() {
        return Ok(adjacent.to_string_lossy().to_string());
    }
    Err(format!("не найден DLL меню проводника (проверены {} и {})", adjacent.display(), resource.display()))
}

/// Register COM verb (shows in Win11 NEW menu + classic).
/// Contacts submenu is served LIVE by the DLL — no registry rewrites needed.
/// Legacy static tree is removed to avoid duplicates.
fn write_ctx_menu(app: &tauri::AppHandle) -> Result<(), String> {
    let exe = exe_quoted()?;
    let dll = shell_dll_path(app)?;
    let base = ctx_base();
    let _ = reg(&["delete", &format!("{base}\\command"), "/f"]);
    let _ = reg(&["delete", &format!("{base}\\shell"), "/f"]);
    let _ = reg(&["delete", base, "/v", "SubCommands", "/f"]); // legacy static submenu
    reg(&["add", base, "/ve", "/d", "Отправить в SafeDrop", "/f"])?;
    reg(&["add", base, "/v", "MUIVerb", "/d", "Отправить в SafeDrop", "/f"])?;
    reg(&["add", base, "/v", "Icon", "/d", &exe, "/f"])?;
    reg(&[
        "add", base, "/v", "ExplorerCommandHandler", "/d", SHELL_CLSID, "/f",
    ])?;
    let cls = clsid_key();
    reg(&["add", &format!("{cls}\\InprocServer32"), "/ve", "/d", &dll, "/f"])?;
    reg(&[
        "add",
        &format!("{cls}\\InprocServer32"),
        "/v",
        "ThreadingModel",
        "/d",
        "Apartment",
        "/f",
    ])?;
    Ok(())
}

#[tauri::command]
async fn set_ctx_menu(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    {
        state.settings.lock().await.ctx_menu = enabled;
    }
    settings::persist_settings(&state);
    if cfg!(not(windows)) {
        return if enabled { Err("Меню «Отправить в SafeDrop» есть только в Проводнике Windows".into()) } else { Ok(()) };
    }
    if !enabled {
        let _ = reg(&["delete", ctx_base(), "/f"]);
        let _ = reg(&["delete", &clsid_key(), "/f"]);
        return Ok(());
    }
    write_ctx_menu(&app)?;
    Ok(())
}

/// Contact count for the menu button (submenu itself is live via COM).
#[tauri::command]
async fn refresh_ctx_menu(state: tauri::State<'_, AppState>) -> Result<usize, String> {
    Ok(state.peers.lock().await.len())
}

/// ---------- window controls (through Rust — always works) ----------

fn main_window(app: &tauri::AppHandle) -> Result<tauri::WebviewWindow, String> {
    app.get_webview_window("main").ok_or("no main window".into())
}

#[tauri::command]
async fn win_min(app: tauri::AppHandle) -> Result<(), String> {
    main_window(&app)?.minimize().map_err(|e| e.to_string())
}

#[tauri::command]
async fn win_max_toggle(app: tauri::AppHandle) -> Result<(), String> {
    let w = main_window(&app)?;
    let maxed = w.is_maximized().map_err(|e| e.to_string())?;
    if maxed {
        w.unmaximize().map_err(|e| e.to_string())
    } else {
        w.maximize().map_err(|e| e.to_string())
    }
}

#[tauri::command]
async fn win_hide(app: tauri::AppHandle) -> Result<(), String> {
    main_window(&app)?.hide().map_err(|e| e.to_string())
}

#[tauri::command]
async fn win_quit(app: tauri::AppHandle) -> Result<(), String> {
    // история пишется на диск с задержкой — перед выходом дописать
    settings::save_chats_now(&app.state::<AppState>());
    app.exit(0);
    Ok(())
}

/// Start native window dragging (called on topbar mousedown).
#[tauri::command]
async fn win_drag(app: tauri::AppHandle) -> Result<(), String> {
    main_window(&app)?.start_dragging().map_err(|e| e.to_string())
}

/// Linux: ask the window manager to highlight the window.
#[cfg(not(windows))]
#[tauri::command]
async fn flash_window(app: tauri::AppHandle) -> Result<(), String> {
    let w = app.get_webview_window("main").ok_or("no window")?;
    w.request_user_attention(Some(tauri::UserAttentionType::Informational))
        .map_err(|e| e.to_string())
}

/// Flash taskbar icon when something arrives while window is hidden/minimized.
#[cfg(windows)]
#[tauri::command]
async fn flash_window(app: tauri::AppHandle) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        FlashWindowEx, FLASHWINFO, FLASHW_ALL, FLASHW_TIMERNOFG,
    };
    let w = app.get_webview_window("main").ok_or("no window")?;
    let raw = w.hwnd().map_err(|e| e.to_string())?;
    // tauri's HWND may come from another windows-crate version: pass via isize
    let hwnd = windows::Win32::Foundation::HWND(raw.0 as _);
    let info = FLASHWINFO {
        cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
        hwnd,
        dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
        uCount: 3,
        dwTimeout: 0,
    };
    // результат (было ли окно активным) не нужен
    let _ = unsafe { FlashWindowEx(&info) };
    Ok(())
}

/// Tell backend we're in a call/screenshare: pause heavy subnet scans
/// (scan bursts choke the Radmin relay and freeze video).
#[tauri::command]
async fn set_busy(state: tauri::State<'_, AppState>, busy: bool) -> Result<(), String> {
    state
        .busy
        .store(busy, std::sync::atomic::Ordering::SeqCst);
    Ok(())
}

/// Take pending file from CLI --send (clears it).
#[tauri::command]
async fn get_pending_send(state: tauri::State<'_, AppState>) -> Result<Option<PendingSend>, String> {
    Ok(state.pending_send.lock().await.take())
}

/// Reset a persistently-denied microphone permission:
/// kills the app, wipes the WebView2 profile dir, restarts.
/// (WebView2 remembers Allow/Deny per profile and skips future prompts.)
#[tauri::command]
async fn fix_mic_blocked(state: tauri::State<'_, AppState>) -> Result<(), String> {
    if cfg!(not(windows)) {
        return Err("На Linux проверь, что микрофон не выключен в настройках звука системы".into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bat_path = std::env::temp_dir().join("safedrop-fix-mic.bat");
    let bat = format!(
        "@echo off\r\ntaskkill /f /im safedrop.exe >nul 2>nul\r\ntimeout /t 2 /nobreak >nul\r\nrmdir /s /q \"%LOCALAPPDATA%\\com.safedrop.app\"\r\necho done > \"%TEMP%\\safedrop-mic-reset.done\"\r\nstart \"\" \"{}\"\r\ndel \"%~f0\"\r\n",
        exe.display()
    );
    std::fs::write(&bat_path, bat).map_err(|e| e.to_string())?;
    std::process::Command::new("cmd")
        .args(["/C", &bat_path.to_string_lossy()])
        .spawn()
        .map_err(|e| e.to_string())?;
    settings::save_chats_now(&state); // история пишется с задержкой
    // give the bat a moment to start, then exit so it can wipe the profile
    std::thread::sleep(std::time::Duration::from_millis(500));
    std::process::exit(0);
}

/// Marker left by the mic-fix bat: profile was wiped, tell the user.
#[tauri::command]
async fn take_mic_reset_flag() -> Result<bool, String> {
    let p = std::env::temp_dir().join("safedrop-mic-reset.done");
    if p.exists() {
        let _ = std::fs::remove_file(&p);
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Clear SafeDrop cache: avatars, temp files, stale .part files,
/// best-effort WebView2 disk cache. Reports freed bytes.
#[tauri::command]
async fn clear_cache(state: tauri::State<'_, AppState>) -> Result<String, String> {
    let mut freed: u64 = 0;
    let mut parts: Vec<String> = Vec::new();
    freed += rm_tree(&state.app_dir.join("avatars"));
    if let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if (name.starts_with("safedrop") || name.ends_with(".safedrop-part")) && !recently_written(&e) {
                if let Ok(m) = e.metadata() {
                    if std::fs::remove_file(e.path()).is_ok() { freed += m.len(); }
                }
            }
        }
    }
    parts.push("временные файлы".to_string());
    let dir = state.settings.lock().await.loaddrop_dir.clone();
    // Недокачанное: временные файлы приёма и части для докачки. Файл, в который
    // сейчас пишет передача, свежий — его не трогаем, иначе приём сорвётся.
    let mut n = 0u32;
    for d in [std::path::PathBuf::from(&dir), std::path::Path::new(&dir).join(".safedrop-partial")] {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let ext = e.path().extension().and_then(|x| x.to_str()).map(|x| x.to_string());
            let is_part = matches!(ext.as_deref(), Some("safedrop-part") | Some("part"))
                || e.file_name().to_string_lossy().ends_with(".part.json");
            if !is_part || recently_written(&e) { continue; }
            if let Ok(m) = e.metadata() {
                if m.is_file() && std::fs::remove_file(e.path()).is_ok() {
                    freed += m.len();
                    if ext.as_deref() != Some("json") { n += 1; }
                }
            }
        }
    }
    if n > 0 {
        parts.push(format!("недокачанных: {n}"));
    }
    // Tauri 2 хранит данные WebView2 в EBWebView\Default; занятые файлы
    // просто пропускаются
    if let Some(local) = dirs::data_local_dir() {
        let web = local.join("com.safedrop.app").join("EBWebView");
        for sub in ["Cache", "Code Cache", "GPUCache", "DawnGraphiteCache", "DawnWebGPUCache"] {
            freed += rm_tree(&web.join("Default").join(sub));
        }
        for sub in ["ShaderCache", "GrShaderCache", "GPUPersistentCache"] {
            freed += rm_tree(&web.join(sub));
        }
    }
    // WebKitGTK держит дисковый кэш в ~/.cache/<identifier>
    #[cfg(not(windows))]
    if let Some(cache) = dirs::cache_dir() {
        freed += rm_tree(&cache.join("com.safedrop.app"));
    }
    Ok(format!("Кэш очищен ({}), освобождено ~{}", parts.join(", "), fmt_mb(freed)))
}

/// Файл менялся последние 2 минуты — скорее всего, в него сейчас пишут.
fn recently_written(e: &std::fs::DirEntry) -> bool {
    e.metadata().and_then(|m| m.modified()).ok()
        .and_then(|t| t.elapsed().ok())
        .map(|age| age < std::time::Duration::from_secs(120))
        .unwrap_or(true)
}

fn rm_tree(p: &std::path::Path) -> u64 {
    let mut size = 0u64;
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                size += rm_tree(&e.path());
            } else if let Ok(m) = e.metadata() {
                // считаем только то, что действительно удалилось
                if std::fs::remove_file(e.path()).is_ok() { size += m.len(); }
            }
        }
    }
    let _ = std::fs::remove_dir(p);
    size
}

fn fmt_mb(b: u64) -> String {
    if b < 1024 {
        format!("{b} Б")
    } else if b < 1024 * 1024 {
        format!("{:.1} КБ", b as f64 / 1024.0)
    } else {
        format!("{:.1} МБ", b as f64 / 1024.0 / 1024.0)
    }
}

/// Cancel an active transfer (upload or download). Receiver side cleans .part.
#[tauri::command]
async fn cancel_transfer(
    state: tauri::State<'_, AppState>,
    transfer_id: String,
) -> Result<(), String> {
    state.cancel.lock().await.insert(transfer_id);
    Ok(())
}

#[tauri::command]
async fn respond_file_offer(
    state: tauri::State<'_, AppState>,
    offer_id: String,
    accepted: bool,
) -> Result<(), String> {
    let tx = state.incoming_file_requests.lock().await.remove(&offer_id)
        .ok_or_else(|| "Запрос на приём файла уже завершён".to_string())?;
    tx.send(accepted).map_err(|_| "Отправитель уже отключился".to_string())
}

/// Fire a sample system notification SYNCHRONOUSLY so real errors surface.
#[tauri::command]
async fn test_notify(state: tauri::State<'_, AppState>) -> Result<String, String> {
    let (nick, av) = {
        let s = state.settings.lock().await;
        (
            if s.profile.nickname.is_empty() { s.device_name.clone() } else { s.profile.nickname.clone() },
            s.profile.avatar.clone(),
        )
    };
    // attempt 1: with avatar icon
    if !av.is_empty() {
        if let Some(ic) = signal::avatar_icon_path(&state.app_dir, "me", &av) {
            let mut n = notify_rust::Notification::new();
            n.summary(&format!("{nick} (проверка)"));
            n.body("Тест с аватаркой.");
            n.icon(&ic);
            n.sound_name("Default");
            #[cfg(windows)]
            n.app_id("com.safedrop.app");
            #[cfg(not(windows))]
            n.appname("SafeDrop");
            match n.show() {
                Ok(_) => return Ok("show() OK с аватаркой — тост передан Windows.".into()),
                Err(e1) => {
                    // attempt 2: same but without icon (bad image kills the toast)
                    let mut n2 = notify_rust::Notification::new();
                    n2.summary(&format!("{nick} (проверка)"));
                    n2.body("Тест без аватарки.");
                    n2.auto_icon();
                    n2.sound_name("Default");
                    #[cfg(windows)]
                    n2.app_id("com.safedrop.app");
                    #[cfg(not(windows))]
                    n2.appname("SafeDrop");
                    match n2.show() {
                        Ok(_) => return Ok(format!("Без иконки — OK. С иконкой ошибка (аватарка битая?): {e1:?}. Смени аватарку.")),
                        Err(e2) => return Err(format!("с иконкой: {e1:?} | без иконки: {e2:?}")),
                    }
                }
            }
        }
    }
    let mut n = notify_rust::Notification::new();
    n.summary(&format!("{nick} (проверка)"));
    n.body("Так выглядят уведомления SafeDrop. Видишь это — всё работает.");
    n.auto_icon();
    #[cfg(windows)]
    n.app_id("com.safedrop.app");
    #[cfg(not(windows))]
    n.appname("SafeDrop");
    n.sound_name("Default");
    match n.show() {
        Ok(_) => Ok("show() OK — тост передан Windows. Если его НЕ видно: Параметры → Система → Уведомления (общий тумблер, «Не беспокоить», правила фокусировки — особенно «при дублировании экрана»).".into()),
        Err(e) => Err(format!("show() failed: {e:?}")),
    }
}

/// Displayed app version: version.txt next to exe (written by updater
/// from the archive name) wins, otherwise the compiled version.
pub(crate) fn app_version() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if let Ok(s) = std::fs::read_to_string(dir.join("version.txt")) {
                let s = s.trim().to_string();
                if !s.is_empty()
                    && s.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
                {
                    return s;
                }
            }
        }
    }
    env!("CARGO_PKG_VERSION").to_string()
}

/// Свой установщик для раздачи друзьям: SafeDrop_<версия>_x64-setup.exe.
/// Ищем рядом со сборкой (dist\..\update), среди полученных обновлений и в
/// «Загрузках». Nothing — пусть пользователь выберет файл сам.
#[tauri::command]
async fn own_installer() -> Result<Option<String>, String> {
    let prefix = format!("SafeDrop_{}_x64-setup", app_version());
    let mut dirs_to_check: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs_to_check.push(dir.join("..").join("update"));
            dirs_to_check.push(dir.to_path_buf());
        }
    }
    dirs_to_check.push(std::env::temp_dir().join("safedrop-updates"));
    if let Some(d) = dirs::download_dir() {
        dirs_to_check.push(d);
    }
    for dir in dirs_to_check {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            // только подписанный: без .sig получатели обновление не примут
            if name.starts_with(&prefix) && name.to_ascii_lowercase().ends_with(".exe") && updsig::sig_path(&entry.path()).exists() {
                return Ok(Some(entry.path().to_string_lossy().to_string()));
            }
        }
    }
    Ok(None)
}

/// Build identity: proves which exe is actually running (kills "old exe" ambiguity).
#[tauri::command]
async fn get_build_info() -> Result<serde_json::Value, String> {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(serde_json::json!({
        "version": app_version(),
        "build_time": option_env!("BUILD_TIME").unwrap_or("?"),
        "exe": exe,
        "os": std::env::consts::OS,
    }))
}

/// Ключ контакта для карточки: свой и его отпечаток, код для сверки голосом.
#[tauri::command]
async fn get_key_info(ip: String) -> Result<serde_json::Value, String> {
    let me = tls::my_fp();
    let pin = tls::pin_of(&ip);
    let fp = pin.as_ref().map(|p| p.fp.clone()).unwrap_or_default();
    Ok(serde_json::json!({
        "my_fp": me,
        "fp": fp,
        "mutual": pin.as_ref().map(|p| p.mutual).unwrap_or(false),
        "pending": pin.as_ref().map(|p| p.pending.clone()).unwrap_or_default(),
        "code": tls::safety_code(&me, &fp),
    }))
}

/// Пользователь подтвердил смену ключа контакта (переустановил SafeDrop и т.п.).
#[tauri::command]
async fn trust_new_key(ip: String) -> Result<String, String> {
    tls::trust_pending(&ip)
}

/// Забыть ключ контакта: при следующем соединении запомнится заново.
#[tauri::command]
async fn forget_key(ip: String) -> Result<(), String> {
    tls::forget_key(&ip);
    Ok(())
}

/// Local call history, stored separately from file transfer history.
#[tauri::command]
async fn get_call_history(state: tauri::State<'_, AppState>) -> Result<Vec<CallRecord>, String> {
    Ok(state.call_history.lock().await.clone())
}

#[tauri::command]
async fn clear_call_history(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.call_history.lock().await.clear();
    std::fs::write(state.call_history_path(), "[]").map_err(|e| e.to_string())
}

#[tauri::command]
async fn add_call_record(state: tauri::State<'_, AppState>, mut record: CallRecord) -> Result<(), String> {
    if !["incoming", "outgoing"].contains(&record.direction.as_str()) { return Err("invalid call direction".into()); }
    if !["completed", "missed", "unanswered"].contains(&record.status.as_str()) { return Err("invalid call status".into()); }
    record.peer.truncate(256);
    record.participants.truncate(64);
    let mut history = state.call_history.lock().await;
    history.push(record);
    let extra = history.len().saturating_sub(300);
    history.drain(..extra);
    let data = serde_json::to_vec_pretty(&*history).map_err(|e| e.to_string())?;
    std::fs::write(state.call_history_path(), data).map_err(|e| e.to_string())
}

/// Clear transfer history (sent files list).
#[tauri::command]
async fn clear_transfers(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.transfers.lock().await.clear();
    let _ = std::fs::remove_file(state.history_path());
    Ok(())
}

#[tauri::command]
async fn get_transfers(state: tauri::State<'_, AppState>) -> Result<Vec<TransferInfo>, String> {
    Ok(state.transfers.lock().await.clone())
}

#[tauri::command]
async fn open_loaddrop(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let dir = state.settings.lock().await.loaddrop_dir.clone();
    open::that(dir).map_err(|e| e.to_string())
}

#[tauri::command]
async fn move_loaddrop(
    state: tauri::State<'_, AppState>,
    new_path: String,
) -> Result<(), String> {
    if new_path.trim().is_empty() { return Err("empty path".into()); }
    std::fs::create_dir_all(&new_path).map_err(|e| e.to_string())?;
    state.settings.lock().await.loaddrop_dir = new_path;
    settings::persist_settings(&state);
    Ok(())
}

/// Parse Explorer context-menu args: --send <file> | --send-to <ip> <file>.
/// Works with or without argv[0] (exe path hits the fallthrough branch).
fn parse_ctx_args(args: &[String]) -> Option<PendingSend> {
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--send-to" => {
                if let (Some(ip), Some(file)) = (args.get(i + 1), args.get(i + 2)) {
                    return Some(PendingSend {
                        contact: Some(ip.clone()),
                        file: file.clone(),
                    });
                }
                i += 1;
            }
            "--send" => {
                if let Some(file) = args.get(i + 1) {
                    return Some(PendingSend {
                        contact: None,
                        file: file.clone(),
                    });
                }
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    None
}

/// Bring main window forward (used for context-menu arrivals).
fn focus_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn main() {
    let dir = settings::app_dir();
    let _ = std::fs::create_dir_all(&dir);
    tls::init(&dir);
    let st = AppState::new(dir);
    settings::load_settings(&st);
    settings::load_peers(&st);
    if let Ok(data) = std::fs::read_to_string(st.call_history_path()) {
        if let Ok(mut rows) = serde_json::from_str::<Vec<CallRecord>>(&data) {
            if rows.len() > 300 { rows.drain(..rows.len() - 300); }
            if let Ok(mut history) = st.call_history.try_lock() { *history = rows; }
        }
    }
    // с кем шифровать соединения — по версиям, известным с прошлого запуска
    if let Ok(peers) = st.peers.try_lock() {
        for p in peers.values() {
            for ip in std::iter::once(&p.ip).chain(p.ips.iter()) {
                tls::note_peer_version(ip, &p.version);
            }
        }
    }
    // chats + groups loaded sync here
    if let Ok(data) = std::fs::read_to_string(st.group_sync_path()) {
        if let Ok(sync) = serde_json::from_str::<state::GroupSyncState>(&data) {
            if let Ok(mut g) = st.group_sync.try_lock() { *g = sync; }
        }
    }
    if let Ok(data) = std::fs::read_to_string(st.groups_path()) {
        if let Ok(v) = serde_json::from_str::<Vec<Group>>(&data) {
            if let Ok(mut g) = st.groups.try_lock() {
                *g = v;
            }
        }
    }
    {
        let sync = st.group_sync.try_lock().map(|s| s.clone()).unwrap_or_default();
        let local_ips: std::collections::HashSet<String> = discovery::my_ips().into_iter().collect();
        let deleted: std::collections::HashSet<String> = sync.deleted.iter().map(|d| d.id.clone()).collect();
        let locally_left: std::collections::HashSet<String> = sync.departures.iter()
            .filter(|d| local_ips.contains(&d.member_ip)).map(|d| d.group_id.clone()).collect();
        if let Ok(mut groups) = st.groups.try_lock() {
            groups.retain(|g| !deleted.contains(&g.id) && !locally_left.contains(&g.id));
            for rejoin in &sync.rejoins {
                if deleted.contains(&rejoin.group_id) { continue; }
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
                group.members.retain(|ip| !sync.departures.iter().any(|d| d.group_id == group.id && d.member_ip == *ip));
            }
        }
    }
    // CLI from Explorer context menu: --send <file> | --send-to <ip> <file>
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if let Some(p) = parse_ctx_args(&args) {
            if let Ok(mut g) = st.pending_send.try_lock() {
                *g = Some(p);
            }
        }
    }
    // chats loaded async after manage; do sync here
    if let Ok(data) = std::fs::read_to_string(st.chats_path()) {
        if let Ok(mut m) = serde_json::from_str::<std::collections::HashMap<String, Vec<ChatMsg>>>(&data) {
            for rows in m.values_mut() {
                rows.sort_by(|a, b| a.sent_at.cmp(&b.sent_at).then_with(|| a.id.cmp(&b.id)));
            }
            if let Ok(mut g) = st.chats.try_lock() {
                *g = m;
            }
        }
    }
    {
        let sync = st.group_sync.try_lock().map(|s| s.clone()).unwrap_or_default();
        let local_ips: std::collections::HashSet<String> = discovery::my_ips().into_iter().collect();
        let deleted: std::collections::HashSet<String> = sync.deleted.iter().map(|d| format!("g:{}", d.id)).collect();
        let locally_left: std::collections::HashSet<String> = sync.departures.iter()
            .filter(|d| local_ips.contains(&d.member_ip)).map(|d| format!("g:{}", d.group_id)).collect();
        if let Ok(mut chats) = st.chats.try_lock() {
            for key in deleted.iter().chain(locally_left.iter()) { chats.remove(key); }
        }
    }
    let shared = Arc::new(st);
    let managed: AppState = match Arc::try_unwrap(shared) {
        Ok(v) => v,
        Err(_) => panic!("state shared too early"),
    };

    tauri::Builder::default()
        .manage(managed)
        .on_window_event(|window, event| {
            // Только главное окно: крестик окна YouTube закрывает лишь его,
            // а не всё приложение.
            if window.label() != "main" {
                return;
            }
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.app_handle().emit("safedrop-quit-requested", ());
            }
        })
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // launched from Explorer while already running:
            // forward the file to the open window instead of a 2nd window
            if let Some(p) = parse_ctx_args(&args) {
                let _ = app.emit(
                    "safedrop-ctx-send",
                    serde_json::json!({"contact": p.contact, "file": p.file}),
                );
            }
            focus_main(app);
        }))
        .plugin(tauri_plugin_notification::init())
        // microphone is core feature: grant it outright so an accidental
        // "deny" can never silently break calls (no prompt at all)
        .on_permission_request(|_w, kind| match kind {
            PermissionKind::Microphone => PermissionResponse::Allow,
            // WebKitGTK не показывает своего запроса, как WebView2, и без ответа
            // отказывает: камеру и экран для звонков разрешаем только окну SafeDrop
            #[cfg(target_os = "linux")]
            PermissionKind::Camera | PermissionKind::DisplayCapture if _w.label() == "main" => PermissionResponse::Allow,
            _ => PermissionResponse::Default,
        })
        .setup(|app| {
            settings::init_chat_saver(app.handle());
            // контакт показал не тот ключ — спросить пользователя
            let key_app = app.handle().clone();
            tls::set_key_notifier(Box::new(move |ip, old, new| {
                let _ = key_app.emit("safedrop-key-changed", serde_json::json!({ "ip": ip, "old": old, "new": new }));
            }));
            // WebRTC в WebKitGTK выключен, пока его не включат в настройках (и есть,
            // только если WebKit собран с ним — см. packaging/arch/webkit2gtk-4.1-webrtc)
            #[cfg(target_os = "linux")]
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.with_webview(|wv| {
                    use webkit2gtk::{glib::prelude::*, DeviceInfoPermissionRequest, PermissionRequestExt, SettingsExt, WebViewExt};
                    let view = wv.inner();
                    if let Some(s) = view.settings() {
                        s.set_enable_webrtc(true);
                        s.set_enable_media_stream(true);
                    }
                    // Без этого WebKit отдаёт enumerateDevices без названий и с пустыми
                    // deviceId: в списке «Микрофон 1», выбор устройства не сохраняется.
                    // Обработчик wry вызывается раньше и такие запросы не решает.
                    view.connect_permission_request(|_, req| {
                        if req.is::<DeviceInfoPermissionRequest>() {
                            req.allow();
                            return true;
                        }
                        false
                    });
                });
            }
            // tray: left click toggles window, menu has Show/Quit
            {
                use tauri::{
                    menu::{Menu, MenuItem},
                    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
                };
                let show = MenuItem::with_id(app, "show", "Показать SafeDrop", true, None::<&str>)?;
                let quit = MenuItem::with_id(app, "quit", "Выйти", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&show, &quit])?;
                TrayIconBuilder::with_id("main")
                    .icon(app.default_window_icon().unwrap().clone())
                    .tooltip("SafeDrop — P2P обмен")
                    .menu(&menu)
                    .show_menu_on_left_click(false)
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "show" => focus_main(app),
                        "quit" => { let _ = app.emit("safedrop-quit-requested", ()); }
                        _ => {}
                    })
                    .on_tray_icon_event(|tray, event| {
                        if let TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } = event
                        {
                            let app = tray.app_handle();
                            if let Some(w) = app.get_webview_window("main") {
                                if w.is_visible().unwrap_or(true) {
                                    let _ = w.hide();
                                } else {
                                    focus_main(app);
                                }
                            }
                        }
                    })
                    .build(app)?;
            }
            // shortcut with AppUserModelID => toasts branded as SafeDrop
            ensure_notif_shortcut();
            // Repoint the Explorer handler to this release's side-by-side DLL.
            // Keeping each version under a unique filename avoids overwriting a
            // DLL that Explorer may still have loaded from an earlier release.
            let ctx_menu_enabled = app.state::<AppState>()
                .settings.try_lock().map(|s| s.ctx_menu).unwrap_or(false);
            if ctx_menu_enabled && cfg!(windows) {
                let _ = write_ctx_menu(app.handle());
            }
            // launched from Explorer context menu: bring window forward
            if app
                .state::<AppState>()
                .pending_send
                .try_lock()
                .map(|g| g.is_some())
                .unwrap_or(false)
            {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.unminimize();
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            // background file server (LoadDrop is read per-connection)
            let handle = app.handle().clone();
            let port: u16 = app.state::<AppState>()
                .settings.try_lock().map(|s| s.tcp_port).unwrap_or(47831);
            tauri::async_runtime::spawn(async move {
                transfer::run_server(handle, port).await;
            });
            // signaling server (profile/chat/voice)
            let shandle = app.handle().clone();
            let sport: u16 = app.state::<AppState>()
                .settings.try_lock().map(|s| s.sig_port).unwrap_or(47832);
            tauri::async_runtime::spawn(async move {
                signal::run_signal_server(shandle, sport).await;
            });
            // QUIC server (beta): only when enabled, TCP path untouched otherwise
            let qhandle = app.handle().clone();
            let (qon, qport): (bool, u16) = app.state::<AppState>()
                .settings.try_lock().map(|s| (s.udp_enabled, s.udp_port)).unwrap_or((false, 47833));
            if qon {
                tauri::async_runtime::spawn(async move {
                    quic::run_quic_server(qhandle, qport).await;
                });
            }
            // auto-scan at startup immediately, then every 30s; a network
            // adapter change (VPN connected, Wi-Fi switched) triggers an
            // immediate re-scan on the next 15s tick
            let handle2 = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                {
                    let state: tauri::State<'_, AppState> = handle2.state();
                    refresh_live(&handle2, &state).await;
                }
                let mut tick: u32 = 0;
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                    tick = tick.wrapping_add(1);
                    let state: tauri::State<'_, AppState> = handle2.state();
                    if state.busy.load(std::sync::atomic::Ordering::SeqCst) {
                        continue; // call/screenshare: don't choke the relay
                    }
                    let sig = discovery::adapter_signature(&discovery::my_adapters());
                    let changed = state.last_adapter_sig.lock().map(|s| *s != sig).unwrap_or(true);
                    if !changed && tick % 2 != 0 {
                        continue; // steady state: full sweep every 30s
                    }
                    refresh_live(&handle2, &state).await;
                }
            });
            // lightweight profile sync heartbeat every 15s; every 30s also
            // retry delivery of messages whose ack was lost
            let handle3 = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut tick: u32 = 0;
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                    signal::broadcast_hash(&handle3).await;
                    tick = tick.wrapping_add(1);
                    if tick % 2 == 0 {
                        signal::retry_pending(&handle3).await;
                    }
                }
            });
            // Check every 2s; signal::dead_online applies a 5s grace period
            // before declaring a peer offline, so brief/high-latency drops do not end calls.
            let handle4 = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    let state: tauri::State<'_, AppState> = handle4.state();
                    let port = state.settings.lock().await.tcp_port;
                    let dead = signal::dead_online(&state, port).await;
                    if !dead.is_empty() {
                        settings::persist_peers(&state);
                        let peers: Vec<Peer> =
                            state.peers.lock().await.values().cloned().collect();
                        let _ = handle4.emit("safedrop-peers", peers);
                        for ip in dead {
                            let _ = handle4
                                .emit("safedrop-peer-lost", serde_json::json!({"ip": ip}));
                        }
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings, save_settings, get_profile, save_profile, set_peer_avatar,
            get_my_ips, get_my_adapters, get_peers, add_peer,
            rename_peer, remove_peer, toggle_trust, ping_peer, scan_network,
            get_call_history, clear_call_history, add_call_record,
            get_chat, send_chat, send_group_chat, chat_edit, chat_react, chat_typing, sig_send, delete_msgs, clear_chat, open_mic_settings, set_busy,
            get_groups, create_group, rename_group, set_group_avatar, delete_group, leave_group, group_add_member, group_remove_member,
            send_file, send_update, apply_update, open_url, open_notif_settings, get_transfers, clear_transfers, cancel_transfer, respond_file_offer, test_notify, open_loaddrop, move_loaddrop, get_loaddrop,
            pick_files, pick_image, pick_folder, pick_audio, read_image_dataurl,
            clear_peers, scan_debug, factory_reset, open_firewall, set_ctx_menu, refresh_ctx_menu, get_pending_send,
            fix_mic_blocked, take_mic_reset_flag, clear_cache, open_mic_settings,
            win_min, win_max_toggle, win_hide, win_quit, win_drag, flash_window, get_build_info, own_installer, get_key_info, trust_new_key, forget_key,
            ytbrowse::ytb_open, ytbrowse::ytb_go, ytbrowse::ytb_nav, ytbrowse::ytb_watch, ytbrowse::ytb_notify, ytbrowse::ytb_state,
            ytbrowse::ytb_set_share, ytbrowse::ytb_remote_nav, ytbrowse::ytb_pause_media, ytbrowse::ytb_media_apply, ytbrowse::ytb_duck, ytbrowse::ytb_chat_send, ytbrowse::ytb_chat_show, ytbrowse::ytb_chat_toggle, ytbrowse::ytb_chat_request, ytbrowse::ytb_chat_history,
            ytbrowse::ytb_close, ytbrowse::ytb_close_all, ytbrowse::ytb_remote_cursor, ytbrowse::ytb_remote_scroll, ytbrowse::ytb_people,
        ])
        .run(tauri::generate_context!())
        .expect("error while running SafeDrop");
}

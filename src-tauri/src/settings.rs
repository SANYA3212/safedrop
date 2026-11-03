use crate::state::{AppState, Settings};
use std::fs;

pub fn app_dir() -> std::path::PathBuf {
    dirs::config_dir()
        .map(|p| p.join("safedrop"))
        .unwrap_or(std::path::PathBuf::from("./safedrop-data"))
}

pub fn load_settings(state: &AppState) {
    let path = state.settings_path();
    if let Ok(data) = fs::read_to_string(&path) {
        if let Ok(mut s) = serde_json::from_str::<Settings>(&data) {
            // an old or hand-edited file without a folder would save files
            // into the working directory
            if s.loaddrop_dir.trim().is_empty() {
                s.loaddrop_dir = Settings::default().loaddrop_dir;
            }
            if let Ok(mut g) = state.settings.try_lock() {
                *g = s;
            }
        }
    }
    // ensure LoadDrop exists
    if let Ok(g) = state.settings.try_lock() {
        let _ = fs::create_dir_all(&g.loaddrop_dir);
    }
    let _ = fs::create_dir_all(state.app_dir.clone());
}

/// The persist helpers are sync and called from many places; a plain
/// `try_lock` silently dropped the write whenever a background task held the
/// lock for a moment. Retry for up to half a second instead.
pub fn try_lock_briefly<T>(m: &tokio::sync::Mutex<T>) -> Option<tokio::sync::MutexGuard<'_, T>> {
    for _ in 0..100 {
        if let Ok(g) = m.try_lock() {
            return Some(g);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    None
}

static CHAT_APP: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();
static CHATS_DIRTY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static CHATS_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn init_chat_saver(app: &tauri::AppHandle) {
    let _ = CHAT_APP.set(app.clone());
}

/// chats.json can be many MB (images are inline). Rewriting it inside every
/// chat command blocked a tokio worker and held the chats lock; now a burst
/// of changes becomes one background write about 0.4 s later.
pub fn save_chats_soon(state: &AppState) {
    use std::sync::atomic::Ordering;
    let Some(app) = CHAT_APP.get() else { save_chats_now(state); return };
    if CHATS_DIRTY.swap(true, Ordering::SeqCst) {
        return; // запись уже запланирована и увидит это изменение
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        use tauri::Manager;
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        CHATS_DIRTY.store(false, Ordering::SeqCst);
        let state = app.state::<AppState>();
        let data = serde_json::to_vec(&*state.chats.lock().await);
        let (dir, path) = (state.app_dir.clone(), state.chats_path());
        if let Ok(data) = data {
            let _ = tauri::async_runtime::spawn_blocking(move || write_atomic(&dir, &path, &data)).await;
        }
    });
}

/// Synchronous save (app exit, or before the saver is set up).
pub fn save_chats_now(state: &AppState) {
    if let Some(g) = try_lock_briefly(&state.chats) {
        if let Ok(data) = serde_json::to_vec(&*g) {
            drop(g);
            write_atomic(&state.app_dir, &state.chats_path(), &data);
        }
    }
}

/// Write to a temp file and swap it in: a crash mid-write no longer leaves a
/// truncated chats.json (which would lose the whole history on next start).
fn write_atomic(dir: &std::path::Path, path: &std::path::Path, data: &[u8]) {
    let _guard = CHATS_WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let _ = fs::create_dir_all(dir);
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, data).is_ok() && fs::rename(&tmp, path).is_err() {
        let _ = fs::write(path, data);
        let _ = fs::remove_file(&tmp);
    }
}

pub fn persist_settings(state: &AppState) {
    if let Some(g) = try_lock_briefly(&state.settings) {
        let _ = fs::create_dir_all(&state.app_dir);
        if let Ok(data) = serde_json::to_string_pretty(&*g) {
            let _ = fs::write(state.settings_path(), data);
        }
        let _ = fs::create_dir_all(&g.loaddrop_dir);
    }
}

pub fn load_peers(state: &AppState) {
    use crate::state::Peer;
    use std::collections::HashMap;
    let path = state.peers_path();
    if let Ok(data) = fs::read_to_string(&path) {
        if let Ok(list) = serde_json::from_str::<Vec<Peer>>(&data) {
            if let Ok(mut g) = state.peers.try_lock() {
                *g = list.into_iter().map(|p| (p.ip.clone(), p)).collect::<HashMap<_, _>>();
            }
        }
    }
}

pub fn persist_peers(state: &AppState) {
    if let Some(g) = try_lock_briefly(&state.peers) {
        let list: Vec<_> = g.values().cloned().collect();
        if let Ok(data) = serde_json::to_string_pretty(&list) {
            let _ = fs::write(state.peers_path(), data);
        }
    }
}

/// Autostart via Startup folder shortcut (stub v1: .bat launcher).
/// Creates/removes %APPDATA%/Microsoft/Windows/Start Menu/Programs/Startup/SafeDrop.bat
pub fn apply_autostart(enabled: bool) -> Result<(), String> {
    let startup = dirs::config_dir()
        .map(|p| {
            p.ancestors().nth(3).unwrap_or(p.as_path()).to_path_buf()
                .join("Microsoft").join("Windows").join("Start Menu")
                .join("Programs").join("Startup")
        })
        .ok_or("no startup dir")?;
    let bat = startup.join("SafeDrop.bat");
    if enabled {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let content = format!("@echo off\r\nstart \"\" \"{}\"\r\n", exe.display());
        std::fs::create_dir_all(&startup).map_err(|e| e.to_string())?;
        std::fs::write(&bat, content).map_err(|e| e.to_string())?;
    } else if bat.exists() {
        let _ = std::fs::remove_file(&bat);
    }
    Ok(())
}

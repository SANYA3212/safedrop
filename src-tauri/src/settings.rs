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
        if let Ok(s) = serde_json::from_str::<Settings>(&data) {
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

pub fn persist_settings(state: &AppState) {
    if let Ok(g) = state.settings.try_lock() {
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
    if let Ok(g) = state.peers.try_lock() {
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

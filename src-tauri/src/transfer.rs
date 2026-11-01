use crate::state::TransferInfo;
use sha2::{Digest, Sha256};
use std::{path::Path, time::Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
};

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub(crate) struct Offer {
    pub(crate) name: String,
    pub(crate) size: u64,
    pub(crate) id: String,
    pub(crate) from: String,
    #[serde(default)]
    pub(crate) kind: String, // "file" | "update"
    #[serde(default)]
    pub(crate) confirm: bool,
    #[serde(default)]
    pub(crate) version: String,
}

fn should_auto_accept(auto_trusted: bool, trusted: bool, size: u64, limit_mb: u64) -> bool {
    auto_trusted && trusted && size <= limit_mb.saturating_mul(1024 * 1024)
}

/// Parse `safedrop-update-<ver>.zip` -> version string.
pub fn update_version(name: &str) -> Option<String> {
    let base = std::path::Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let rest = base.strip_prefix("safedrop-update-")?;
    let ver = rest.strip_suffix(".zip")?;
    if ver.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())) {
        Some(ver.to_string())
    } else {
        None
    }
}

const CHUNK: usize = 4 * 1024 * 1024;

/// Check whether this transfer id was cancelled (consumes the flag).
pub(crate) async fn is_cancelled(app: &AppHandle, id: &str) -> bool {
    let state: tauri::State<'_, crate::state::AppState> = app.state();
    let x = state.cancel.lock().await.remove(id);
    x
}

/// TCP file server: 1 connection = 1 JSON-line header + raw bytes.
/// LoadDrop dir is read from settings per connection (moving folder works without restart).
pub async fn run_server(app: AppHandle, port: u16) {
    let addr = format!("0.0.0.0:{port}");
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("SafeDrop bind {addr} failed: {e}");
            return;
        }
    };
    println!("SafeDrop listening on {addr}");
    loop {
        let (mut sock, peer_addr) = match listener.accept().await {
            Ok(x) => x,
            Err(_) => continue,
        };
        let app2 = app.clone();
        // current LoadDrop (user may move it in settings anytime)
        let drop_dir: String = app
            .state::<crate::state::AppState>()
            .settings
            .try_lock()
            .map(|s| s.loaddrop_dir.clone())
            .unwrap_or_default();
        tokio::spawn(async move {
            let _ = sock.set_nodelay(true);
            if let Err(e) = handle_conn(app2, &mut sock, peer_addr.to_string(), drop_dir).await {
                eprintln!("conn error: {e}");
            }
        });
    }
}

async fn handle_conn(
    app: AppHandle,
    sock: &mut tokio::net::TcpStream,
    peer_addr: String,
    drop_dir: String,
) -> anyhow::Result<()> {
    let peer_ip = peer_addr.split(':').next().unwrap_or(&peer_addr).to_string();
    let (read_half, mut write_half) = tokio::io::split(&mut *sock);
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let offer: Offer = serde_json::from_str(line.trim())?;
    let accepted = authorize_offer(&app, &offer, &peer_ip).await;
    if offer.confirm {
        let reply = serde_json::json!({"kind":"file-offer-result","accepted":accepted});
        write_half.write_all(format!("{}\n", reply).as_bytes()).await?;
        write_half.flush().await?;
    }
    if !accepted { return Ok(()); }
    receive_stream(app, offer, &mut reader, peer_ip, drop_dir).await
}

/// Auto-accept only trusted peers under the configured size limit. Other
/// transfers pause before any file data is written until the user responds.
pub(crate) async fn authorize_offer(app: &AppHandle, offer: &Offer, peer_ip: &str) -> bool {
    let state: tauri::State<'_, crate::state::AppState> = app.state();
    let (auto_trusted, limit_mb) = {
        let settings = state.settings.lock().await;
        (settings.auto_accept_trusted, settings.auto_accept_limit_mb)
    };
    let trusted = state.peers.lock().await.get(peer_ip).map(|p| p.trusted).unwrap_or(false);
    // An update offer is followed by a separate, explicit install prompt after
    // download. Do not block the transfer behind the generic file approval UI.
    if offer.kind == "update" || should_auto_accept(auto_trusted, trusted, offer.size, limit_mb) {
        return true;
    }

    let (tx, rx) = tokio::sync::oneshot::channel();
    state.incoming_file_requests.lock().await.insert(offer.id.clone(), tx);
    let sender = state.peers.lock().await.get(peer_ip)
        .map(|p| if !p.nickname.is_empty() { p.nickname.clone() } else if !p.name.is_empty() { p.name.clone() } else { peer_ip.to_string() })
        .unwrap_or_else(|| peer_ip.to_string());
    let _ = app.emit("safedrop-file-offer", serde_json::json!({
        "id": offer.id, "filename": sanitize(&offer.name), "size": offer.size,
        "peer": peer_ip, "sender": sender,
    }));
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
    let response = tokio::time::timeout(std::time::Duration::from_secs(120), rx).await;
    state.incoming_file_requests.lock().await.remove(&offer.id);
    matches!(response, Ok(Ok(true)))
}

/// Shared receive path for TCP and QUIC transports.
/// Reads the file body, verifies size, moves into LoadDrop (or updates dir),
/// updates progress / history / notifications, cleans up on errors.
pub(crate) async fn receive_stream(
    app: AppHandle,
    offer: Offer,
    reader: &mut (impl AsyncRead + Unpin),
    peer_ip: String,
    drop_dir: String,
) -> anyhow::Result<()> {
    let total = offer.size;
    let safe_name = sanitize(&offer.name);
    let is_update = offer.kind == "update";
    // updates go to temp dir (never LoadDrop) and trigger the update prompt
    let target_dir = if is_update {
        let d = std::env::temp_dir().join("safedrop-updates");
        let _ = tokio::fs::create_dir_all(&d).await;
        d.to_string_lossy().to_string()
    } else {
        drop_dir
    };
    let _ = tokio::fs::create_dir_all(&target_dir).await;
    let final_path = Path::new(&target_dir).join(&safe_name);
    // avoid overwrite: file (1).ext
    let final_path = unique_path(final_path);
    let part_path = final_path.with_extension("safedrop-part");

    let id = offer.id.clone();
    push_transfer(&app, TransferInfo {
        id: id.clone(), filename: safe_name.clone(), size: total, done: 0,
        speed_bps: 0, direction: "down".into(), peer: peer_ip.clone(),
        status: "active".into(), progress: 0.0, error: String::new(),
    }).await;

    let mut out = match File::create(&part_path).await {
        Ok(f) => f,
        Err(e) => {
            let msg = format!("can't write to disk: {e}");
            set_transfer_error(&app, &id, &msg).await;
            return Err(anyhow::anyhow!(msg));
        }
    };
    let mut hasher = Sha256::new();
    let mut done: u64 = 0;
    let mut buf = vec![0u8; CHUNK];
    let start = Instant::now();
    let mut last_emit = Instant::now();
    // reader borrows sock mutably; read remainder manually.
    // NB: every failure below must mark the transfer — otherwise it hangs
    // in "active" forever with a stuck progress bar.
    loop {
        if is_cancelled(&app, &id).await {
            break;
        }
        let n = match reader.read(&mut buf).await {
            Ok(n) => n,
            Err(e) => {
                let msg = format!("connection lost: {e}");
                let _ = tokio::fs::remove_file(&part_path).await;
                set_transfer_error(&app, &id, &msg).await;
                return Err(anyhow::anyhow!(msg));
            }
        };
        if n == 0 { break; }
        if let Err(e) = out.write_all(&buf[..n]).await {
            let msg = format!("can't write to disk: {e}");
            let _ = tokio::fs::remove_file(&part_path).await;
            set_transfer_error(&app, &id, &msg).await;
            return Err(anyhow::anyhow!(msg));
        }
        hasher.update(&buf[..n]);
        done += n as u64;
        if last_emit.elapsed().as_millis() > 200 || done == total {
            let el = start.elapsed().as_secs_f64().max(0.01);
            let speed = (done as f64 / el) as u64;
            update_transfer(&app, &id, done, speed, total).await;
            last_emit = Instant::now();
        }
        if done >= total { break; }
    }
    if let Err(e) = out.flush().await {
        let msg = format!("can't flush file: {e}");
        let _ = tokio::fs::remove_file(&part_path).await;
        set_transfer_error(&app, &id, &msg).await;
        return Err(anyhow::anyhow!(msg));
    }
    drop(out);
    if done < total {
        // cancelled or broken connection: no half-baked files, clean the cache
        let _ = tokio::fs::remove_file(&part_path).await;
        set_transfer_status(&app, &id, "cancelled").await;
        let _ = app.emit("safedrop-cancelled", serde_json::json!({
            "filename": safe_name, "peer": peer_ip, "direction": "down",
        }));
        return Ok(());
    }
    if tokio::fs::rename(&part_path, &final_path).await.is_err() {
        // antivirus/file lock can hold the fresh file briefly — one retry
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        if let Err(e) = tokio::fs::rename(&part_path, &final_path).await {
            let msg = format!("can't move into place: {e}");
            let _ = tokio::fs::remove_file(&part_path).await;
            set_transfer_error(&app, &id, &msg).await;
            return Err(anyhow::anyhow!(msg));
        }
    }
    let hash = hex::encode(hasher.finalize());
    println!("received {} ({} bytes, sha256 {})", final_path.display(), done, hash);
    finish_transfer(&app, &id).await;
    if is_update {
        let ver = if offer.version.is_empty() {
            update_version(&safe_name).unwrap_or_default()
        } else {
            offer.version.clone()
        };
        let _ = app.emit("safedrop-update-offer", serde_json::json!({
            "version": ver, "filename": safe_name,
            "path": final_path.to_string_lossy(),
            "size": done, "peer": peer_ip, "sha256": hash,
        }));
        crate::signal::notify("⬆ Доступно обновление SafeDrop", &format!("{safe_name} — от {peer_ip}"), None);
        return Ok(());
    }
    let _ = app.emit("safedrop-received", serde_json::json!({
        "filename": safe_name, "path": final_path.to_string_lossy(),
        "size": done, "peer": peer_ip, "sha256": hash,
    }));
    crate::signal::notify("📥 Файл получен", &format!("{safe_name} — {peer_ip}"), None);
    Ok(())
}

/// Client: send local file to peer_ip:port (kind "file" or "update")
pub async fn send_file_to(
    app: AppHandle,
    peer_ip: String,
    port: u16,
    file_path: String,
    device_name: String,
    kind: String,
    require_confirmation: bool,
    version: String,
) -> Result<String, String> {
    let prep = prepare_upload(&file_path).await?;
    let mut sock = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::net::TcpStream::connect(format!("{peer_ip}:{port}")),
    )
    .await
    .map_err(|_| format!("тайм-аут подключения к {peer_ip}:{port}"))?
    .map_err(|e| format!("connect {peer_ip}:{port}: {e}"))?;
    let _ = sock.set_nodelay(true);
    let offer = serde_json::json!({"name": prep.fname, "size": prep.total, "id": prep.id, "from": device_name, "kind": kind, "confirm": require_confirmation, "version": version});
    let mut header = serde_json::to_string(&offer).map_err(|e| e.to_string())?;
    header.push('\n');
    tokio::time::timeout(std::time::Duration::from_secs(10), sock.write_all(header.as_bytes()))
        .await.map_err(|_| "тайм-аут отправки заголовка файла".to_string())?
        .map_err(|e| e.to_string())?;
    if require_confirmation {
        let result = {
            let mut reader = BufReader::new(&mut sock);
            let mut line = String::new();
            tokio::time::timeout(std::time::Duration::from_secs(130), reader.read_line(&mut line))
                .await
                .map_err(|_| "Тайм-аут ожидания подтверждения приёма файла".to_string())?
                .map_err(|e| format!("Не удалось получить ответ получателя: {e}"))?;
            serde_json::from_str::<serde_json::Value>(line.trim()).map_err(|e| format!("Некорректный ответ получателя: {e}"))?
        };
        if result.get("accepted").and_then(|v| v.as_bool()) != Some(true) {
            return Err("Получатель отклонил приём файла".into());
        }
    }
    pump_upload(&app, prep, &peer_ip, &mut sock).await
}

pub(crate) struct UploadPrep {
    pub real_path: std::path::PathBuf,
    pub total: u64,
    pub fname: String,
    pub id: String,
    pub tmp_zip: Option<std::path::PathBuf>,
}

/// Shared upload preparation (exists check, dir zip, metadata).
pub(crate) async fn prepare_upload(file_path: &str) -> Result<UploadPrep, String> {
    let path = Path::new(&file_path);
    if !path.exists() { return Err("file not found".into()); }
    // directory -> zip it first
    let mut tmp_zip: Option<std::path::PathBuf> = None;
    let real_path: std::path::PathBuf;
    if path.is_dir() {
        let zip_path = std::env::temp_dir().join(format!(
            "{}-{}.zip",
            path.file_name().and_then(|s| s.to_str()).unwrap_or("folder"),
            &uuid::Uuid::new_v4().to_string()[..8]
        ));
        zip_dir(path, &zip_path).map_err(|e| e.to_string())?;
        tmp_zip = Some(zip_path.clone());
        real_path = zip_path;
    } else {
        real_path = path.to_path_buf();
    }
    let meta = std::fs::metadata(&real_path).map_err(|e| e.to_string())?;
    let total = meta.len();
    let fname = real_path.file_name().and_then(|s| s.to_str()).unwrap_or("file").to_string();
    let id = uuid::Uuid::new_v4().to_string();
    Ok(UploadPrep { real_path, total, fname, id, tmp_zip })
}

/// Shared upload pump (progress, cancel, errors, history) for TCP and QUIC.
/// `writer` must already carry the Offer header line.
pub(crate) async fn pump_upload(
    app: &AppHandle,
    prep: UploadPrep,
    peer_ip: &str,
    writer: &mut (impl AsyncWrite + Unpin),
) -> Result<String, String> {
    let UploadPrep { real_path, total, fname, id, tmp_zip } = prep;
    push_transfer(app, TransferInfo {
        id: id.clone(), filename: fname.clone(), size: total, done: 0,
        speed_bps: 0, direction: "up".into(), peer: peer_ip.to_string(),
        status: "active".into(), progress: 0.0, error: String::new(),
    }).await;

    let mut f = match File::open(&real_path).await {
        Ok(f) => f,
        Err(e) => {
            let msg = format!("can't read file: {e}");
            set_transfer_error(app, &id, &msg).await;
            if let Some(z) = tmp_zip { let _ = std::fs::remove_file(z); }
            return Err(msg.into());
        }
    };
    let mut buf = vec![0u8; CHUNK];
    let mut done: u64 = 0;
    let start = Instant::now();
    let mut last_emit = Instant::now();
    loop {
        if is_cancelled(app, &id).await {
            set_transfer_status(app, &id, "cancelled").await;
            let _ = app.emit("safedrop-cancelled", serde_json::json!({
                "filename": fname, "peer": peer_ip, "direction": "up",
            }));
            if let Some(z) = tmp_zip { let _ = std::fs::remove_file(z); }
            return Err("cancelled by user".into());
        }
        let n = match f.read(&mut buf).await {
            Ok(n) => n,
            Err(e) => {
                let msg = format!("can't read file: {e}");
                set_transfer_error(app, &id, &msg).await;
                if let Some(z) = tmp_zip { let _ = std::fs::remove_file(z); }
                return Err(msg.into());
            }
        };
        if n == 0 { break; }
        let write_result = tokio::time::timeout(std::time::Duration::from_secs(30), writer.write_all(&buf[..n])).await;
        if let Err(e) = match write_result {
            Ok(result) => result,
            Err(_) => Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "получатель не принимает данные 30 секунд")),
        } {
            let msg = format!("send failed: {e}");
            set_transfer_error(app, &id, &msg).await;
            if let Some(z) = tmp_zip { let _ = std::fs::remove_file(z); }
            return Err(msg.into());
        }
        done += n as u64;
        if last_emit.elapsed().as_millis() > 200 || done == total {
            let el = start.elapsed().as_secs_f64().max(0.01);
            update_transfer(app, &id, done, (done as f64 / el) as u64, total).await;
            last_emit = Instant::now();
        }
    }
    if let Err(e) = writer.flush().await {
        let msg = format!("send failed: {e}");
        set_transfer_error(app, &id, &msg).await;
        if let Some(z) = tmp_zip { let _ = std::fs::remove_file(z); }
        return Err(msg.into());
    }
    finish_transfer(app, &id).await;
    if let Some(z) = tmp_zip { let _ = std::fs::remove_file(z); }
    Ok(id)
}

pub(crate) fn sanitize(name: &str) -> String {
    let base = Path::new(name).file_name().and_then(|s| s.to_str()).unwrap_or("file");
    base.chars().map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c }).collect()
}

fn unique_path(p: std::path::PathBuf) -> std::path::PathBuf {
    if !p.exists() { return p; }
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("file").to_string();
    let ext = p.extension().and_then(|s| s.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
    let parent = p.parent().map(|x| x.to_path_buf()).unwrap_or(std::path::PathBuf::from("."));
    for i in 1..1000 {
        let cand = parent.join(format!("{stem} ({i}){ext}"));
        if !cand.exists() { return cand; }
    }
    p
}

fn zip_dir(src: &Path, dst: &Path) -> anyhow::Result<()> {
    // minimal recursive zip via `zip` crate-less approach: use powershell Compress-Archive on Windows
    #[cfg(target_os = "windows")]
    {
        let st = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command",
                &format!("Compress-Archive -Path '{}/*' -DestinationPath '{}' -Force",
                    src.display(), dst.display())])
            .status()?;
        if st.success() { return Ok(()); }
        anyhow::bail!("Compress-Archive failed");
    }
    #[cfg(not(target_os = "windows"))]
    {
        anyhow::bail!("directory send supported on Windows v1 (zip it manually)");
    }
}

pub(crate) async fn push_transfer(app: &AppHandle, t: TransferInfo) {
    let state: tauri::State<'_, crate::state::AppState> = app.state();
    {
        let mut g = state.transfers.lock().await;
        g.push(t.clone());
        if g.len() > 200 { g.remove(0); }
    }
    let _ = app.emit("safedrop-transfers", snapshot(app).await);
}

pub(crate) async fn update_transfer(app: &AppHandle, id: &str, done: u64, speed: u64, total: u64) {
    let state: tauri::State<'_, crate::state::AppState> = app.state();
    {
        let mut g = state.transfers.lock().await;
        if let Some(t) = g.iter_mut().find(|x| x.id == id) {
            t.done = done; t.speed_bps = speed;
            t.progress = if total > 0 { done as f32 / total as f32 } else { 0.0 };
        }
    }
    let _ = app.emit("safedrop-transfers", snapshot(app).await);
}

pub(crate) async fn set_transfer_status(app: &AppHandle, id: &str, status: &str) {
    let state: tauri::State<'_, crate::state::AppState> = app.state();
    {
        let mut g = state.transfers.lock().await;
        if let Some(t) = g.iter_mut().find(|x| x.id == id) {
            t.status = status.into();
            if status == "done" {
                t.progress = 1.0;
            }
        }
        // persist history
        let hist_path = state.history_path();
        let _ = std::fs::create_dir_all(&state.app_dir);
        let _ = std::fs::write(&hist_path, serde_json::to_string_pretty(&*g).unwrap_or_default());
    }
    let _ = app.emit("safedrop-transfers", snapshot(app).await);
}

pub(crate) async fn finish_transfer(app: &AppHandle, id: &str) {
    set_transfer_status(app, id, "done").await;
}

/// Mark transfer as failed with a visible reason (never leaves it "active").
pub(crate) async fn set_transfer_error(app: &AppHandle, id: &str, msg: &str) {
    let state: tauri::State<'_, crate::state::AppState> = app.state();
    {
        let mut g = state.transfers.lock().await;
        if let Some(t) = g.iter_mut().find(|x| x.id == id) {
            t.status = "error".into();
            t.error = msg.chars().take(160).collect();
        }
        let hist_path = state.history_path();
        let _ = std::fs::create_dir_all(&state.app_dir);
        let _ = std::fs::write(&hist_path, serde_json::to_string_pretty(&*g).unwrap_or_default());
    }
    let _ = app.emit("safedrop-transfers", snapshot(app).await);
}

async fn snapshot(app: &AppHandle) -> Vec<TransferInfo> {
    let state: tauri::State<'_, crate::state::AppState> = app.state();
    let x = state.transfers.lock().await.clone();
    x
}

// tiny hex helper to avoid extra dep
mod hex {
    pub fn encode(d: impl AsRef<[u8]>) -> String {
        d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod acceptance_tests {
    use super::should_auto_accept;

    #[test]
    fn auto_accept_requires_all_three_conditions() {
        let one_mb = 1024 * 1024;
        assert!(should_auto_accept(true, true, one_mb, 1));
        assert!(!should_auto_accept(false, true, one_mb, 1));
        assert!(!should_auto_accept(true, false, one_mb, 1));
        assert!(!should_auto_accept(true, true, one_mb + 1, 1));
        assert!(!should_auto_accept(true, true, 1, 0));
    }
}

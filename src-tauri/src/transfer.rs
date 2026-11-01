use crate::state::TransferInfo;
use sha2::{Digest, Sha256};
use std::{path::{Path, PathBuf}, time::Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt, BufReader},
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
    /// Докачка (с 0.3.26): получатель хранит недокачанную часть под этим
    /// ключом файла и отвечает, с какого байта продолжить.
    #[serde(default)]
    pub(crate) resume: bool,
    #[serde(default)]
    pub(crate) fkey: String,
}

/// С какой версии получатель понимает докачку (и всегда отвечает на заголовок).
const RESUME_SINCE: (u32, u32, u32) = (0, 3, 26);
const RESUME_ATTEMPTS: u32 = 6;

fn parse_ver(v: &str) -> Option<(u32, u32, u32)> {
    let mut it = v.trim().split('.').map(|p| p.parse::<u32>().ok());
    Some((it.next()??, it.next()??, it.next().flatten().unwrap_or(0)))
}

async fn peer_supports_resume(app: &AppHandle, peer_ip: &str) -> bool {
    let state: tauri::State<'_, crate::state::AppState> = app.state();
    let ver = state.peers.lock().await.get(peer_ip).map(|p| p.version.clone()).unwrap_or_default();
    parse_ver(&ver).map(|v| v >= RESUME_SINCE).unwrap_or(false)
}

fn valid_fkey(k: &str) -> bool {
    (16..=64).contains(&k.len()) && k.chars().all(|c| c.is_ascii_hexdigit())
}

/// Недокачанные части лежат рядом с папкой приёма (тот же диск — быстрый rename).
fn partial_dir(drop_dir: &str) -> PathBuf {
    Path::new(drop_dir).join(".safedrop-partial")
}

fn partial_path(drop_dir: &str, peer_ip: &str, fkey: &str) -> PathBuf {
    let who: String = peer_ip.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    partial_dir(drop_dir).join(format!("{who}_{fkey}.part"))
}

/// Сколько уже скачано; часть больше файла (другой файл с тем же ключом) удаляется.
async fn partial_len(path: &Path, total: u64) -> u64 {
    match tokio::fs::metadata(path).await {
        Ok(m) if m.len() <= total => m.len(),
        Ok(_) => { let _ = tokio::fs::remove_file(path).await; 0 }
        Err(_) => 0,
    }
}

/// Старые недокачанные части (неделя без продолжения) удаляем.
async fn clean_old_partials(drop_dir: &str) {
    let Ok(mut rd) = tokio::fs::read_dir(partial_dir(drop_dir)).await else { return };
    while let Ok(Some(e)) = rd.next_entry().await {
        let old = e.metadata().await.ok().and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_secs() > 7 * 24 * 3600)
            .unwrap_or(false);
        if old { let _ = tokio::fs::remove_file(e.path()).await; }
    }
}

/// Ключ файла: один и тот же файл (путь, размер, время изменения) даёт тот же
/// ключ при повторной отправке — по нему получатель находит недокачанную часть.
fn file_key(prep: &UploadPrep) -> String {
    let mtime = std::fs::metadata(&prep.real_path).ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = Sha256::new();
    h.update(prep.real_path.to_string_lossy().as_bytes());
    h.update(prep.total.to_le_bytes());
    h.update(mtime.to_le_bytes());
    hex::encode(h.finalize())[..32].to_string()
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
    let resume = if offer.resume && offer.kind != "update" && valid_fkey(&offer.fkey) {
        clean_old_partials(&drop_dir).await;
        let p = partial_path(&drop_dir, &peer_ip, &offer.fkey);
        let have = partial_len(&p, offer.size).await;
        Some((p, have))
    } else {
        None
    };
    // Продолжение уже принятого файла от того же отправителя второй раз не спрашиваем.
    let accepted = match &resume {
        Some((_, have)) if *have > 0 => true,
        _ => authorize_offer(&app, &offer, &peer_ip).await,
    };
    if offer.confirm || resume.is_some() {
        let offset = resume.as_ref().map(|(_, h)| *h).unwrap_or(0);
        let reply = serde_json::json!({"kind":"file-offer-result","accepted":accepted,"offset":offset});
        write_half.write_all(format!("{}\n", reply).as_bytes()).await?;
        write_half.flush().await?;
    }
    if !accepted { return Ok(()); }
    receive_stream(app, offer, &mut reader, peer_ip, drop_dir, resume).await
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
    resume: Option<(PathBuf, u64)>,
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
    let resumable = resume.is_some();
    let (part_path, offset) = match resume {
        Some((p, have)) => {
            if let Some(dir) = p.parent() { let _ = tokio::fs::create_dir_all(dir).await; }
            (p, have)
        }
        None => (final_path.with_extension("safedrop-part"), 0),
    };
    // при обрыве недокачанное оставляем для докачки, иначе — удаляем
    let drop_part = |p: PathBuf| async move { if !resumable { let _ = tokio::fs::remove_file(p).await; } };

    let id = offer.id.clone();
    push_transfer(&app, TransferInfo {
        id: id.clone(), filename: safe_name.clone(), size: total, done: offset,
        speed_bps: 0, direction: "down".into(), peer: peer_ip.clone(),
        status: "active".into(), progress: if total > 0 { offset as f32 / total as f32 } else { 0.0 },
        error: String::new(),
    }).await;

    let mut hasher = Sha256::new();
    let opened = if offset > 0 {
        // хэш всего файла: сначала уже скачанная часть
        match File::open(&part_path).await {
            Ok(mut f) => {
                let mut b = vec![0u8; CHUNK];
                let mut left = offset;
                while left > 0 {
                    let n = f.read(&mut b[..(left.min(CHUNK as u64) as usize)]).await.unwrap_or(0);
                    if n == 0 { break; }
                    hasher.update(&b[..n]);
                    left -= n as u64;
                }
            }
            Err(_) => {}
        }
        tokio::fs::OpenOptions::new().append(true).open(&part_path).await
    } else {
        File::create(&part_path).await
    };
    let mut out = match opened {
        Ok(f) => f,
        Err(e) => {
            let msg = format!("can't write to disk: {e}");
            set_transfer_error(&app, &id, &msg).await;
            return Err(anyhow::anyhow!(msg));
        }
    };
    let mut done: u64 = offset;
    let mut user_cancel = false;
    let mut buf = vec![0u8; CHUNK];
    let start = Instant::now();
    let mut last_emit = Instant::now();
    // reader borrows sock mutably; read remainder manually.
    // NB: every failure below must mark the transfer — otherwise it hangs
    // in "active" forever with a stuck progress bar.
    loop {
        if is_cancelled(&app, &id).await {
            user_cancel = true;
            break;
        }
        let n = match reader.read(&mut buf).await {
            Ok(n) => n,
            Err(e) => {
                let msg = if resumable {
                    format!("связь оборвалась ({e}) — недокачанное сохранено, передача продолжится")
                } else {
                    format!("connection lost: {e}")
                };
                let _ = out.flush().await;
                drop(out);
                drop_part(part_path.clone()).await;
                set_transfer_error(&app, &id, &msg).await;
                return Err(anyhow::anyhow!(msg));
            }
        };
        if n == 0 { break; }
        if let Err(e) = out.write_all(&buf[..n]).await {
            let msg = format!("can't write to disk: {e}");
            drop_part(part_path.clone()).await;
            set_transfer_error(&app, &id, &msg).await;
            return Err(anyhow::anyhow!(msg));
        }
        hasher.update(&buf[..n]);
        done += n as u64;
        if last_emit.elapsed().as_millis() > 200 || done == total {
            let el = start.elapsed().as_secs_f64().max(0.01);
            let speed = ((done - offset) as f64 / el) as u64;
            update_transfer(&app, &id, done, speed, total).await;
            last_emit = Instant::now();
        }
        if done >= total { break; }
    }
    if let Err(e) = out.flush().await {
        let msg = format!("can't flush file: {e}");
        drop_part(part_path.clone()).await;
        set_transfer_error(&app, &id, &msg).await;
        return Err(anyhow::anyhow!(msg));
    }
    drop(out);
    if done < total && resumable && !user_cancel {
        // отправитель пропал без ошибки сокета — часть остаётся для докачки
        set_transfer_error(&app, &id, "связь оборвалась — недокачанное сохранено, передача продолжится").await;
        return Ok(());
    }
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
    let resume = kind == "file" && peer_supports_resume(&app, &peer_ip).await;
    let fkey = if resume { file_key(&prep) } else { String::new() };
    let mut attempt = 0u32;
    let result = loop {
        attempt += 1;
        let r = send_attempt(&app, &peer_ip, port, &prep, &device_name, &kind, require_confirmation, &version, resume, &fkey, attempt > 1).await;
        match r {
            Ok(id) => break Ok(id),
            Err(SendErr::Retry(msg)) if resume && attempt < RESUME_ATTEMPTS => {
                let delay = [3u64, 5, 10, 20, 30][(attempt as usize - 1).min(4)];
                set_transfer_error(&app, &prep.id, &format!("{msg} — повтор {attempt}/{} через {delay} с", RESUME_ATTEMPTS - 1)).await;
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                if is_cancelled(&app, &prep.id).await {
                    set_transfer_status(&app, &prep.id, "cancelled").await;
                    break Err("cancelled by user".into());
                }
            }
            Err(SendErr::Retry(msg)) | Err(SendErr::Fatal(msg)) => break Err(msg),
        }
    };
    if let Some(z) = &prep.tmp_zip { let _ = std::fs::remove_file(z); }
    result
}

/// Ошибка попытки отправки: Retry — связь, имеет смысл повторить; Fatal — нет.
pub(crate) enum SendErr {
    Retry(String),
    Fatal(String),
}

#[allow(clippy::too_many_arguments)]
async fn send_attempt(
    app: &AppHandle,
    peer_ip: &str,
    port: u16,
    prep: &UploadPrep,
    device_name: &str,
    kind: &str,
    require_confirmation: bool,
    version: &str,
    resume: bool,
    fkey: &str,
    retrying: bool,
) -> Result<String, SendErr> {
    // на первой попытке «не подключилось» — сразу ошибка; при повторе — ждём дальше
    let conn_err = |m: String| if retrying { SendErr::Retry(m) } else { SendErr::Fatal(m) };
    let mut sock = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::net::TcpStream::connect(format!("{peer_ip}:{port}")),
    )
    .await
    .map_err(|_| conn_err(format!("тайм-аут подключения к {peer_ip}:{port}")))?
    .map_err(|e| conn_err(format!("connect {peer_ip}:{port}: {e}")))?;
    let _ = sock.set_nodelay(true);
    let mut offer = serde_json::json!({"name": prep.fname, "size": prep.total, "id": prep.id, "from": device_name, "kind": kind, "confirm": require_confirmation, "version": version});
    if resume {
        offer["resume"] = serde_json::json!(true);
        offer["fkey"] = serde_json::json!(fkey);
    }
    let mut header = serde_json::to_string(&offer).map_err(|e| SendErr::Fatal(e.to_string()))?;
    header.push('\n');
    tokio::time::timeout(std::time::Duration::from_secs(10), sock.write_all(header.as_bytes()))
        .await.map_err(|_| conn_err("тайм-аут отправки заголовка файла".to_string()))?
        .map_err(|e| conn_err(e.to_string()))?;
    let mut offset = 0u64;
    if require_confirmation || resume {
        let result = {
            let mut reader = BufReader::new(&mut sock);
            let mut line = String::new();
            tokio::time::timeout(std::time::Duration::from_secs(130), reader.read_line(&mut line))
                .await
                .map_err(|_| SendErr::Fatal("Тайм-аут ожидания подтверждения приёма файла".to_string()))?
                .map_err(|e| conn_err(format!("Не удалось получить ответ получателя: {e}")))?;
            serde_json::from_str::<serde_json::Value>(line.trim()).map_err(|e| SendErr::Fatal(format!("Некорректный ответ получателя: {e}")))?
        };
        if result.get("accepted").and_then(|v| v.as_bool()) != Some(true) {
            return Err(SendErr::Fatal("Получатель отклонил приём файла".into()));
        }
        if resume {
            offset = result.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);
            if offset > prep.total { offset = 0; }
        }
    }
    pump_upload(app, prep, offset, peer_ip, &mut sock).await
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
    prep: &UploadPrep,
    offset: u64,
    peer_ip: &str,
    writer: &mut (impl AsyncWrite + Unpin),
) -> Result<String, SendErr> {
    let UploadPrep { real_path, total, fname, id, .. } = prep;
    let (total, id) = (*total, id.clone());
    push_transfer(app, TransferInfo {
        id: id.clone(), filename: fname.clone(), size: total, done: offset,
        speed_bps: 0, direction: "up".into(), peer: peer_ip.to_string(),
        status: "active".into(), progress: if total > 0 { offset as f32 / total as f32 } else { 0.0 },
        error: String::new(),
    }).await;

    let mut f = match File::open(real_path).await {
        Ok(f) => f,
        Err(e) => {
            let msg = format!("can't read file: {e}");
            set_transfer_error(app, &id, &msg).await;
            return Err(SendErr::Fatal(msg));
        }
    };
    if offset > 0 {
        if let Err(e) = f.seek(std::io::SeekFrom::Start(offset)).await {
            let msg = format!("can't read file: {e}");
            set_transfer_error(app, &id, &msg).await;
            return Err(SendErr::Fatal(msg));
        }
    }
    let mut buf = vec![0u8; CHUNK];
    let mut done: u64 = offset;
    let start = Instant::now();
    let mut last_emit = Instant::now();
    loop {
        if is_cancelled(app, &id).await {
            set_transfer_status(app, &id, "cancelled").await;
            let _ = app.emit("safedrop-cancelled", serde_json::json!({
                "filename": fname, "peer": peer_ip, "direction": "up",
            }));
            return Err(SendErr::Fatal("cancelled by user".into()));
        }
        let n = match f.read(&mut buf).await {
            Ok(n) => n,
            Err(e) => {
                let msg = format!("can't read file: {e}");
                set_transfer_error(app, &id, &msg).await;
                return Err(SendErr::Fatal(msg));
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
            return Err(SendErr::Retry(msg));
        }
        done += n as u64;
        if last_emit.elapsed().as_millis() > 200 || done == total {
            let el = start.elapsed().as_secs_f64().max(0.01);
            update_transfer(app, &id, done, ((done - offset) as f64 / el) as u64, total).await;
            last_emit = Instant::now();
        }
    }
    if let Err(e) = writer.flush().await {
        let msg = format!("send failed: {e}");
        set_transfer_error(app, &id, &msg).await;
        return Err(SendErr::Retry(msg));
    }
    finish_transfer(app, &id).await;
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
        if let Some(old) = g.iter_mut().find(|x| x.id == t.id) {
            *old = t.clone(); // повтор той же передачи — та же строка
        } else {
            g.push(t.clone());
            if g.len() > 200 { g.remove(0); }
        }
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
mod resume_tests {
    use super::{parse_ver, partial_path, valid_fkey, RESUME_SINCE};

    #[test]
    fn versions_gate_resume() {
        assert!(parse_ver("0.3.26").unwrap() >= RESUME_SINCE);
        assert!(parse_ver("0.4").unwrap() >= RESUME_SINCE);
        assert!(parse_ver("0.3.25").unwrap() < RESUME_SINCE);
        assert!(parse_ver("").is_none());
        assert!(parse_ver("abc").is_none());
    }

    #[test]
    fn fkey_must_be_hex_and_sized() {
        assert!(valid_fkey("0123456789abcdef0123456789abcdef"));
        assert!(!valid_fkey("../../evil"));
        assert!(!valid_fkey("short"));
        assert!(!valid_fkey(&"a".repeat(65)));
    }

    #[test]
    fn partial_path_stays_inside_partial_dir() {
        let p = partial_path("C:/LoadDrop","26.1.2.3", "0123456789abcdef0123456789abcdef");
        let s = p.to_string_lossy();
        assert!(s.contains(".safedrop-partial"));
        assert!(s.ends_with("26-1-2-3_0123456789abcdef0123456789abcdef.part"));
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

//! Обновления с GitHub. Раз в минуту (галочка «Искать обновления») приложение
//! смотрит, куда ведёт `github.com/<репо>/releases/latest`: GitHub отвечает
//! переадресацией на `/releases/tag/v<версия>`, и это не тратит лимит GitHub API.
//! Установщик качается по постоянной ссылке `/releases/download/v<версия>/
//! SafeDrop_<версия>_x64-setup.exe` и ставится, только если встроенная подпись
//! SafeDrop верна (`updsig`) и версия в подписи совпадает с ожидаемой —
//! подменённый файл или чужой ключ не пройдут, как и при обновлении по P2P.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tauri::Emitter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const REPO: &str = "SANYA3212/safedrop";
/// Установщик ~10 МБ; всё намного больше — не наш файл.
const MAX_DOWNLOAD: usize = 200 * 1024 * 1024;
const MAX_HEAD: usize = 64 * 1024;

fn tls() -> Result<Arc<rustls::ClientConfig>, String> {
    static CFG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    if let Some(c) = CFG.get() {
        return Ok(c.clone());
    }
    use rustls_platform_verifier::BuilderVerifierExt;
    // сертификаты сайта проверяет хранилище Windows, как в браузере
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_platform_verifier()
        .map_err(|e| e.to_string())?
        .with_no_client_auth();
    Ok(CFG.get_or_init(|| Arc::new(cfg)).clone())
}

struct Resp {
    status: u16,
    location: Option<String>,
    body: Vec<u8>,
}

/// Одна загрузка по HTTPS (HTTP/1.1, без переадресаций). Переадресация
/// возвращается как есть: тело не читается.
async fn get(url: &tauri::Url, limit: usize, mut progress: impl FnMut(usize, Option<usize>)) -> Result<Resp, String> {
    if url.scheme() != "https" {
        return Err("обновления — только по https".into());
    }
    let host = url.host_str().ok_or("нет адреса сервера")?.to_string();
    let port = url.port().unwrap_or(443);
    let mut path = url.path().to_string();
    if let Some(q) = url.query() {
        path.push('?');
        path.push_str(q);
    }
    let step = Duration::from_secs(30);
    let tcp = tokio::time::timeout(step, tokio::net::TcpStream::connect((host.as_str(), port)))
        .await
        .map_err(|_| "GitHub не отвечает".to_string())?
        .map_err(|e| format!("нет связи с GitHub: {e}"))?;
    let name = rustls::pki_types::ServerName::try_from(host.clone()).map_err(|e| e.to_string())?;
    let mut s = tokio::time::timeout(step, tokio_rustls::TlsConnector::from(tls()?).connect(name, tcp))
        .await
        .map_err(|_| "GitHub не отвечает".to_string())?
        .map_err(|e| format!("защищённое соединение с {host} не установлено: {e}"))?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: SafeDrop/{}\r\nAccept: */*\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n",
        crate::app_version()
    );
    s.write_all(req.as_bytes()).await.map_err(|e| e.to_string())?;

    // заголовки
    let mut buf = Vec::with_capacity(16 * 1024);
    let mut chunk = vec![0u8; 64 * 1024];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > MAX_HEAD {
            return Err("слишком длинный ответ сервера".into());
        }
        let n = tokio::time::timeout(step, s.read(&mut chunk)).await.map_err(|_| "GitHub не отвечает".to_string())?.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("сервер оборвал ответ".into());
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .ok_or("непонятный ответ сервера")?;
    let mut length: Option<usize> = None;
    let mut chunked = false;
    let mut location = None;
    for l in lines {
        let Some((k, v)) = l.split_once(':') else { continue };
        let v = v.trim();
        match k.trim().to_ascii_lowercase().as_str() {
            "content-length" => length = v.parse().ok(),
            "transfer-encoding" => chunked = v.to_ascii_lowercase().contains("chunked"),
            "location" => location = Some(v.to_string()),
            _ => {}
        }
    }
    if (300..400).contains(&status) || status >= 400 {
        return Ok(Resp { status, location, body: Vec::new() });
    }
    if length.is_some_and(|n| n > limit) {
        return Err("файл обновления слишком большой".into());
    }

    // тело
    let mut body = buf[head_end..].to_vec();
    let want = if chunked { None } else { length };
    progress(body.len(), want);
    loop {
        if want.is_some_and(|n| body.len() >= n) {
            break;
        }
        let n = match tokio::time::timeout(step, s.read(&mut chunk)).await.map_err(|_| "загрузка остановилась".to_string())? {
            Ok(n) => n,
            // сервер закрыл соединение без close_notify: для ответа без длины это конец
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && want.is_none() => 0,
            Err(e) => return Err(e.to_string()),
        };
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
        if body.len() > limit + 64 * 1024 {
            return Err("файл обновления слишком большой".into());
        }
        progress(body.len(), want);
    }
    if let Some(n) = want {
        if body.len() < n {
            return Err("загрузка оборвалась".into());
        }
        body.truncate(n);
    }
    if chunked {
        body = dechunk(&body)?;
    }
    Ok(Resp { status, location, body })
}

/// Тело `Transfer-Encoding: chunked` → обычные байты.
fn dechunk(mut data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let eol = data.windows(2).position(|w| w == b"\r\n").ok_or("повреждённый ответ")?;
        let size_txt = std::str::from_utf8(&data[..eol]).map_err(|_| "повреждённый ответ")?;
        let size = usize::from_str_radix(size_txt.split(';').next().unwrap_or("").trim(), 16).map_err(|_| "повреждённый ответ")?;
        data = &data[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        if data.len() < size + 2 {
            return Err("загрузка оборвалась".into());
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

/// `…/releases/tag/v0.3.39` → `0.3.39`.
fn version_from_tag_url(location: &str) -> Option<String> {
    let tag = location.split("/releases/tag/").nth(1)?.split(['?', '#', '/']).next()?;
    let ver = tag.strip_prefix('v').unwrap_or(tag);
    let ok = ver.split('.').count() == 3 && ver.split('.').all(|p| !p.is_empty() && p.len() <= 5 && p.chars().all(|c| c.is_ascii_digit()));
    ok.then(|| ver.to_string())
}

/// Переадресация только на GitHub и его хранилище файлов, только https.
fn allowed_redirect(u: &tauri::Url) -> bool {
    u.scheme() == "https"
        && u.host_str().is_some_and(|h| h == "github.com" || h.ends_with(".githubusercontent.com"))
}

async fn latest_version() -> Result<String, String> {
    let url = tauri::Url::parse(&format!("https://github.com/{REPO}/releases/latest")).map_err(|e| e.to_string())?;
    let r = get(&url, MAX_HEAD, |_, _| {}).await?;
    if !(300..400).contains(&r.status) {
        return Err(format!("GitHub ответил {}", r.status));
    }
    r.location
        .as_deref()
        .and_then(version_from_tag_url)
        .ok_or_else(|| "на GitHub нет выпусков SafeDrop".to_string())
}

fn download_dir() -> PathBuf {
    std::env::temp_dir().join("safedrop-updates").join("github")
}

/// Убрать скачанные установщики (сброс настроек).
pub fn clear_downloads() {
    let _ = std::fs::remove_dir_all(download_dir());
}

/// Есть ли на GitHub версия новее этой: {current, latest, available}.
#[tauri::command]
pub async fn check_github_update() -> Result<serde_json::Value, String> {
    let current = crate::app_version();
    let latest = latest_version().await?;
    let available = crate::transfer::cmp_ver(&latest, &current) == std::cmp::Ordering::Greater;
    Ok(serde_json::json!({ "current": current, "latest": latest, "available": available }))
}

/// Скачать установщик версии `version` и проверить подпись. Возвращает путь
/// к файлу для `apply_update`. Ход загрузки — событие `safedrop-gh-update-progress`.
#[tauri::command]
pub async fn download_github_update(app: tauri::AppHandle, version: String) -> Result<String, String> {
    if cfg!(not(windows)) {
        return Err("Обновление с GitHub работает только в Windows".into());
    }
    let version = version.trim().to_string();
    if version_from_tag_url(&format!("/releases/tag/v{version}")).as_deref() != Some(version.as_str()) {
        return Err("неверный номер версии".into());
    }
    if crate::transfer::cmp_ver(&version, &crate::app_version()) != std::cmp::Ordering::Greater {
        return Err(format!("версия {version} не новее установленной"));
    }
    let mut last: Option<std::time::Instant> = None;
    let emit = |done: usize, total: Option<usize>| {
        let finished = total.is_some_and(|t| done >= t);
        if finished || last.is_none_or(|t| t.elapsed() >= Duration::from_millis(200)) {
            last = Some(std::time::Instant::now());
            let _ = app.emit("safedrop-gh-update-progress", serde_json::json!({ "done": done, "total": total }));
        }
    };
    let body = fetch_installer(&version, emit).await?;
    save_verified(&download_dir(), &version, &body).map(|p| p.to_string_lossy().to_string())
}

/// Установщик версии `version` с GitHub (с переадресациями только на GitHub).
async fn fetch_installer(version: &str, mut progress: impl FnMut(usize, Option<usize>)) -> Result<Vec<u8>, String> {
    let file = format!("SafeDrop_{version}_x64-setup.exe");
    let mut url = tauri::Url::parse(&format!("https://github.com/{REPO}/releases/download/v{version}/{file}")).map_err(|e| e.to_string())?;
    for _ in 0..6 {
        let r = tokio::time::timeout(Duration::from_secs(600), get(&url, MAX_DOWNLOAD, &mut progress))
            .await
            .map_err(|_| "загрузка заняла больше 10 минут".to_string())??;
        if (300..400).contains(&r.status) {
            let next = r.location.ok_or("GitHub не сказал, где файл")?;
            let next = url.join(&next).map_err(|e| e.to_string())?;
            if !allowed_redirect(&next) {
                return Err(format!("GitHub отправил на чужой адрес: {}", next.host_str().unwrap_or("?")));
            }
            url = next;
            continue;
        }
        if r.status != 200 {
            return Err(if r.status == 404 { format!("в выпуске v{version} нет {file}") } else { format!("GitHub ответил {}", r.status) });
        }
        return Ok(r.body);
    }
    Err("слишком много переадресаций".into())
}

/// Сохранить установщик в `dir` и проверить подпись той же функцией, что и
/// установщик, присланный по P2P. Версия в подписи должна быть `version`.
fn save_verified(dir: &std::path::Path, version: &str, body: &[u8]) -> Result<PathBuf, String> {
    let file = format!("SafeDrop_{version}_x64-setup.exe");
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    // прежние скачанные версии больше не нужны
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let path = dir.join(&file);
    let part = dir.join(format!("{file}.part"));
    std::fs::write(&part, body).map_err(|e| e.to_string())?;
    std::fs::rename(&part, &path).map_err(|e| e.to_string())?;
    match crate::updsig::verify_file(&path) {
        Ok(v) if v == version => Ok(path),
        Ok(v) => {
            let _ = std::fs::remove_file(&path);
            Err(format!("в подписи другая версия ({v}) — не ставлю"))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            Err(format!("установщик с GitHub не прошёл проверку подписи: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_url_version() {
        let v = |s: &str| version_from_tag_url(s);
        assert_eq!(v("https://github.com/SANYA3212/safedrop/releases/tag/v0.3.38").as_deref(), Some("0.3.38"));
        assert_eq!(v("https://github.com/x/y/releases/tag/0.4.1?x=1").as_deref(), Some("0.4.1"));
        assert_eq!(v("https://github.com/x/y/releases/tag/v0.3.33-untested"), None);
        assert_eq!(v("https://github.com/x/y/releases"), None);
        assert_eq!(v("https://github.com/x/y/releases/tag/v1.2"), None);
    }

    #[test]
    fn redirects_only_to_github() {
        let ok = |s: &str| allowed_redirect(&tauri::Url::parse(s).unwrap());
        assert!(ok("https://github.com/SANYA3212/safedrop/releases/download/v1/a.exe"));
        assert!(ok("https://release-assets.githubusercontent.com/x"));
        assert!(!ok("http://release-assets.githubusercontent.com/x"));
        assert!(!ok("https://githubusercontent.com.evil.example/x"));
        assert!(!ok("https://example.com/x"));
    }

    #[test]
    fn chunked_body() {
        assert_eq!(dechunk(b"4\r\nWiki\r\n5;x=1\r\npedia\r\n0\r\n\r\n").unwrap(), b"Wikipedia");
        assert!(dechunk(b"4\r\nWi").is_err());
        assert!(dechunk(b"zz\r\n").is_err());
    }

    /// Настоящий GitHub: последняя версия, её установщик и подпись.
    /// `cargo test --release live_github -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_github() {
        let v = latest_version().await.expect("последняя версия");
        println!("последняя версия на GitHub: {v}");
        let mut seen = 0;
        let body = fetch_installer(&v, |d, _| seen = d).await.expect("установщик");
        println!("скачано {} байт (прогресс {seen})", body.len());
        let dir = std::env::temp_dir().join(format!("safedrop-ghupdate-test-{}", std::process::id()));
        let path = save_verified(&dir, &v, &body).expect("подпись");
        println!("подпись верна: {}", path.display());
        // испорченный файл не проходит и удаляется
        let mut bad = body.clone();
        bad[1000] ^= 1;
        assert!(save_verified(&dir, &v, &bad).is_err());
        assert!(!dir.join(format!("SafeDrop_{v}_x64-setup.exe")).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

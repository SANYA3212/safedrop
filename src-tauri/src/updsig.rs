//! Подпись обновлений. Установщик, присланный по P2P, ставится только с
//! подписью ключа SafeDrop (minisign, формат `cargo tauri signer sign`):
//! с 0.3.32 `build-release.bat` встраивает подпись в конец установщика
//! (раньше она лежала рядом в `<имя>.exe.sig` — такие файлы тоже принимаются).
//! По сети установщик едет без хвоста, а подпись — в `Offer.sig`: так его
//! принимают и 0.3.31, которые проверяют `.sig` по всему файлу.
//! Получатель проверяет подпись открытым ключом ниже.
//! Закрытый ключ лежит только у автора: `%USERPROFILE%\.tauri\safedrop-update.key`.
//!
//! Версия берётся из подписанного комментария (`file:SafeDrop_<версия>_x64-setup.exe`),
//! а не из заголовка передачи: его отправитель может написать любым.

use std::path::Path;

const PUBKEY: &str = "RWSUKg3m9UGJx/A2qrpAQKzJViwx98eH1mOS7uMmcr3nZMJ5OKiqGwqT";

/// Подпись лежит рядом с файлом: `<файл>.sig`.
pub fn sig_path(file: &Path) -> std::path::PathBuf {
    let mut p = file.as_os_str().to_owned();
    p.push(".sig");
    p.into()
}

/// Проверить данные и подпись (base64 текста minisign, как в `.sig`).
/// Возвращает версию из подписанного имени установщика.
pub fn verify_bytes(data: &[u8], sig_b64: &str) -> Result<String, String> {
    use base64::Engine;
    let pk = minisign_verify::PublicKey::from_base64(PUBKEY).map_err(|e| e.to_string())?;
    let text = base64::engine::general_purpose::STANDARD
        .decode(sig_b64.trim())
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or("подпись повреждена")?;
    let sig = minisign_verify::Signature::decode(&text).map_err(|_| "подпись повреждена")?;
    pk.verify(data, &sig, false).map_err(|_| "подпись не совпадает: файл изменён или подписан чужим ключом")?;
    sig.trusted_comment()
        .split('\t')
        .find_map(|p| p.strip_prefix("file:"))
        .and_then(installer_version)
        .ok_or_else(|| "в подписи нет версии установщика".to_string())
}

/// Встроенная подпись (с 0.3.32): в конец установщика дописан хвост
/// `[подпись — base64 из .sig][u32 LE длина подписи][MAGIC]`. Подпись
/// покрывает установщик без хвоста; NSIS лишние байты в конце не читает
/// (CRC он считает по длине из своего заголовка), как и Authenticode-сертификат.
const MAGIC: &[u8; 8] = b"SDSIGv1\0";
const TAIL: usize = 4 + MAGIC.len();
/// .sig от `cargo tauri signer` ~420 байт; всё, что заметно больше, — не наше.
const MAX_SIG: usize = 4096;

/// Разобрать хвост: (длина установщика без хвоста, подпись).
/// None — хвоста нет (старый формат с `.sig` рядом). Err — хвост есть, но испорчен.
fn split_embedded(data: &[u8]) -> Option<Result<(usize, String), String>> {
    if data.len() < TAIL || &data[data.len() - MAGIC.len()..] != MAGIC {
        return None;
    }
    let at = data.len() - TAIL;
    let len = u32::from_le_bytes(data[at..at + 4].try_into().ok()?) as usize;
    if len == 0 || len > MAX_SIG || len > at {
        return Some(Err("встроенная подпись повреждена".into()));
    }
    let body = at - len;
    Some(
        std::str::from_utf8(&data[body..at])
            .map(|s| (body, s.to_string()))
            .map_err(|_| "встроенная подпись повреждена".to_string()),
    )
}

/// Дописать подпись в конец установщика. В сборке это делает PowerShell в
/// build-release.bat тем же форматом; здесь — для тестов.
#[cfg(test)]
fn embed(installer: &[u8], sig_b64: &str) -> Vec<u8> {
    let sig = sig_b64.trim().as_bytes();
    let mut out = installer.to_vec();
    out.extend_from_slice(sig);
    out.extend_from_slice(&(sig.len() as u32).to_le_bytes());
    out.extend_from_slice(MAGIC);
    out
}

/// Подписанный установщик в памяти: байты без хвоста и подпись к ним.
pub struct Signed {
    pub data: Vec<u8>,
    pub sig: String,
    pub version: String,
    /// подпись была встроена в файл (а не лежала рядом в `.sig`)
    pub embedded: bool,
}

/// Прочитать и проверить установщик: встроенная подпись, а если её нет —
/// `.sig` рядом. Если хвост есть, `.sig` уже не смотрим: файл с испорченным
/// хвостом отклоняется, а не проверяется «как-нибудь ещё».
pub fn load_signed(file: &Path) -> Result<Signed, String> {
    let mut data = std::fs::read(file).map_err(|e| e.to_string())?;
    match split_embedded(&data) {
        Some(parts) => {
            let (body, sig) = parts?;
            data.truncate(body);
            let version = verify_bytes(&data, &sig)?;
            Ok(Signed { data, sig, version, embedded: true })
        }
        None => {
            let sig = std::fs::read_to_string(sig_path(file)).map_err(|_| "у обновления нет подписи")?;
            let version = verify_bytes(&data, &sig)?;
            Ok(Signed { data, sig: sig.trim().to_string(), version, embedded: false })
        }
    }
}

/// Проверить установщик на диске (встроенная подпись или `.sig` рядом).
pub fn verify_file(file: &Path) -> Result<String, String> {
    load_signed(file).map(|s| s.version)
}

/// Быстрая проверка без чтения всего файла: есть ли у установщика подпись
/// (хвост с MAGIC в конце или `.sig` рядом). Саму подпись проверяет `load_signed`.
pub fn has_signature(file: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    if sig_path(file).is_file() {
        return true;
    }
    let Ok(mut f) = std::fs::File::open(file) else { return false };
    let mut tail = [0u8; 8];
    f.seek(SeekFrom::End(-(MAGIC.len() as i64))).is_ok() && f.read_exact(&mut tail).is_ok() && &tail == MAGIC
}

/// `SafeDrop_0.3.31_x64-setup.exe` → `0.3.31`.
fn installer_version(name: &str) -> Option<String> {
    let ver = name.strip_prefix("SafeDrop_")?.split('_').next()?;
    let ok = ver.split('.').count() == 3 && ver.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    ok.then(|| ver.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // подписано настоящим ключом: echo -n test-installer > SafeDrop_9.9.9_x64-setup.exe
    const SIG: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVTVUtnM205VUdKeDI5enhNdzBSRHpTbjNhRmVWeFRRZExqQ25FVlhXd29qY0NYcGRiZ3k1MnhVa1AwRytsTm5IQzRVMFE1a05TOEx4dU5LREszQVo4UnZTTngzYlFvVGcwPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkzNzY4MjM3CWZpbGU6U2FmZURyb3BfOS45LjlfeDY0LXNldHVwLmV4ZQpsSlBSNlEySTB6WTRwVnM1QWtwZ1FEdGV6RWNCaGNtdVdSb3hDQk1mTnkvcFB6UHRNY01VYnNZcmZ6N202eG5CaE1ITjFOdllUQnA2Z1pMRFBZdllCUT09Cg==";

    #[test]
    fn signed_installer_passes_and_tampered_fails() {
        assert_eq!(verify_bytes(b"test-installer", SIG).unwrap(), "9.9.9");
        assert!(verify_bytes(b"test-installer!", SIG).is_err());
        assert!(verify_bytes(b"test-installer", "bm90IGEgc2lnbmF0dXJl").is_err());
        assert!(verify_bytes(b"test-installer", "").is_err());
    }

    /// Проверка собранного установщика той же функцией, что у получателя:
    /// `SAFEDROP_VERIFY=путь\к\SafeDrop_x_x64-setup.exe cargo test --release verify_release_file -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn verify_release_file() {
        let p = std::env::var("SAFEDROP_VERIFY").expect("SAFEDROP_VERIFY");
        let v = verify_file(Path::new(&p)).expect("подпись установщика");
        println!("подпись верна, версия {v}");
    }

    #[test]
    fn embedded_signature() {
        let dir = std::env::temp_dir().join(format!("safedrop-updsig-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("SafeDrop_9.9.9_x64-setup.exe");

        // встроенная подпись: проверяется по байтам без хвоста, .sig не нужен
        std::fs::write(&file, embed(b"test-installer", SIG)).unwrap();
        assert!(has_signature(&file));
        let s = load_signed(&file).unwrap();
        assert_eq!((s.data.as_slice(), s.version.as_str(), s.embedded), (&b"test-installer"[..], "9.9.9", true));
        assert_eq!(s.sig, SIG);

        // изменённый установщик с тем же хвостом
        std::fs::write(&file, embed(b"test-installer!", SIG)).unwrap();
        assert!(verify_file(&file).is_err());
        // испорченный хвост не проверяется через .sig рядом — отклоняется
        std::fs::write(sig_path(&file), SIG).unwrap();
        let mut bad = embed(b"test-installer", SIG);
        let n = bad.len();
        bad[n - TAIL..n - MAGIC.len()].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&file, &bad).unwrap();
        assert!(verify_file(&file).is_err());
        bad[n - TAIL..n - MAGIC.len()].copy_from_slice(&((n - TAIL + 1) as u32).to_le_bytes());
        std::fs::write(&file, &bad).unwrap();
        assert!(verify_file(&file).is_err());

        // старый формат: установщик без хвоста и .sig рядом
        std::fs::write(&file, b"test-installer").unwrap();
        let s = load_signed(&file).unwrap();
        assert_eq!((s.version.as_str(), s.embedded), ("9.9.9", false));
        std::fs::remove_file(sig_path(&file)).unwrap();
        assert!(!has_signature(&file));
        assert!(verify_file(&file).is_err());
        // файл короче хвоста
        std::fs::write(&file, b"x").unwrap();
        assert!(!has_signature(&file) && verify_file(&file).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_from_installer_name() {
        assert_eq!(installer_version("SafeDrop_0.3.31_x64-setup.exe").as_deref(), Some("0.3.31"));
        assert_eq!(installer_version("SafeDrop_0.3_x64-setup.exe"), None);
        assert_eq!(installer_version("Other_0.3.31_x64-setup.exe"), None);
    }
}

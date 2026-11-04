//! Подпись обновлений. Установщик, присланный по P2P, ставится только с
//! подписью ключа SafeDrop (minisign, формат `cargo tauri signer sign`):
//! `build-release.bat` кладёт рядом с установщиком `<имя>.exe.sig`, подпись
//! едет вместе с файлом, а получатель проверяет её открытым ключом ниже.
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

/// Проверить установщик на диске по его `.sig`.
pub fn verify_file(file: &Path) -> Result<String, String> {
    let sig = std::fs::read_to_string(sig_path(file)).map_err(|_| "у обновления нет подписи")?;
    let data = std::fs::read(file).map_err(|e| e.to_string())?;
    verify_bytes(&data, &sig)
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

    #[test]
    fn version_from_installer_name() {
        assert_eq!(installer_version("SafeDrop_0.3.31_x64-setup.exe").as_deref(), Some("0.3.31"));
        assert_eq!(installer_version("SafeDrop_0.3_x64-setup.exe"), None);
        assert_eq!(installer_version("Other_0.3.31_x64-setup.exe"), None);
    }
}

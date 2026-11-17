//! Вход в свой аккаунт с нового устройства по коду (как в Telegram: ПК и
//! телефон — один аккаунт).
//!
//! Аккаунт — это ключ устройства (его отпечаток и есть ID на сервере), поэтому
//! новое устройство получает тот же ключ с уже вошедшего:
//! 1. ПК показывает код `NNNN-SSSSSS`. Первые 4 цифры (`slot`) знает сервер —
//!    по ним он сводит устройства (`PairWait`/`PairJoin`); остальные 6 сервер
//!    не видит.
//! 2. Телефон вводит код, сервер открывает канал к ПК (один раз на код). Внутри
//!    канала — TLS, а поверх него SPAKE2 с кодом как паролем: общий секрет
//!    получится только у того, кто знает код, а подтверждения (HMAC) привязаны
//!    к ключам обоих концов TLS. Сервер, даже подменив оба TLS, получает одну
//!    попытку угадать 6 цифр — и на этом код сгорает.
//! 3. Только после подтверждений ПК отдаёт ключ, ник и аватар. Телефон
//!    проверяет, что ключ — от того самого ПК (отпечаток = ID), ставит его и
//!    переподключается к серверу уже своим аккаунтом.

use std::time::{Duration, Instant};

use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::relay::proto;
use crate::state::AppState;

/// Сколько действует код.
const CODE_TTL: Duration = Duration::from_secs(300);
/// Весь обмен ключом.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_LINE: usize = 1024 * 1024;
const ID_PHONE: &[u8] = b"safedrop-pair-new";
const ID_PC: &[u8] = b"safedrop-pair-host";

/// ПК ждёт новое устройство.
pub struct Waiting {
    slot: String,
    code: String,
    at: Instant,
}

type HmacSha256 = Hmac<Sha256>;

fn digits(n: usize) -> String {
    let mut out = String::new();
    while out.len() < n {
        for b in uuid::Uuid::new_v4().as_bytes() {
            // без перекоса: только байты < 250
            if *b < 250 && out.len() < n {
                out.push(char::from(b'0' + b % 10));
            }
        }
    }
    out
}

/// Код в виде, который сравнивают обе стороны: только цифры.
fn normalize(code: &str) -> Option<String> {
    let d: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
    let rest_ok = code.chars().all(|c| c.is_ascii_digit() || c == '-' || c == ' ');
    (d.len() == 10 && rest_ok).then_some(d)
}

fn pretty(code: &str) -> String {
    format!("{}-{}", &code[..4], &code[4..])
}

fn emit(app: &AppHandle, state: &str, msg: &str) {
    let _ = app.emit("safedrop-pair", serde_json::json!({"state": state, "msg": msg}));
}

// ---------- ПК: показать код ----------

/// Новый код для входа другого своего устройства.
pub fn start() -> Result<serde_json::Value, String> {
    if !crate::relay::multi_online() {
        return Err("Нужна связь с сервером SafeDrop новой версии (с поддержкой нескольких устройств)".into());
    }
    let code = format!("{}{}", digits(4), digits(6));
    let slot = code[..4].to_string();
    crate::relay::with_pair(|p| *p = Some(Waiting { slot: slot.clone(), code: code.clone(), at: Instant::now() }));
    crate::relay::ctl_send(&proto::ToServer::PairWait { slot })?;
    let conf = crate::relay::conf();
    Ok(serde_json::json!({"code": pretty(&code), "secs": CODE_TTL.as_secs(), "qr": qr_text(&conf.addr, &conf.pass, &pretty(&code))}))
}

/// Что зашито в QR-код для телефона: адрес и пароль сервера и сам код.
/// `safedrop://pair?v=1&s=<адрес>&p=<пароль>&c=<код>` (значения — в URL-кодировке).
pub fn qr_text(addr: &str, pass: &str, code: &str) -> String {
    fn enc(s: &str) -> String {
        s.bytes()
            .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
            .collect()
    }
    format!("safedrop://pair?v=1&s={}&p={}&c={}", enc(addr.trim()), enc(pass), enc(code))
}

pub fn cancel() {
    crate::relay::with_pair(|p| *p = None);
    let _ = crate::relay::ctl_send(&proto::ToServer::PairCancel);
}

pub fn waiting() -> bool {
    crate::relay::with_pair(|p| p.as_ref().is_some_and(|w| w.at.elapsed() < CODE_TTL))
}

/// Сервер не принял код (занят другим) — интерфейс попросит новый.
pub fn failed(app: &AppHandle, slot: &str, msg: &str) {
    let ours = crate::relay::with_pair(|p| {
        if p.as_ref().is_some_and(|w| w.slot == slot) {
            *p = None;
            true
        } else {
            false
        }
    });
    if ours {
        emit(app, "error", msg);
    }
}

/// Новое устройство ввело код: проверить его и отдать ключ.
pub async fn host(app: AppHandle, host: String, port: u16, from: String, tok: String) {
    // код — на одну попытку (сервер его уже забыл)
    let code = crate::relay::with_pair(|p| p.take().filter(|w| w.at.elapsed() < CODE_TTL).map(|w| w.code));
    let Some(code) = code else { return };
    emit(&app, "joining", "");
    let r = tokio::time::timeout(EXCHANGE_TIMEOUT, host_exchange(&app, &host, port, &from, &tok, &code)).await;
    match r {
        Ok(Ok(name)) => emit(&app, "done", &name),
        Ok(Err(e)) => emit(&app, "error", &e),
        Err(_) => emit(&app, "error", "новое устройство не ответило вовремя"),
    }
}

async fn host_exchange(app: &AppHandle, host: &str, port: u16, from: &str, tok: &str, code: &str) -> Result<String, String> {
    let sock = crate::relay::data_conn_pub(host, port, tok).await?;
    let (stream, auth) = crate::tls::accept_relayed(sock).await.map_err(|e| e.to_string())?;
    let their_fp = auth.fp.ok_or("новое устройство не показало ключ")?;
    if their_fp != from {
        return Err("ключ нового устройства не совпал с его ID".into());
    }
    let my_fp = crate::tls::my_fp();
    let mut s = BufReader::new(stream);
    let hello: serde_json::Value = read_json(&mut s).await?;
    let their_msg = hex_field(&hello, "msg")?;
    let name = proto::clean_name(hello.get("name").and_then(|x| x.as_str()).unwrap_or(""));
    let (spake, my_msg) = spake2::Spake2::<spake2::Ed25519Group>::start_b(
        &spake2::Password::new(code.as_bytes()),
        &spake2::Identity::new(ID_PHONE),
        &spake2::Identity::new(ID_PC),
    );
    let key = spake.finish(&their_msg).map_err(|_| "код не подошёл".to_string())?;
    let confirm = mac(&key, b"host", &my_fp, &their_fp);
    write_json(&mut s, &serde_json::json!({"t": "pake", "msg": hex(&my_msg), "confirm": hex(&confirm)})).await?;
    let reply: serde_json::Value = read_json(&mut s).await?;
    let their_mac = hex_field(&reply, "mac")?;
    if !verify(&key, b"new", &my_fp, &their_fp, &their_mac) {
        return Err("на новом устройстве введён неверный код".into());
    }
    // код подтверждён: отдать аккаунт
    let (cert, pkey) = crate::tls::my_key_der().ok_or("нет ключа устройства")?;
    let profile = app.state::<AppState>().settings.lock().await.profile.clone();
    let b64 = base64::engine::general_purpose::STANDARD;
    write_json(&mut s, &serde_json::json!({
        "t": "key",
        "cert": b64.encode(&cert),
        "key": b64.encode(&pkey),
        "nickname": profile.nickname,
        "avatar": profile.avatar,
    }))
    .await?;
    // дождаться, что ключ дошёл
    let _ = tokio::time::timeout(Duration::from_secs(10), read_json(&mut s)).await;
    Ok(if name.is_empty() { "новое устройство".into() } else { name })
}

// ---------- новое устройство: войти по коду ----------

/// Войти в аккаунт с другого своего устройства: адрес и пароль сервера, код с
/// экрана того устройства. После успеха это устройство — тот же аккаунт.
pub async fn join(app: &AppHandle, addr: &str, pass: &str, code: &str) -> Result<(), String> {
    let code = normalize(code).ok_or("Код — 10 цифр, как на экране ПК (например 1234-567890)")?;
    let slot = code[..4].to_string();
    let addr = addr.trim().to_string();
    let (host, port) = crate::relay::split_addr_pub(&addr)?;
    if pass.chars().count() > 256 {
        return Err("пароль слишком длинный".into());
    }
    tokio::time::timeout(EXCHANGE_TIMEOUT + Duration::from_secs(20), join_inner(app, &addr, &host, port, pass, &code, &slot))
        .await
        .map_err(|_| "ПК не ответил вовремя — проверьте, что окно с кодом открыто".to_string())?
}

async fn join_inner(app: &AppHandle, addr: &str, host: &str, port: u16, pass: &str, code: &str, slot: &str) -> Result<(), String> {
    let old = crate::relay::conf();
    // управляющее соединение с сервером: временный ключ этого устройства, нас не видно
    let sock = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect((host, port)))
        .await
        .map_err(|_| format!("сервер {host}:{port} не отвечает"))?
        .map_err(|e| format!("не удалось подключиться к {host}:{port}: {e}"))?;
    let _ = sock.set_nodelay(true);
    let cfg = crate::tls::client_config().ok_or("нет ключа устройства")?;
    let name = rustls::pki_types::ServerName::try_from("safedrop").map_err(|e| e.to_string())?;
    let tls = tokio::time::timeout(Duration::from_secs(10), tokio_rustls::TlsConnector::from(cfg.clone()).connect(name.clone(), sock))
        .await
        .map_err(|_| "сервер не завершил TLS-рукопожатие".to_string())?
        .map_err(|e| format!("TLS с сервером: {e}"))?;
    let server_fp = tls.get_ref().1.peer_certificates().and_then(|c| c.first()).map(|c| crate::tls::fp_of(c)).ok_or("сервер не показал ключ")?;
    // тот же сервер, что уже запомнен, — ключ обязан совпасть
    if old.addr.trim() == addr && !old.fp.is_empty() && old.fp != server_fp {
        return Err("Ключ сервера изменился. Если сервер переустанавливали — подтвердите новый ключ в настройках; иначе это может быть подмена".into());
    }
    let (r, mut w) = tokio::io::split(tls);
    let mut r = BufReader::new(r);
    let my_dev = crate::relay::my_dev();
    let device = app.state::<AppState>().settings.lock().await.device_name.clone();
    // своё временное «устройство»: с тем же dev сервер принял бы вход за
    // переподключение и закрыл бы основное соединение (и наоборот)
    let temp_dev = uuid::Uuid::new_v4().simple().to_string()[..16].to_string();
    let hello = proto::ToServer::Hello {
        proto: proto::PROTO,
        pass: pass.to_string(),
        name: proto::clean_name(&device),
        ver: crate::app_version(),
        dev: temp_dev,
        form: if cfg!(target_os = "android") { proto::FORM_PHONE.into() } else { proto::FORM_DESKTOP.into() },
        quiet: true,
        watch: false,
    };
    send_ctl(&mut w, &hello).await?;
    // ждём welcome (пароль верный и сервер умеет несколько устройств)
    loop {
        let l = read_line(&mut r, proto::MAX_CTL_LINE, Duration::from_secs(10)).await.map_err(|_| "сервер не ответил".to_string())?;
        match serde_json::from_slice::<proto::ToClient>(&l) {
            Ok(proto::ToClient::Welcome { multi, .. }) => {
                if !multi {
                    return Err("Сервер SafeDrop старой версии: обновите его, чтобы входить по коду".into());
                }
                break;
            }
            Ok(proto::ToClient::Deny { msg }) => return Err(format!("Сервер отказал: {}", proto::clean_name(&msg))),
            _ => {}
        }
    }
    let tok = uuid::Uuid::new_v4().simple().to_string();
    send_ctl(&mut w, &proto::ToServer::PairJoin { slot: slot.to_string(), tok: tok.clone() }).await?;
    // отказ сервера (код не найден) приходит по управляющему соединению
    let fail = async {
        loop {
            let l = read_line(&mut r, proto::MAX_CTL_LINE, Duration::from_secs(proto::PING_SECS * 3)).await?;
            if let Ok(proto::ToClient::Fail { tok: t, msg }) = serde_json::from_slice::<proto::ToClient>(&l) {
                if t == tok {
                    return Err::<(), String>(proto::clean_name(&msg));
                }
            }
        }
    };
    let exchange = async {
        let sock = crate::relay::data_conn_pub(host, port, &tok).await?;
        let tls = tokio::time::timeout(Duration::from_secs(25), tokio_rustls::TlsConnector::from(cfg).connect(name, sock))
            .await
            .map_err(|_| "ПК не ответил — проверьте, что окно с кодом на ПК открыто".to_string())?
            .map_err(|e| format!("TLS с ПК: {e}"))?;
        let pc_fp = tls.get_ref().1.peer_certificates().and_then(|c| c.first()).map(|c| crate::tls::fp_of(c)).ok_or("ПК не показал ключ")?;
        exchange_new(app, tls, &pc_fp, code, &device).await.map(|b| (b, pc_fp))
    };
    let (bundle, pc_fp) = tokio::select! {
        r = exchange => r?,
        r = fail => return Err(r.err().unwrap_or_else(|| "сервер прервал вход".into())),
    };
    let _ = w.shutdown().await;
    install(app, bundle, &pc_fp, addr, pass, &server_fp, &my_dev).await
}

struct Bundle {
    cert: Vec<u8>,
    key: Vec<u8>,
    nickname: String,
    avatar: String,
}

async fn exchange_new(
    _app: &AppHandle,
    tls: tokio_rustls::client::TlsStream<TcpStream>,
    pc_fp: &str,
    code: &str,
    device: &str,
) -> Result<Bundle, String> {
    let my_fp = crate::tls::my_fp();
    let mut s = BufReader::new(tls);
    let (spake, my_msg) = spake2::Spake2::<spake2::Ed25519Group>::start_a(
        &spake2::Password::new(code.as_bytes()),
        &spake2::Identity::new(ID_PHONE),
        &spake2::Identity::new(ID_PC),
    );
    write_json(&mut s, &serde_json::json!({"t": "pake", "msg": hex(&my_msg), "name": proto::clean_name(device)})).await?;
    let reply: serde_json::Value = read_json(&mut s).await.map_err(|_| "ПК прервал вход (код не подошёл?)".to_string())?;
    let their_msg = hex_field(&reply, "msg")?;
    let confirm = hex_field(&reply, "confirm")?;
    let key = spake.finish(&their_msg).map_err(|_| "код не подошёл".to_string())?;
    // ПК знает код и это тот же ПК, с которым у нас TLS
    if !verify(&key, b"host", pc_fp, &my_fp, &confirm) {
        return Err("Код не подошёл. Получите на ПК новый и введите его".into());
    }
    write_json(&mut s, &serde_json::json!({"t": "confirm", "mac": hex(&mac(&key, b"new", pc_fp, &my_fp))})).await?;
    let b: serde_json::Value = read_json(&mut s).await.map_err(|_| "ПК не прислал ключ".to_string())?;
    let b64 = base64::engine::general_purpose::STANDARD;
    let field = |k: &str| b.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let cert = b64.decode(field("cert")).map_err(|_| "ПК прислал неверный ключ".to_string())?;
    let key = b64.decode(field("key")).map_err(|_| "ПК прислал неверный ключ".to_string())?;
    let _ = write_json(&mut s, &serde_json::json!({"t": "ok"})).await;
    let _ = s.get_mut().shutdown().await;
    Ok(Bundle { cert, key, nickname: field("nickname"), avatar: field("avatar") })
}

/// Ключ подходит к сертификату, а сертификат — это аккаунт `pc_fp`.
fn check_bundle(cert: &[u8], key: &[u8], pc_fp: &str) -> Result<(), String> {
    if cert.is_empty() || key.is_empty() || cert.len() > 16 * 1024 || key.len() > 16 * 1024 {
        return Err("ПК прислал неверный ключ".into());
    }
    if crate::tls::fp_of(cert) != pc_fp {
        return Err("ПК прислал чужой ключ".into());
    }
    let provider = crate::quic::ring_provider();
    let ck = rustls::sign::CertifiedKey::from_der(
        vec![rustls::pki_types::CertificateDer::from(cert.to_vec())],
        rustls::pki_types::PrivateKeyDer::Pkcs8(key.to_vec().into()),
        &provider,
    )
    .map_err(|_| "ключ от ПК не читается".to_string())?;
    ck.keys_match().map_err(|_| "ключ от ПК не подходит к его сертификату".to_string())
}

async fn install(app: &AppHandle, b: Bundle, pc_fp: &str, addr: &str, pass: &str, server_fp: &str, dev: &str) -> Result<(), String> {
    check_bundle(&b.cert, &b.key, pc_fp)?;
    let state = app.state::<AppState>();
    let dir = state.app_dir.clone();
    crate::quic::install_cert(&dir, &b.cert, &b.key)?;
    let fp = crate::tls::reload(&dir)?;
    if fp != pc_fp {
        return Err("ключ не встал".into());
    }
    let _ = std::fs::write(dir.join(crate::relay::LINKED_MARK), b"1");
    // это другой аккаунт: прежние чаты этого устройства — не его. Очистка и
    // «что видели свои устройства» меняются вместе, под блокировкой чатов:
    // иначе очистка ушла бы на ПК надгробиями и стёрла бы там историю
    {
        let mut chats = state.chats.lock().await;
        chats.clear();
        crate::own::start_fresh(app);
    }
    state.groups.lock().await.clear();
    *state.group_sync.lock().await = crate::state::GroupSyncState::default();
    crate::settings::save_chats_now(&state);
    crate::signal::persist_groups(&state).await;
    let _ = std::fs::remove_file(state.group_sync_path());
    {
        let mut s = state.settings.lock().await;
        s.profile.nickname = b.nickname.trim().chars().take(32).collect();
        s.profile.avatar = if b.avatar.len() <= 300_000 && crate::signal::is_data_image(&b.avatar) { b.avatar } else { String::new() };
    }
    crate::settings::persist_settings(&state);
    crate::relay::set_conf(
        app,
        crate::relay::ServerConf {
            mode: crate::relay::MODE_SERVER.into(),
            addr: addr.to_string(),
            pass: pass.to_string(),
            fp: server_fp.to_string(),
            dev: dev.to_string(),
        },
    );
    Ok(())
}

// ---------- мелочи ----------

fn mac(key: &[u8], who: &[u8], host_fp: &str, new_fp: &str) -> Vec<u8> {
    let mut m = <HmacSha256 as Mac>::new_from_slice(key).expect("hmac key");
    m.update(b"safedrop-pair-v1|");
    m.update(who);
    m.update(b"|");
    m.update(host_fp.as_bytes());
    m.update(b"|");
    m.update(new_fp.as_bytes());
    m.finalize().into_bytes().to_vec()
}

fn verify(key: &[u8], who: &[u8], host_fp: &str, new_fp: &str, got: &[u8]) -> bool {
    let mut m = <HmacSha256 as Mac>::new_from_slice(key).expect("hmac key");
    m.update(b"safedrop-pair-v1|");
    m.update(who);
    m.update(b"|");
    m.update(host_fp.as_bytes());
    m.update(b"|");
    m.update(new_fp.as_bytes());
    m.verify_slice(got).is_ok()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || s.len() > 512 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn hex_field(v: &serde_json::Value, k: &str) -> Result<Vec<u8>, String> {
    v.get(k).and_then(|x| x.as_str()).and_then(unhex).ok_or_else(|| "неверный ответ при входе".to_string())
}

async fn read_line<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R, max: usize, timeout: Duration) -> Result<Vec<u8>, String> {
    let l = crate::quic::read_line_limited(r, max, timeout).await?;
    if l.is_empty() {
        return Err("соединение закрыто".into());
    }
    Ok(l)
}

async fn read_json<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R) -> Result<serde_json::Value, String> {
    let l = read_line(r, MAX_LINE, Duration::from_secs(30)).await?;
    serde_json::from_slice(&l).map_err(|e| e.to_string())
}

async fn write_json<W: AsyncWriteExt + Unpin>(w: &mut W, v: &serde_json::Value) -> Result<(), String> {
    let mut l = serde_json::to_vec(v).map_err(|e| e.to_string())?;
    l.push(b'\n');
    w.write_all(&l).await.map_err(|e| e.to_string())?;
    w.flush().await.map_err(|e| e.to_string())
}

async fn send_ctl<W: AsyncWriteExt + Unpin>(w: &mut W, m: &proto::ToServer) -> Result<(), String> {
    let mut l = serde_json::to_vec(m).map_err(|e| e.to_string())?;
    l.push(b'\n');
    w.write_all(&l).await.map_err(|e| e.to_string())?;
    w.flush().await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_format() {
        let c = digits(10);
        assert_eq!(c.len(), 10);
        assert!(c.bytes().all(|b| b.is_ascii_digit()));
        assert_eq!(normalize("1234-567890").as_deref(), Some("1234567890"));
        assert_eq!(normalize(" 1234 567 890 ").as_deref(), Some("1234567890"));
        assert!(normalize("1234-56789").is_none());
        assert!(normalize("1234-56789a").is_none());
        assert_eq!(pretty("1234567890"), "1234-567890");
        assert_eq!(qr_text("203.0.113.5:47840", "п&a b", "1234-567890"),
            "safedrop://pair?v=1&s=203.0.113.5%3A47840&p=%D0%BF%26a%20b&c=1234-567890");
    }

    /// SPAKE2 + подтверждения: с верным кодом обе стороны сходятся, с неверным
    /// подтверждение ПК не проходит (ключ не будет отправлен).
    #[test]
    fn pake_confirms_only_with_same_code() {
        let run = |code_a: &str, code_b: &str| -> (bool, bool) {
            let (a, ma) = spake2::Spake2::<spake2::Ed25519Group>::start_a(&spake2::Password::new(code_a.as_bytes()), &spake2::Identity::new(ID_PHONE), &spake2::Identity::new(ID_PC));
            let (b, mb) = spake2::Spake2::<spake2::Ed25519Group>::start_b(&spake2::Password::new(code_b.as_bytes()), &spake2::Identity::new(ID_PHONE), &spake2::Identity::new(ID_PC));
            let kb = b.finish(&ma).unwrap();
            let ka = a.finish(&mb).unwrap();
            let host_confirm = mac(&kb, b"host", "pc", "phone");
            let phone_ok = verify(&ka, b"host", "pc", "phone", &host_confirm);
            let phone_mac = mac(&ka, b"new", "pc", "phone");
            let host_ok = verify(&kb, b"new", "pc", "phone", &phone_mac);
            (phone_ok, host_ok)
        };
        assert_eq!(run("1234567890", "1234567890"), (true, true));
        assert_eq!(run("1234567890", "1234567891"), (false, false));
        // подтверждение привязано к ключам TLS: подмена конца не проходит
        let (a, ma) = spake2::Spake2::<spake2::Ed25519Group>::start_a(&spake2::Password::new(b"1"), &spake2::Identity::new(ID_PHONE), &spake2::Identity::new(ID_PC));
        let (b, mb) = spake2::Spake2::<spake2::Ed25519Group>::start_b(&spake2::Password::new(b"1"), &spake2::Identity::new(ID_PHONE), &spake2::Identity::new(ID_PC));
        let (ka, kb) = (a.finish(&mb).unwrap(), b.finish(&ma).unwrap());
        assert!(!verify(&ka, b"host", "mitm", "phone", &mac(&kb, b"host", "pc", "phone")));
        // своё же подтверждение не выдать за чужое
        assert!(!verify(&kb, b"new", "pc", "phone", &mac(&kb, b"host", "pc", "phone")));
    }

    #[test]
    fn bundle_must_match_account() {
        let ck = rcgen::generate_simple_self_signed(vec!["safedrop".into()]).unwrap();
        let cert = ck.cert.der().to_vec();
        let key = ck.key_pair.serialize_der();
        let fp = crate::tls::fp_of(&cert);
        assert!(check_bundle(&cert, &key, &fp).is_ok());
        assert!(check_bundle(&cert, &key, &"0".repeat(64)).is_err(), "чужой аккаунт");
        let other = rcgen::generate_simple_self_signed(vec!["safedrop".into()]).unwrap();
        assert!(check_bundle(&cert, &other.key_pair.serialize_der(), &fp).is_err(), "ключ не от этого сертификата");
        assert!(check_bundle(&[], &key, &fp).is_err());
        assert_eq!(unhex("0aff"), Some(vec![10, 255]));
        assert!(unhex("0g").is_none() && unhex("abc").is_none());
    }
}

//! Шифрование TCP-соединений (файлы 47831 и сигналы 47832), с 0.3.26.
//!
//! Сервер смотрит на первый байт: TLS-рукопожатие начинается с 0x16, старый
//! клиент шлёт JSON (`{`). Поэтому один порт принимает и новых, и старых.
//! Клиент шифрует, только если контакт сообщил версию 0.3.26+ (в hello); если
//! рукопожатие не удалось (контакт откатился на старую версию), соединение
//! повторяется без шифрования, а контакт перестаёт считаться «шифрующим».
//!
//! Ключ устройства — самоподписанный сертификат (тот же, что у QUIC). С 0.3.31
//! обе стороны предъявляют его (взаимный TLS), подпись рукопожатия проверяется,
//! а отпечаток сертификата запоминается при первом контакте (TOFU, `pins.json`).
//! Контакт с запомненным ключом:
//! - исходящее соединение — только TLS и только с тем же ключом, без отката;
//! - входящее — после того как он хоть раз предъявил свой сертификат
//!   (`mutual`), без сертификата или с другим ключом не принимается.
//! Смена ключа не принимается молча: интерфейс спрашивает пользователя.
//! Свой же ключ (адрес контакта ведёт на этот ПК, например 127.0.0.1 у
//! виртуальной машины за NAT) — не контакт: соединение отклоняется молча.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
/// Поток соединения: обычный TCP или TLS поверх него.
pub type Stream = Box<dyn Io>;

const TLS_SINCE: (u32, u32, u32) = (0, 3, 26);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_NAME: &str = "safedrop";

fn peers() -> &'static Mutex<HashSet<String>> {
    static P: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(HashSet::new()))
}

fn parse_ver(v: &str) -> Option<(u32, u32, u32)> {
    let mut it = v.trim().split('.').map(|p| p.parse::<u32>().ok());
    Some((it.next()??, it.next()??, it.next().flatten().unwrap_or(0)))
}

/// Запомнить версию контакта (из hello / peers.json): шифровать ли с ним.
pub fn note_peer_version(ip: &str, version: &str) {
    let ok = parse_ver(version).map(|v| v >= TLS_SINCE).unwrap_or(false);
    if let Ok(mut set) = peers().lock() {
        if ok { set.insert(ip.to_string()); } else { set.remove(ip); }
    }
}

pub fn peer_ok(ip: &str) -> bool {
    peers().lock().map(|s| s.contains(ip)).unwrap_or(false)
}

fn forget(ip: &str) {
    if let Ok(mut s) = peers().lock() { s.remove(ip); }
}

// ---------- ключ устройства и запомненные ключи контактов ----------

/// Отпечаток сертификата: SHA-256 его DER в hex. Сертификат создаётся один раз
/// и хранится в папке приложения, поэтому отпечаток — постоянный ID устройства.
pub fn fp_of(cert_der: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(cert_der).iter().map(|b| format!("{b:02x}")).collect()
}

struct Identity {
    cert: rustls::pki_types::CertificateDer<'static>,
    key: Vec<u8>,
    fp: String,
}

fn identity() -> &'static OnceLock<Identity> {
    static I: OnceLock<Identity> = OnceLock::new();
    &I
}

/// Свой сертификат и PKCS#8-ключ (для QUIC).
pub(crate) fn my_cert() -> Option<(rustls::pki_types::CertificateDer<'static>, rustls::pki_types::PrivateKeyDer<'static>)> {
    identity().get().map(|i| (i.cert.clone(), rustls::pki_types::PrivateKeyDer::Pkcs8(i.key.clone().into())))
}

pub fn my_fp() -> String {
    identity().get().map(|i| i.fp.clone()).unwrap_or_default()
}

/// Собеседник предъявил наш же ключ: мы подключились к самим себе.
fn is_me(fp: &str) -> bool {
    #[cfg(test)]
    if SELF_IS_PEER.with(|c| c.get()) { return false; }
    let me = my_fp();
    !me.is_empty() && fp == me
}

// тесту TLS нужен «собеседник», а второго ключа в процессе нет
#[cfg(test)]
thread_local! { static SELF_IS_PEER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

/// Убрать из запомненных свой же ключ (мог остаться от прежних версий:
/// «Доверять новому ключу» на адресе, ведущем на этот ПК). true — что-то убрано.
fn drop_own_key(map: &mut HashMap<String, Pin>, me: &str) -> bool {
    if me.is_empty() { return false; }
    let before = map.len();
    map.retain(|_, p| p.fp != me);
    let mut changed = map.len() != before;
    for p in map.values_mut().filter(|p| p.pending == me) {
        p.pending.clear();
        changed = true;
    }
    changed
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Pin {
    pub fp: String,
    /// контакт сам предъявлял этот ключ при входящем соединении (0.3.31+)
    #[serde(default)]
    pub mutual: bool,
    /// другой ключ, который пришёл вместо запомненного (ждёт решения пользователя)
    #[serde(default)]
    pub pending: String,
}

fn pins() -> &'static Mutex<HashMap<String, Pin>> {
    static P: OnceLock<Mutex<HashMap<String, Pin>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(HashMap::new()))
}

fn pins_path() -> &'static OnceLock<PathBuf> {
    static P: OnceLock<PathBuf> = OnceLock::new();
    &P
}

fn save_pins(map: &HashMap<String, Pin>) {
    if let (Some(path), Ok(data)) = (pins_path().get(), serde_json::to_string_pretty(map)) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, data).is_ok() { let _ = std::fs::rename(&tmp, path); }
    }
}

/// Кто-то показал не тот ключ: (ip, запомненный, пришедший). Ставит main.rs.
type KeyNotifier = Box<dyn Fn(&str, &str, &str) + Send + Sync>;
fn notifier() -> &'static OnceLock<KeyNotifier> {
    static N: OnceLock<KeyNotifier> = OnceLock::new();
    &N
}

pub fn set_key_notifier(f: KeyNotifier) {
    let _ = notifier().set(f);
}

pub fn pin_of(ip: &str) -> Option<Pin> {
    pins().lock().ok()?.get(ip).cloned()
}

/// Чужой ключ вместо запомненного: запомнить его как ожидающий и сообщить
/// интерфейсу (один раз на каждый новый ключ, а не на каждую попытку).
fn key_mismatch(map: &mut HashMap<String, Pin>, ip: &str, got: &str) -> String {
    let Some(p) = map.get_mut(ip) else { return String::new() };
    let fresh = p.pending != got;
    p.pending = got.to_string();
    let old = p.fp.clone();
    save_pins(map);
    if fresh {
        if let Some(n) = notifier().get() { n(ip, &old, got); }
    }
    format!("{ip}: ключ контакта изменился — соединение отклонено до подтверждения")
}

/// Мы подключились к ip и он предъявил сертификат `fp`.
fn check_outbound(ip: &str, fp: &str) -> Result<(), String> {
    if is_me(fp) { return Err(format!("{ip}: по этому адресу отвечает этот же компьютер")); }
    let mut map = pins().lock().map_err(|_| "pins lock")?;
    match map.get(ip) {
        None => {
            map.insert(ip.to_string(), Pin { fp: fp.to_string(), ..Default::default() });
            save_pins(&map);
            Ok(())
        }
        Some(p) if p.fp == fp => Ok(()),
        Some(_) => Err(key_mismatch(&mut map, ip, fp)),
    }
}

/// Как подключился собеседник: по TLS ли и с каким сертификатом.
#[derive(Clone, Debug, Default)]
pub struct PeerAuth {
    pub tls: bool,
    pub fp: Option<String>,
}

/// Можно ли принять входящее соединение от ip (до разбора кадров).
pub fn check_inbound(ip: &str, auth: &PeerAuth) -> Result<(), String> {
    // через сервер: адрес — это и есть ключ, другого быть не может
    if let Some(id) = crate::relay::id_of(ip) {
        if auth.fp.as_deref() != Some(id) {
            return Err("канал через сервер: ключ собеседника не совпал с его ID".into());
        }
    }
    if auth.fp.as_deref().is_some_and(is_me) {
        return Err(format!("{ip}: подключение от этого же компьютера"));
    }
    let mut map = pins().lock().map_err(|_| "pins lock")?;
    let Some(p) = map.get(ip).cloned() else {
        if let Some(fp) = &auth.fp {
            map.insert(ip.to_string(), Pin { fp: fp.clone(), mutual: true, pending: String::new() });
            save_pins(&map);
        }
        return Ok(());
    };
    match &auth.fp {
        Some(fp) if *fp == p.fp => {
            if !p.mutual {
                if let Some(e) = map.get_mut(ip) { e.mutual = true; }
                save_pins(&map);
            }
            Ok(())
        }
        Some(fp) => Err(key_mismatch(&mut map, ip, fp)),
        // открытый текст от контакта с ключом — никогда (ключ мы получили по TLS,
        // значит, он умеет TLS); TLS без своего сертификата — старая версия
        // (0.3.26–0.3.30), пока контакт ни разу его не показывал
        None if p.mutual || !auth.tls => Err(format!(
            "{ip}: соединение {} от контакта с запомненным ключом отклонено",
            if auth.tls { "без ключа" } else { "без шифрования" }
        )),
        None => Ok(()),
    }
}

/// Пользователь подтвердил новый ключ `expected` (код которого он видел):
/// заменить старый ключ на всех адресах этого контакта (с тем же старым
/// ключом). Другие контакты, даже если им пришёл тот же новый ключ, не меняются.
pub fn trust_pending(ip: &str, expected: &str) -> Result<String, String> {
    let mut map = pins().lock().map_err(|_| "pins lock")?;
    let p = map.get(ip).cloned().ok_or("для этого адреса нет запомненного ключа")?;
    if p.pending.is_empty() { return Err("новый ключ не приходил".into()); }
    if p.pending != expected {
        return Err("пока вы решали, пришёл ещё один ключ — проверьте код заново".into());
    }
    for e in map.values_mut().filter(|e| e.fp == p.fp) {
        // требование показывать ключ сохраняется: новый ключ — та же версия SafeDrop
        *e = Pin { fp: p.pending.clone(), mutual: e.mutual, pending: String::new() };
    }
    save_pins(&map);
    Ok(p.pending)
}

/// Забыть ключ контакта (на всех его адресах): следующий контакт запомнит заново.
pub fn forget_key(ip: &str) {
    if let Ok(mut map) = pins().lock() {
        if let Some(fp) = map.get(ip).map(|p| p.fp.clone()) {
            map.retain(|_, e| e.fp != fp);
            save_pins(&map);
        }
    }
}

pub fn clear_pins() {
    if let Ok(mut map) = pins().lock() {
        map.clear();
        save_pins(&map);
    }
}

/// Код для сверки голосом: одинаковый у обоих собеседников.
pub fn safety_code(a: &str, b: &str) -> String {
    use sha2::{Digest, Sha256};
    if a.is_empty() || b.is_empty() { return String::new(); }
    let (x, y) = if a <= b { (a, b) } else { (b, a) };
    let h = Sha256::digest(format!("safedrop-code:{x}:{y}").as_bytes());
    let n = u64::from_be_bytes(h[..8].try_into().unwrap_or_default()) % 1_000_000_000_000;
    let s = format!("{n:012}");
    format!("{} {} {}", &s[..4], &s[4..8], &s[8..])
}

// ---------- проверка подписи рукопожатия ----------

fn verify13(
    message: &[u8],
    cert: &rustls::pki_types::CertificateDer<'_>,
    dss: &rustls::DigitallySignedStruct,
) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
    // сертификат никем не подписан (самоподписанный), но собеседник обязан
    // доказать, что владеет его ключом: иначе чужой сертификат можно было бы
    // просто показать, и запоминание ключа ничего бы не дало
    rustls::crypto::verify_tls13_signature(message, cert, dss, &crate::quic::ring_provider().signature_verification_algorithms)
}

fn schemes() -> Vec<rustls::SignatureScheme> {
    crate::quic::ring_provider().signature_verification_algorithms.supported_schemes()
}

/// Сертификат сервера: любой самоподписанный (ключ сверяем после рукопожатия
/// по отпечатку), но с настоящей проверкой подписи.
#[derive(Debug)]
pub(crate) struct KeyVerify;

impl rustls::client::danger::ServerCertVerifier for KeyVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(rustls::PeerIncompatible::Tls12NotOffered))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        verify13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        schemes()
    }
}

/// Сертификат клиента: просим, но не требуем (старые версии его не шлют).
#[derive(Debug)]
pub(crate) struct ClientKeyVerify;

impl rustls::server::danger::ClientCertVerifier for ClientKeyVerify {
    fn offer_client_auth(&self) -> bool { true }

    fn client_auth_mandatory(&self) -> bool { false }

    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] { &[] }

    fn verify_client_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        Ok(rustls::server::danger::ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(rustls::PeerIncompatible::Tls12NotOffered))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        verify13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        schemes()
    }
}

// ---------- TLS-конфиги и соединения ----------

fn server_config() -> &'static OnceLock<Option<Arc<rustls::ServerConfig>>> {
    static S: OnceLock<Option<Arc<rustls::ServerConfig>>> = OnceLock::new();
    &S
}

/// Подготовить TLS: ключ устройства из папки приложения и запомненные ключи
/// контактов. Без сертификата сервер принимает только незашифрованные
/// соединения, как раньше.
pub fn init(app_dir: &Path) {
    let path = app_dir.join("pins.json");
    if let Ok(data) = std::fs::read_to_string(&path) {
        if let (Ok(map), Ok(mut g)) = (serde_json::from_str::<HashMap<String, Pin>>(&data), pins().lock()) {
            *g = map;
        }
    }
    let _ = pins_path().set(path);
    match crate::quic::load_or_gen_cert(app_dir) {
        Ok((cert_der, key_der)) => {
            let fp = fp_of(&cert_der);
            if let Ok(mut map) = pins().lock() {
                if drop_own_key(&mut map, &fp) { save_pins(&map); }
            }
            let _ = identity().set(Identity { cert: cert_der.into(), key: key_der, fp });
        }
        #[cfg(target_os = "linux")]
        Err(e) => eprintln!("SafeDrop: не удалось загрузить ключ устройства: {e}"),
        #[cfg(not(target_os = "linux"))]
        Err(_) => {}
    }
    server_config().get_or_init(|| {
        let (cert, key) = my_cert()?;
        let mut cfg = rustls::ServerConfig::builder_with_provider(crate::quic::ring_provider())
            .with_protocol_versions(&[&rustls::version::TLS13]).ok()?
            .with_client_cert_verifier(Arc::new(ClientKeyVerify))
            .with_single_cert(vec![cert], key).ok()?;
        // возобновление сессий не используем (одно соединение — один кадр), а
        // билеты приходят отправителю уже после рукопожатия, и он их не читает:
        // закрытие с непрочитанными данными рвёт соединение сбросом и теряет
        // хвост большого кадра у отправителей 0.3.31–0.3.32 (см. finish)
        cfg.send_tls13_tickets = 0;
        Some(Arc::new(cfg))
    });
}

/// TLS-клиент с ключом устройства (к контактам и к серверу SafeDrop).
pub(crate) fn client_config() -> Option<Arc<rustls::ClientConfig>> {
    static C: OnceLock<Option<Arc<rustls::ClientConfig>>> = OnceLock::new();
    C.get_or_init(|| {
        let builder = rustls::ClientConfig::builder_with_provider(crate::quic::ring_provider())
            .with_protocol_versions(&[&rustls::version::TLS13]).ok()?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(KeyVerify));
        let cfg = match my_cert() {
            Some((cert, key)) => builder.with_client_auth_cert(vec![cert], key).ok()?,
            None => builder.with_no_client_auth(),
        };
        Some(Arc::new(cfg))
    }).clone()
}

/// Входящее соединение: TLS, если клиент начал рукопожатие, иначе как есть.
/// Вместе с потоком — как собеседник подключился (для `check_inbound`).
pub async fn accept(sock: TcpStream) -> std::io::Result<(Stream, PeerAuth)> {
    let mut first = [0u8; 1];
    // зонды скана подключаются и молчат — не ждём их вечно
    let peeked = tokio::time::timeout(HANDSHAKE_TIMEOUT, sock.peek(&mut first)).await;
    // подключился и закрыл, ничего не прислав, — это проверка «в сети ли»
    // (tcp_ping): не соединение, ругаться в лог не на что
    if matches!(peeked, Ok(Ok(0))) {
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "ping"));
    }
    let is_tls = matches!(peeked, Ok(Ok(1))) && first[0] == 0x16;
    if is_tls {
        if let Some(Some(cfg)) = server_config().get() {
            let acceptor = tokio_rustls::TlsAcceptor::from(cfg.clone());
            let tls = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(sock))
                .await
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "TLS handshake timeout"))??;
            let fp = tls.get_ref().1.peer_certificates().and_then(|c| c.first()).map(|c| fp_of(c));
            return Ok((Box::new(tls), PeerAuth { tls: true, fp }));
        }
    }
    Ok((Box::new(sock), PeerAuth::default()))
}

/// TCP до контакта: напрямую или каналом через сервер (адрес `sd-…`).
/// Режимы не смешиваются: в режиме «через сервер» прямых соединений нет.
async fn tcp(ip: &str, port: u16, timeout: Duration) -> Result<Stream, String> {
    if crate::relay::is_relay(ip) {
        let s = tokio::time::timeout(timeout, crate::relay::open(ip, port))
            .await
            .map_err(|_| "тайм-аут подключения через сервер".to_string())??;
        return Ok(Box::new(s));
    }
    if crate::relay::active() {
        return Err(format!("{ip}: включён режим «через сервер», прямые соединения не используются"));
    }
    let s = tokio::time::timeout(timeout, TcpStream::connect(format!("{ip}:{port}")))
        .await
        .map_err(|_| format!("тайм-аут подключения к {ip}:{port}"))?
        .map_err(|e| format!("connect {ip}:{port}: {e}"))?;
    let _ = s.set_nodelay(true);
    Ok(Box::new(s))
}

/// Исходящее соединение с контактом: по лучшему из его адресов (`ident::route`
/// — у одного устройства их бывает несколько: LAN, Radmin, через сервер).
/// Ключ проверяется по тому адресу, к которому подключились.
pub async fn connect(ip: &str, port: u16, timeout: Duration) -> Result<Stream, String> {
    let addrs = crate::ident::route(ip);
    let last = addrs.len().saturating_sub(1);
    let mut err = String::new();
    for (i, addr) in addrs.iter().enumerate() {
        // запасные адреса пробуем быстрее, чтобы не ждать мёртвый адрес целиком
        let t = if i < last { timeout.min(Duration::from_secs(4)) } else { timeout };
        let t0 = std::time::Instant::now();
        match connect_one(addr, port, t).await {
            Ok(s) => {
                crate::ident::note_alive(addr, t0.elapsed().as_millis() as u64);
                return Ok(s);
            }
            Err(e) => {
                crate::ident::note_dead(addr);
                err = e;
            }
        }
    }
    Err(err)
}

/// Исходящее соединение: зашифрованное, если контакт это умеет. С контактом,
/// чей ключ запомнен, — только зашифрованное и только с этим ключом.
/// Через сервер — всегда зашифрованное, и ключ обязан совпасть с ID в адресе.
async fn connect_one(ip: &str, port: u16, timeout: Duration) -> Result<Stream, String> {
    let relayed = crate::relay::id_of(ip);
    let pinned = relayed.is_some() || pin_of(ip).is_some();
    let sock = tcp(ip, port, timeout).await?;
    if !pinned && !peer_ok(ip) {
        return Ok(sock);
    }
    if let (Some(cfg), Ok(name)) = (client_config(), rustls::pki_types::ServerName::try_from(SERVER_NAME)) {
        let connector = tokio_rustls::TlsConnector::from(cfg);
        if let Ok(Ok(tls)) = tokio::time::timeout(HANDSHAKE_TIMEOUT, connector.connect(name.to_owned(), sock)).await {
            let fp = tls.get_ref().1.peer_certificates().and_then(|c| c.first()).map(|c| fp_of(c));
            let Some(fp) = fp else { return Err(format!("{ip}: собеседник не показал ключ")) };
            if relayed.is_some_and(|id| id != fp) {
                return Err("через сервер ответило не то устройство (ключ не совпал с ID)".into());
            }
            check_outbound(ip, &fp)?;
            return Ok(Box::new(tls));
        }
    }
    if relayed.is_some() {
        return Err("не удалось установить защищённое соединение через сервер".into());
    }
    if pinned {
        // без отката: подменивший контакт мог бы просто «сломать» рукопожатие
        return Err(format!("{ip}: не удалось установить защищённое соединение"));
    }
    // контакт не ответил на рукопожатие (старая версия) — как раньше, без шифрования
    forget(ip);
    tcp(ip, port, timeout).await
}

/// Входящий канал через сервер: только TLS с ключом собеседника (сервер
/// не должен иметь возможности подсунуть открытый текст).
pub async fn accept_relayed(sock: TcpStream) -> std::io::Result<(Stream, PeerAuth)> {
    let Some(Some(cfg)) = server_config().get() else {
        return Err(std::io::Error::other("нет ключа устройства"));
    };
    let acceptor = tokio_rustls::TlsAcceptor::from(cfg.clone());
    let tls = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(sock))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "TLS handshake timeout"))??;
    let fp = tls.get_ref().1.peer_certificates().and_then(|c| c.first()).map(|c| fp_of(c));
    Ok((Box::new(tls), PeerAuth { tls: true, fp }))
}

/// Закрыть исходящее соединение после отправки кадра, не потеряв его хвост.
/// Сервер TLS 1.3 сразу после рукопожатия присылает свои служебные данные
/// (session tickets), а мы их не читаем. Закрытие сокета с непрочитанными
/// входящими данными Windows (и Linux) делает сбросом (RST), и всё, что ещё
/// лежало в буфере отправки, пропадает: короткий текст успевал уйти, а
/// картинка в несколько сотен КБ по Radmin — нет. Поэтому после shutdown
/// дочитываем до EOF: получатель закрывает соединение, когда прочитал кадр.
pub async fn finish(mut s: Stream) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let _ = s.flush().await;
    let _ = s.shutdown().await;
    let drain = async {
        let mut buf = [0u8; 4096];
        let mut total = 0usize;
        // ответ нам не нужен; лимит — чтобы чужая сторона не держала нас вечно
        while total < 1024 * 1024 {
            match s.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => total += n,
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(30), drain).await;
}

/// QUIC: сверить ключ сервера после подключения (тот же учёт, что у TCP).
pub(crate) fn check_quic_outbound(ip: &str, conn: &quinn::Connection) -> Result<(), String> {
    let fp = quic_peer_fp(conn).ok_or_else(|| format!("{ip}: собеседник не показал ключ"))?;
    check_outbound(ip, &fp)
}

pub(crate) fn quic_peer_fp(conn: &quinn::Connection) -> Option<String> {
    let certs = conn.peer_identity()?.downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>().ok()?;
    certs.first().map(|c| fp_of(c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[test]
    fn version_gate() {
        note_peer_version("10.0.0.1", "0.3.26");
        note_peer_version("10.0.0.2", "0.3.25");
        note_peer_version("10.0.0.3", "");
        assert!(peer_ok("10.0.0.1"));
        assert!(!peer_ok("10.0.0.2"));
        assert!(!peer_ok("10.0.0.3"));
        note_peer_version("10.0.0.1", "0.3.20");
        assert!(!peer_ok("10.0.0.1"));
    }

    #[test]
    fn safety_code_is_symmetric() {
        let c = safety_code("aa", "bb");
        assert_eq!(c, safety_code("bb", "aa"));
        assert_eq!(c.len(), 14);
        assert_ne!(c, safety_code("aa", "cc"));
        assert_eq!(safety_code("", "bb"), "");
    }

    #[test]
    fn pins_tofu_mismatch_and_trust() {
        let ip = "10.9.9.1";
        let alt = "10.9.9.2"; // второй адрес того же ПК
        let none = PeerAuth { tls: false, fp: None };
        let tls_only = PeerAuth { tls: true, fp: None };
        let key = |k: &str| PeerAuth { tls: true, fp: Some(k.into()) };
        // первый контакт запоминает ключ; старый клиент (TLS без своего ключа)
        // пока проходит, открытый текст — нет
        check_outbound(ip, "k1").unwrap();
        check_outbound(alt, "k1").unwrap();
        assert!(check_inbound(ip, &tls_only).is_ok());
        assert!(check_inbound(ip, &none).is_err());
        // показал ключ — дальше без ключа нельзя
        assert!(check_inbound(ip, &key("k1")).is_ok());
        assert!(pin_of(ip).unwrap().mutual);
        assert!(check_inbound(ip, &none).is_err());
        assert!(check_inbound(ip, &tls_only).is_err());
        // чужой ключ — отказ, ключ ждёт подтверждения
        assert!(check_inbound(ip, &key("k2")).is_err());
        assert!(check_outbound(ip, "k2").is_err());
        assert_eq!(pin_of(ip).unwrap().pending, "k2");
        // другой контакт с тем же новым ключом (подменщик выдаёт себя за обоих)
        let other = "10.9.9.3";
        check_outbound(other, "z1").unwrap();
        assert!(check_outbound(other, "k2").is_err());
        // подтвердить можно только тот ключ, код которого видел пользователь
        assert!(trust_pending(ip, "k3").is_err());
        assert_eq!(trust_pending(ip, "k2").unwrap(), "k2");
        // ключ меняется на всех адресах этого контакта, но не у другого
        assert_eq!(pin_of(alt).unwrap().fp, "k2");
        assert_eq!(pin_of(other).unwrap().fp, "z1");
        assert!(pin_of(ip).unwrap().mutual);
        assert!(check_outbound(alt, "k2").is_ok());
        forget_key(ip);
        assert!(pin_of(ip).is_none() && pin_of(alt).is_none());
        forget_key(other);
    }

    #[test]
    fn own_key_dropped_from_pins() {
        let pin = |fp: &str, pending: &str| Pin { fp: fp.into(), mutual: true, pending: pending.into() };
        let mut map = HashMap::from([
            ("10.0.0.1".to_string(), pin("vm", "me")),
            ("10.0.0.2".to_string(), pin("me", "")),
            ("10.0.0.3".to_string(), pin("friend", "other")),
        ]);
        assert!(!drop_own_key(&mut map, ""));
        assert!(drop_own_key(&mut map, "me"));
        assert_eq!(map.get("10.0.0.1"), Some(&pin("vm", "")));
        assert!(map.get("10.0.0.2").is_none());
        assert_eq!(map.get("10.0.0.3"), Some(&pin("friend", "other")));
        assert!(!drop_own_key(&mut map, "me"));
    }

    /// Картинка в чате — кадр в сотни КБ. Получатель читает его не сразу
    /// (медленный Radmin, занятый поток): отправитель к этому моменту уже закрыл
    /// соединение, и закрытие не должно отменить ещё не доставленные байты.
    #[tokio::test]
    async fn big_frame_survives_sender_close() {
        let dir = std::env::temp_dir().join(format!("safedrop-tls-big-{}", std::process::id()));
        init(&dir);
        let listener = tokio::net::TcpListener::bind("127.0.0.4:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        const N: usize = 4 * 1024 * 1024;
        // получатель 0.3.31–0.3.32 ещё шлёт билеты: от сброса спасает только finish
        let mut old = (**server_config().get().unwrap().as_ref().unwrap()).clone();
        old.send_tls13_tickets = 2;
        let old = tokio_rustls::TlsAcceptor::from(Arc::new(old));
        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let mut s = old.accept(sock).await.unwrap();
            tokio::time::sleep(Duration::from_millis(800)).await;
            let mut line = Vec::new();
            let r = BufReader::new(&mut s).read_until(b'\n', &mut line).await;
            (r.map_err(|e| e.to_string()), line.len(), listener)
        });
        note_peer_version("127.0.0.4", "0.3.26");
        SELF_IS_PEER.with(|c| c.set(true)); // сервер с тем же ключом — «собеседник»
        let mut s = connect("127.0.0.4", port, Duration::from_secs(5)).await.unwrap();
        let mut data = vec![b'a'; N];
        data.push(b'\n');
        s.write_all(&data).await.unwrap();
        finish(s).await;
        let (r, got, listener) = server.await.unwrap();
        assert_eq!((r.is_ok(), got), (true, N + 1), "{r:?}");

        // отправитель 0.3.31–0.3.32 закрывал сразу после shutdown, не дочитывая:
        // наш сервер не шлёт ему билеты, и закрытие не превращается в сброс
        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (mut s, _) = accept(sock).await.unwrap();
            tokio::time::sleep(Duration::from_millis(800)).await;
            let mut line = Vec::new();
            let r = BufReader::new(&mut s).read_until(b'\n', &mut line).await;
            (r.map_err(|e| e.to_string()), line.len())
        });
        let mut s = connect("127.0.0.4", port, Duration::from_secs(5)).await.unwrap();
        s.write_all(&data).await.unwrap();
        s.flush().await.unwrap();
        let _ = s.shutdown().await;
        drop(s);
        let (r, got) = server.await.unwrap();
        assert_eq!((r.is_ok(), got), (true, N + 1), "{r:?}");
        forget_key("127.0.0.4");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Сервер на одном порту принимает и TLS-клиента, и старого (без TLS);
    /// клиент и сервер видят ключи друг друга.
    #[tokio::test]
    async fn one_port_serves_tls_and_plain() {
        let dir = std::env::temp_dir().join(format!("safedrop-tls-test-{}", std::process::id()));
        init(&dir);
        let me = my_fp();
        assert_eq!(me.len(), 64);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            for _ in 0..3 {
                let (sock, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let Ok((mut s, auth)) = accept(sock).await else { return };
                    let mut line = String::new();
                    if BufReader::new(&mut s).read_line(&mut line).await.is_err() { return; }
                    let who = if auth.fp.is_some() { "key" } else { "nokey" };
                    let reply = format!("{}|{}|{}", if auth.tls { "tls" } else { "plain" }, who, line);
                    let _ = s.write_all(reply.as_bytes()).await;
                    let _ = s.flush().await;
                    let _ = s.shutdown().await;
                });
            }
        });
        note_peer_version("127.0.0.1", "0.3.26");
        // адрес ведёт на этот же ПК: свой ключ не запоминается и не «меняется»
        assert!(connect("127.0.0.1", port, Duration::from_secs(5)).await.is_err());
        assert!(pin_of("127.0.0.1").is_none());
        assert!(check_inbound("127.0.0.1", &PeerAuth { tls: true, fp: Some(me.clone()) }).is_err());
        assert!(pin_of("127.0.0.1").is_none());
        // дальше сервер играет собеседника (второго ключа в процессе нет)
        SELF_IS_PEER.with(|c| c.set(true));
        // новый контакт — зашифровано, обе стороны с ключом; ключ сервера запомнен
        let mut s = connect("127.0.0.1", port, Duration::from_secs(5)).await.unwrap();
        s.write_all(b"hello\n").await.unwrap();
        s.flush().await.unwrap();
        let mut got = String::new();
        BufReader::new(&mut s).read_line(&mut got).await.unwrap();
        assert_eq!(got, "tls|key|hello\n");
        assert_eq!(pin_of("127.0.0.1").unwrap().fp, me);
        // старый контакт без запомненного ключа — как раньше
        forget_key("127.0.0.1");
        note_peer_version("127.0.0.1", "0.3.20");
        let mut s = connect("127.0.0.1", port, Duration::from_secs(5)).await.unwrap();
        s.write_all(b"hi\n").await.unwrap();
        s.flush().await.unwrap();
        let mut got = String::new();
        BufReader::new(&mut s).read_line(&mut got).await.unwrap();
        assert_eq!(got, "plain|nokey|hi\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! Протокол между SafeDrop и сервером-ретранслятором. Файл общий: его
//! подключает и сервер (`server/src/main.rs`), и приложение
//! (`src-tauri/src/relay.rs` через `#[path]`), чтобы описание было одно.
//!
//! Один TCP-порт (по умолчанию 47840) принимает два вида соединений:
//! - управляющее: TLS 1.3, клиент обязан показать ключ своего устройства
//!   (тот же самоподписанный сертификат, что и для P2P). Его отпечаток —
//!   ID устройства на сервере. Дальше — JSON-строки (`ToServer`/`ToClient`);
//! - канал: начинается с `DATA_MAGIC` и 32 hex-символов токена, без TLS.
//!   Сервер сводит два канала с одинаковым токеном и просто пересылает байты.
//!   Внутри канала устройства сами устанавливают свой взаимный TLS с проверкой
//!   ключа собеседника, поэтому сервер содержимого не видит и подменить
//!   собеседника не может.
//!
//! Новые поля — только с `#[serde(default)]`, новые сообщения — новый вариант:
//! старые версии незнакомое пропускают (`Unknown`).

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: u16 = 47840;
pub const DEFAULT_TURN_PORT: u16 = 3478;
/// Начало канала (после него — токен).
pub const DATA_MAGIC: &[u8; 4] = b"SDT1";
pub const TOKEN_LEN: usize = 32;
/// Предел одной строки управляющего соединения.
pub const MAX_CTL_LINE: usize = 64 * 1024;
/// Версия протокола (для будущих несовместимых изменений).
pub const PROTO: u32 = 1;
/// Клиент шлёт ping не реже этого; сервер ждёт втрое дольше.
pub const PING_SECS: u64 = 20;

/// Порт внутри устройства, к которому ведёт канал.
pub const PORT_SIG: &str = "sig";
pub const PORT_FILE: &str = "file";
/// Канал сопряжения нового устройства (`PairJoin`): только к тому, кто ждёт код.
pub const PORT_PAIR: &str = "pair";

/// Вид устройства (`Hello.form`): кому отдавать каналы старых клиентов.
pub const FORM_DESKTOP: &str = "desktop";
pub const FORM_PHONE: &str = "phone";
/// Сколько устройств одного аккаунта (ID) сервер держит одновременно.
pub const MAX_DEVS: usize = 8;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "t")]
pub enum ToServer {
    #[serde(rename = "hello")]
    Hello {
        #[serde(default)]
        proto: u32,
        #[serde(default)]
        pass: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        ver: String,
        /// Своё устройство внутри аккаунта (случайный hex, постоянный для
        /// установки). Один ключ может быть на нескольких устройствах (ПК и
        /// телефон одного человека); пусто — старая версия, одно устройство.
        #[serde(default)]
        dev: String,
        /// `desktop` | `phone`
        #[serde(default)]
        form: String,
        /// Не показывать в списке (телефон, который пока только входит по коду).
        #[serde(default)]
        quiet: bool,
    },
    /// Открыть канал к устройству `to` (порт `port`) с токеном `tok`,
    /// придуманным клиентом. Сразу после этого клиент подключается каналом.
    /// `dev` — к какому из устройств аккаунта (пусто — к основному; так шлют
    /// старые версии). К своему же ID можно — к другому своему устройству.
    #[serde(rename = "open")]
    Open {
        to: String,
        port: String,
        tok: String,
        #[serde(default)]
        dev: String,
    },
    /// Ждать новое устройство по коду: `slot` — открытая часть кода (4 цифры),
    /// секретную часть сервер не видит (её проверяют сами устройства, SPAKE2).
    #[serde(rename = "pair-wait")]
    PairWait { slot: String },
    #[serde(rename = "pair-cancel")]
    PairCancel,
    /// Новое устройство ввело код: открыть канал (`PORT_PAIR`) к тому, кто
    /// ждёт `slot`. Попытка одна: код после неё больше не действует.
    #[serde(rename = "pair-join")]
    PairJoin { slot: String, tok: String },
    /// Сменилось имя, которое видят остальные.
    #[serde(rename = "name")]
    Name { name: String },
    #[serde(rename = "ping")]
    Ping,
    /// Свой пинг до сервера (мс, замер ping→pong). Сервер в ответ шлёт
    /// `Rtts`: пинг до собеседника через сервер = свой + его.
    #[serde(rename = "rtt")]
    Rtt { ms: u32 },
    #[serde(other)]
    Unknown,
}

/// Пинг до сервера у другого устройства.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct PeerRtt {
    pub id: String,
    pub ms: u32,
}

/// Пинг дольше этого — не пинг, а ошибка замера.
pub const MAX_RTT_MS: u32 = 60_000;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "t")]
pub enum ToClient {
    #[serde(rename = "welcome")]
    Welcome {
        id: String,
        #[serde(default)]
        server_ver: String,
        #[serde(default)]
        turn: Option<TurnInfo>,
        /// Сервер умеет несколько устройств на один ID и сопряжение по коду.
        #[serde(default)]
        multi: bool,
    },
    #[serde(rename = "deny")]
    Deny { msg: String },
    /// Полный список аккаунтов на сервере. Свой ID в нём есть, только если
    /// подключены другие свои устройства (в `devs` — они, без самого клиента).
    #[serde(rename = "peers")]
    Peers { list: Vec<PeerInfo> },
    /// Кто-то открывает канал к этому устройству: подключиться с токеном.
    /// `dev` — с какого устройства аккаунта `from`.
    #[serde(rename = "incoming")]
    Incoming {
        from: String,
        port: String,
        tok: String,
        #[serde(default)]
        dev: String,
    },
    /// Код для нового устройства не принят (занят другим и т. п.).
    #[serde(rename = "pair-fail")]
    PairFail { slot: String, msg: String },
    /// Канал не открылся (адресат не в сети и т. п.).
    #[serde(rename = "fail")]
    Fail { tok: String, msg: String },
    /// Новые данные для TURN (старые скоро истекут).
    #[serde(rename = "turn")]
    Turn { turn: TurnInfo },
    #[serde(rename = "pong")]
    Pong,
    /// Пинг до сервера у остальных устройств (ответ на `Rtt`).
    #[serde(rename = "rtts")]
    Rtts { list: Vec<PeerRtt> },
    #[serde(other)]
    Unknown,
}

/// Доступ к TURN на этом же сервере (для звонков и демонстраций).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct TurnInfo {
    pub port: u16,
    pub user: String,
    pub cred: String,
    /// Когда истекает (unix-секунды).
    #[serde(default)]
    pub expires: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct PeerInfo {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub ver: String,
    /// Подключённые устройства этого аккаунта (новый сервер; у старого пусто).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devs: Vec<DevInfo>,
}

/// Одно устройство аккаунта.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct DevInfo {
    #[serde(default)]
    pub dev: String,
    #[serde(default)]
    pub form: String,
    #[serde(default)]
    pub ver: String,
}

/// ID устройства: SHA-256 сертификата, 64 строчных hex-символа.
pub fn valid_id(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn valid_token(s: &str) -> bool {
    s.len() == TOKEN_LEN && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn valid_port(s: &str) -> bool {
    s == PORT_SIG || s == PORT_FILE
}

/// Устройство аккаунта: пусто (старая версия) или 8–32 строчных hex.
pub fn valid_dev(s: &str) -> bool {
    s.is_empty() || ((8..=32).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

/// Открытая часть кода сопряжения: 4 цифры.
pub fn valid_slot(s: &str) -> bool {
    s.len() == 4 && s.bytes().all(|b| b.is_ascii_digit())
}

pub fn clean_form(s: &str) -> String {
    if s == FORM_PHONE { FORM_PHONE.into() } else if s == FORM_DESKTOP { FORM_DESKTOP.into() } else { String::new() }
}

/// Имя для списка: без управляющих символов, не длиннее 64 символов.
pub fn clean_name(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(64).collect::<String>().trim().to_string()
}

/// Отпечаток сертификата (так же считает приложение, `tls::fp_of`).
pub fn fp_of(cert_der: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(cert_der).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_roundtrip_and_unknown_is_tolerated() {
        let m = ToServer::Open { to: "a".repeat(64), port: PORT_SIG.into(), tok: "0".repeat(32), dev: String::new() };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"t\":\"open\""));
        assert_eq!(serde_json::from_str::<ToServer>(&s).unwrap(), m);
        assert_eq!(serde_json::from_str::<ToServer>(r#"{"t":"future","x":1}"#).unwrap(), ToServer::Unknown);
        assert_eq!(serde_json::from_str::<ToClient>(r#"{"t":"future"}"#).unwrap(), ToClient::Unknown);
        let w: ToClient = serde_json::from_str(r#"{"t":"welcome","id":"x"}"#).unwrap();
        assert_eq!(w, ToClient::Welcome { id: "x".into(), server_ver: String::new(), turn: None, multi: false });
        // пинги: новые сообщения, старые версии видят в них Unknown
        assert_eq!(serde_json::to_string(&ToServer::Rtt { ms: 42 }).unwrap(), r#"{"t":"rtt","ms":42}"#);
        let r: ToClient = serde_json::from_str(r#"{"t":"rtts","list":[{"id":"a","ms":7}]}"#).unwrap();
        assert_eq!(r, ToClient::Rtts { list: vec![PeerRtt { id: "a".into(), ms: 7 }] });
        // несколько устройств: старые сообщения читаются, новые поля по умолчанию
        let h: ToServer = serde_json::from_str(r#"{"t":"hello","pass":"p"}"#).unwrap();
        assert!(matches!(h, ToServer::Hello { ref dev, quiet: false, .. } if dev.is_empty()));
        let o: ToServer = serde_json::from_str(r#"{"t":"open","to":"a","port":"sig","tok":"b"}"#).unwrap();
        assert!(matches!(o, ToServer::Open { ref dev, .. } if dev.is_empty()));
        let i: ToClient = serde_json::from_str(r#"{"t":"incoming","from":"a","port":"sig","tok":"b"}"#).unwrap();
        assert!(matches!(i, ToClient::Incoming { ref dev, .. } if dev.is_empty()));
        // старый клиент получает список с devs и не спотыкается: PeerInfo без devs
        let p = PeerInfo { id: "a".into(), name: "n".into(), ver: "v".into(), devs: vec![] };
        assert_eq!(serde_json::to_string(&p).unwrap(), r#"{"id":"a","name":"n","ver":"v"}"#);
        let j = serde_json::to_string(&ToServer::PairJoin { slot: "1234".into(), tok: "t".into() }).unwrap();
        assert_eq!(j, r#"{"t":"pair-join","slot":"1234","tok":"t"}"#);
    }

    #[test]
    fn validators() {
        assert!(valid_id(&"ab".repeat(32)));
        assert!(!valid_id(&"AB".repeat(32)));
        assert!(!valid_id("abc"));
        assert!(valid_token(&"f".repeat(32)));
        assert!(!valid_token(&"g".repeat(32)));
        assert!(valid_port("sig") && valid_port("file") && !valid_port("x") && !valid_port(PORT_PAIR));
        assert!(valid_dev("") && valid_dev(&"a1".repeat(8)) && !valid_dev("abc") && !valid_dev(&"A1".repeat(8)) && !valid_dev(&"a".repeat(33)));
        assert!(valid_slot("0042") && !valid_slot("42") && !valid_slot("12a4"));
        assert_eq!(clean_form("phone"), "phone");
        assert_eq!(clean_form("toaster"), "");
        assert_eq!(clean_name("  Саня\u{0}\n "), "Саня");
        assert_eq!(clean_name(&"я".repeat(100)).chars().count(), 64);
    }
}

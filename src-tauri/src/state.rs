use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeConfig {
    #[serde(default = "def_bg_mode")]
    pub bg_mode: String, // color | gradient | image
    #[serde(default = "def_bg_color")]
    pub bg_color: String,
    #[serde(default = "def_gradient")]
    pub bg_gradient: String,
    #[serde(default)]
    pub bg_image: String, // file:// path or ""
    #[serde(default = "def_blur")]
    pub blur: u8, // 0-30
    #[serde(default = "def_opacity")]
    pub opacity: f32, // 0.5-1.0
    #[serde(default = "def_accent")]
    pub accent: String,
    #[serde(default = "def_radius")]
    pub radius: u8,
    #[serde(default)]
    pub font: String,
    #[serde(default)]
    pub custom_css: String,
    #[serde(default = "def_text")]
    pub text_color: String,
    #[serde(default = "def_muted")]
    pub muted_color: String,
    #[serde(default = "def_btn")]
    pub btn_color: String,
    #[serde(default = "def_btn_text")]
    pub btn_text: String,
    #[serde(default = "def_link")]
    pub link_color: String,
    #[serde(default = "def_button_gradient_from")]
    pub button_gradient_from: String,
    #[serde(default = "def_button_gradient_to")]
    pub button_gradient_to: String,
    #[serde(default = "def_button_gradient_angle")]
    pub button_gradient_angle: u16,
}
fn def_bg_mode() -> String { "gradient".into() }
fn def_bg_color() -> String { "#141824".into() }
fn def_gradient() -> String { "linear-gradient(135deg,#1a2138,#0d1020)".into() }
fn def_blur() -> u8 { 14 }
fn def_opacity() -> f32 { 0.92 }
fn def_accent() -> String { "#5b8cff".into() }
fn def_radius() -> u8 { 14 }
fn def_text() -> String { "#eef1f8".into() }
fn def_muted() -> String { "#9aa3b8".into() }
fn def_btn() -> String { "rgba(255,255,255,.1)".into() }
fn def_btn_text() -> String { "#eef1f8".into() }
fn def_link() -> String { "#7aa2ff".into() }
fn def_button_gradient_from() -> String { "#5b8cff".into() }
fn def_button_gradient_to() -> String { "#9b59ff".into() }
fn def_button_gradient_angle() -> u16 { 135 }

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            bg_mode: def_bg_mode(), bg_color: def_bg_color(),
            bg_gradient: def_gradient(), bg_image: String::new(),
            blur: def_blur(), opacity: def_opacity(),
            accent: def_accent(), radius: def_radius(),
            font: "Segoe UI".into(), custom_css: String::new(),
            text_color: def_text(), muted_color: def_muted(),
            btn_color: def_btn(), btn_text: def_btn_text(),
            link_color: def_link(),
            button_gradient_from: def_button_gradient_from(),
            button_gradient_to: def_button_gradient_to(),
            button_gradient_angle: def_button_gradient_angle(),
        }
    }
}

/// Own profile: nickname + avatar shown to other peers (synced).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Profile {
    #[serde(default)]
    pub nickname: String,
    #[serde(default)]
    pub avatar: String, // dataURL (data:image/...;base64) or ""
}

/// Appearance of the call screen and the compact in-chat call panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallUi {
    #[serde(default = "def_call_tile")]
    pub tile_size: u16, // px, participant tile width
    #[serde(default = "def_call_avatar")]
    pub avatar_size: u16, // px, participant avatar diameter
    #[serde(default = "def_true")]
    pub show_names: bool,
    #[serde(default = "def_call_ring")]
    pub ring_color: String,
    #[serde(default = "def_call_ring_width")]
    pub ring_width: u8, // px, 0 = no ring
    #[serde(default = "def_call_people_pos")]
    pub people_pos: String, // bottom | center
    #[serde(default = "def_true")]
    pub show_controls: bool,
    #[serde(default = "def_call_bg_mode")]
    pub bg_mode: String, // gradient | solid
    #[serde(default = "def_call_bg_color")]
    pub bg_color: String,
    #[serde(default = "def_call_bg_from")]
    pub bg_from: String,
    #[serde(default = "def_call_bg_to")]
    pub bg_to: String,
    #[serde(default = "def_call_accent")]
    pub accent_color: String,
    #[serde(default = "def_call_text")]
    pub text_color: String,
    #[serde(default = "def_true")]
    pub show_logo: bool,
}
fn def_call_tile() -> u16 { 120 }
fn def_call_avatar() -> u16 { 120 }
fn def_call_ring() -> String { "#23a559".into() }
fn def_call_ring_width() -> u8 { 4 }
fn def_call_people_pos() -> String { "bottom".into() }
fn def_call_bg_mode() -> String { "gradient".into() }
fn def_call_bg_color() -> String { "#1b1230".into() }
fn def_call_bg_from() -> String { "#1b1230".into() }
fn def_call_bg_to() -> String { "#120d20".into() }
fn def_call_accent() -> String { "#7c3aed".into() }
fn def_call_text() -> String { "#f2f3f5".into() }

impl Default for CallUi {
    fn default() -> Self {
        Self {
            tile_size: def_call_tile(),
            avatar_size: def_call_avatar(),
            show_names: true,
            ring_color: def_call_ring(),
            ring_width: def_call_ring_width(),
            people_pos: def_call_people_pos(),
            show_controls: true,
            bg_mode: def_call_bg_mode(),
            bg_color: def_call_bg_color(),
            bg_from: def_call_bg_from(),
            bg_to: def_call_bg_to(),
            accent_color: def_call_accent(),
            text_color: def_call_text(),
            show_logo: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "def_device")]
    pub device_name: String,
    #[serde(default)]
    pub loaddrop_dir: String,
    #[serde(default = "def_port")]
    pub tcp_port: u16,
    #[serde(default = "def_sig_port")]
    pub sig_port: u16, // signaling/chat/voice port = tcp_port + 1
    #[serde(default = "def_true")]
    pub auto_accept_trusted: bool,
    #[serde(default = "def_limit")]
    pub auto_accept_limit_mb: u64,
    #[serde(default = "def_true")]
    pub minimize_to_tray: bool,
    // значения для отсутствующих в файле полей совпадают с Settings::default()
    #[serde(default)]
    pub autostart: bool,
    #[serde(default = "def_true")]
    pub sound: bool,
    #[serde(default)]
    pub theme: ThemeConfig,
    #[serde(default)]
    pub call_ui: CallUi,
    #[serde(default)]
    pub profile: Profile,
    #[serde(default)]
    pub mic_id: String, // selected microphone deviceId (frontend)
    #[serde(default)]
    pub speaker_id: String, // selected speaker deviceId (frontend)
    #[serde(default)]
    pub camera_id: String, // selected camera deviceId (frontend)
    #[serde(default)]
    pub ringtone: String, // path to custom ringtone audio file ("")
    #[serde(default)]
    pub incoming_ringtone_melody: String, // built-in incoming-call melody
    #[serde(default)]
    pub ringback_melody: String, // selected outgoing-call ringback melody
    #[serde(default)]
    pub ringback_file: String, // optional custom outgoing-call ringback audio
    #[serde(default)]
    pub call_join_sound: String, // optional custom sound when a participant answers
    #[serde(default = "def_call_join_melody")]
    pub call_join_melody: String,
    #[serde(default)]
    pub call_leave_sound: String, // optional custom sound when a participant leaves
    #[serde(default = "def_call_leave_melody")]
    pub call_leave_melody: String,
    #[serde(default)]
    pub ctx_menu: bool, // Windows Explorer context menu enabled
    #[serde(default)]
    pub message_sound: String, // path to custom incoming-message sound ("")
    #[serde(default)]
    pub file_sound: String, // path to custom file-received sound ("")
    #[serde(default = "def_vol")]
    pub mic_volume: u16, // 0..300, 100 = as-is
    #[serde(default = "def_gate")]
    pub noise_gate: u8, // 0..60, 0 = off; threshold ~ v/1000 RMS
    #[serde(default = "def_noise_suppress")]
    pub noise_suppress: u8, // 0..100
    #[serde(default = "def_noise_filter")]
    pub noise_filter: String, // rnnoise | webrtc | off
    #[serde(default = "def_noise_strength")]
    pub noise_strength: u8, // 0..100 blend of clean and original microphone signal
    #[serde(default = "def_noise_smooth")]
    pub noise_smooth: u8, // 0..100
    #[serde(default = "def_true")]
    pub noise_suppression: bool,
    #[serde(default = "def_true")]
    pub echo_cancellation: bool,
    #[serde(default = "def_true")]
    pub auto_gain_control: bool,
    #[serde(default)]
    pub peer_volumes: HashMap<String, u8>, // ip -> 0..200
    #[serde(default = "def_quality")]
    pub share_quality: String, // low | medium | high
    #[serde(default)]
    pub udp_enabled: bool, // beta: dual TCP+UDP (QUIC) transport
    #[serde(default)]
    pub stun_enabled: bool,
    #[serde(default)]
    pub push_to_talk: bool,
    #[serde(default)]
    pub push_to_talk_key: String,
    #[serde(default = "def_udp_port")]
    pub udp_port: u16, // QUIC endpoint port
    #[serde(default = "def_true")]
    pub show_typing: bool, // send "typing…" indicator while composing
    #[serde(default = "def_true")]
    pub read_receipts: bool,
    #[serde(default = "def_true")]
    pub duck_media: bool, // quieter YouTube/browser video while someone in the call talks
    #[serde(default = "def_duck_level")]
    pub duck_level: u8, // % of the normal video volume while ducked
    #[serde(default)]
    pub scan_adapters: Vec<String>, // adapter keys (name + ip) enabled for network scan (empty = all)
    #[serde(default)]
    pub scan_manual: Vec<String>, // manually added IPs/subnets for scanning
    #[serde(default)]
    pub sections_open: std::collections::HashMap<String, bool>, // settings accordion open state
}
fn def_device() -> String { whoami_host() }
fn def_duck_level() -> u8 { 30 }
fn def_port() -> u16 { 47831 }
fn def_sig_port() -> u16 { 47832 }
fn def_true() -> bool { true }
fn def_limit() -> u64 { 500 }
fn def_vol() -> u16 { 100 }
fn def_gate() -> u8 { 12 }
fn def_noise_suppress() -> u8 { 70 }
fn def_noise_filter() -> String { "rnnoise".into() }
fn def_noise_strength() -> u8 { 100 }
fn def_noise_smooth() -> u8 { 50 }
fn def_call_join_melody() -> String { "classic".into() }
fn def_call_leave_melody() -> String { "default".into() }
fn def_quality() -> String { "medium".into() }
fn def_udp_port() -> u16 { 47833 }
fn def_idle() -> String { "idle".into() }

fn whoami_host() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "My-PC".into())
}

impl Default for Settings {
    fn default() -> Self {
        let mut loaddrop = dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or(PathBuf::from("."));
        loaddrop.push("LoadDrop");
        Self {
            device_name: whoami_host(),
            loaddrop_dir: loaddrop.to_string_lossy().to_string(),
            tcp_port: 47831,
            sig_port: 47832,
            auto_accept_trusted: true,
            auto_accept_limit_mb: 500,
            minimize_to_tray: true,
            autostart: false,
            sound: true,
            theme: ThemeConfig::default(),
            call_ui: CallUi::default(),
            profile: Profile::default(),
            mic_id: String::new(),
            speaker_id: String::new(),
            camera_id: String::new(),
            ringtone: String::new(),
            incoming_ringtone_melody: "default".into(),
            ringback_melody: "default".into(),
            ringback_file: String::new(),
            call_join_sound: String::new(),
            call_join_melody: def_call_join_melody(),
            call_leave_sound: String::new(),
            call_leave_melody: def_call_leave_melody(),
            ctx_menu: false,
            message_sound: String::new(),
            file_sound: String::new(),
            mic_volume: 100,
            noise_gate: 12,
            noise_suppress: def_noise_suppress(),
            noise_filter: def_noise_filter(),
            noise_strength: def_noise_strength(),
            noise_smooth: def_noise_smooth(),
            noise_suppression: true,
            echo_cancellation: true,
            auto_gain_control: true,
            peer_volumes: HashMap::new(),
            share_quality: def_quality(),
            udp_enabled: false,
            stun_enabled: false,
            push_to_talk: false,
            push_to_talk_key: "Space".into(),
            udp_port: def_udp_port(),
            show_typing: true,
            read_receipts: true,
            duck_media: true,
            duck_level: 30,
            scan_adapters: Vec::new(),
            scan_manual: Vec::new(),
            sections_open: std::collections::HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub ip: String,
    #[serde(default)]
    pub name: String, // local alias (rename)
    #[serde(default)]
    pub nickname: String, // synced nickname from peer
    #[serde(default)]
    pub avatar: String, // synced avatar dataURL from peer
    #[serde(default)]
    pub custom_avatar: String, // local override avatar (you assign)
    // неизвестный контакт не получает автоприём файлов
    #[serde(default)]
    pub trusted: bool,
    #[serde(default)]
    pub supports_file_confirm: bool,
    #[serde(default)]
    pub supports_chat_ack: bool,
    #[serde(default)]
    pub supports_watch: bool,
    /// Версия SafeDrop у контакта (из hello). Пусто — старая версия без этого поля.
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub online: bool,
    #[serde(default)]
    pub ping_ms: Option<u64>,
    #[serde(default)]
    pub last_seen: Option<String>,
    #[serde(default)]
    pub phash: String, // hash of peer profile (for lightweight sync)
    #[serde(default = "def_idle")]
    pub state: String, // idle | incall | sharing | incall+sharing
    #[serde(default)]
    pub detail: String, // e.g. who they're calling / group name
    #[serde(default)]
    pub ips: Vec<String>, // all known addresses of this contact (primary = ip)
}

impl Peer {
    /// Display avatar priority: local override > synced
    pub fn display_avatar(&self) -> String {
        if !self.custom_avatar.is_empty() {
            return self.custom_avatar.clone();
        }
        self.avatar.clone()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferInfo {
    pub id: String,
    pub filename: String,
    pub size: u64,
    pub done: u64,
    pub speed_bps: u64,
    pub direction: String, // up | down
    pub peer: String,
    pub status: String, // active | done | error | waiting_accept
    pub progress: f32,
    #[serde(default)]
    pub error: String, // human-readable error when status == error
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Group {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub members: Vec<String>, // peer ips
    #[serde(default)]
    pub avatar: String, // dataURL, like profile avatars
    #[serde(default)]
    pub updated_at: i64, // last name/avatar change (UTC ms)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GroupSyncState {
    #[serde(default)]
    pub deleted: Vec<GroupDeletion>,
    #[serde(default)]
    pub departures: Vec<GroupDeparture>,
    #[serde(default)]
    pub rejoins: Vec<GroupRejoin>,
    #[serde(default)]
    pub metas: Vec<GroupMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct GroupMeta {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub avatar: String,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct GroupDeletion {
    pub id: String,
    #[serde(default)]
    pub deleted_at: i64,
    #[serde(default)]
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct GroupDeparture {
    pub group_id: String,
    pub member_ip: String,
    #[serde(default)]
    pub left_at: i64,
    #[serde(default)]
    pub members: Vec<String>,
}

/// A member explicitly added back to a group by one of its current members.
/// The full member list lets the returning device restore its group even after
/// it removed the local copy when it left.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct GroupRejoin {
    pub group_id: String,
    pub member_ip: String,
    #[serde(default)]
    pub joined_at: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatReaction {
    #[serde(default)]
    pub emoji: String,
    #[serde(default)]
    pub by: Vec<String>, // nicknames who chose this emoji
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMsg {
    pub id: String,
    pub peer: String, // peer ip (conversation id)
    pub from_me: bool,
    pub from_nick: String,
    pub text: String,
    pub ts: String,
    #[serde(default)]
    pub sent_at: i64,
    #[serde(default)]
    pub image: String, // legacy single image dataURL
    #[serde(default)]
    pub images: Vec<String>, // multiple image dataURLs
    #[serde(default)]
    pub audio: String, // local absolute path to an audio file in chat ("")
    #[serde(default)]
    pub forwarded_from: String,
    #[serde(default)]
    pub status: String, // "" (legacy) | sent | delivered
    #[serde(default)]
    pub delivered_to: Vec<String>, // ips that acknowledged a group message
    #[serde(default)]
    pub read: bool,
    #[serde(default)]
    pub reply_to: String, // id of the quoted message ("" = none)
    #[serde(default)]
    pub edited_at: i64, // UTC ms of the last edit (0 = never edited)
    #[serde(default)]
    pub reactions: Vec<ChatReaction>, // emoji -> who chose it
    /// Адрес, с которого сообщение пришло напрямую от автора (для правок в
    /// группах). Пусто у своих и у восстановленных из чужой истории.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from_ip: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingSend {
    pub contact: Option<String>, // target ip or None (choose in UI)
    pub file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallRecord {
    pub id: String,
    pub started_at: i64,
    pub duration_secs: u64,
    pub direction: String, // incoming | outgoing
    pub status: String, // completed | missed
    pub peer: String,
    #[serde(default)]
    pub participants: Vec<String>,
    #[serde(default)]
    pub group: bool,
}

pub struct AppState {
    pub settings: Mutex<Settings>,
    pub peers: Mutex<HashMap<String, Peer>>, // key = ip
    pub transfers: Mutex<Vec<TransferInfo>>,
    pub call_history: Mutex<Vec<CallRecord>>,
    pub chats: Mutex<HashMap<String, Vec<ChatMsg>>>, // key = peer ip or g:<group>
    pub groups: Mutex<Vec<Group>>,
    pub group_sync: Mutex<GroupSyncState>,
    pub cancel: Mutex<std::collections::HashSet<String>>, // transfer ids to abort
    pub incoming_file_requests: Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>,
    pub pending_send: Mutex<Option<PendingSend>>, // from CLI --send
    pub busy: std::sync::atomic::AtomicBool, // in call/screenshare: skip heavy scans
    pub scanning: std::sync::atomic::AtomicBool, // scan in progress (avoid parallel sweeps)
    pub unreachable_since: Mutex<HashMap<String, std::time::Instant>>,
    pub chat_sync_at: Mutex<HashMap<String, std::time::Instant>>,
    pub last_adapter_sig: std::sync::Mutex<String>, // network signature of active adapters (scan re-run on change)
    pub app_dir: PathBuf,
}

impl AppState {
    pub fn new(app_dir: PathBuf) -> Self {
        Self {
            settings: Mutex::new(Settings::default()),
            peers: Mutex::new(HashMap::new()),
            transfers: Mutex::new(Vec::new()),
            call_history: Mutex::new(Vec::new()),
            chats: Mutex::new(HashMap::new()),
            groups: Mutex::new(Vec::new()),
            group_sync: Mutex::new(GroupSyncState::default()),
            cancel: Mutex::new(std::collections::HashSet::new()),
            incoming_file_requests: Mutex::new(HashMap::new()),
            pending_send: Mutex::new(None),
            busy: std::sync::atomic::AtomicBool::new(false),
            scanning: std::sync::atomic::AtomicBool::new(false),
            unreachable_since: Mutex::new(HashMap::new()),
            chat_sync_at: Mutex::new(HashMap::new()),
            last_adapter_sig: std::sync::Mutex::new(String::new()),
            app_dir,
        }
    }
    pub fn settings_path(&self) -> PathBuf { self.app_dir.join("settings.json") }
    pub fn peers_path(&self) -> PathBuf { self.app_dir.join("peers.json") }
    pub fn history_path(&self) -> PathBuf { self.app_dir.join("history.json") }
    pub fn call_history_path(&self) -> PathBuf { self.app_dir.join("call-history.json") }
    pub fn chats_path(&self) -> PathBuf { self.app_dir.join("chats.json") }
    pub fn groups_path(&self) -> PathBuf { self.app_dir.join("groups.json") }
    pub fn group_sync_path(&self) -> PathBuf { self.app_dir.join("group-sync.json") }
}


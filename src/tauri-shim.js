// Shim: в Tauri — настоящие invoke/listen, в браузере — демо (сеть недоступна).
// Детект по обоим мостам: window.__TAURI__ (нужен withGlobalTauri)
// и низкоуровневый window.__TAURI_INTERNALS__ (есть всегда).
const T = () => window.__TAURI__;
const TI = () => window.__TAURI_INTERNALS__;
const hasInvoke = () =>
  !!(T()?.core?.invoke || TI()?.invoke);
export const isDemo = !hasInvoke();
export const bridgeInfo = () =>
  `__TAURI__=${!!T()} core=${!!T()?.core?.invoke} event=${!!T()?.event?.listen} internals=${!!TI()}`;

export function tauriFile(path) {
  try {
    if (!path) return "";
    if (TI()?.convertFileSrc) return TI().convertFileSrc(path);
    if (T()?.core?.convertFileSrc) return T().core.convertFileSrc(path);
  } catch {}
  return "file:///" + String(path).replace(/\\/g, "/");
}

export async function invoke(cmd, args = {}) {
  if (T()?.core?.invoke) return T().core.invoke(cmd, args);
  if (TI()?.invoke) return TI().invoke(cmd, args || {});
  // --- demo: всё локально, сети нет ---
  await new Promise((r) => setTimeout(r, 60));
  switch (cmd) {
    case "get_settings": return demo.settings;
    case "save_settings": demo.settings = args.settings; persist(); return;
    case "get_profile": return demo.settings.profile;
    case "save_profile": demo.settings.profile = { nickname: args.nickname, avatar: args.avatar }; persist(); return demo.settings.profile;
    case "set_peer_avatar": return;
    case "get_my_ips": return ["26.0.0.1 (демо)"];
    case "get_my_adapters": return [{ name: "Radmin VPN (демо)", ip: "26.0.0.1", prefix: 24, up: true }, { name: "Ethernet (демо)", ip: "192.168.1.10", prefix: 24, up: true }];
    case "get_peers": return demoPeers();
    case "add_peer": return { ip: args.ip, name: args.name || args.ip };
    case "rename_peer": case "remove_peer": return;
    case "toggle_trust": return true;
    case "ping_peer": return null;
    case "scan_network": return [];
    case "get_transfers": return [];
    case "send_file": throw "демо-режим: запусти приложение";
    case "open_loaddrop": return;
    case "move_loaddrop": return;
    case "get_loaddrop": return "C:\\LoadDrop (демо)";
    case "get_chat": return demoChat(args.peerIp);
    case "send_chat": throw "демо-режим: запусти приложение";
    case "chat_edit": case "chat_react": case "chat_typing": return;
    case "get_groups": return [];
    case "create_group": case "send_group_chat": throw "демо-режим: запусти приложение";
    case "rename_group": case "delete_group": case "leave_group": case "group_add_member": case "group_remove_member": return;
    case "sig_send": return;
    case "pick_files": case "pick_image": case "pick_folder": case "pick_audio": return null;
    case "clear_peers": case "open_firewall": case "set_ctx_menu": case "refresh_ctx_menu": case "fix_mic_blocked": return null;
    case "take_mic_reset_flag": return false;
    case "clear_cache": return "демо: нечего чистить";
    case "open_mic_settings": case "open_notif_settings": return;
    case "send_update": case "apply_update": throw "демо-режим: запусти приложение";
    case "scan_debug": return { my_ips: ["демо"], radmin_ips: [], tcp_port: 47831, sig_port: 47832, contacts_total: 0, online: [] };
    case "factory_reset": localStorage.clear(); location.reload(); return;
    case "read_image_dataurl": throw "демо-режим";
    default: return null;
  }
}

export async function listen(event, handler) {
  if (window.__TAURI__?.event?.listen) return window.__TAURI__.event.listen(event, handler);
  return () => {};
}

// демо-настройки только для просмотра вёрстки
const demo = {
  settings: JSON.parse(localStorage.getItem("sd-settings") || "null") || {
    device_name: "My-PC", loaddrop_dir: "C:\\LoadDrop", tcp_port: 47831, sig_port: 47832,
    auto_accept_trusted: true, auto_accept_limit_mb: 500,
    minimize_to_tray: true, autostart: false, sound: true, mic_id: "", speaker_id: "", camera_id: "",
    theme: { bg_mode: "gradient", bg_color: "#141824", bg_gradient: "linear-gradient(135deg,#1a2138,#0d1020)", bg_image: "", blur: 14, opacity: 0.85, accent: "#5b8cff", radius: 18, font: "Segoe UI", custom_css: "" },
    call_ui: { tile_size: 120, avatar_size: 120, show_names: true, ring_color: "#23a559", ring_width: 4, people_pos: "bottom", show_controls: true, bg_mode: "gradient", bg_color: "#1b1230", bg_from: "#1b1230", bg_to: "#120d20", accent_color: "#7c3aed", text_color: "#f2f3f5", show_logo: true },
    profile: { nickname: "", avatar: "" },
  },
};
function persist() { localStorage.setItem("sd-settings", JSON.stringify(demo.settings)); }

// демо-контакты и переписка: чтобы вёрстку (и скриншоты для README) было видно
// не на пустом окне. Только браузерный демо-режим, в приложении не используется.
function demoPeer(ip, name, nickname, online, state = "idle", ping = 18) {
  return {
    ip, name, nickname, avatar: "", custom_avatar: "", trusted: true,
    supports_file_confirm: true, supports_chat_ack: true, supports_watch: true, version: "0.3.25",
    online, ping_ms: online ? ping : null, last_seen: online ? "сейчас" : "вчера",
    phash: "", state, detail: "", ips: [ip],
  };
}
function demoPeers() {
  return [
    demoPeer("26.0.0.2", "Лёша", "Alex", true, "idle", 12),
    demoPeer("26.0.0.3", "Маша", "Masha", true, "incall", 24),
    demoPeer("26.0.0.4", "Рабочий ПК", "", false),
  ];
}
function demoChat(ip) {
  if (ip !== "26.0.0.2") return [];
  const now = Date.now();
  const m = (id, mine, text, minAgo) => ({
    id, peer: ip, from_me: mine, from_nick: mine ? "Я" : "Alex", text,
    ts: new Date(now - minAgo * 60000).toLocaleTimeString("ru-RU", { hour: "2-digit", minute: "2-digit" }),
    sent_at: now - minAgo * 60000, image: "", images: [], audio: "",
    status: mine ? "delivered" : "", delivered_to: [], reply_to: "", edited_at: 0, reactions: [],
  });
  return [
    m("d1", false, "Привет! Скинешь запись вчерашней катки?", 14),
    m("d2", true, "Да, лови — 2,3 ГБ, через SafeDrop быстрее, чем через облако", 13),
    m("d3", false, "Дошло за минуту 🔥 Вечером созвон и посмотрим что-нибудь вместе?", 9),
    m("d4", true, "Давай, включу «Смотреть вместе» с YouTube", 8),
  ];
}

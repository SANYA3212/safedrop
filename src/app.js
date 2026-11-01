import { invoke, listen, isDemo, tauriFile, bridgeInfo } from "./tauri-shim.js";

function fft(re, im, n) {
  for (let i = 0; i < n; i++) {
    let j = 0;
    for (let b = n >> 1; b > 0; b >>= 1) { j |= (i & b) ? 1 : 0; j <<= 1; }
    j >>= 1;
    if (j > i) { [re[i], re[j]] = [re[j], re[i]]; [im[i], im[j]] = [im[j], im[i]]; }
  }
  for (let len = 2; len <= n; len <<= 1) {
    const ang = -2 * Math.PI / len;
    const wr = Math.cos(ang), wi = Math.sin(ang);
    for (let i = 0; i < n; i += len) {
      let cwr = 1, cwi = 0;
      for (let j = 0; j < len / 2; j++) {
        const ur = re[i + j], ui = im[i + j];
        const vr = re[i + j + len / 2] * cwr - im[i + j + len / 2] * cwi;
        const vi = re[i + j + len / 2] * cwi + im[i + j + len / 2] * cwr;
        re[i + j] = ur + vr; im[i + j] = ui + vi;
        re[i + j + len / 2] = ur - vr; im[i + j + len / 2] = ui - vi;
        const nwr = cwr * wr - cwi * wi;
        cwi = cwr * wi + cwi * wr; cwr = nwr;
      }
    }
  }
}

function ifft(re, im, n) {
  for (let i = 0; i < n; i++) im[i] = -im[i];
  fft(re, im, n);
  for (let i = 0; i < n; i++) { re[i] /= n; im[i] = -im[i] / n; }
}

// все немые падения — сразу тостом на экран
window.addEventListener("error", (e) => {
  try { toast("JS-ошибка: " + (e.message || e.error), true); } catch {}
});
window.addEventListener("unhandledrejection", (e) => {
  try { toast("Ошибка: " + (e.reason?.message || e.reason), true); } catch {}
});

const $ = (s) => document.querySelector(s);
let settings = null, profile = { nickname: "", avatar: "" };
let peers = [], selectedIp = null, chats = {}, unread = {};
let groups = [], selGroup = null, myIp = "";
const seenIds = new Set(); // защита от дублей сообщений
let pendingFile = null;

// ---------- helpers ----------
function toast(msg, err = false) {
  const d = document.createElement("div");
  d.className = "toast" + (err ? " err" : "");
  // Toasts are plain text. Drop legacy inline icon markup instead of showing
  // raw SVG/HTML or interpreting untrusted message content as HTML.
  d.textContent = String(msg ?? "")
    .replace(/<svg\b[^>]*>[\s\S]*?<\/svg>/gi, " ")
    .replace(/<\/?[a-z][^>]*>/gi, " ")
    .replace(/\s+/g, " ")
    .trim();
  const host = $("#toast");
  if (!host) return;
  host.appendChild(d);
  setTimeout(() => d.remove(), err ? 7000 : 4500);
}
function esc(s) { return String(s ?? "").replace(/[&<>"]/g, (c) => ({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;"}[c])); }
function fmtSpeed(bps) { if (!bps) return "—"; const u=["Б/с","КБ/с","МБ/с","ГБ/с"]; let i=0,v=bps; while(v>1024&&i<3){v/=1024;i++;} return `${v.toFixed(1)} ${u[i]}`; }
function fmtSize(b) { const u=["Б","КБ","МБ","ГБ","ТБ"]; let i=0,v=b||0; while(v>1024&&i<4){v/=1024;i++;} return `${v.toFixed(1)} ${u[i]}`; }
function fileName(p) { return String(p || "").split(/[\\/]/).pop(); }
function dispName(p) { if (p.name && p.name !== p.ip) return p.nickname ? `${p.name} · ${p.nickname}` : p.name; return p.nickname || p.name || p.ip; }
function dispAva(p) { return p.custom_avatar || p.avatar || ""; }
function avaInner(a, letter) { return a ? `<img src="${a}" />` : esc(letter); }
function avaHTML(p, cls) {
  const letter = (dispName(p) || "?")[0].toUpperCase();
  return `<span class="ava ${cls}${p.online === false ? " off" : ""}"><span>${avaInner(dispAva(p), letter)}</span></span>`;
}
function showHelp(title, html, actionLabel, actionFn) {
  $("#help-title").textContent = title;
  $("#help-body").innerHTML = html;
  const b = $("#help-ok");
  const nb = b.cloneNode(true); b.replaceWith(nb);
  nb.textContent = "Понятно";
  nb.onclick = () => $("#help-modal").classList.add("hidden");
  let ab = $("#help-act");
  if (ab) ab.remove();
  if (actionLabel) {
    ab = document.createElement("button");
    ab.id = "help-act"; ab.className = "danger big"; ab.textContent = actionLabel;
    ab.onclick = actionFn;
    nb.after(ab);
  }
  $("#help-modal").classList.remove("hidden");
}
$("#help-ok").onclick = () => $("#help-modal").classList.add("hidden");

// ---------- theme engine ----------
function rgbaToHex(s) {
  const m = String(s || "").match(/rgba?\((\d+),\s*(\d+),\s*(\d+)/);
  if (m) return "#" + [1, 2, 3].map((i) => Number(m[i]).toString(16).padStart(2, "0")).join("");
  if (/^#[0-9a-fA-F]{6}$/.test(String(s || "").trim())) return s.trim();
  return null;
}
function hexA(hex, a) {
  let h = String(hex || "#141824").replace("#", "");
  if (h.length === 3) h = h.split("").map((c) => c + c).join("");
  const n = parseInt(h.slice(0, 6), 16);
  if (Number.isNaN(n)) return `rgba(20,24,40,${a})`;
  return `rgba(${(n >> 16) & 255},${(n >> 8) & 255},${n & 255},${a})`;
}
function applyTheme() {
  const t = settings.theme || {};
  const base = t.bg_color || "#141824";
  const alpha = t.opacity ?? 0.85;
  const bg = $("#bg");
  if (t.bg_mode === "color") bg.style.background = base;
  else if (t.bg_mode === "image" && t.bg_image) bg.style.background = `url("${tauriFile(t.bg_image)}") center/cover`;
  else bg.style.background = t.bg_gradient || base;
  document.body.style.background = base;
  const r = document.documentElement.style;
  r.setProperty("--bg", base);
  r.setProperty("--accent", t.accent || "#5b8cff");
  r.setProperty("--button-gradient-from", t.button_gradient_from || "#5b8cff");
  r.setProperty("--button-gradient-to", t.button_gradient_to || "#9b59ff");
  r.setProperty("--button-gradient-angle", `${t.button_gradient_angle ?? 135}deg`);
  r.setProperty("--radius", `${t.radius ?? 18}px`);
  r.setProperty("--blur", `${t.blur ?? 14}px`);
  r.setProperty("--font", t.font || "Segoe UI");
  r.setProperty("--text", t.text_color || "#eef1f8");
  r.setProperty("--muted", t.muted_color || "#9aa3b8");
  r.setProperty("--btn", t.btn_color || "rgba(255,255,255,.1)");
  r.setProperty("--btn-text", t.btn_text || "#eef1f8");
  r.setProperty("--link", t.link_color || "#7aa2ff");
  r.setProperty("--panel", hexA(base, alpha));
  r.setProperty("--panel2", hexA(base, Math.min(1, alpha + 0.08)));
  r.setProperty("--card", "rgba(255,255,255,.055)");
  r.setProperty("--input", "rgba(0,0,0,.35)");
  r.setProperty("--hover", "rgba(255,255,255,.08)");
  $("#custom-css").textContent = t.custom_css || "";
  const bv = $("#th-blur-v"); if (bv) bv.textContent = `${t.blur ?? 14}px`;
  const ov = $("#th-opacity-v"); if (ov) ov.textContent = `${Math.round(alpha * 100)}%`;
  const rv = $("#th-radius-v"); if (rv) rv.textContent = `${t.radius ?? 18}px`;
  document.querySelectorAll(".seg-theme .seg-btn").forEach((b) => b.classList.toggle("active", b.dataset.bg === (t.bg_mode || "gradient")));
}

// ---------- call screen appearance ----------
const CALL_UI_DEFAULT = {
  tile_size: 120,
  avatar_size: 120,
  show_names: true,
  ring_color: "#23a559",
  ring_width: 4,
  people_pos: "bottom",
  show_controls: true,
  bg_mode: "gradient",
  bg_color: "#1b1230",
  bg_from: "#1b1230",
  bg_to: "#120d20",
  accent_color: "#7c3aed",
  text_color: "#f2f3f5",
  show_logo: true,
};
function callUiSettings() {
  return { ...CALL_UI_DEFAULT, ...(settings?.call_ui || {}) };
}
function applyCallUi() {
  const u = callUiSettings();
  const cs = $("#call-screen");
  if (cs) {
    cs.style.setProperty("--cs-tile", `${u.tile_size}px`);
    cs.style.setProperty("--cs-avatar", `${u.avatar_size}px`);
    cs.style.setProperty("--cs-ring", u.ring_color);
    cs.style.setProperty("--cs-ring-w", `${u.ring_width}px`);
    cs.style.setProperty("--cs-bg", u.bg_color);
    cs.style.setProperty("--cs-bg-from", u.bg_from);
    cs.style.setProperty("--cs-bg-to", u.bg_to);
    cs.style.setProperty("--cs-accent", u.accent_color);
    cs.style.setProperty("--cs-text", u.text_color);
    cs.classList.toggle("cs-solid", u.bg_mode === "solid");
    cs.classList.toggle("cs-hide-logo", !u.show_logo);
    cs.classList.toggle("cs-hide-names", !u.show_names);
    cs.classList.toggle("cs-people-center", u.people_pos === "center");
    cs.classList.toggle("cs-hide-controls", !u.show_controls);
  }
  const dlg = $("#dialog");
  if (dlg) {
    dlg.style.setProperty("--vtile-w", `${Math.round(Number(u.tile_size) * 1.08)}px`);
    dlg.style.setProperty("--vava", `${Math.round(Number(u.avatar_size) * 0.73)}px`);
    dlg.style.setProperty("--vring", u.ring_color);
    dlg.style.setProperty("--vring-w", `${u.ring_width}px`);
    dlg.classList.toggle("call-hide-names", !u.show_names);
  }
}
function syncCallUiControls() {
  const u = callUiSettings();
  const setVal = (id, v) => { const el = document.getElementById(id); if (el && document.activeElement !== el) el.value = v; };
  const setText = (id, v) => { const el = document.getElementById(id); if (el) el.textContent = v; };
  setVal("call-tile-size", u.tile_size); setText("call-tile-size-v", `${u.tile_size}px`);
  setVal("call-avatar-size", u.avatar_size); setText("call-avatar-size-v", `${u.avatar_size}px`);
  const names = $("#call-show-names"); if (names) names.checked = !!u.show_names;
  setVal("call-ring-color", u.ring_color);
  setVal("call-ring-width", u.ring_width); setText("call-ring-width-v", `${u.ring_width}px`);
  setVal("call-people-pos", u.people_pos);
  const ctl = $("#call-show-controls"); if (ctl) ctl.checked = !!u.show_controls;
  setVal("call-bg-mode", u.bg_mode);
  setVal("call-bg-color", u.bg_color);
  setVal("call-bg-from", u.bg_from);
  setVal("call-bg-to", u.bg_to);
  setVal("call-accent-color", u.accent_color);
  setVal("call-text-color", u.text_color);
  const logo = $("#call-show-logo"); if (logo) logo.checked = !!u.show_logo;
}
let callUiSaveTimer = null;
function readCallUiControls() {
  if (!settings) return;
  settings.call_ui = {
    tile_size: Number($("#call-tile-size")?.value) || CALL_UI_DEFAULT.tile_size,
    avatar_size: Number($("#call-avatar-size")?.value) || CALL_UI_DEFAULT.avatar_size,
    show_names: !!$("#call-show-names")?.checked,
    ring_color: $("#call-ring-color")?.value || CALL_UI_DEFAULT.ring_color,
    ring_width: Number($("#call-ring-width")?.value) || 0,
    people_pos: $("#call-people-pos")?.value === "center" ? "center" : "bottom",
    show_controls: !!$("#call-show-controls")?.checked,
    bg_mode: $("#call-bg-mode")?.value === "solid" ? "solid" : "gradient",
    bg_color: $("#call-bg-color")?.value || CALL_UI_DEFAULT.bg_color,
    bg_from: $("#call-bg-from")?.value || CALL_UI_DEFAULT.bg_from,
    bg_to: $("#call-bg-to")?.value || CALL_UI_DEFAULT.bg_to,
    accent_color: $("#call-accent-color")?.value || CALL_UI_DEFAULT.accent_color,
    text_color: $("#call-text-color")?.value || CALL_UI_DEFAULT.text_color,
    show_logo: !!$("#call-show-logo")?.checked,
  };
  applyCallUi();
  syncCallUiControls();
  clearTimeout(callUiSaveTimer);
  callUiSaveTimer = setTimeout(() => invoke("save_settings", { settings }).catch((e) => toast("" + e, true)), 250);
}
function bindCallUiControls() {
  ["call-tile-size", "call-avatar-size", "call-show-names", "call-ring-color", "call-ring-width", "call-people-pos", "call-show-controls", "call-bg-mode", "call-bg-color", "call-bg-from", "call-bg-to", "call-accent-color", "call-text-color", "call-show-logo"].forEach((id) => {
    const el = document.getElementById(id);
    if (!el) return;
    el.addEventListener("input", readCallUiControls);
    el.addEventListener("change", readCallUiControls);
  });
  const reset = $("#btn-call-ui-reset");
  if (reset) reset.onclick = () => {
    if (!settings) return;
    settings.call_ui = { ...CALL_UI_DEFAULT };
    applyCallUi(); syncCallUiControls();
    invoke("save_settings", { settings }).then(() => toast("Вид звонка сброшен")).catch((e) => toast("" + e, true));
  };
}
bindCallUiControls();

// ---------- load ----------
async function loadAll() {
  settings = await invoke("get_settings");
  try { profile = await invoke("get_profile"); } catch { profile = { nickname: "", avatar: "" }; }
  peers = await invoke("get_peers");
  const t = settings.theme || {};
  $("#th-color").value = t.bg_color || "#141824";
  $("#th-button-gradient-from").value = t.button_gradient_from || "#5b8cff";
  $("#th-button-gradient-to").value = t.button_gradient_to || "#9b59ff";
  $("#th-button-gradient-angle").value = t.button_gradient_angle ?? 135;
  $("#th-button-gradient-angle-v").textContent = `${$("#th-button-gradient-angle").value}°`;
  $("#button-gradient-preview").style.background = `linear-gradient(${t.button_gradient_angle ?? 135}deg,${t.button_gradient_from || "#5b8cff"},${t.button_gradient_to || "#9b59ff"})`;
  $("#th-blur").value = t.blur ?? 14;
  $("#th-opacity").value = Math.round((t.opacity ?? 0.85) * 100);
  $("#th-accent").value = t.accent || "#5b8cff";
  $("#th-radius").value = t.radius ?? 18;
  $("#th-font").value = t.font || "Segoe UI";
  $("#th-css").value = t.custom_css || "";
  const tc = $("#th-text"); if (tc) tc.value = t.text_color || "#eef1f8";
  const mc = $("#th-muted"); if (mc) mc.value = t.muted_color || "#9aa3b8";
  const bc = $("#th-btn"); if (bc) bc.value = rgbaToHex(t.btn_color) || "#333a4d";
  const bt = $("#th-btntext"); if (bt) bt.value = t.btn_text || "#eef1f8";
  const lk = $("#th-link"); if (lk) lk.value = t.link_color || "#7aa2ff";
  $("#set-device").value = settings.device_name;
  $("#set-port").value = settings.tcp_port;
  $("#set-udp").checked = !!settings.udp_enabled;
  $("#set-udpport").value = settings.udp_port || 47833;
  $("#set-udpport").disabled = !settings.udp_enabled;
  $("#set-autotrust").checked = settings.auto_accept_trusted;
  $("#set-limit").value = settings.auto_accept_limit_mb;
  $("#set-autostart").checked = settings.autostart;
  $("#set-tray").checked = settings.minimize_to_tray;
  $("#set-ctxmenu").checked = !!settings.ctx_menu;
  const st = $("#set-show-typing");
  if (st) st.checked = settings.show_typing !== false;
  renderAccordionState();
  renderScanUI();
  renderMyAdapters();
  $("#set-nick").value = profile.nickname || "";
  const ringMelody = settings.ringback_melody || "default";
  document.querySelectorAll("[data-ringback]").forEach((b) => b.classList.toggle("active", b.dataset.ringback === ringMelody));
  const ringbackName = $("#ringback-name");
  if (ringbackName) {
    const selectedMelody = document.querySelector(`[data-ringback="${CSS.escape(ringMelody)}"]`);
    ringbackName.textContent = ringMelody === "custom"
      ? (settings.ringback_file ? fileName(settings.ringback_file) : "Своя мелодия")
      : (selectedMelody?.textContent || "Стандартный гудок");
  }
  const incomingMelody = settings.incoming_ringtone_melody || "default";
  document.querySelectorAll("[data-incoming-ring]").forEach((b) => b.classList.toggle("active", b.dataset.incomingRing === incomingMelody));
  const incomingName = $("#incoming-ring-name");
  if (incomingName) incomingName.textContent = settings.ringtone ? fileName(settings.ringtone) : (document.querySelector(`[data-incoming-ring="${CSS.escape(incomingMelody)}"]`)?.textContent || "Стандартный");
  const joinMelody = settings.call_join_melody || "classic";
  document.querySelectorAll("[data-call-join]").forEach((b) => b.classList.toggle("active", !settings.call_join_sound && b.dataset.callJoin === joinMelody));
  const joinName = $("#call-join-sound-name");
  if (joinName) joinName.textContent = settings.call_join_sound
    ? fileName(settings.call_join_sound)
    : (document.querySelector(`[data-call-join="${CSS.escape(joinMelody)}"]`)?.textContent || "Классика");
  const leaveMelody = settings.call_leave_melody || "default";
  document.querySelectorAll("[data-call-leave]").forEach((b) => b.classList.toggle("active", !settings.call_leave_sound && b.dataset.callLeave === leaveMelody));
  const leaveName = $("#call-leave-sound-name");
  if (leaveName) leaveName.textContent = settings.call_leave_sound
    ? fileName(settings.call_leave_sound)
    : (document.querySelector(`[data-call-leave="${CSS.escape(leaveMelody)}"]`)?.textContent || "Стандартный");
  $("#msg-name").textContent = settings.message_sound ? fileName(settings.message_sound) : "Стандартный «блуп»";
  $("#snd-file-name").textContent = settings.file_sound ? fileName(settings.file_sound) : "Стандартный сигнал";
  $("#gate-vol").value = settings.noise_gate ?? 12;
  $("#gate-vol-v").textContent = `${settings.noise_gate ?? 12}`;
  $("#noise-engine").value = ["rnnoise", "webrtc", "off"].includes(settings.noise_filter) ? settings.noise_filter : "rnnoise";
  $("#noise-strength").value = settings.noise_strength ?? 100;
  $("#noise-strength-v").textContent = `${settings.noise_strength ?? 100}%`;
  $("#echo-cancel").checked = settings.echo_cancellation !== false;
  $("#auto-gain").checked = settings.auto_gain_control !== false;
  $("#noise-smooth").value = settings.noise_smooth ?? 50;
  $("#noise-smooth-v").textContent = `${settings.noise_smooth ?? 50}%`;
  $("#share-q").value = settings.share_quality || "medium";
  $("#share-q").onchange = (e) => { settings.share_quality = e.target.value; invoke("save_settings", { settings }).then(() => toast(`Качество: ${shareQ().label} — применится к следующей демонстрации`)).catch((err) => toast("" + err, true)); };
  $("#mic-vol").value = settings.mic_volume ?? 100;
  $("#mic-vol-v").textContent = `${settings.mic_volume ?? 100}%`;
  syncCallUiControls(); applyCallUi();
  renderMyAvatar();
  applyTheme(); renderAll();
  try {
    const ips = await invoke("get_my_ips");
    $("#me-ip").textContent = ips.join(" · ");
    myIp = ips.find((x) => x.startsWith("26.")) || ips[0] || "";
  } catch {}
  try { groups = await invoke("get_groups"); normalizeGroups(); } catch { groups = []; }
  renderGroups();
  $("#me-nick").textContent = profile.nickname || settings.device_name;
  try {
    const ld = await invoke("get_loaddrop");
    $("#loaddrop-path").textContent = ld;
    $("#set-loaddrop-view").textContent = ld;
  } catch (e) { toast("LoadDrop: " + e, true); }
  await initDevices();
  try {
    const b = await invoke("get_build_info");
    const dt = b.build_time && b.build_time !== "?" ? new Date(Number(b.build_time) * 1000).toLocaleString("ru-RU") : "?";
    $("#build-info").textContent = `Сборка v${b.version} от ${dt}`;
    console.log("[SafeDrop] running:", b.exe);
  } catch {}
}
function renderMyAvatar() {
  const letter = ((profile.nickname || settings?.device_name || "?"))[0].toUpperCase();
  const inner = avaInner(profile.avatar, letter);
  $("#me-ava").outerHTML = `<span id="me-ava" class="ava sm"><span>${inner}</span></span>`;
  $("#set-my-avatar").outerHTML = `<span id="set-my-avatar" class="ava lg"><span>${inner}</span></span>`;
  document.getElementById("set-my-avatar").onclick = pickMyAvatar;
}

// ---------- contacts ----------
function filteredPeers() {
  const q = ($("#peer-search").value || "").toLowerCase();
  return peers.filter((p) => !q || dispName(p).toLowerCase().includes(q) || p.ip.includes(q));
}
function renderAll() { renderContacts(); renderGroups(); renderConv(); renderPeerCard(); }
function renderContacts() {
  const box = $("#contact-list"); box.innerHTML = "";
  const list = filteredPeers();
  const on = peers.filter((p) => p.online).length;
  $("#cnt-online").textContent = peers.length ? `${on} в сети` : "";
  if (!list.length) {
    box.innerHTML = peers.length ? `<div class="empty-hint">Ничего не найдено.</div>`
      : `<div class="empty-hint"><b>Пока пусто.</b><br>1. Нажми <svg class="ic" viewBox="0 0 24 24"><circle cx="10" cy="10" r="8"/><path d="M21.71,21.71a1,1,0,0,1-1.42,0L15.61,17A8.67,8.67,0,0,0,17,15.61l4.68,4.68A1,1,0,0,1,21.71,21.71Z"/></svg> — автоскан сети Radmin добавит всех.<br>2. Или <svg class="ic" viewBox="0 0 24 24"><path d="M19,11H13V5a1,1,0,0,0-2,0v6H5a1,1,0,0,0,0,2h6v6a1,1,0,0,0,2,0V13h6a1,1,0,0,0,0-2Z"/></svg> и введи IP вручную (26.x.x.x).<br>Контакты сохраняются автоматически.</div>`;
    return;
  }
  for (const p of list) {
    const d = document.createElement("div");
    d.className = "contact" + (p.ip === selectedIp ? " active" : "");
    const typingKey = p.ip; // личный чат
    const typingLabelText = typingPeers.has(typingKey) && typingPeers.get(typingKey)?.size ? typingLabel(typingKey) : "";
    d.innerHTML = `${avaHTML(p, "sm")}<div class="meta"><b><span class="nm">${esc(dispName(p))}</span>${typingLabelText ? `<span class="typing-badge" style="margin-left:6px;font-size:11px;color:#3ddc84;">${esc(typingLabelText)}</span>` : ""}${p.trusted ? '<svg class="ic" viewBox="0 0 24 24" style="color:#f5c518"><path d="M12,2l3.09,6.26L22,9.27l-5,4.87,1.18,6.88L12,17.77l-6.18,3.25L7,14.14,2,9.27l6.91-1.01Z"/></svg>' : ""}</b><small>${p.online ? `<span style="color:#3ddc84">●</span> ${p.ping_ms ?? "?"} мс · ${esc(p.ip)}` : `<span style="color:#666">●</span> ${esc(p.ip)}`}${stateBadge(p.state)}${(p.state && p.state !== "idle" && p.detail) ? ` · ${esc(p.detail)}` : ""}</small></div>${unread[p.ip] ? `<span class="unread">${unread[p.ip]}</span>` : ""}`;
    d.onclick = () => selectPeer(p.ip);
    box.appendChild(d);
  }
}
function sel() { return peers.find((p) => p.ip === selectedIp) || null; }
async function selectPeer(ip) {
  selectedIp = ip; selGroup = null; unread[ip] = 0;
  setReply(null);
  hideChatSearch();
  renderTypingBadge(curKey());
  renderContacts(); renderGroups(); renderConv(); renderPeerCard();
  try { chats[ip] = await invoke("get_chat", { peerIp: ip }); chats[ip].forEach((m) => seenIds.add(m.id)); } catch { chats[ip] = []; }
  renderChat();
}
function renderConv() {
  const g = curGroup();
  if (g) {
    const on = g.members.filter((m) => peers.find((p) => p.ip === m)?.online);
    $("#conv-name").textContent = g.name;
    $("#conv-sub").innerHTML = `<svg class="ic" viewBox="0 0 24 24" style="vertical-align:-3px;margin-right:2px"><circle cx="9" cy="8" r="3"/><path d="M14,11H4A2,2,0,0,0,2,13v5H6V15H4V13A1,1,0,0,1,5,12H13a1,1,0,0,1,1,1v2H12v3h4V13A2,2,0,0,0,14,11Z"/><circle cx="17" cy="9" r="2.5"/><path d="M20,11.5A1.5,1.5,0,0,0,18.5,10H15a3,3,0,0,1,2,2.82V18H15v3h4V14.82A3,3,0,0,1,20,13Z"/></svg> ${g.members.length} · <span style="color:#3ddc84">●</span> ${on.length} в сети`;
    $("#conv-ava").outerHTML = groupAva(g, "md").replace('class="ava md', 'id="conv-ava" class="ava md');
    $("#btn-call").innerHTML = '<svg class="ic" viewBox="0 0 24 24" style="vertical-align:-3px;margin-right:4px"><path d="M21,15v3.93a2,2,0,0,1-2.29,2A18,18,0,0,1,3.14,5.29,2,2,0,0,1,5.13,3H9a1,1,0,0,1,1,.89,10.74,10.74,0,0,0,1,3.78,1,1,0,0,1-.42,1.26l-.86.49a1,1,0,0,0-.33,1.46,14.08,14.08,0,0,0,3.69,3.69,1,1,0,0,0,1.46-.33l.49-.86A1,1,0,0,1,16.33,13a10.74,10.74,0,0,0,3.78,1A1,1,0,0,1,21,15Z"/></svg> Группе';
    $("#btn-hangup").classList.toggle("hidden", !groupCall);
    return;
  }
  $("#btn-call").innerHTML = '<svg class="ic" viewBox="0 0 24 24" style="vertical-align:-3px;margin-right:4px"><path d="M21,15v3.93a2,2,0,0,1-2.29,2A18,18,0,0,1,3.14,5.29,2,2,0,0,1,5.13,3H9a1,1,0,0,1,1,.89,10.74,10.74,0,0,0,1,3.78,1,1,0,0,1-.42,1.26l-.86.49a1,1,0,0,0-.33,1.46,14.08,14.08,0,0,0,3.69,3.69,1,1,0,0,0,1.46-.33l.49-.86A1,1,0,0,1,16.33,13a10.74,10.74,0,0,0,3.78,1A1,1,0,0,1,21,15Z"/></svg> Звонок';
  const p = sel();
  if (!p) {
    $("#conv-name").textContent = "Выбери контакт";
    $("#conv-sub").textContent = "слева, чтобы начать общение";
    $("#conv-ava").outerHTML = `<span id="conv-ava" class="ava md"><span>?</span></span>`;
    return;
  }
  $("#conv-name").textContent = dispName(p);
  $("#conv-sub").textContent = `${p.ip} · ${p.online ? `в сети · ${p.ping_ms ?? "?"} мс` : "не в сети"}`;
  $("#conv-ava").outerHTML = avaHTML(p, "md").replace('class="ava md', 'id="conv-ava" class="ava md');
  $("#btn-hangup").classList.toggle("hidden", !currentCall);
}
function renderPeerCard() {
  const g = curGroup();
  if (g) {
    const on = g.members.filter((m) => peers.find((p) => p.ip === m)?.online);
    $("#rc-nick").textContent = g.name;
    $("#rc-sub").textContent = g.members.map((m) => { const p = peers.find((x) => x.ip === m); return p ? dispName(p) : m; }).join(", ") || "пустая группа";
    $("#rc-ping").innerHTML = `<svg class="ic" viewBox="0 0 24 24" style="vertical-align:-3px;margin-right:2px"><circle cx="9" cy="8" r="3"/><path d="M14,11H4A2,2,0,0,0,2,13v5H6V15H4V13A1,1,0,0,1,5,12H13a1,1,0,0,1,1,1v2H12v3h4V13A2,2,0,0,0,14,11Z"/><circle cx="17" cy="9" r="2.5"/><path d="M20,11.5A1.5,1.5,0,0,0,18.5,10H15a3,3,0,0,1,2,2.82V18H15v3h4V14.82A3,3,0,0,1,20,13Z"/></svg> ${g.members.length} · <span style="color:#3ddc84">●</span> ${on.length}`;
    $("#rc-avatar").outerHTML = groupAva(g, "lg").replace('class="ava lg', 'id="rc-avatar" class="ava lg');
    $("#rc-rename").onclick = renameSel;
    $("#rc-rename").title = "Изменить группу";
    $("#rc-del").onclick = delSel;
    $("#rc-del").title = "Удалить группу";
    $("#rc-avatar-btn").style.display = "none";
    $("#rc-trust").style.display = "none";
    return;
  }
  $("#rc-avatar-btn").style.display = "";
  $("#rc-trust").style.display = "";
  $("#rc-rename").onclick = renameSel;
  $("#rc-rename").title = "Локальное имя";
  $("#rc-del").onclick = delSel;
  $("#rc-del").title = "Удалить";
  $("#rc-avatar-btn").onclick = pickPeerAvatar;
  $("#rc-trust").title = "Доверенный";
  $("#rc-trust").onclick = toggleTrustSel;
  const p = sel();
  if (!p) { $("#rc-nick").textContent = "—"; $("#rc-sub").textContent = "Выбери контакт слева"; return; }
  $("#rc-nick").textContent = dispName(p);
  $("#rc-sub").textContent = p.nickname ? `ник: ${p.nickname} · ${p.ip}` : p.ip;
  $("#rc-ping").innerHTML = p.online ? `<span style="color:#3ddc84">●</span> в сети · ${p.ping_ms ?? "?"} мс` : `<span style="color:#666">●</span> не в сети`;
  $("#rc-avatar").outerHTML = avaHTML(p, "lg").replace('class="ava lg', 'id="rc-avatar" class="ava lg');
  $("#rc-trust").innerHTML = p.trusted ? '<svg class="ic" viewBox="0 0 24 24" style="color:#f5c518"><path d="M12,2l3.09,6.26L22,9.27l-5,4.87,1.18,6.88L12,17.77l-6.18,3.25L7,14.14,2,9.27l6.91-1.01Z"/></svg>' : '<svg class="ic" viewBox="0 0 24 24"><path d="M12,2l3.09,6.26L22,9.27l-5,4.87,1.18,6.88L12,17.77l-6.18,3.25L7,14.14,2,9.27l6.91-1.01Z" fill="none" stroke="currentColor" stroke-width="1.5"/></svg>';
}

// ---------- groups ----------
function curKey() { return selGroup ? `g:${selGroup}` : selectedIp; }
function curGroup() { return groups.find((g) => g.id === selGroup) || null; }
// чистим состав групп: без дублей (иначе двоятся участники звонка)
function normalizeGroups() {
  for (const g of groups) {
    if (Array.isArray(g.members)) g.members = [...new Set(g.members)];
    else g.members = [];
  }
}
function groupAva(g, cls) {
  const letter = esc(((g?.name || "?"))[0].toUpperCase());
  return `<span class="ava ${cls}"><span>${avaInner(g?.avatar || "", letter)}</span></span>`;
}
let groupAvatarEdit = "";
function renderGroupAvatarPreview() {
  const preview = $("#group-avatar-preview");
  if (!preview) return;
  const letter = esc(($("#group-name")?.value || "?")[0].toUpperCase());
  preview.innerHTML = `<span>${avaInner(groupAvatarEdit, letter)}</span>`;
}
function renderGroups() {
  const box = $("#group-list"); if (!box) return; box.innerHTML = "";
  const q = ($("#peer-search").value || "").toLowerCase();
  const list = groups.filter((g) => !q || g.name.toLowerCase().includes(q));
  if (!list.length) { box.innerHTML = `<div class="empty-hint">Групп нет. Создай кнопкой ＋.</div>`; return; }
  for (const g of list) {
    const on = g.members.filter((m) => peers.find((p) => p.ip === m)?.online).length;
    const busy = g.members.some((m) => { const st = peers.find((p) => p.ip === m)?.state || ""; return st && st !== "idle"; });
    const d = document.createElement("div");
    d.className = "contact" + (selGroup === g.id ? " active" : "");
    const groupTypingKey = `g:${g.id}`;
    const groupTypingText = typingPeers.has(groupTypingKey) && typingPeers.get(groupTypingKey)?.size ? typingLabel(groupTypingKey) : "";
    d.innerHTML = `${groupAva(g, "sm")}<div class="meta"><b><span class="nm">${esc(g.name)}</span>${groupTypingText ? `<span class="typing-badge" style="margin-left:6px;font-size:11px;color:#3ddc84;">${esc(groupTypingText)}</span>` : ""}</b><small><svg class="ic" viewBox="0 0 24 24" style="vertical-align:-3px;margin-right:2px"><circle cx="9" cy="8" r="3"/><path d="M14,11H4A2,2,0,0,0,2,13v5H6V15H4V13A1,1,0,0,1,5,12H13a1,1,0,0,1,1,1v2H12v3h4V13A2,2,0,0,0,14,11Z"/><circle cx="17" cy="9" r="2.5"/><path d="M20,11.5A1.5,1.5,0,0,0,18.5,10H15a3,3,0,0,1,2,2.82V18H15v3h4V14.82A3,3,0,0,1,20,13Z"/></svg> ${g.members.length} · <span style="color:#3ddc84">●</span> ${on}${busy ? ' · <svg class="ic" viewBox="0 0 24 24" style="vertical-align:-3px"><path d="M21,15v3.93a2,2,0,0,1-2.29,2A18,18,0,0,1,3.14,5.29,2,2,0,0,1,5.13,3H9a1,1,0,0,1,1,.89,10.74,10.74,0,0,0,1,3.78,1,1,0,0,1-.42,1.26l-.86.49a1,1,0,0,0-.33,1.46,14.08,14.08,0,0,0,3.69,3.69,1,1,0,0,0,1.46-.33l.49-.86A1,1,0,0,1,16.33,13a10.74,10.74,0,0,0,3.78,1A1,1,0,0,1,21,15Z"/></svg>' : ""}${unread[`g:${g.id}`] ? ` · <svg class="ic" viewBox="0 0 24 24" style="vertical-align:-3px"><path d="M20,2H4A2,2,0,0,0,2,4V22l4-4H20a2,2,0,0,0,2-2V4A2,2,0,0,0,20,2Z"/></svg>${unread[`g:${g.id}`]}` : ""}</small></div>`;
    d.onclick = () => selectGroup(g.id);
    box.appendChild(d);
  }
}
async function selectGroup(id) {
  selGroup = id; selectedIp = null; unread[`g:${id}`] = 0;
  setReply(null);
  hideChatSearch();
  renderTypingBadge(curKey());
  renderContacts(); renderGroups(); renderConv(); renderPeerCard();
  try { chats[`g:${id}`] = await invoke("get_chat", { peerIp: `g:${id}` }); chats[`g:${id}`].forEach((m) => seenIds.add(m.id)); } catch { chats[`g:${id}`] = []; }
  renderChat();
}
let editingGroup = null;
function openGroupModal(g) {
  editingGroup = g?.id || null;
  $("#group-title").textContent = g ? "Изменить группу" : "Новая группа";
  $("#group-name").value = g?.name || "";
  groupAvatarEdit = g?.avatar || "";
  renderGroupAvatarPreview();
  $("#group-del").classList.toggle("hidden", !g);
  $("#group-leave").classList.toggle("hidden", !g);
  const box = $("#group-members"); box.innerHTML = "";
  const inG = new Set(g?.members || []);
  for (const p of peers) {
    const l = document.createElement("label");
    l.innerHTML = `<input type="checkbox" data-ip="${esc(p.ip)}" ${inG.has(p.ip) ? "checked" : ""} /> ${esc(dispName(p))} <span class="muted">${esc(p.ip)}</span>`;
    box.appendChild(l);
  }
  if (!peers.length) box.innerHTML = `<div class="empty-hint">Нет контактов — сначала добавь.</div>`;
  $("#group-modal").classList.remove("hidden");
}
$("#group-cancel").onclick = () => $("#group-modal").classList.add("hidden");
$("#btn-group-add").onclick = () => openGroupModal(null);
$("#group-name").addEventListener("input", renderGroupAvatarPreview);
$("#group-avatar-pick").onclick = async () => {
  try {
    const path = await invoke("pick_image");
    if (!path) return;
    const raw = await invoke("read_image_dataurl", { path });
    groupAvatarEdit = await shrinkChatImage(raw, 256);
    renderGroupAvatarPreview();
  } catch (e) { toast("" + e, true); }
};
$("#group-avatar-clear").onclick = () => { groupAvatarEdit = ""; renderGroupAvatarPreview(); };
$("#group-save").onclick = async () => {
  const name = $("#group-name").value.trim();
  if (!name) return toast("Введи название", true);
  const members = [...document.querySelectorAll("#group-members input:checked")].map((x) => x.dataset.ip);
  try {
    if (editingGroup) {
      await invoke("rename_group", { id: editingGroup, name });
      const cur = groups.find((g) => g.id === editingGroup);
      const old = new Set(cur?.members || []);
      for (const m of members.filter((x) => !old.has(x))) await invoke("group_add_member", { id: editingGroup, ip: m });
      for (const m of [...old].filter((x) => !members.includes(x))) await invoke("group_remove_member", { id: editingGroup, ip: m });
      if ((cur?.avatar || "") !== groupAvatarEdit) await invoke("set_group_avatar", { id: editingGroup, avatar: groupAvatarEdit });
    } else {
      const g = await invoke("create_group", { name, members });
      editingGroup = g.id;
      if (groupAvatarEdit) await invoke("set_group_avatar", { id: editingGroup, avatar: groupAvatarEdit });
    }
    groups = await invoke("get_groups"); normalizeGroups(); renderGroups();
    $("#group-modal").classList.add("hidden"); toast("Группа сохранена <svg class='ic' viewBox='0 0 24 24'><path d='M9,16.17,4.83,12l-1.42,1.41L9,19,21,7l-1.41-1.41Z'/></svg>");
  } catch (e) { toast("" + e, true); }
};
$("#group-del").onclick = async () => {
  if (!editingGroup) return;
  if (!await askConfirm("Удалить группу?", "Переписка группы тоже удалится.", "Удалить")) return;
  try {
    await invoke("delete_group", { id: editingGroup });
    if (selGroup === editingGroup) { selGroup = null; }
    groups = await invoke("get_groups"); renderGroups(); renderConv(); renderPeerCard(); renderChat();
    $("#group-modal").classList.add("hidden");
  } catch (e) { toast("" + e, true); }
};
$("#group-leave").onclick = async () => {
  if (!editingGroup) return;
  const group = groups.find((g) => g.id === editingGroup);
  if (!await askConfirm(`Выйти из группы «${group?.name || "Группа"}»?`, "Группа останется у остальных участников, а у тебя больше не будет появляться от новых сообщений.", "Выйти")) return;
  try {
    await invoke("leave_group", { id: editingGroup });
    if (selGroup === editingGroup) selGroup = null;
    groups = await invoke("get_groups"); normalizeGroups(); renderAll(); renderChat();
    $("#group-modal").classList.add("hidden"); toast("Ты вышел из группы");
  } catch (e) { toast("" + e, true); }
};

// ---------- chat (строки как в Discord: ава + ник + время) ----------
// ссылки кликабельны, ютуб и видео — плеером прямо в чате
function linkHTML(url) {
  const safe = esc(url);
  const yt = url.match(/(?:youtube\.com\/(?:watch\?[^ ]*v=|shorts\/|embed\/|live\/)|youtu\.be\/)([A-Za-z0-9_-]{6,})/);
  if (yt) {
    return `<div class="yt" data-yt="${yt[1]}"><img src="https://i.ytimg.com/vi/${yt[1]}/hqdefault.jpg" loading="lazy" alt="" /><button><svg class="ic" viewBox="0 0 24 24" style="width:22px;height:22px"><path d="M8,5v14l11-7z"/></svg></button></div><div><a class="chat-link" data-url="${safe}" href="#">${safe}</a></div>`;
  }
  if (/\.(mp4|webm|ogg)(\?[^ ]*)?$/i.test(url)) {
    return `<video class="chat-vid" controls preload="metadata" src="${safe}"></video><div><a class="chat-link" data-url="${safe}" href="#">${safe}</a></div>`;
  }
  return `<a class="chat-link" data-url="${safe}" href="#">${safe}</a>`;
}
function renderMsgText(raw) {
  const parts = String(raw || "").split(/(https?:\/\/[^\s<>"']+)/g);
  return parts.map((p, i) => (i % 2 === 0 ? esc(p) : linkHTML(p))).join("");
}
document.addEventListener("click", (e) => {
  const a = e.target.closest(".chat-link");
  if (a) {
    e.preventDefault();
    invoke("open_url", { url: a.dataset.url }).catch((err) => toast("" + err, true));
    return;
  }
  const t = e.target.closest(".yt");
  if (t && t.dataset.yt) {
    playingYt.add(t.dataset.yt);
    t.outerHTML = ytFrameHTML(t.dataset.yt, true);
  }
});
const playingYt = new Set(); // ютубы, которые уже смотрим — не сбрасывать
function ytFrameHTML(id, autoplay) {
  return `<iframe class="yt-frame" src="https://www.youtube-nocookie.com/embed/${id}?autoplay=${autoplay ? 1 : 0}&rel=0" allow="autoplay; encrypted-media; fullscreen" allowfullscreen></iframe>`;
}
function pushMsg(m) {
  if (!m || seenIds.has(m.id)) return false; // дубль — игнор
  seenIds.add(m.id);
  (chats[m.peer] = chats[m.peer] || []).push(m);
  chats[m.peer].sort((a, b) => (Number(a.sent_at) || 0) - (Number(b.sent_at) || 0) || String(a.id).localeCompare(String(b.id)));
  return true;
}
let renderedKey = null;
let lastChatAtBottom = true;
function pauseMediaIn(root) {
  try {
    root.querySelectorAll("video,audio").forEach((v) => { try { v.pause(); } catch {} });
  } catch {}
}
function rowAvatar(m, p, g, myLetter) {
  const me = !!m.from_me;
  if (me) return { av: profile.avatar, letter: myLetter };
  if (g) {
    const cand = g.members
      .map((ip) => peers.find((x) => x.ip === ip))
      .find((x) => x && (x.nickname === m.from_nick || x.name === m.from_nick));
    if (cand) return { av: dispAva(cand), letter: (dispName(cand) || "?")[0].toUpperCase() };
    return { av: "", letter: (m.from_nick || "?")[0].toUpperCase() };
  }
  if (p) return { av: dispAva(p), letter: (dispName(p) || "?")[0].toUpperCase() };
  return { av: "", letter: "?" };
}
function msgStatusHTML(m) {
  if (m.status === "delivered") return `<span class="mstatus ok" title="Доставлено">✓✓</span>`;
  if (m.status === "sent") return `<span class="mstatus" title="Отправлено, ждёт получателя">✓</span>`;
  return "";
}
// Message time is rendered from the receiver's own clock (sent_at, UTC ms).
// The legacy "ts" string (sender's local HH:MM) is only used when sent_at is absent.
function msgTime(m) {
  const at = Number(m?.sent_at) || 0;
  if (at > 0) {
    const d = new Date(at);
    if (!Number.isNaN(d.getTime())) {
      try { return d.toLocaleTimeString("ru-RU", { hour: "2-digit", minute: "2-digit" }); }
      catch { return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`; }
    }
  }
  return String(m?.ts || "");
}
function msgImages(m) {
  const arr = Array.isArray(m?.images) ? m.images.filter(Boolean) : [];
  if (arr.length) return arr;
  return m?.image ? [m.image] : [];
}
// содержимое ответа-цитаты: автор и начало текста/тип вложения
function quoteOf(m) {
  if (!m?.reply_to) return null;
  const list = chats[m.peer] || [];
  const src = list.find((x) => x.id === m.reply_to);
  if (!src) return { removed: true, author: "", text: "Сообщение удалено" };
  const author = src.from_me ? (profile.nickname || settings?.device_name || "Я") : src.from_nick;
  let text;
  if (src.text && src.text.trim()) text = src.text.trim().split("\n")[0].slice(0, 80);
  else if (msgImages(src).length) text = "Фото";
  else if (src.audio) text = "Аудио";
  else if (src.text && src.text.trim() === "") text = "Файл";
  else text = src.text || "Файл";
  return { removed: false, author, text, id: src.id };
}
function quoteHTML(q) {
  if (!q) return "";
  return `<div class="mquote${q.removed ? " removed" : ""}" data-quote="${esc(q.id || "")}"><b>${esc(q.author || "")}</b><span>${esc(q.text)}</span></div>`;
}
function reactionsHTML(m, myNick) {
  const rs = m.reactions || [];
  if (!rs.length) return "";
  const items = rs.map((r) => {
    const mine = r.by.includes(myNick);
    return `<button class="react${mine ? " mine" : ""}" data-emoji="${esc(r.emoji)}" title="${esc(r.by.join(", "))}">${esc(r.emoji)} ${r.by.length}</button>`;
  }).join("");
  return `<div class="mreacts">${items}</div>`;
}
function buildRow(m, p, g, myLetter, myNick) {
  const me = !!m.from_me;
  const { av, letter } = rowAvatar(m, p, g, myLetter);
  const images = msgImages(m);
  const imgsHTML = images.length ? `<div class="chat-imgs">${images.map((src, i) => `<img class="chat-img" data-i="${i}" src="${src}" />`).join("")}</div>` : "";
  const editMark = m.edited_at ? `<span class="medit">изм.</span>` : "";
  const d = document.createElement("div");
  d.className = "msgrow" + (me ? " me" : "");
  d.dataset.mid = m.id;
  d.innerHTML = `<span class="ava sm"><span>${avaInner(av, letter)}</span></span><div class="mbody"><div class="mhead"><b>${esc(me ? myNick : m.from_nick)}</b><time>${esc(msgTime(m))}</time>${editMark}${msgStatusHTML(m)}</div>${quoteHTML(quoteOf(m))}${imgsHTML}${m.audio ? `<div class="chat-audio"><audio controls preload="metadata" src="${tauriFile(m.audio)}"></audio><span>${esc(fileName(m.audio))}</span></div>` : ""}<p data-raw="${esc(m.text || "")}">${renderMsgText(m.text)}</p>${reactionsHTML(m, myNick)}</div><button class="mdel" title="Удалить"><svg class="ic" viewBox="0 0 24 24"><path d="M13.41,12l4.29-4.29a1,1,0,0,0-1.41-1.41L12,10.59,7.71,6.29A1,1,0,0,0,6.29,7.71L10.59,12,6.29,16.29a1,1,0,0,0,1.41,1.41L12,13.41l4.29,4.29a1,1,0,0,0,1.41-1.41Z"/></svg></button>`;
  d.querySelectorAll(".chat-img").forEach((img) => { img.onclick = () => openImg(images[Number(img.dataset.i)]); });
  d.querySelector(".mdel").onclick = () => openDel({ mode: "msg", id: m.id, mine: me });
  d.oncontextmenu = (e) => { e.preventDefault(); openMsgMenu(e, m); };
  const q = d.querySelector(".mquote");
  if (q) q.onclick = () => scrollToMsg(q.dataset.quote);
  d.querySelectorAll(".react").forEach((b) => {
    b.onclick = () => {
      const emoji = b.dataset.emoji;
      const mine = b.classList.contains("mine");
      invoke("chat_react", { peerIp: curKey(), id: m.id, emoji, remove: mine })
        .catch((e) => toast("" + e, true));
    };
  });
  return d;
}
function refreshRow(row, m, p, g, myLetter, myNick) {
  // обновить шапку без трогания медиа (ник/ава могли смениться)
  const me = !!m.from_me;
  try {
    const { av, letter } = rowAvatar(m, p, g, myLetter);
    row.querySelector(".mhead b").textContent = me ? myNick : m.from_nick;
    row.querySelector(".mhead time").textContent = msgTime(m);
    row.querySelector(".ava.sm > span").innerHTML = avaInner(av, letter);
    // текст мог измениться (правка сообщения) — перерисовываем только если реально другой
    const pEl = row.querySelector("p");
    if (pEl && pEl.dataset.raw !== (m.text || "")) {
      pEl.dataset.raw = m.text || "";
      pEl.innerHTML = renderMsgText(m.text);
    }
    // метка «изменено»
    const head = row.querySelector(".mhead");
    let ed = head?.querySelector(".medit");
    if (m.edited_at) {
      if (!ed && head) { ed = document.createElement("span"); ed.className = "medit"; ed.textContent = "изм."; head.insertBefore(ed, head.querySelector(".mstatus")); }
    } else if (ed) ed.remove();
    let st = row.querySelector(".mstatus");
    if (me && (m.status === "delivered" || m.status === "sent")) {
      if (!st) { st = document.createElement("span"); st.className = "mstatus"; row.querySelector(".mhead").appendChild(st); }
      st.textContent = m.status === "delivered" ? "✓✓" : "✓";
      st.title = m.status === "delivered" ? "Доставлено" : "Отправлено, ждёт получателя";
      st.classList.toggle("ok", m.status === "delivered");
    } else if (st) st.remove();
    // реакции: перерисовать только контейнер, медиа не трогаем
    let rr = row.querySelector(".mreacts");
    const html = reactionsHTML(m, myNick);
    if (rr) rr.remove();
    if (html) {
      const body = row.querySelector(".mbody");
      body.insertAdjacentHTML("beforeend", html);
      row.querySelectorAll(".react").forEach((b) => {
        b.onclick = () => {
          const emoji = b.dataset.emoji;
          const mine = b.classList.contains("mine");
          invoke("chat_react", { peerIp: curKey(), id: m.id, emoji, remove: mine })
            .catch((e) => toast("" + e, true));
        };
      });
    }
  } catch (e) {
    console.error("refreshRow", e);
  }
}
function renderChat() {
  const key = curKey();
  const box = $("#messages"); const list = chats[key] || [];
  const keyChanged = key !== renderedKey;
  const wasAtBottom = keyChanged || (box.clientHeight > 0
    ? box.scrollHeight - box.scrollTop - box.clientHeight < 64
    : lastChatAtBottom);
  if (keyChanged) lastChatAtBottom = true;
  const p = sel(), g = curGroup();
  if (keyChanged) {
    // смена чата: глушим старое медиа (иначе ghost audio) и строим заново
    pauseMediaIn(box);
    box.innerHTML = "";
    renderedKey = key;
  }
  const title = g ? g.name : (p ? dispName(p) : "");
  let hint = box.querySelector(".empty-hint");
  if (!list.length) {
    if (!hint) box.innerHTML = `<div class="empty-hint">Это начало переписки с <b>${esc(title)}</b>. Сообщения летят напрямую, без серверов. <svg class="ic" viewBox="0 0 24 24"><path d="M12,2C6.48,2,2,6.48,2,12s4.48,10,10,10,10-4.48,10-10S17.52,2,12,2Zm0,18c-4.41,0-8-3.59-8-8s3.59-8,8-8,8,3.59,8,8-3.59,8-8,8Z"/><path d="M12,6c-3.31,0-6,2.69-6,6s2.69,6,6,6,6-2.69,6-6-2.69-6-6-6Zm0,10c-2.21,0-4-1.79-4-4s1.79-4,4-4,4,1.79,4,4-1.79,4-4,4Z"/></svg></div>`;
    updateChatBottomButton();
    return;
  }
  if (hint) hint.remove();
  const myLetter = ((profile.nickname || settings?.device_name || "?"))[0].toUpperCase();
  const myNick = profile.nickname || settings?.device_name || "Я";
  const ids = new Set(list.map((m) => m.id));
  // удалить строки удалённых (медиа внутри сначала глушим)
  box.querySelectorAll(".msgrow[data-mid]").forEach((row) => {
    if (!ids.has(row.dataset.mid)) { pauseMediaIn(row); row.remove(); }
  });
  // добавить новые, обновить шапки старых — играющее не трогаем вообще
  const have = new Map();
  box.querySelectorAll(".msgrow[data-mid]").forEach((row) => have.set(row.dataset.mid, row));
  for (let i = 0; i < list.length; i++) {
    const m = list[i];
    const row = have.get(m.id);
    if (row) { refreshRow(row, m, p, g, myLetter, myNick); continue; }
    const newRow = buildRow(m, p, g, myLetter, myNick);
    let nextRow = null;
    for (let j = i + 1; j < list.length; j++) {
      nextRow = have.get(list[j].id);
      if (nextRow) break;
    }
    box.insertBefore(newRow, nextRow);
    have.set(m.id, newRow);
  }
  // Capture the user's position before adding rows: checking after appending
  // could mistake a previously-bottom reader for someone browsing history.
  const keepAtBottom = () => {
    if (wasAtBottom) box.scrollTop = box.scrollHeight;
    updateChatBottomButton();
  };
  keepAtBottom();
  requestAnimationFrame(keepAtBottom);
  if (wasAtBottom) box.querySelectorAll("img.chat-img").forEach((img) => {
    if (!img.complete) img.addEventListener("load", keepAtBottom, { once: true });
  });
}
function updateChatBottomButton() {
  const box = $("#messages"), button = $("#chat-scroll-bottom");
  if (!box || !button) return;
  if (box.clientHeight <= 0) return;
  const distance = box.scrollHeight - box.scrollTop - box.clientHeight;
  lastChatAtBottom = distance <= 48;
  button.classList.toggle("hidden", box.scrollHeight <= box.clientHeight + 16 || distance <= 48);
}
$("#messages").addEventListener("scroll", updateChatBottomButton, { passive: true });
$("#chat-scroll-bottom").onclick = () => {
  const box = $("#messages");
  box.scrollTo({ top: box.scrollHeight, behavior: "smooth" });
  setTimeout(updateChatBottomButton, 250);
};
window.addEventListener("resize", updateChatBottomButton);

// ---------- поиск по чату и глобальный поиск ----------
function flashRow(row) {
  if (!row) return;
  row.classList.add("flash");
  setTimeout(() => row.classList.remove("flash"), 1600);
}
function scrollToMsg(id) {
  if (!id) return;
  const box = $("#messages");
  const row = box.querySelector(`.msgrow[data-mid="${CSS.escape(id)}"]`);
  if (row) { row.scrollIntoView({ behavior: "smooth", block: "center" }); flashRow(row); setTimeout(updateChatBottomButton, 350); }
}
function hideChatSearch() {
  const bar = $("#chat-searchbar");
  if (bar) { bar.classList.add("hidden"); const r = $("#chat-search-results"); if (r) r.classList.add("hidden"); const i = $("#chat-search-input"); if (i) i.value = ""; }
}
$("#btn-msg-search").onclick = () => {
  const bar = $("#chat-searchbar");
  if (!bar) return;
  bar.classList.toggle("hidden");
  if (!bar.classList.contains("hidden")) { $("#chat-search-input")?.focus(); runChatSearch(); }
  else $("#chat-search-results")?.classList.add("hidden");
};
$("#btn-chat-search-close").onclick = hideChatSearch;

function runChatSearch() {
  const results = $("#chat-search-results");
  if (!results) return;
  const q = ($("#chat-search-input")?.value || "").trim().toLowerCase();
  const key = curKey();
  if (!q) { results.classList.add("hidden"); results.innerHTML = ""; return; }
  const rows = chats[key] || [];
  const hits = rows.filter((m) => (m.text || "").toLowerCase().includes(q));
  if (!hits.length) {
    results.innerHTML = `<div class="csr-empty">Ничего не найдено</div>`;
    results.classList.remove("hidden");
    return;
  }
  results.innerHTML = hits.reverse().slice(0, 50).map((m) => {
    const src = m.from_me ? (profile.nickname || settings?.device_name || "Я") : m.from_nick;
    const text = m.text.trim().slice(0, 90);
    return `<div class="csr-item" data-mid="${esc(m.id)}"><b>${esc(src)}</b><span>${esc(text)}</span><time>${esc(msgTime(m))}</time></div>`;
  }).join("");
  results.classList.remove("hidden");
  results.querySelectorAll(".csr-item").forEach((it) => {
    it.onclick = () => scrollToMsg(it.dataset.mid);
  });
}
$("#chat-search-input").addEventListener("input", runChatSearch);

// ---------- глобальный поиск во всех чатах ----------
let allChatsCache = null; // { key: rows }
async function loadAllChats(force = false) {
  if (allChatsCache && !force) return allChatsCache;
  const out = {};
  for (const p of peers) {
    try { out[p.ip] = await invoke("get_chat", { peerIp: p.ip }); } catch { out[p.ip] = []; }
  }
  for (const g of groups) {
    const key = `g:${g.id}`;
    try { out[key] = await invoke("get_chat", { peerIp: key }); } catch { out[key] = []; }
  }
  allChatsCache = out;
  return out;
}
function chatTitle(key) {
  if (key.startsWith("g:")) {
    const g = groups.find((x) => `g:${x.id}` === key);
    return g ? g.name : key;
  }
  const p = peers.find((x) => x.ip === key);
  return p ? dispName(p) : key;
}
$("#btn-search-global").onclick = () => {
  const modal = $("#search-modal");
  if (!modal) return;
  modal.classList.remove("hidden");
  $("#search-all-input").value = "";
  $("#search-all-results").innerHTML = "";
  $("#search-all-input")?.focus();
  allChatsCache = null; // при открытии — свежие данные, без устаревшего кэша
};
function closeSearchModal() {
  $("#search-modal")?.classList.add("hidden");
}
$("#search-all-close").onclick = closeSearchModal;
$("#search-modal").addEventListener("click", (e) => { if (e.target.id === "search-modal") closeSearchModal(); });
$("#search-all-input").addEventListener("input", () => { runGlobalSearch(); });
$("#search-all-input").addEventListener("keydown", (e) => { if (e.key === "Escape") closeSearchModal(); });

async function runGlobalSearch() {
  const host = $("#search-all-results");
  if (!host) return;
  const q = ($("#search-all-input").value || "").trim().toLowerCase();
  if (!q) { host.innerHTML = ""; return; }
  host.innerHTML = `<div class="csr-empty">Ищу…</div>`;
  const data = await loadAllChats();
  const hits = [];
  for (const [key, rows] of Object.entries(data)) {
    for (const m of rows) {
      if ((m.text || "").toLowerCase().includes(q)) hits.push({ key, m });
    }
  }
  hits.sort((a, b) => Number(b.m.ts?.split(":").join("") || 0) - Number(a.m.ts?.split(":").join("") || 0));
  if (!hits.length) { host.innerHTML = `<div class="csr-empty">Ничего не найдено</div>`; return; }
  host.innerHTML = hits.slice(0, 60).map(({ key, m }) => {
    const src = m.from_me ? (profile.nickname || settings?.device_name || "Я") : (m.from_nick || chatTitle(key));
    const text = (m.text || "").trim().slice(0, 100);
    return `<div class="csr-item" data-key="${esc(key)}" data-mid="${esc(m.id)}"><b>${esc(chatTitle(key))}</b><em>${esc(src)}</em><span>${esc(text)}</span><time>${esc(msgTime(m))}</time></div>`;
  }).join("");
  host.querySelectorAll(".csr-item").forEach((it) => {
    it.onclick = async () => {
      const key = it.dataset.key;
      closeSearchModal();
      if (key.startsWith("g:")) { const gid = key.slice(2); if (selGroup !== gid) await selectGroup(gid); }
      else { if (selectedIp !== key) await selectPeer(key); }
      setTimeout(() => scrollToMsg(it.dataset.mid), 120);
    };
  });
}

document.addEventListener("keydown", (e) => {
  if (e.key !== "Escape") return;
  const modal = $("#search-modal");
  if (modal && !modal.classList.contains("hidden")) { closeSearchModal(); return; }
  const bar = $("#chat-searchbar");
  if (bar && !bar.classList.contains("hidden")) { hideChatSearch(); return; }
});
async function sendChat() {
  const inp = $("#chat-input"); const text = inp.value.trim();
  const images = pendingImages.slice();
  if (!text && !images.length) return;
  if (!selectedIp && !selGroup) return toast("Сначала выбери контакт или группу", true);
  inp.value = "";
  pendingImages = [];
  renderPendingAttachments();
  try {
    const payload = { text, image: images[0] || "", images, audio: "" };
    if (replyToId) payload.replyTo = replyToId;
    const m = selGroup
      ? await invoke("send_group_chat", { groupId: selGroup, ...payload })
      : await invoke("send_chat", { peerIp: selectedIp, ...payload });
    if (pushMsg(m)) renderChat(); // событие из Rust дубль отсечёт по id
    setReply(null);
    stopTypingLocal(); // своё сообщение = сразу снять «печатает»
  } catch (e) { toast("Не отправлено: " + e, true); }
}

// ---------- ответ / правка / реакции ----------
let replyToId = null; // id сообщения, на которое отвечаем
let editingId = null; // id своего сообщения, которое правим
let openMenuId = null; // для какого сообщения открыто контекстное меню

function makeMyNick() { return profile.nickname || settings?.device_name || "Я"; }

function replyToggleUI() {
  // подкрашиваем строку-оригинал в чате
  const box = $("#messages");
  if (!box) return;
  const act = editingId || replyToId;
  box.querySelectorAll(".msgrow.replying").forEach((r) => r.classList.remove("replying"));
  if (act) box.querySelector(`.msgrow[data-mid="${CSS.escape(act)}"]`)?.classList.add("replying");
}

function setReply(entry) {
  // entry: { id, author, text } | null
  const bar = $("#reply-bar"), info = $("#reply-info"), inp = $("#chat-input");
  if (!bar) return;
  const wasEditing = !!editingId; // выход из режима правки — чистим поле
  replyToId = null; editingId = null;
  if (!entry) {
    bar.classList.add("hidden");
    info.textContent = "";
  } else {
    replyToId = entry.id;
    info.innerHTML = `<b>Ответ ${esc(entry.author)}</b><span>${esc(entry.text)}</span>`;
    bar.classList.remove("hidden");
  }
  if (wasEditing && inp) inp.value = "";
  replyToggleUI();
  inp?.focus();
}

function setEdit(entry) {
  // entry: { id, text } — правка своего сообщения
  const bar = $("#reply-bar"), info = $("#reply-info"), inp = $("#chat-input");
  if (!bar || !inp) return;
  replyToId = null; editingId = entry.id;
  info.innerHTML = `<b>Изменение</b><span>${esc(entry.text)}</span>`;
  bar.classList.remove("hidden");
  replyToggleUI();
  inp.value = entry.text;
  inp.focus();
}

async function saveEdit() {
  const inp = $("#chat-input");
  const text = inp.value.trim();
  if (!editingId) return;
  const key = curKey();
  const rows = chats[key] || [];
  const src = rows.find((m) => m.id === editingId);
  if (!src) { setReply(null); return; }
  if (!text || text === src.text) { setReply(null); inp.value = ""; return; }
  if (text.length > 2000) return toast("Максимум 2000 символов", true);
  inp.value = "";
  try {
    await invoke("chat_edit", { peerIp: key, id: editingId, text });
    src.text = text; src.edited_at = Date.now();
    renderChat(); // фронт обновится и сам, но без ожидания
    setReply(null); // успех: выходим из режима правки
  } catch (e) {
    toast("Не изменено: " + e, true);
    inp.value = text; // не теряем введённое — можно повторить
  }
}

function msgPreview(m) {
  if (m.text && m.text.trim()) return m.text.trim().split("\n")[0].slice(0, 80);
  if (msgImages(m).length) return "Фото";
  if (m.audio) return "Аудио";
  return "Файл";
}

// ---------- контекстное меню сообщения ----------
function closeMsgMenu() {
  $("#msg-menu")?.classList.add("hidden");
  openMenuId = null;
}
function openMsgMenu(e, m) {
  const menu = $("#msg-menu");
  if (!menu) return;
  openMenuId = m.id;
  const mine = !!m.from_me;
  const emojis = ["👍", "❤️", "😂", "😮", "😢", "🙏"];
  const reactRow = emojis.map((em) => `<button data-act="react" data-emoji="${em}" title="Реакция «${em}»">${em}</button>`).join("");
  const actions = [
    `<button data-act="reply">Ответить</button>`,
    mine ? `<button data-act="edit">Изменить</button>` : "",
    mine ? `<button data-act="copy">Копировать текст</button>` : "",
    `<button data-act="del" class="danger">Удалить</button>`,
  ].filter(Boolean).join("");
  menu.innerHTML = `<div class="mm-reacts">${reactRow}</div>${actions}`;
  menu.classList.remove("hidden");
  const mw = menu.offsetWidth || 180, mh = menu.offsetHeight || 200;
  let x = e.clientX + 4, y = e.clientY + 4;
  if (x + mw > innerWidth - 8) x = e.clientX - mw - 4;
  if (y + mh > innerHeight - 8) y = innerHeight - mh - 8;
  if (y < 8) y = 8;
  menu.style.left = x + "px"; menu.style.top = y + "px";
  menu.onclick = (ev) => {
    const b = ev.target.closest("button[data-act]");
    if (!b) return;
    const rows = chats[curKey()] || [];
    const src = rows.find((x) => x.id === openMenuId);
    closeMsgMenu();
    if (!src) return;
    const act = b.dataset.act;
    if (act === "react") {
      const emoji = b.dataset.emoji;
      const exists = (src.reactions || []).find((r) => r.emoji === emoji);
      const remove = exists ? exists.by.includes(makeMyNick()) : false;
      invoke("chat_react", { peerIp: curKey(), id: src.id, emoji, remove })
        .catch((err) => toast("" + err, true));
    } else if (act === "reply") {
      setReply({ id: src.id, author: src.from_me ? makeMyNick() : src.from_nick, text: msgPreview(src) });
    } else if (act === "edit") {
      if (Date.now() > 12 * 3600 * 1000) {} // правка доступна всегда в этой версии
      setEdit({ id: src.id, text: src.text || "" });
    } else if (act === "copy") {
      navigator.clipboard.writeText(src.text || "").then(() => toast("Скопировано")).catch(() => {});
    } else if (act === "del") {
      openDel({ mode: "msg", id: src.id, mine });
    }
  };
}
document.addEventListener("click", (e) => {
  const menu = $("#msg-menu");
  if (!menu || menu.classList.contains("hidden")) return;
  if (e.target.closest("#msg-menu")) return;
  closeMsgMenu();
}, true);

// ---------- картинки в чате ----------
async function shrinkChatImage(dataUrl, maxDim = 1024) {
  const img = await dataUrlToImg(dataUrl);
  const k = Math.min(1, maxDim / Math.max(img.width, img.height));
  const c = document.createElement("canvas");
  c.width = Math.round(img.width * k); c.height = Math.round(img.height * k);
  c.getContext("2d").drawImage(img, 0, 0, c.width, c.height);
  return c.toDataURL("image/jpeg", 0.82);
}
let pendingImages = [];
function renderPendingAttachments() {
  const host = $("#pending-attachments"); if (!host) return;
  host.innerHTML = "";
  host.classList.toggle("hidden", pendingImages.length === 0);
  pendingImages.forEach((src, i) => {
    const chip = document.createElement("div");
    chip.className = "att-chip";
    chip.innerHTML = `<img src="${src}" alt="" /><button title="Убрать">×</button>`;
    chip.querySelector("img").onclick = () => openImg(src);
    chip.querySelector("button").onclick = () => { pendingImages.splice(i, 1); renderPendingAttachments(); };
    host.appendChild(chip);
  });
}
async function sendImageDataUrl(raw) {
  if (!selectedIp && !selGroup) return toast("Сначала выбери контакт или группу", true);
  try {
    const small = await shrinkChatImage(raw);
    if (pendingImages.length >= 10) return toast("Можно прикрепить до 10 картинок", true);
    pendingImages.push(small);
    renderPendingAttachments();
    $("#chat-input")?.focus();
  } catch (e) { toast("Картинка не добавлена: " + e, true); }
}
async function pickAndSendImage() {
  if (!selectedIp && !selGroup) return toast("Сначала выбери контакт или группу", true);
  try {
    const path = await invoke("pick_image");
    if (!path) return;
    const raw = await invoke("read_image_dataurl", { path });
    await sendImageDataUrl(raw);
  } catch (e) { toast("" + e, true); }
}
function openImg(dataUrl) {
  $("#img-full").src = dataUrl;
  $("#img-modal").classList.remove("hidden");
}
function closeImg() { $("#img-modal").classList.add("hidden"); $("#img-full").src = ""; }
$("#img-close").onclick = closeImg;
$("#img-full").onclick = closeImg;
$("#img-modal").addEventListener("click", (e) => { if (e.target.id === "img-modal") closeImg(); });
document.addEventListener("keydown", (e) => {
  if (e.key !== "Escape") return;
  if (!$("#img-modal").classList.contains("hidden")) { closeImg(); return; }
  if (!$("#call-panel").classList.contains("hidden")) { $("#call-panel").classList.add("hidden"); return; }
  const wm = $("#watch-modal");
  if (!wm.classList.contains("hidden")) {
    if (wm.classList.contains("watch-fullscreen")) wm.classList.remove("watch-fullscreen");
    else minimizeWatch();
  }
});
$("#btn-img-copy").onclick = async () => {
  try {
    const blob = await (await fetch($("#img-full").src)).blob();
    await navigator.clipboard.write([new ClipboardItem({ [blob.type]: blob })]);
    toast("Картинка скопирована <svg class='ic' viewBox='0 0 24 24'><path d='M16,1H4A2,2,0,0,0,2,3V17H4V3H16Z'/><path d='M19,5H8A2,2,0,0,0,6,7V21a2,2,0,0,0,2,2H19a2,2,0,0,0,2-2V7A2,2,0,0,0,19,5ZM19,21H8V7H19Z'/></svg>");
  } catch (e) { toast("Не скопировалось: " + e, true); }
};

// ---------- files ----------
async function sendPaths(peerIp, paths) {
  if (!paths?.length) return;
  // группа: разослать всем онлайн-участникам
  const targets = peerIp === null && selGroup
    ? (curGroup()?.members || []).filter((ip) => peers.find((p) => p.ip === ip)?.online)
    : [peerIp];
  if (!targets.length || (targets.length === 1 && !targets[0])) return toast("Нет получателей в сети", true);
  for (const t of targets) {
    if (!t) continue;
    for (const fp of paths) {
      try { toast(`Отправка ${fileName(fp)} <svg class='ic' viewBox='0 0 24 24'><path d='M21.66,12a2,2,0,0,1-1.14,1.81L5.87,20.75A2.08,2.08,0,0,1,5,21a2,2,0,0,1-1.82-2.82L5.46,13H11a1,1,0,0,0,0-2H5.46L3.18,5.87A2,2,0,0,1,5.86,3.25h0l14.65,6.94A2,2,0,0,1,21.66,12Z'/></svg> ${t}…`); await invoke("send_file", { peerIp: t, filePath: fp }); }
      catch (e) {
        if (String(e).includes("cancelled")) toast("Отправка отменена");
        else toast("Ошибка отправки: " + e, true);
      }
    }
  }
  refreshTransfers();
}
function trStatus(s) { return { active: "идёт", done: "готово", cancelled: "отменено", error: "ошибка" }[s] || s; }
const incomingFileOffers = [];
let activeIncomingFileOffer = null;
function showNextIncomingFileOffer() {
  if (activeIncomingFileOffer || !incomingFileOffers.length) return;
  activeIncomingFileOffer = incomingFileOffers.shift();
  $("#incoming-file-sender").textContent = activeIncomingFileOffer.sender || activeIncomingFileOffer.peer || "Неизвестный контакт";
  $("#incoming-file-name").textContent = activeIncomingFileOffer.filename || "Файл";
  $("#incoming-file-size").textContent = fmtSize(activeIncomingFileOffer.size || 0);
  $("#incoming-file-modal").classList.remove("hidden");
}
async function answerIncomingFileOffer(accepted) {
  const offer = activeIncomingFileOffer;
  if (!offer) return;
  activeIncomingFileOffer = null;
  $("#incoming-file-modal").classList.add("hidden");
  try { await invoke("respond_file_offer", { offerId: offer.id, accepted }); }
  catch (e) { toast(`Не удалось ответить отправителю: ${e}`, true); }
  showNextIncomingFileOffer();
}
$("#incoming-file-accept").onclick = () => answerIncomingFileOffer(true);
$("#incoming-file-reject").onclick = () => answerIncomingFileOffer(false);
$("#incoming-file-modal").addEventListener("click", (e) => { if (e.target.id === "incoming-file-modal") answerIncomingFileOffer(false); });
async function refreshTransfers() {
  let list = [];
  try { list = await invoke("get_transfers"); } catch {}
  updateUpdateProgress(list);
  const box = $("#transfers");
  box.innerHTML = list.length ? "" : `<div class="empty-hint">Передач пока нет. Файлы выбранному контакту — кнопкой выше или перетаскиванием на окно.</div>`;
  [...list].reverse().slice(0, 60).forEach((t) => {
    const d = document.createElement("div");
    d.className = "tcard";
    d.innerHTML = `<b>${t.direction === "up" ? '<svg class="ic" viewBox="0 0 24 24" style="vertical-align:-3px"><path d="M21.66,12a2,2,0,0,1-1.14,1.81L5.87,20.75A2.08,2.08,0,0,1,5,21a2,2,0,0,1-1.82-2.82L5.46,13H11a1,1,0,0,0,0-2H5.46L3.18,5.87A2,2,0,0,1,5.86,3.25h0l14.65,6.94A2,2,0,0,1,21.66,12Z"/></svg>' : '<svg class="ic" viewBox="0 0 24 24" style="vertical-align:-3px"><path d="M20,11H7.83l5.59-5.59L12,4l-8,8,8,8,1.41-1.41L7.83,13H20Z"/></svg>'} ${esc(t.filename)}</b><div class="muted">${esc(t.peer)} · ${fmtSize(t.done)} / ${fmtSize(t.size)} · ${fmtSpeed(t.speed_bps)} · ${esc(trStatus(t.status))}${t.status === "error" && t.error ? ` · <svg class="ic" viewBox="0 0 24 24" style="vertical-align:-3px"><path d="M13.41,12l4.29-4.29a1,1,0,0,0-1.41-1.41L12,10.59,7.71,6.29A1,1,0,0,0,6.29,7.71L10.59,12,6.29,16.29a1,1,0,0,0,1.41,1.41L12,13.41l4.29,4.29a1,1,0,0,0,1.41-1.41Z"/></svg> ${esc(t.error)}` : ""}</div><div class="bar"><div style="width:${Math.round(t.progress * 100)}%"></div></div>${t.status === "active" ? `<div class="row"><button data-cancel="${esc(t.id)}"><svg class="ic" viewBox="0 0 24 24" style="vertical-align:-3px"><path d="M6,6H18V18H6Z"/></svg> Отменить</button></div>` : ""}`;
    box.appendChild(d);
  });
  box.querySelectorAll("[data-cancel]").forEach((b) => b.onclick = async () => {
    try { await invoke("cancel_transfer", { transferId: b.dataset.cancel }); toast("Отмена…"); }
    catch (e) { toast("" + e, true); }
  });
  const act = list.filter((t) => t.status === "active");
  $("#files-badge").textContent = act.length ? `• ${act.length}` : "";
  const strip = $("#transfer-strip");
  if (act.length) {
    const t = act[act.length - 1];
    strip.classList.remove("hidden");
    strip.innerHTML = `${t.direction === "up" ? '<svg class="ic" viewBox="0 0 24 24" style="vertical-align:-3px"><path d="M21.66,12a2,2,0,0,1-1.14,1.81L5.87,20.75A2.08,2.08,0,0,1,5,21a2,2,0,0,1-1.82-2.82L5.46,13H11a1,1,0,0,0,0-2H5.46L3.18,5.87A2,2,0,0,1,5.86,3.25h0l14.65,6.94A2,2,0,0,1,21.66,12Z"/></svg> Отправка' : '<svg class="ic" viewBox="0 0 24 24" style="vertical-align:-3px"><path d="M20,11H7.83l5.59-5.59L12,4l-8,8,8,8,1.41-1.41L7.83,13H20Z"/></svg> Приём'}: <b>${esc(t.filename)}</b> — ${Math.round(t.progress * 100)}% · ${fmtSpeed(t.speed_bps)} <button data-cancel="${esc(t.id)}"><svg class="ic" viewBox="0 0 24 24"><path d="M6,6H18V18H6Z"/></svg></button><div class="bar"><div style="width:${Math.round(t.progress * 100)}%"></div></div>`;
    strip.querySelector("[data-cancel]").onclick = async () => {
      try { await invoke("cancel_transfer", { transferId: t.id }); toast("Отмена…"); }
      catch (e) { toast("" + e, true); }
    };
  } else strip.classList.add("hidden");
}
function updateUpdateProgress(list) {
  const rows = [...document.querySelectorAll("#upd-log [data-update-peer]")];
  for (const row of rows) {
    const peer = row.dataset.updatePeer;
    const name = row.dataset.updateFile;
    const transfer = [...list].reverse().find((t) => t.direction === "up" && t.peer === peer && t.filename === name);
    if (!transfer) continue;
    const details = row.querySelector(".update-details");
    const bar = row.querySelector(".bar > div");
    if (details) {
      const pct = Math.round((Number(transfer.progress) || 0) * 100);
      details.textContent = transfer.status === "active"
        ? `${fmtSize(transfer.done)} / ${fmtSize(transfer.size)} · ${pct}% · ${fmtSpeed(transfer.speed_bps)}`
        : transfer.status === "done" ? "Передача завершена"
        : transfer.status === "error" ? `Ошибка: ${transfer.error || "передача прервана"}`
        : transfer.status === "cancelled" ? "Передача отменена" : trStatus(transfer.status);
      if (bar) bar.style.width = `${Math.max(0, Math.min(100, pct))}%`;
    }
  }
}

// ---------- avatars / images ----------
function dataUrlToImg(dataUrl) {
  return new Promise((res, rej) => { const i = new Image(); i.onload = () => res(i); i.onerror = rej; i.src = dataUrl; });
}
async function shrinkAvatar(dataUrl) {
  const img = await dataUrlToImg(dataUrl);
  const c = document.createElement("canvas"); c.width = c.height = 128;
  const ctx = c.getContext("2d");
  const s = Math.min(img.width, img.height);
  ctx.drawImage(img, (img.width - s) / 2, (img.height - s) / 2, s, s, 0, 0, 128, 128);
  return c.toDataURL("image/jpeg", 0.85);
}
async function pickMyAvatar() {
  try {
    const path = await invoke("pick_image");
    if (!path) return;
    const raw = await invoke("read_image_dataurl", { path });
    const small = await shrinkAvatar(raw);
    profile = await invoke("save_profile", { nickname: $("#set-nick").value.trim(), avatar: small });
    toast("Профиль разослан всем в сети <svg class='ic' viewBox='0 0 24 24'><path d='M9,16.17,4.83,12l-1.42,1.41L9,19,21,7l-1.41-1.41Z'/></svg>"); renderMyAvatar(); renderAll();
    $("#me-nick").textContent = profile.nickname || settings.device_name;
  } catch (e) { toast(String(e), true); }
}
async function pickPeerAvatar() {
  if (!selectedIp) return;
  try {
    const path = await invoke("pick_image");
    if (!path) return;
    const raw = await invoke("read_image_dataurl", { path });
    const small = await shrinkAvatar(raw);
    await invoke("set_peer_avatar", { ip: selectedIp, avatar: small });
    toast("Аватарка назначена"); peers = await invoke("get_peers"); renderAll();
  } catch (e) { toast(String(e), true); }
}

// ---------- devices + mic permission ----------
let localStream = null, micMuted = false;
function setMicState(st) {
  const el = $("#mic-state");
  el.className = "mic-state" + (st === "ok" ? " ok" : st === "no" ? " no" : "");
  el.textContent = st === "ok" ? "микрофон OK" : st === "no" ? "заблокирован" : "проверка…";
}
const MIC_HELP = `<ol>
<li>Микрофон в SafeDrop <b>разрешён автоматически</b> — отдельного окна может вообще не быть, это нормально.</li>
<li>Нажми «Запросить доступ»: если всё хорошо — статус станет «микрофон OK».</li>
<li>Если пишет «заблокирован»: открой кнопкой «Настройки Windows» страницу микрофона и включи доступ + доступ классическим приложениям. Закрой программы, занявшие микрофон (Discord, Zoom, OBS).</li>
<li>Если раньше нажимал «Запретить» и запрет запомнился — жми красную кнопку ниже: приложение перезапустится и забудет старый запрет (контакты и настройки сохранятся).</li>
</ol>`;
async function initDevices() {
  setMicState("…");
  try { const tmp = await navigator.mediaDevices.getUserMedia({ audio: true }).catch(() => null); tmp?.getTracks().forEach((t) => t.stop()); } catch {}
  let devs = [];
  try { devs = await navigator.mediaDevices.enumerateDevices(); } catch { return; }
  const ms = $("#mic-select"), ss = $("#spk-select");
  ms.innerHTML = ""; ss.innerHTML = "";
  devs.filter((d) => d.kind === "audioinput").forEach((d, i) => { const o = document.createElement("option"); o.value = d.deviceId; o.textContent = d.label || `Микрофон ${i + 1}`; ms.appendChild(o); });
  devs.filter((d) => d.kind === "audiooutput").forEach((d, i) => { const o = document.createElement("option"); o.value = d.deviceId; o.textContent = d.label || `Динамики ${i + 1}`; ss.appendChild(o); });
  if (settings.mic_id) ms.value = settings.mic_id;
  if (settings.speaker_id) ss.value = settings.speaker_id;
  ms.onchange = async () => { settings.mic_id = ms.value; await invoke("save_settings", { settings }); toast("Микрофон сохранён"); if (localStream) await startMic().catch(() => {}); };
  ss.onchange = async () => {
    settings.speaker_id = ss.value; await invoke("save_settings", { settings }); toast("Динамики сохранены");
    try { await $("#remote-audio").setSinkId(ss.value); } catch {}
    try { if (ampCtx && typeof ampCtx.setSinkId === "function") await ampCtx.setSinkId(ss.value); } catch {}
  };
  try { if (settings.speaker_id) await $("#remote-audio").setSinkId(settings.speaker_id); } catch {}
  fillCameraSelect(devs);
  setMicState(ms.options.length ? "ok" : "no");
}
function fillCameraSelect(devs) {
  const cs = $("#cam-select"); if (!cs) return;
  const cams = devs.filter((d) => d.kind === "videoinput");
  const prev = settings?.camera_id || cs.value;
  cs.innerHTML = "";
  cams.forEach((d, i) => {
    const o = document.createElement("option");
    o.value = d.deviceId;
    o.textContent = d.label || `Камера ${i + 1}`;
    cs.appendChild(o);
  });
  if (!cams.length) {
    const o = document.createElement("option");
    o.value = ""; o.textContent = "Камера не найдена";
    cs.appendChild(o);
    cs.disabled = true;
  } else {
    cs.disabled = false;
    if (prev && cams.some((d) => d.deviceId === prev)) cs.value = prev;
    else settings.camera_id = cs.value;
  }
  cs.onchange = async () => {
    settings.camera_id = cs.value;
    await invoke("save_settings", { settings }).catch(() => {});
    await switchCamera(cs.value);
    if (document.body.classList.contains("details-open")) previewCamera();
  };
}
async function refreshCameraList() {
  try {
    const devs = await navigator.mediaDevices.enumerateDevices();
    fillCameraSelect(devs);
  } catch {}
}
let camPreviewStream = null;
let camPreviewOwned = false;
function stopCameraPreview() {
  if (camPreviewOwned) { try { camPreviewStream?.getTracks().forEach((t) => t.stop()); } catch {} }
  camPreviewStream = null;
  camPreviewOwned = false;
  const v = $("#cam-preview");
  if (v) v.srcObject = null;
}
async function cameraStream(width = 1280, height = 720, fps = 30, deviceId = "") {
  const base = { width: { max: width }, height: { max: height }, frameRate: { max: fps } };
  const id = deviceId || settings?.camera_id;
  try {
    return await navigator.mediaDevices.getUserMedia({ video: id ? { ...base, deviceId: { exact: id } } : base, audio: false });
  } catch (e) {
    if (!id) throw e;
    return await navigator.mediaDevices.getUserMedia({ video: base, audio: false });
  }
}
async function previewCamera() {
  const v = $("#cam-preview"); if (!v) return;
  stopCameraPreview();
  const active = Object.values(myShares).find((s) => s.kind === "camera");
  if (active?.stream) {
    camPreviewStream = active.stream;
    camPreviewOwned = false;
    v.srcObject = camPreviewStream;
    v.play().catch(() => {});
    return;
  }
  try {
    camPreviewStream = await cameraStream(640, 360, 30);
    camPreviewOwned = true;
    v.srcObject = camPreviewStream;
    v.play().catch(() => {});
    refreshCameraList();
  } catch (e) {
    v.srcObject = null;
    toast("Камера недоступна: " + (e?.message || e), true);
  }
}
async function switchCamera(deviceId) {
  const entry = Object.entries(myShares).find(([, s]) => s.kind === "camera");
  if (!entry) return;
  const [shareId, share] = entry;
  let next;
  try { next = await cameraStream(1280, 720, 30, deviceId); }
  catch (e) { toast("Не удалось переключить камеру: " + (e?.message || e), true); return; }
  const track = next.getVideoTracks()[0];
  if (!track) { next.getTracks().forEach((t) => t.stop()); return; }
  const old = share.stream.getVideoTracks()[0];
  for (const pc of Object.values(share.pcs)) {
    for (const sender of pc.getSenders()) {
      if (sender.track?.kind === "video") {
        try { await sender.replaceTrack(track); } catch {}
      }
    }
  }
  try { if (old) share.stream.removeTrack(old); } catch {}
  try { share.stream.addTrack(track); } catch {}
  if (old) { try { old.stop(); } catch {} }
  track.addEventListener("ended", () => stopShare(shareId));
  renderStages(); renderCallStage(); renderShareList();
  toast("Камера переключена");
}
async function startMic() {
  try {
    if (localStream) localStream.getTracks().forEach((t) => t.stop());
    try { if (micCtx) micCtx.close(); } catch {}
    micCtx = micGain = micDest = gateNode = rnnoiseNode = rnnoiseWetGain = rnnoiseDryGain = micCompressor = null;
    const noiseEngine = settings.noise_filter || "rnnoise";
    localStream = await navigator.mediaDevices.getUserMedia({ audio: {
      ...(settings.mic_id ? { deviceId: { exact: settings.mic_id } } : {}),
      echoCancellation: { ideal: settings.echo_cancellation !== false },
      noiseSuppression: { ideal: noiseEngine === "webrtc" },
      autoGainControl: { ideal: settings.auto_gain_control !== false },
    } });
    setMicState("ok");
    localStream.getAudioTracks().forEach((t) => (t.enabled = !micMuted));
    // Local audio chain: RNNoise (optional), level, and a soft expander gate.
    try {
      const AudioCtx = window.AudioContext || window.webkitAudioContext;
      try { micCtx = new AudioCtx({ sampleRate: 48000 }); } catch { micCtx = new AudioCtx(); }
      if (micCtx.state === "suspended") micCtx.resume().catch(() => {});
      const src = micCtx.createMediaStreamSource(localStream);
      micGain = micCtx.createGain();
      const hp = micCtx.createBiquadFilter();
      hp.type = "highpass";
      hp.frequency.value = 100;
      const lp = micCtx.createBiquadFilter();
      lp.type = "lowpass";
      lp.frequency.value = 7000;
      if (noiseEngine === "rnnoise") {
        try {
          if (micCtx.sampleRate !== 48000) throw new Error(`RNNoise requires 48 kHz (current ${micCtx.sampleRate})`);
          const { RNNoiseNode, rnnoise_loadAssets } = await import("./vendor/rnnoise/rnnoise.mjs");
          const workletUrl = new URL("./vendor/rnnoise/rnnoise.worklet.js", import.meta.url);
          const wasmUrl = new URL("./vendor/rnnoise/rnnoise.wasm", import.meta.url);
          const wasmResponse = await fetch(wasmUrl);
          if (!wasmResponse.ok) throw new Error(`RNNoise WASM: ${wasmResponse.status}`);
          const assets = await rnnoise_loadAssets({ scriptSrc: workletUrl, moduleSrc: await wasmResponse.arrayBuffer() });
          await RNNoiseNode.register(micCtx, assets);
          rnnoiseNode = new RNNoiseNode(micCtx);
          rnnoiseWetGain = micCtx.createGain();
          rnnoiseDryGain = micCtx.createGain();
          const strength = Math.min(100, Math.max(0, Number(settings.noise_strength ?? 100))) / 100;
          rnnoiseWetGain.gain.value = strength;
          rnnoiseDryGain.gain.value = 1 - strength;
          src.connect(rnnoiseNode); rnnoiseNode.connect(rnnoiseWetGain); rnnoiseWetGain.connect(hp);
          src.connect(rnnoiseDryGain); rnnoiseDryGain.connect(hp);
        } catch (e) {
          console.warn("RNNoise недоступен, переключаюсь на встроенный WebRTC", e);
          try { rnnoiseNode?.disconnect(); rnnoiseWetGain?.disconnect(); rnnoiseDryGain?.disconnect(); } catch {}
          rnnoiseNode = rnnoiseWetGain = rnnoiseDryGain = null;
          try { await Promise.all(localStream.getAudioTracks().map((track) => track.applyConstraints({ noiseSuppression: true }))); } catch {}
          toast("RNNoise не загрузился — включён встроенный шумодав WebRTC", true);
          src.connect(hp);
        }
      } else src.connect(hp);
      await micCtx.audioWorklet.addModule(new URL("./noise-gate-processor.js", import.meta.url));
      gateNode = new AudioWorkletNode(micCtx, "safedrop-noise-gate", {
        numberOfInputs: 1, numberOfOutputs: 1, outputChannelCount: [1],
        processorOptions: { threshold: (settings.noise_gate ?? 12) / 3000, smooth: settings.noise_smooth ?? 50 },
      });
      micDest = micCtx.createMediaStreamDestination();
      micCompressor = micCtx.createDynamicsCompressor();
      micCompressor.threshold.value = -2;
      micCompressor.knee.value = 0;
      micCompressor.ratio.value = 12;
      micCompressor.attack.value = 0.003;
      micCompressor.release.value = 0.12;
      hp.connect(lp); lp.connect(gateNode); gateNode.connect(micGain);
      micGain.connect(micCompressor); micCompressor.connect(micDest);
    } catch (e) { console.warn("AudioWorklet шумодав недоступен, передаю исходный микрофон", e); micDest = null; gateNode = null; }
    applyMicVolume();
    gateMeter();
    const tracks = micTracks();
    if (pc) tracks.forEach((t) => { try { pc.addTrack(t, localStream); } catch {} });
    for (const ip of Object.keys(gpc)) { try { tracks.forEach((t) => gpc[ip].pc.addTrack(t, localStream)); } catch {} }
    micMeter(micTrackStream());
    monWatch("me", (micDest?.stream) || localStream);
  } catch (e) {
    setMicState("no");
    throw e;
  }
}
let micCtx = null, micGain = null, micDest = null, gateNode = null, rnnoiseNode = null, rnnoiseWetGain = null, rnnoiseDryGain = null, micCompressor = null;
function micTracks() {
  try {
    const live = micDest?.stream?.getAudioTracks().filter((t) => t.readyState === "live");
    if (live?.length) return live;
  } catch {}
  return localStream ? localStream.getAudioTracks() : [];
}
function micTrackStream() {
  const processed = micDest?.stream;
  return processed?.getAudioTracks().some((t) => t.readyState === "live") ? processed : localStream;
}
function applyMicVolume() {
  const v = Math.min(300, Math.max(0, Number(settings.mic_volume ?? 100)));
  if (micGain) {
    try { micGain.gain.setTargetAtTime(v / 100, micCtx.currentTime, 0.03); } catch {}
  }
  const el = $("#mic-vol-v"); if (el) el.textContent = `${v}%`;
  const cp = $("#cp-micvol"); if (cp && document.activeElement !== cp) cp.value = v;
}
function peerVol(ip) {
  const m = settings.peer_volumes || {};
  return m[ip] != null ? m[ip] : 100;
}
function voiceElId(ip) { return "voice-" + String(ip).replace(/[^0-9a-z]/gi, ""); }
// усилитель 0..200%: гоняем элемент через GainNode (element.volume упирается в 100%)
let ampCtx = null;
const ampGains = {};
function ampEnsure() {
  if (!ampCtx) { try { ampCtx = new (window.AudioContext || window.webkitAudioContext)(); } catch {} }
  if (ampCtx?.state === "suspended") ampCtx.resume().catch(() => {});
  if (ampCtx && settings?.speaker_id && typeof ampCtx.setSinkId === "function") {
    ampCtx.setSinkId(settings.speaker_id).catch(() => {});
  }
  return ampCtx;
}
function ampFor(key, stream, el) {
  try {
    const ctx = ampEnsure(); if (!ctx) return null;
    let rec = ampGains[key];
    if (!rec || rec.stream !== stream) {
      try { rec?.source?.disconnect(); } catch {}
      try { rec?.gain?.disconnect(); } catch {}
      const source = ctx.createMediaStreamSource(stream);
      const gain = ctx.createGain();
      source.connect(gain); gain.connect(ctx.destination);
      rec = ampGains[key] = { source, gain, stream, el };
    }
    rec.el = el;
    if (el) { el.muted = true; el.volume = 1; }
    return rec.gain;
  } catch { return null; }
}
function ampSet(key, pct) {
  const rec = ampGains[key];
  const value = Number.isFinite(Number(pct)) ? Math.max(0, Math.min(200, Number(pct))) : 0;
  const el = rec?.el || (key.startsWith("v:") ? document.getElementById(voiceElId(key.slice(2))) : null);
  if (!rec) {
    if (el) {
      try { el.muted = value === 0; el.volume = Math.min(1, value / 100); } catch {}
    }
    return;
  }
  if (el) { try { el.muted = true; el.volume = 1; } catch {} }
  try {
    const ctx = ampEnsure();
    const t = ctx?.currentTime || 0;
    rec.gain.gain.cancelScheduledValues(t);
    rec.gain.gain.setTargetAtTime(value / 100, t, 0.015);
    if (ctx?.state === "suspended") ctx.resume().catch(() => {});
  } catch {}
}
function ampDrop(key) {
  const rec = ampGains[key];
  if (rec) {
    try { rec.source.disconnect(); } catch {}
    try { rec.gain.disconnect(); } catch {}
    delete ampGains[key];
  }
}
function attachRemoteVoice(peerIp, stream) {
  if (!stream) stream = new MediaStream();
  let a = document.getElementById(voiceElId(peerIp));
  if (!a) {
    a = document.createElement("audio"); a.id = voiceElId(peerIp);
    a.autoplay = true; a.playsInline = true; a.muted = true;
    a.style.display = "none"; document.body.appendChild(a);
  }
  a.srcObject = stream;
  ampFor("v:" + peerIp, stream, a);
  ampSet("v:" + peerIp, deafened ? 0 : peerVol(peerIp));
  const resumePlayback = () => {
    const ctx = ampEnsure();
    if (ctx?.state === "suspended") ctx.resume().catch(() => {});
    a.play().catch((e) => console.warn("Не удалось запустить воспроизведение голоса", e));
  };
  a.onloadedmetadata = resumePlayback;
  resumePlayback();
  voiceEls[peerIp] = a;
  monWatch(peerIp, stream);
  return a;
}
function dropRemoteVoice(peerIp) {
  document.getElementById(voiceElId(peerIp))?.remove();
  delete voiceEls[peerIp];
  ampDrop("v:" + peerIp);
  monDrop(peerIp);
}
async function setPeerVol(ip, v) {
  v = Math.max(0, Math.min(200, v));
  settings.peer_volumes = settings.peer_volumes || {};
  settings.peer_volumes[ip] = v;
  ampSet("v:" + ip, (deafened || v === 0) ? 0 : v);
  renderCallPanel();
}
async function requestMic() {
    try { await startMic(); toast("Микрофон работает <svg class='ic' viewBox='0 0 24 24'><path d='M9,16.17,4.83,12l-1.42,1.41L9,19,21,7l-1.41-1.41Z'/></svg>"); }
    catch (e) {
    const name = e?.name || "";
    if (name === "NotAllowedError" || name === "SecurityError") showHelp("Микрофон заблокирован", MIC_HELP, "<svg class='ic' viewBox='0 0 24 24'><path d='M20,11A8,8,0,0,0,5.64,6.64L4,8.29V4H8.29l1.59,1.59A10,10,0,0,1,22,13Z'/><path d='M4,13A8,8,0,0,0,18.36,17.36L20,15.71V20H15.71l-1.59-1.59A10,10,0,0,1,2,11Z'/></svg> Сбросить запрет (перезапуск)", async () => { toast("Перезапускаю для сброса…"); await invoke("fix_mic_blocked").catch((x) => toast("" + x, true)); });
    else toast("Микрофон: " + (e?.message || e), true);
  }
}
let gateGen = 0;
function gateMeter() {
  try {
    if (!micCtx || !gateNode) return;
    const myGen = ++gateGen;
    const an = micCtx.createAnalyser(); an.fftSize = 256;
    gateNode.connect(an);
    const data = new Uint8Array(an.frequencyBinCount);
    const tick = () => {
      if (myGen !== gateGen || !micCtx || !gateNode) return;
      try {
        an.getByteFrequencyData(data);
        let sum = 0;
        for (let i = 0; i < data.length; i++) sum += data[i];
        const v = Math.min(100, Math.round((sum / data.length / 255) * 300));
        const el = document.getElementById("mic-level-out");
        if (el) el.style.width = v + "%";
        const st = document.getElementById("gate-state");
        if (st) {
          const open = v > 1;
          st.style.color = open ? "#3ddc84" : "#da373c";
          st.textContent = open ? "● открыт" : "● закрыт";
        }
      } catch {}
      requestAnimationFrame(tick);
    };
    tick();
  } catch {}
}
function micMeter(stream) {
  try {
    const ctx = new (window.AudioContext || window.webkitAudioContext)();
    const an = ctx.createAnalyser(); an.fftSize = 256;
    ctx.createMediaStreamSource(stream).connect(an);
    const data = new Uint8Array(an.frequencyBinCount);
    const tick = () => {
      if (stream.getTracks().every((t) => t.readyState === "ended")) return;
      an.getByteFrequencyData(data);
      const v = Math.min(100, Math.round((data.reduce((a, b) => a + b, 0) / data.length / 255) * 300));
      const el = $("#mic-level"); if (el) el.style.width = v + "%";
      requestAnimationFrame(tick);
    };
    tick();
  } catch {}
}
const RING_MELODIES = {
  default: [[880, 0, 300], [660, 350, 300]],
  classic: [[523, 0, 200], [659, 250, 200], [784, 500, 400]],
  soft: [[440, 0, 400], [554, 500, 400], [659, 1000, 600]],
  urgent: [[880, 0, 150], [880, 200, 150], [880, 400, 150], [1100, 600, 300]],
  vibrant: [[523, 0, 150], [659, 200, 150], [784, 400, 150], [1047, 600, 400], [784, 1050, 150], [1047, 1250, 300]],
};

function playRingMelody(name) {
  if (!settings?.sound) return;
  const notes = RING_MELODIES[name];
  if (!notes) return;
  try {
    const ctx = new (window.AudioContext || window.webkitAudioContext)();
    if (ctx.state === "suspended") ctx.resume().catch(() => {});
    for (const [freq, offset, dur] of notes) {
      const o = ctx.createOscillator(), g = ctx.createGain();
      o.type = "sine";
      o.frequency.value = freq;
      const t = ctx.currentTime + offset / 1000;
      g.gain.setValueAtTime(0, t);
      g.gain.linearRampToValueAtTime(0.3, t + 0.02);
      g.gain.linearRampToValueAtTime(0, t + dur / 1000);
      o.connect(g); g.connect(ctx.destination);
      o.start(t); o.stop(t + dur / 1000 + 0.05);
    }
    setTimeout(() => ctx.close().catch(() => {}), 3000);
  } catch {}
}

function blip(freq = 880, ms = 120) {
  if (!settings?.sound) return;
  try {
    const ctx = new (window.AudioContext || window.webkitAudioContext)();
    if (ctx.state === "suspended") ctx.resume().catch(() => {});
    const o = ctx.createOscillator(), g = ctx.createGain();
    o.frequency.value = freq; o.connect(g); g.connect(ctx.destination);
    o.start(); setTimeout(() => o.stop(), ms);
  } catch {}
}
function playOneShot(path, fallback) {
  if (!path) { fallback?.(); return; }
  try {
    const a = new Audio(tauriFile(path));
    if (settings.speaker_id && a.setSinkId) a.setSinkId(settings.speaker_id).catch(() => {});
    // если файл виснет — не молчим, а играем запасной звук
    const to = setTimeout(() => { try { a.pause(); } catch {} fallback?.(); }, 6000);
    a.addEventListener("ended", () => clearTimeout(to));
    a.addEventListener("error", () => { clearTimeout(to); fallback?.(); });
    a.play().then(() => {}).catch(() => { clearTimeout(to); fallback?.(); });
  } catch { fallback?.(); }
}
function playMsgSound() {
  if (!settings?.sound) return;
  playOneShot(settings.message_sound, blip);
}
function playFileSound() {
  if (!settings?.sound) return;
  playOneShot(settings.file_sound, () => blip(660));
}
function playCallJoinSound() {
  if (!settings?.sound) return;
  const melody = settings.call_join_melody || "classic";
  const fallback = () => playRingMelody(melody);
  if (settings.call_join_sound) playOneShot(settings.call_join_sound, fallback);
  else fallback();
}
function playCallLeaveSound() {
  if (!settings?.sound) return;
  const melody = settings.call_leave_melody || "default";
  const fallback = () => playRingMelody(melody);
  if (settings.call_leave_sound) playOneShot(settings.call_leave_sound, fallback);
  else fallback();
}
// разблокировка звука первым жестом (иначе браузер душит Audio без клика)
let audioUnlocked = false;
function unlockAudio() {
  if (audioUnlocked) return; audioUnlocked = true;
  try {
    const ctx = new (window.AudioContext || window.webkitAudioContext)();
    if (ctx.state === "suspended") ctx.resume().catch(() => {});
    if (ampCtx?.state === "suspended") ampCtx.resume().catch(() => {});
    if (micCtx?.state === "suspended") micCtx.resume().catch(() => {});
  } catch {}
  for (const p of [settings?.message_sound, settings?.ringtone, settings?.file_sound, settings?.call_join_sound, settings?.call_leave_sound]) {
    if (!p) continue;
    try { const t = new Audio(tauriFile(p)); t.preload = "auto"; t.load(); } catch {}
  }
}
["pointerdown", "keydown"].forEach((ev) => window.addEventListener(ev, unlockAudio));
// проверка аудиофайла: реально ли браузер его прочитает
function checkAudioFile(path) {
  return new Promise((resolve) => {
    const a = new Audio();
    const to = setTimeout(() => resolve("таймаут загрузки (8с) — файл недоступен"), 8000);
    a.addEventListener("canplaythrough", () => { clearTimeout(to); resolve(null); });
    a.addEventListener("error", () => { clearTimeout(to); resolve("файл не читается браузером (кодек/путь)"); });
    try { a.src = tauriFile(path); a.load(); }
    catch (e) { clearTimeout(to); resolve(String(e)); }
  });
}
// пинг свёрнутому окну: мигание иконкой (звук и тост идут отдельно и всегда)
function pingHidden() {
  if (!document.hidden || isDemo) return;
  invoke("flash_window").catch(() => {});
}
// гудок: кастомная мелодия файлом или встроенные мелодии
let ringbackTimer = null, ringbackAudio = null;
let incomingRingTimer = null, incomingRingAudio = null;

function incomingRingtoneStop() {
  if (incomingRingTimer) { clearTimeout(incomingRingTimer); incomingRingTimer = null; }
  if (incomingRingAudio) { try { incomingRingAudio.pause(); incomingRingAudio.currentTime = 0; } catch {} incomingRingAudio = null; }
}

function incomingRingtoneStart() {
  ringbackStop();
  incomingRingtoneStop();
  if (!settings?.sound) return;
  if (settings.ringtone) {
    try {
      incomingRingAudio = new Audio(tauriFile(settings.ringtone));
      incomingRingAudio.loop = true;
      const spk = settings.speaker_id;
      if (spk && incomingRingAudio.setSinkId) incomingRingAudio.setSinkId(spk).catch(() => {});
      incomingRingAudio.play().catch(() => incomingRingtoneStartBuiltIn());
      return;
    } catch {}
  }
  incomingRingtoneStartBuiltIn();
}

function incomingRingtoneStartBuiltIn() {
  if (!settings?.sound) return;
  const melody = settings.incoming_ringtone_melody || "default";
  const notes = RING_MELODIES[melody] || RING_MELODIES.default;
  const duration = Math.max(...notes.map(([, offset, dur]) => offset + dur));
  const play = () => {
    playRingMelody(melody);
    incomingRingTimer = setTimeout(play, duration + 1100);
  };
  play();
}

function ringbackStart() {
  ringbackStop();
  if (!settings?.sound) return;
  if (settings?.ringback_file) {
    try {
      ringbackAudio = new Audio(tauriFile(settings.ringback_file));
      ringbackAudio.loop = true;
      const spk = settings.speaker_id;
      if (spk && ringbackAudio.setSinkId) ringbackAudio.setSinkId(spk).catch(() => {});
      ringbackAudio.play().catch(() => ringbackBeeps());
      return;
    } catch {}
  }
  const melody = settings?.ringback_melody || "default";
  if (melody !== "default" && RING_MELODIES[melody]) {
    const play = () => {
      playRingMelody(melody);
      ringbackTimer = setTimeout(play, 2500);
    };
    play();
    return;
  }
  const beep = () => {
    blip(440, 400);
    ringbackTimer = setTimeout(beep, 2000);
  };
  beep();
}

function ringbackBeeps() {
  if (!settings?.sound) return;
  const beep = () => {
    blip(440, 400);
    ringbackTimer = setTimeout(beep, 2000);
  };
  beep();
}

function ringbackStop() {
  if (ringbackTimer) { clearTimeout(ringbackTimer); ringbackTimer = null; }
  if (ringbackAudio) { try { ringbackAudio.pause(); } catch {} ringbackAudio = null; }
  incomingRingtoneStop();
}
// ---------- WebRTC calls ----------
let pc = null, currentCall = null, callTimer = null, callSec = 0, pendingOffer = null, callSetupPeer = null;
let iceOutBuf = [], iceInBuf = []; // кандидаты, пришедшие раньше готовности
function sendIce(peerIp, cand) {
  invoke("sig_send", { peerIp, payload: { kind: "ice", from: profile.nickname || settings.device_name, candidate: JSON.stringify(cand) } }).catch(() => {});
}
function flushOutIce(peerIp) {
  iceOutBuf = iceOutBuf.filter((x) => {
    if (x.peerIp === peerIp) { sendIce(peerIp, x.cand); return false; }
    return true;
  });
}
async function flushInIce() {
  if (!pc || !pc.remoteDescription) return;
  const rest = [];
  for (const c of iceInBuf) {
    try { await pc.addIceCandidate(JSON.parse(c)); } catch { rest.push(c); }
  }
  iceInBuf = rest;
}
function ensurePC(peerIp) {
  if (pc) return pc;
  pc = new RTCPeerConnection({ iceServers: [{ urls: "stun:stun.l.google.com:19302" }] });
  pc.onicecandidate = (e) => {
    if (!e.candidate) return;
    if (currentCall === peerIp) sendIce(peerIp, e.candidate);
    else iceOutBuf.push({ peerIp, cand: e.candidate }); // уйдут при showCallUI
  };
  pc.ontrack = (e) => { attachRemoteVoice(peerIp, e.streams?.[0] || new MediaStream([e.track])); };
  pc.onconnectionstatechange = () => {
    const st = pc?.connectionState;
    if (st === "failed") toast("Прямое соединение не установилось — проверь Radmin и firewall", true);
    if (st === "connected") { toast("Голосовое соединение установлено <svg class='ic' viewBox='0 0 24 24'><path d='M9,16.17,4.83,12l-1.42,1.41L9,19,21,7l-1.41-1.41Z'/></svg>"); }
  };
  const audioStream = micTrackStream();
  micTracks().forEach((t) => { try { pc.addTrack(t, audioStream); } catch {} });
  return pc;
}
function showCallUI(peerIp) {
  currentCall = peerIp;
  callScreenOpen = true;
  bindSharesToCall(screenCallId(), [peerIp]);
  flushOutIce(peerIp); // отправить накопившиеся кандидаты
  ringbackStop();
  $("#btn-hangup").classList.remove("hidden");
  startCallTimer(); renderConv(); renderCallPanel(); renderCallStage(); updateVoiceBar(); updateBusy();
}
function hideCallUI() {
  const endedPeer = currentCall;
  if (endedPeer) {
    dropRemoteVoice(endedPeer);
    void closeScreensForPeer(endedPeer);
  }
  currentCall = null; clearInterval(callTimer); ringbackStop(); ringbackStop();
  try { wtEndCall(); } catch (e) { console.error("watch end", e); }
  iceOutBuf = []; iceInBuf = [];
  $("#btn-hangup").classList.add("hidden");
  $("#call-panel").classList.add("hidden");
  monDrop("me");
  if (pc) { try { pc.close(); } catch {} pc = null; }
  const a = $("#remote-audio"); if (a) a.srcObject = null;
  updateVoiceBar(); renderConv(); renderCallPanel(); renderCallStage(); updateBusy();
}
async function startCall(peerIp) {
  if (!peerIp) return toast("Сначала выбери контакт", true);
  if (currentCall || groupCall || pendingOffer || callSetupPeer) return toast("Сначала заверши текущий звонок или ответь на приглашение", true);
  callSetupPeer = peerIp;
  try { await startMic(); } catch { callSetupPeer = null; showHelp("Микрофон заблокирован", MIC_HELP, "<svg class='ic' viewBox='0 0 24 24'><path d='M20,11A8,8,0,0,0,5.64,6.64L4,8.29V4H8.29l1.59,1.59A10,10,0,0,1,22,13Z'/><path d='M4,13A8,8,0,0,0,18.36,17.36L20,15.71V20H15.71l-1.59-1.59A10,10,0,0,1,2,11Z'/></svg> Сбросить запрет (перезапуск)", async () => invoke("fix_mic_blocked").catch((x) => toast("" + x, true))); return; }
  try {
    ensurePC(peerIp);
    const offer = await pc.createOffer();
    await pc.setLocalDescription(offer);
    showCallUI(peerIp);
    ringbackStart();
    await invoke("sig_send", { peerIp, payload: { kind: "call-offer", from: profile.nickname || settings.device_name, sdp: JSON.stringify(offer) } });
  } catch (e) { toast("Звонок не вышел: " + e, true); hideCallUI(); }
  finally { callSetupPeer = null; }
}
async function acceptCall() {
  if (!pendingOffer || currentCall || groupCall || callSetupPeer) return;
  const { ip, sdp } = pendingOffer; pendingOffer = null;
  callSetupPeer = ip;
  $("#call-modal").classList.add("hidden"); ringbackStop();
  try { await startMic(); } catch { callSetupPeer = null; return; }
  try {
    ensurePC(ip);
    await pc.setRemoteDescription(JSON.parse(sdp));
    await flushInIce();
    const ans = await pc.createAnswer();
    await pc.setLocalDescription(ans);
    await invoke("sig_send", { peerIp: ip, payload: { kind: "call-answer", from: profile.nickname || settings.device_name, sdp: JSON.stringify(ans) } });
    showCallUI(ip);
  } catch (e) { toast("Не удалось принять: " + e, true); hideCallUI(); }
  finally { callSetupPeer = null; }
}
async function hangup(peerIp = currentCall) {
  ringbackStop();
  const target = peerIp;
  hideCallUI();
  if (target) invoke("sig_send", { peerIp: target, payload: { kind: "call-hangup", from: myNick() } }).catch(() => {});
}

// ---------- contact picker (для отправки из проводника) ----------
function openPicker(file) {
  pendingFile = file;
  $("#pick-file").textContent = fileName(file);
  const box = $("#pick-list"); box.innerHTML = "";
  if (!peers.length) box.innerHTML = `<div class="empty-hint">Нет контактов — добавь через <svg class="ic" viewBox="0 0 24 24"><path d="M19,11H13V5a1,1,0,0,0-2,0v6H5a1,1,0,0,0,0,2h6v6a1,1,0,0,0,2,0V13h6a1,1,0,0,0,0-2Z"/></svg> или <svg class="ic" viewBox="0 0 24 24"><circle cx="10" cy="10" r="8"/><path d="M21.71,21.71a1,1,0,0,1-1.42,0L15.61,17A8.67,8.67,0,0,0,17,15.61l4.68,4.68A1,1,0,0,1,21.71,21.71Z"/></svg>.</div>`;
  for (const p of peers) {
    const d = document.createElement("div");
    d.className = "pick-row";
    d.innerHTML = `${avaHTML(p, "sm")}<div><b>${esc(dispName(p))}</b><div class="muted">${esc(p.ip)} · ${p.online ? "в сети" : "не в сети"}</div></div>`;
    d.onclick = async () => { $("#pick-modal").classList.add("hidden"); await selectPeer(p.ip); sendPaths(p.ip, [pendingFile]); };
    box.appendChild(d);
  }
  $("#pick-modal").classList.remove("hidden");
}
$("#pick-cancel").onclick = () => $("#pick-modal").classList.add("hidden");
async function handlePending(pend) {
  if (!pend?.file) return;
  toast(`<svg class='ic' viewBox='0 0 24 24'><path d='M10,4H4A2,2,0,0,0,2,6V18a2,2,0,0,0,2,2H20a2,2,0,0,0,2-2V8a2,2,0,0,0-2-2H12Z'/></svg> Файл из проводника: ${fileName(pend.file)}`);
  if (pend.contact) {
    const p = peers.find((x) => x.ip === pend.contact);
    if (!p) { toast(`Контакт ${pend.contact} не найден — выбери вручную`, true); openPicker(pend.file); return; }
    await selectPeer(p.ip);
    sendPaths(p.ip, [pend.file]);
  } else openPicker(pend.file);
}

// ---------- call panel (громкости без захода в настройки) ----------
function inVoice() { return !!(currentCall || groupCall); }
// занят звонком/показом: просим бэкенд не душить ретрансляцию сканами
function updateBusy() {
  const busy = inVoice() || Object.keys(myShares).length > 0 || Object.keys(views).length > 0;
  invoke("set_busy", { busy }).catch(() => {});
}
function renderCallPanel() {
  const box = $("#cp-users"); if (!box) return; box.innerHTML = "";
  const parts = [];
  if (currentCall) parts.push(currentCall);
  if (groupCall) {
    $("#cp-title").textContent = `Группа: ${groupCall.name}`;
    for (const m of groupCall.members) if (!parts.includes(m)) parts.push(m);
  } else if (currentCall) {
    const p = peers.find((x) => x.ip === currentCall);
    $("#cp-title").textContent = p ? dispName(p) : currentCall;
  }
  for (const ip of parts) {
    const p = peers.find((x) => x.ip === ip);
    const alive = currentCall === ip || !!gpc[ip]?.pc;
    const v = peerVol(ip);
    const row = document.createElement("div");
    row.className = "cp-user";
    row.innerHTML = `<b>${esc(p ? dispName(p) : ip)}${alive ? "" : " (вышел)"}</b><span class="cp-vol">${v}%</span><input type="range" min="0" max="200" value="${v}" /><button>${v > 0 ? '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><path d="M16.5,12a4.5,4.5,0,0,0-2.5-4v8A4.5,4.5,0,0,0,16.5,12Z"/><path d="M14,3.23v2.06a7,7,0,0,1,0,13.42v2.06a9,9,0,0,0,0-17.54Z"/></svg>' : '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><line x1="23" y1="9" x2="17" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><line x1="17" y1="9" x2="23" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>'}</button>`;
    const r = row.querySelector("input"), b = row.querySelector("button"), pctLabel = row.querySelector(".cp-vol");
    r.oninput = () => {
      ampSet("v:" + ip, (deafened || r.value == 0) ? 0 : r.value);
      if (pctLabel) pctLabel.textContent = `${r.value}%`;
      b.innerHTML = r.value > 0 ? '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><path d="M16.5,12a4.5,4.5,0,0,0-2.5-4v8A4.5,4.5,0,0,0,16.5,12Z"/><path d="M14,3.23v2.06a7,7,0,0,1,0,13.42v2.06a9,9,0,0,0,0-17.54Z"/></svg>' : '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><line x1="23" y1="9" x2="17" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><line x1="17" y1="9" x2="23" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>';
    };
    r.onchange = () => setPeerVol(ip, Number(r.value)).then(() => invoke("save_settings", { settings }).catch(() => {}));
    b.onclick = () => {
      const nv = (Number(r.value) > 0 ? 0 : 100);
      r.value = nv;
      ampSet("v:" + ip, (deafened || nv === 0) ? 0 : nv);
      b.innerHTML = nv > 0 ? '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><path d="M16.5,12a4.5,4.5,0,0,0-2.5-4v8A4.5,4.5,0,0,0,16.5,12Z"/><path d="M14,3.23v2.06a7,7,0,0,1,0,13.42v2.06a9,9,0,0,0,0-17.54Z"/></svg>' : '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><line x1="23" y1="9" x2="17" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><line x1="17" y1="9" x2="23" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>';
      setPeerVol(ip, nv).then(() => invoke("save_settings", { settings }).catch(() => {}));
    };
    box.appendChild(row);
  }
  renderShareList();
}
$("#cp-mute").onclick = toggleMic;
$("#cp-micvol").addEventListener("input", (e) => { settings.mic_volume = Number(e.target.value); applyMicVolume(); });
$("#cp-micvol").addEventListener("change", () => invoke("save_settings", { settings }).catch(() => {}));
$("#cp-hang").onclick = () => { if (groupCall) hangupGroup(); else hangup(); };
$("#cp-close").onclick = () => { $("#call-panel").classList.add("hidden"); syncCallScreenControls(); };
document.addEventListener("pointerdown", (e) => {
  const panel = $("#call-panel");
  if (!panel || panel.classList.contains("hidden")) return;
  if (panel.contains(e.target) || e.target.closest?.("#vb-panel") || e.target.closest?.("#cp-users")) return;
  panel.classList.add("hidden");
});

// ---------- Discord-style call UI: сцена сверху + голосовой бар ----------
// deafen (заглушить весь входящий звук), speaking-индикаторы, единый мут
let deafened = false;
const voiceEls = {};
let monCtx = null, monTimer = null;
const voiceMon = {};
function effPeerVol(ip) { return (deafened || peerVol(ip) === 0) ? 0 : peerVol(ip); }
function applyDeafen() {
  for (const ip of Object.keys(voiceEls)) ampSet("v:" + ip, deafened ? 0 : peerVol(ip));
  for (const id of Object.keys(views)) applyViewVolume(id);
  syncVbDeaf();
}
function toggleDeafen() {
  deafened = !deafened;
  applyDeafen(); syncVbDeaf();
  toast(deafened ? "<svg class='ic' viewBox='0 0 24 24'><path d='M3,9v6h4l5,5V4L7,9Z'/><line x1='23' y1='9' x2='17' y2='15' stroke='currentColor' stroke-width='2' stroke-linecap='round'/><line x1='17' y1='9' x2='23' y2='15' stroke='currentColor' stroke-width='2' stroke-linecap='round'/></svg> Весь звук заглушён" : "<svg class='ic' viewBox='0 0 24 24'><path d='M3,9v6h4l5,5V4L7,9Z'/><path d='M16.5,12a4.5,4.5,0,0,0-2.5-4v8A4.5,4.5,0,0,0,16.5,12Z'/></svg> Звук включён");
}
function monEnsure() {
  if (!monCtx) { try { monCtx = new (window.AudioContext || window.webkitAudioContext)(); } catch {} }
  if (monCtx?.state === "suspended") monCtx.resume().catch(() => {});
  return monCtx;
}
function monWatch(key, stream) {
  try {
    const old = voiceMon[key];
    if (old) { try { old.src.disconnect(); } catch {} }
    const ctx = monEnsure(); if (!ctx) return;
    const src = ctx.createMediaStreamSource(stream);
    const an = ctx.createAnalyser(); an.fftSize = 512; an.smoothingTimeConstant = 0.25;
    src.connect(an);
    voiceMon[key] = { an, data: new Uint8Array(an.fftSize), src, speaking: false };
    startMonTimer();
  } catch {}
}
function monDrop(key) {
  const old = voiceMon[key];
  if (old) { try { old.src.disconnect(); } catch {} }
  delete voiceMon[key];
  document.querySelectorAll(`[data-spk="${key}"]`).forEach((el) => el.classList.remove("speaking"));
  if (!Object.keys(voiceMon).length && monTimer) { clearInterval(monTimer); monTimer = null; }
}
function startMonTimer() {
  if (monTimer) return;
  monTimer = setInterval(() => {
    for (const [key, m] of Object.entries(voiceMon)) {
      try {
        m.an.getByteTimeDomainData(m.data);
        let sum = 0;
        for (let i = 0; i < m.data.length; i++) {
          const x = (m.data[i] - 128) / 128;
          sum += x * x;
        }
        const rms = Math.sqrt(sum / m.data.length);
        // hysteresis: speech starts at 0.025 RMS and stops at 0.012 RMS,
        // so a silent track (or room noise) cannot keep the ring lit.
        const on = m.speaking ? rms > 0.012 : rms > 0.025;
        if (on === m.speaking) continue;
        m.speaking = on;
        document.querySelectorAll(`[data-spk="${key}"]`).forEach((el) => el.classList.toggle("speaking", on));
      } catch {}
    }
  }, 150);
}
function tileHTML({ key, ava, name, sub, mic, off, screen, onwatch }) {
  return `<div class="vtile${off ? " off" : ""}" data-spk="${esc(key)}">${ava}<b>${esc(name)}</b><small>${esc(sub)}</small><span class="mic-ic">${mic}</span>${screen || ""}${onwatch || ""}</div>`;
}
function renderCallStage() {
  const box = $("#call-stage"); if (!box) return;
  if (!inVoice()) { box.classList.add("hidden"); box.innerHTML = ""; renderCallScreen(); return; }
  box.classList.remove("hidden");
  box.innerHTML = "";
  const myLetter = ((profile.nickname || settings?.device_name || "?"))[0].toUpperCase();
  const me = document.createElement("div");
  me.innerHTML = tileHTML({ key: "me", ava: `<span class="ava lg"><span>${avaInner(profile.avatar, myLetter)}</span></span>`, name: "Ты", sub: deafened ? "звук выключен" : (micMuted ? "мут" : "в звонке"), mic: micMuted ? '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><line x1="23" y1="9" x2="17" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><line x1="17" y1="9" x2="23" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>' : '<svg class="ic" viewBox="0 0 24 24"><path d="M12,2a3,3,0,0,0-3,3v6a3,3,0,0,0,6,0V5A3,3,0,0,0,12,2Z"/><path d="M19,10v1a7,7,0,0,1-14,0V10H3v1a9,9,0,0,0,8,8.94V22h2V19.94A9,9,0,0,0,21,11V10Z"/></svg>' });
  box.appendChild(me.firstChild);
  const addPeer = (ip) => {
    const p = peers.find((x) => x.ip === ip);
    const alive = currentCall === ip || !!gpc[ip]?.pc;
    const d = document.createElement("div");
    d.innerHTML = tileHTML({ key: ip, ava: avaHTML(p || { name: ip, ip, online: alive }, "lg"), name: p ? dispName(p) : ip, sub: alive ? `${p?.ping_ms ?? "?"} мс` : "вышел", mic: '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><path d="M16.5,12a4.5,4.5,0,0,0-2.5-4v8A4.5,4.5,0,0,0,16.5,12Z"/><path d="M14,3.23v2.06a7,7,0,0,1,0,13.42v2.06a9,9,0,0,0,0-17.54Z"/></svg>', off: !alive });
    box.appendChild(d.firstChild);
  };
  if (currentCall) addPeer(currentCall);
  if (groupCall) for (const m of groupCall.members) {
    if (myIp && m === myIp) continue; // себя не дублируем (есть плитка "Ты")
    addPeer(m);
  }
  // активные демонстрации — мини-плитки со стрелкой смотреть
  for (const [id, s] of Object.entries(myShares)) {
    const d = document.createElement("div");
    d.className = "vtile screen-tile"; d.dataset.spk = "share-" + id;
    d.innerHTML = `<b><svg class="ic" viewBox="0 0 24 24" style="vertical-align:-3px;margin-right:4px"><path d="M14,10V22H4a2,2,0,0,1-2-2V10Z"/><path d="M22,10V20a2,2,0,0,1-2,2H16V10Z"/><path d="M22,4V8H2V4A2,2,0,0,1,4,2H20A2,2,0,0,1,22,4Z"/></svg> мой экран</b><small>${Object.keys(s.pcs).length} зрит.</small><span class="mic-ic"><svg class="ic" viewBox="0 0 24 24"><path d="M8,5v14l11-7z"/></svg></span>`;
    const pv = document.createElement("video");
    pv.className = "mini-prev"; pv.muted = true; pv.playsInline = true; pv.srcObject = s.stream;
    pv.play().catch(() => {});
    d.insertBefore(pv, d.querySelector("small"));
    d.onclick = (e) => { if (e.target.closest(".mini-prev")) return; switchSeg("screens"); };
    box.appendChild(d);
  }
  for (const [id, v] of Object.entries(views)) {
    const d = document.createElement("div");
    d.className = "vtile screen-tile"; d.dataset.spk = "share-" + id;
    d.innerHTML = `<b>${esc(v.label)}</b><small>демонстрация</small><button class="watch-btn">Смотреть</button>`;
    d.querySelector(".watch-btn").onclick = (e) => { e.stopPropagation(); openWatch(id); };
    d.onclick = () => openWatch(id);
    box.appendChild(d);
  }
  const wtInfo = typeof wtSessionInfo === "function" ? wtSessionInfo() : null;
  if (wtInfo) {
    const d = document.createElement("div");
    d.className = "vtile screen-tile wt-call-tile";
    d.innerHTML = wtTileHTML(wtInfo);
    d.querySelector(".watch-btn").onclick = (e) => { e.stopPropagation(); wtOpen(); };
    d.onclick = () => wtOpen();
    box.appendChild(d);
  }
  renderCallScreen();
}
function switchSeg(name) {
  document.querySelectorAll("#seg .seg-btn").forEach((x) => x.classList.toggle("active", x.dataset.seg === name));
  const filesBtn = $("#btn-files-pane");
  if (filesBtn) filesBtn.classList.toggle("active", name === "files");
  $("#chat-pane").classList.toggle("hidden", name !== "chat");
  $("#files-pane").classList.toggle("hidden", name !== "files");
  $("#screens-pane").classList.toggle("hidden", name !== "screens");
  if (name === "chat") requestAnimationFrame(() => {
    const box = $("#messages");
    if (lastChatAtBottom) box.scrollTop = box.scrollHeight;
    updateChatBottomButton();
  });
}
$("#btn-files-pane").onclick = () => switchSeg($("#files-pane").classList.contains("hidden") ? "files" : "chat");
function updateVoiceBar() {
  const bar = $("#voice-bar"); if (!bar) return;
  if (!inVoice()) { bar.classList.add("hidden"); $("#vb-watch").classList.add("hidden"); renderCallScreen(); return; }
  bar.classList.remove("hidden"); $("#vb-watch").classList.remove("hidden");
  let label = "В звонке";
  if (groupCall) label = `Группа: ${groupCall.name}`;
  else if (currentCall) { const p = peers.find((x) => x.ip === currentCall); label = p ? dispName(p) : currentCall; }
  $("#vb-text").textContent = label;
  syncVbMic(); syncVbDeaf();
  renderCallScreen();
}
function syncVbMic() {
  const b = $("#vb-mic"); if (b) { b.innerHTML = micMuted ? '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><line x1="23" y1="9" x2="17" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><line x1="17" y1="9" x2="23" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>' : '<svg class="ic" viewBox="0 0 24 24"><path d="M12,2a3,3,0,0,0-3,3v6a3,3,0,0,0,6,0V5A3,3,0,0,0,12,2Z"/><path d="M19,10v1a7,7,0,0,1-14,0V10H3v1a9,9,0,0,0,8,8.94V22h2V19.94A9,9,0,0,0,21,11V10Z"/></svg>'; b.classList.toggle("off", micMuted); }
  const m = $("#me-mic"); if (m) m.innerHTML = micMuted ? '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><line x1="23" y1="9" x2="17" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><line x1="17" y1="9" x2="23" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>' : '<svg class="ic" viewBox="0 0 24 24"><path d="M12,2a3,3,0,0,0-3,3v6a3,3,0,0,0,6,0V5A3,3,0,0,0,12,2Z"/><path d="M19,10v1a7,7,0,0,1-14,0V10H3v1a9,9,0,0,0,8,8.94V22h2V19.94A9,9,0,0,0,21,11V10Z"/></svg>';
  const c = $("#cp-mute"); if (c) c.innerHTML = micMuted ? '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><line x1="23" y1="9" x2="17" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><line x1="17" y1="9" x2="23" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>' : '<svg class="ic" viewBox="0 0 24 24"><path d="M12,2a3,3,0,0,0-3,3v6a3,3,0,0,0,6,0V5A3,3,0,0,0,12,2Z"/><path d="M19,10v1a7,7,0,0,1-14,0V10H3v1a9,9,0,0,0,8,8.94V22h2V19.94A9,9,0,0,0,21,11V10Z"/></svg>';
  syncCallScreenControls();
}
function syncVbDeaf() {
  const b = $("#vb-deaf"); if (b) { b.innerHTML = deafened ? '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><line x1="23" y1="9" x2="17" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><line x1="17" y1="9" x2="23" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>' : '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><path d="M16.5,12a4.5,4.5,0,0,0-2.5-4v8A4.5,4.5,0,0,0,16.5,12Z"/></svg>'; b.classList.toggle("off", deafened); }
  syncCallScreenControls();
}
let callScreenOpen = true;
let callScreenEscBound = false;
function onCallScreenEsc(e) {
  if (e.key !== "Escape" || !callScreenOpen) return;
  if (document.querySelector(".overlay:not(.hidden)")) return;
  if (!$("#call-panel")?.classList.contains("hidden")) return;
  const menu = $("#cs-menu");
  if (menu && !menu.classList.contains("hidden")) {
    menu.classList.add("hidden");
    e.preventDefault();
    return;
  }
  e.preventDefault();
  callScreenOpen = false;
  renderCallScreen();
}
function syncCallScreenEsc(active) {
  if (active && !callScreenEscBound) {
    document.addEventListener("keydown", onCallScreenEsc, true);
    callScreenEscBound = true;
  } else if (!active && callScreenEscBound) {
    document.removeEventListener("keydown", onCallScreenEsc, true);
    callScreenEscBound = false;
  }
}
function syncCallScreenControls() {
  const mic = $("#cs-mic");
  if (mic) {
    mic.classList.toggle("off", micMuted);
    mic.innerHTML = micMuted
      ? '<svg viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><line x1="23" y1="9" x2="17" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><line x1="17" y1="9" x2="23" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg><svg class="chev" viewBox="0 0 24 24"><path d="M7 10l5 5 5-5z"/></svg>'
      : '<svg viewBox="0 0 24 24"><path d="M12 2a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3z"/><path d="M19 10v1a7 7 0 0 1-14 0v-1H3v1a9 9 0 0 0 8 8.9V22h2v-2.1a9 9 0 0 0 8-8.9v-1z"/></svg><svg class="chev" viewBox="0 0 24 24"><path d="M7 10l5 5 5-5z"/></svg>';
  }
  const deaf = $("#cs-deaf");
  if (deaf) {
    deaf.classList.toggle("off", deafened);
    deaf.innerHTML = deafened
      ? '<svg viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><line x1="23" y1="9" x2="17" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><line x1="17" y1="9" x2="23" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg><svg class="chev" viewBox="0 0 24 24"><path d="M7 10l5 5 5-5z"/></svg>'
      : '<svg viewBox="0 0 24 24"><path d="M12 2a9 9 0 0 0-9 9v7a3 3 0 0 0 3 3h2v-8H5v-2a7 7 0 0 1 14 0v2h-3v8h2a3 3 0 0 0 3-3v-7a9 9 0 0 0-9-9z"/></svg><svg class="chev" viewBox="0 0 24 24"><path d="M7 10l5 5 5-5z"/></svg>';
  }
  const cam = $("#cs-cam");
  if (cam) cam.classList.toggle("off", Object.values(myShares).some((s) => s.kind === "camera"));
  const screen = $("#cs-screen");
  if (screen) screen.classList.toggle("off", Object.values(myShares).some((s) => s.kind !== "camera"));
  const panel = $("#cs-panel");
  if (panel) panel.classList.toggle("off", !$("#call-panel")?.classList.contains("hidden"));
}
function callPeerByIp(ip) {
  return peers.find((x) => x.ip === ip) || null;
}
function callAvatarHTML(p, name) {
  const nm = name || (p ? dispName(p) : "?");
  return `<span class="ava lg${p?.online === false ? " off" : ""}"><span>${avaInner(p ? dispAva(p) : "", (nm || "?").toUpperCase()[0])}</span></span>`;
}
function callBadgeInfo() {
  if (groupCall) {
    const g = groups.find((x) => x.id === groupCall.id);
    const title = groupCall.name || g?.name || "Группа";
    return { title, av: g?.avatar || "", letter: (title || "G")[0].toUpperCase() };
  }
  if (currentCall) {
    const p = callPeerByIp(currentCall);
    const title = p ? dispName(p) : currentCall;
    return { title, av: p ? dispAva(p) : "", letter: (title || "?")[0].toUpperCase() };
  }
  return { title: "Звонок", av: "", letter: "S" };
}
function renderCallScreenStage() {
  const box = $("#cs-stage"), sharesBox = $("#cs-shares"), peopleBox = $("#cs-people");
  if (!box || !sharesBox || !peopleBox) return;
  sharesBox.innerHTML = "";
  peopleBox.innerHTML = "";
  const q = ($("#cs-search")?.value || "").trim().toLowerCase();
  const match = (s) => !q || String(s || "").toLowerCase().includes(q);
  const shares = [];
  for (const [id, s] of Object.entries(myShares)) {
    if (!match(s.label || (s.kind === "camera" ? "камера" : "экран"))) continue;
    shares.push({ id, stream: s.stream, label: s.label || (s.kind === "camera" ? "Моя камера" : "Мой экран"), mine: true });
  }
  for (const [id, v] of Object.entries(views)) {
    if (!match(v.label)) continue;
    shares.push({ id, stream: v.stream, label: v.label, mine: false });
  }
  box.classList.toggle("has-shares", shares.length > 0);
  for (const s of shares) {
    const tile = document.createElement("div");
    tile.className = "cs-share";
    const vid = document.createElement("video");
    vid.muted = true; vid.playsInline = true; vid.autoplay = true;
    if (vid.srcObject !== s.stream) vid.srcObject = s.stream;
    vid.play().catch(() => {});
    tile.appendChild(vid);
    const label = document.createElement("span");
    label.className = "cs-share-label";
    label.textContent = s.label;
    tile.appendChild(label);
    const btn = document.createElement("button");
    btn.className = "watch-btn";
    btn.textContent = s.mine ? "Остановить" : "Смотреть";
    btn.onclick = (e) => { e.stopPropagation(); if (s.mine) stopShare(s.id); else openWatch(s.id, true); };
    tile.appendChild(btn);
    tile.onclick = () => { if (!s.mine) openWatch(s.id, true); };
    sharesBox.appendChild(tile);
  }
  const wtInfo = typeof wtSessionInfo === "function" ? wtSessionInfo() : null;
  if (wtInfo && match(wtInfo.title)) {
    const tile = document.createElement("div");
    tile.className = "cs-share wt-tile";
    tile.innerHTML = wtTileHTML(wtInfo);
    tile.querySelector(".watch-btn").onclick = (e) => { e.stopPropagation(); wtOpen(); };
    tile.onclick = () => wtOpen();
    sharesBox.appendChild(tile);
    box.classList.add("has-shares");
  }
  const people = [];
  if (groupCall) {
    people.push({ key: "me", name: "Ты", ava: `<span class="ava lg"><span>${avaInner(profile.avatar, ((profile.nickname || settings?.device_name || "?")[0] || "?").toUpperCase())}</span></span>`, muted: micMuted });
    for (const m of groupCall.members) {
      if (myIp && m === myIp) continue;
      const p = callPeerByIp(m);
      const nm = p ? dispName(p) : m;
      if (!match(nm)) continue;
      people.push({ key: m, name: nm, ava: callAvatarHTML(p, nm), muted: false });
    }
  } else if (currentCall) {
    const p = callPeerByIp(currentCall);
    const nm = p ? dispName(p) : currentCall;
    if (match(nm)) people.push({ key: currentCall, name: nm, ava: callAvatarHTML(p, nm), muted: false });
  }
  for (const person of people) {
    const tile = document.createElement("div");
    tile.className = "cs-person";
    tile.dataset.spk = person.key;
    tile.innerHTML = `${person.ava}<b>${esc(person.name)}</b>${person.muted ? '<span class="cs-mic"><svg viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><line x1="23" y1="9" x2="17" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><line x1="17" y1="9" x2="23" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg></span>' : ""}`;
    peopleBox.appendChild(tile);
  }
  box.classList.toggle("single", !shares.length && people.length === 1);
  if (!shares.length && !people.length && !wtInfo) sharesBox.innerHTML = '<div class="muted">Никого не найдено</div>';
}
function renderCallScreen() {
  const scr = $("#call-screen"); if (!scr) return;
  const active = inVoice() && callScreenOpen;
  scr.classList.toggle("hidden", !active);
  document.body.classList.toggle("call-screen", active);
  document.body.classList.toggle("call-compact", inVoice() && !callScreenOpen);
  syncCallScreenEsc(active);
  if (!active) {
    const sb = $("#cs-shares"), pb = $("#cs-people");
    if (sb) sb.innerHTML = "";
    if (pb) pb.innerHTML = "";
    return;
  }
  const info = callBadgeInfo();
  const badge = $("#cs-badge"), name = $("#cs-title"), search = $("#cs-search");
  if (badge) {
    badge.innerHTML = info.av ? `<img src="${esc(info.av)}" alt="${esc(info.letter)}">` : esc(info.letter);
    const img = badge.querySelector("img");
    if (img) img.onerror = () => { badge.textContent = info.letter; };
  }
  if (name) name.textContent = info.title;
  if (search) search.placeholder = `Искать «${info.title}»`;
  syncCallScreenControls();
  renderCallScreenStage();
}
function toggleMic() {
  micMuted = !micMuted;
  localStream?.getAudioTracks().forEach((t) => (t.enabled = !micMuted));
  syncVbMic(); renderCallStage();
  toast(micMuted ? "Микрофон выключен" : "Микрофон включён");
}
$("#vb-mic").onclick = toggleMic;
$("#vb-deaf").onclick = toggleDeafen;
$("#vb-screen").onclick = startScreen;
$("#vb-cam").onclick = startCamera;
$("#vb-panel").onclick = () => $("#call-panel").classList.toggle("hidden");
$("#vb-hang").onclick = () => { if (groupCall) hangupGroup(); else hangup(); };
$("#vb-open").onclick = () => { callScreenOpen = true; renderCallScreen(); };
$("#cs-mic").onclick = toggleMic;
$("#cs-deaf").onclick = toggleDeafen;
$("#cs-cam").onclick = startCamera;
$("#cs-screen").onclick = startScreen;
$("#cs-panel").onclick = () => { $("#call-panel").classList.toggle("hidden"); syncCallScreenControls(); };
$("#cs-hang").onclick = () => { if (groupCall) hangupGroup(); else hangup(); };
$("#cs-chat").onclick = () => { callScreenOpen = false; renderCallScreen(); };
$("#cs-more").onclick = (e) => { e.stopPropagation(); $("#cs-menu").classList.toggle("hidden"); };
$("#cs-menu-chat").onclick = () => { $("#cs-menu").classList.add("hidden"); callScreenOpen = false; renderCallScreen(); };
$("#cs-menu-files").onclick = () => { $("#cs-menu").classList.add("hidden"); callScreenOpen = false; renderCallScreen(); switchSeg("files"); };
$("#cs-menu-panel").onclick = () => { $("#cs-menu").classList.add("hidden"); $("#call-panel").classList.toggle("hidden"); };
$("#cs-search").addEventListener("input", renderCallScreenStage);
document.addEventListener("pointerdown", (e) => {
  const menu = $("#cs-menu");
  if (!menu || menu.classList.contains("hidden")) return;
  if (!e.target.closest?.("#cs-menu") && !e.target.closest?.("#cs-more")) menu.classList.add("hidden");
});

// ---------- group voice mesh ----------
let gpc = {}, groupCall = null;
let ginBuf = {};
function ensureGPC(peerIp) {
  if (gpc[peerIp]?.pc) return gpc[peerIp].pc;
  const c = new RTCPeerConnection({ iceServers: [{ urls: "stun:stun.l.google.com:19302" }] });
  gpc[peerIp] = { pc: c };
  c.onicecandidate = (e) => {
    if (!e.candidate) return;
    invoke("sig_send", { peerIp, payload: { kind: "ice", from: myNick(), candidate: JSON.stringify(e.candidate) } }).catch(() => {});
  };
  c.ontrack = (e) => { attachRemoteVoice(peerIp, e.streams?.[0] || new MediaStream([e.track])); renderCallPanel(); };
  c.onconnectionstatechange = () => {
    if (c.connectionState === "connected") { toast(`<svg class='ic' viewBox='0 0 24 24'><path d='M3,9v6h4l5,5V4L7,9Z'/><path d='M16.5,12a4.5,4.5,0,0,0-2.5-4v8A4.5,4.5,0,0,0,16.5,12Z'/><path d='M14,3.23v2.06a7,7,0,0,1,0,13.42v2.06a9,9,0,0,0,0-17.54Z'/></svg> ${peerIp} в звонке`); renderCallPanel(); }
    if (c.connectionState === "failed") toast(`${peerIp}: нет прямого соединения`, true);
  };
  const audioStream = micTrackStream();
  micTracks().forEach((t) => { try { c.addTrack(t, audioStream); } catch {} });
  return c;
}
async function flushGin(ip) {
  const g = gpc[ip];
  if (!g?.pc?.remoteDescription) return;
  const buf = ginBuf[ip] || []; ginBuf[ip] = [];
  for (const cd of buf) { try { await g.pc.addIceCandidate(JSON.parse(cd)); } catch {} }
}
async function offerTo(peerIp, group) {
  const c = ensureGPC(peerIp);
  const offer = await c.createOffer();
  await c.setLocalDescription(offer);
  await invoke("sig_send", { peerIp, payload: { kind: "call-offer", from: myNick(), sdp: JSON.stringify(offer), group: { id: group.id, name: group.name, members: group.members, initiator: group.initiator || myIp } } });
}
async function startGroupCall(g) {
  if (!g?.members?.length) return toast("В группе нет участников", true);
  if (currentCall || groupCall || pendingOffer || callSetupPeer) return toast("Сначала заверши текущий звонок или ответь на приглашение", true);
  try { await startMic(); } catch { requestMic(); return; }
  endGroupCall(true);
  groupCall = { id: g.id, name: g.name, members: [...g.members], initiator: myIp };
  showGroupUI();
  ringbackStart();
  for (const m of groupCall.members) {
    if (myIp && m === myIp) continue;
    try {
      // Invite every member; the initiator is carried in the group ref so
      // joining peers can form the remaining mesh without offer glare.
      await offerTo(m, groupCall);
    } catch (e) { toast(`${m}: ${e}`, true); }
  }
  toast(`Звоним группе «${g.name}»…`);
}
function startCallTimer() {
  callSec = 0;
  const t = $("#vb-time"); if (t) t.textContent = "00:00";
  clearInterval(callTimer);
  callTimer = setInterval(() => {
    callSec++;
    const el = $("#vb-time");
    if (el) el.textContent = `${String(Math.floor(callSec / 60)).padStart(2, "0")}:${String(callSec % 60).padStart(2, "0")}`;
  }, 1000);
}
function showGroupUI() {
  callScreenOpen = true;
  bindSharesToCall(screenCallId(), groupCall?.members || []);
  $("#btn-hangup").classList.remove("hidden");
  startCallTimer(); renderConv(); renderCallPanel(); renderCallStage(); updateVoiceBar(); updateBusy();
}
async function acceptMeshOffer(ip, sdp) {
  const c = ensureGPC(ip);
  await c.setRemoteDescription(JSON.parse(sdp));
  await flushGin(ip);
  const ans = await c.createAnswer();
  await c.setLocalDescription(ans);
  await invoke("sig_send", { peerIp: ip, payload: { kind: "call-answer", from: myNick(), sdp: JSON.stringify(ans) } });
}
async function joinGroupCall(gref, viaIp, viaSdp) {
  try { await startMic(); } catch { requestMic(); return; }
  endGroupCall(true);
  groupCall = { id: gref.id, name: gref.name || "Группа", members: [...(gref.members || [])], initiator: gref.initiator || viaIp };
  if (!groupCall.members.includes(viaIp)) groupCall.members.push(viaIp);
  showGroupUI();
  try { await acceptMeshOffer(viaIp, viaSdp); }
  catch (e) { toast(`Не вступил: ${e}`, true); endGroupCall(); return; }
  for (const m of groupCall.members) {
    if (m === viaIp || m === groupCall.initiator || (myIp && m === myIp)) continue;
    if (!myIp || myIp < m) { try { await offerTo(m, groupCall); } catch (e) { toast(`${m}: ${e}`, true); } }
  }
}
function endGroupCall(silent) {
  const endedGroup = groupCall;
  const hadCall = !!endedGroup || Object.keys(gpc).length > 0;
  ringbackStop();
  if (endedGroup) void closeScreensForCall(`group:${endedGroup.id}`, endedGroup.members);
  for (const ip of Object.keys(gpc)) {
    try { gpc[ip].pc.close(); } catch {}
    dropRemoteVoice(ip);
  }
  gpc = {}; ginBuf = {}; groupCall = null;
  try { wtEndCall(); } catch (e) { console.error("watch end", e); }
  monDrop("me");
  if (!currentCall) {
    clearInterval(callTimer);
    const t = $("#vb-time"); if (t) t.textContent = "00:00";
    $("#btn-hangup").classList.add("hidden");
    $("#call-panel").classList.add("hidden");
  }
  if (hadCall && !silent) toast("Групповой звонок завершён");
  updateVoiceBar(); renderConv(); renderCallPanel(); renderCallStage(); updateBusy();
}
let groupLeaveInFlight = false;
async function hangupGroup() {
  if (groupLeaveInFlight) return;
  groupLeaveInFlight = true;
  const targets = groupCall ? [...new Set(groupCall.members)].filter((m) => m && m !== myIp && gpc[m]) : [];
  endGroupCall();
  try {
    const sends = targets.map((m) => invoke("sig_send", { peerIp: m, payload: { kind: "call-hangup", from: myNick() } }).catch(() => {}));
    await Promise.race([Promise.allSettled(sends), new Promise((r) => setTimeout(r, 1500))]);
  } finally { groupLeaveInFlight = false; }
}
async function leaveCallsBeforeClose() {
  if (pendingOffer) {
    await invoke("sig_send", { peerIp: pendingOffer.ip, payload: { kind: "call-hangup", from: myNick() } }).catch(() => {});
    pendingOffer = null;
    $("#call-modal").classList.add("hidden");
  }
  if (groupCall) await hangupGroup();
  if (currentCall) await hangup(currentCall);
  ringbackStop();
}
let closeInProgress = false;
async function closeAppWindow(quit = false) {
  if (closeInProgress) return;
  closeInProgress = true;
  try {
    await leaveCallsBeforeClose();
    await invoke(quit ? "win_quit" : "win_hide");
    if (!quit) closeInProgress = false;
  } catch (e) {
    closeInProgress = false;
    toast("Не удалось закрыть приложение: " + e, true);
  }
}

// качество демонстрации: низкое — для слабого канала, высокое — для локалки
const SHARE_Q = {
  low: { w: 1280, h: 720, fps: 15, br: 900000, label: "низкое" },
  medium: { w: 1920, h: 1080, fps: 30, br: 2200000, label: "среднее" },
  high: { w: 1920, h: 1080, fps: 60, br: 4500000, label: "высокое" },
};
function shareQ() { return SHARE_Q[settings.share_quality] || SHARE_Q.medium; }
// ---------- screenshare ----------
let myShares = {}, views = {}, sinBuf = {};
const stageTiles = {};
function screenCallId() {
  if (groupCall) return `group:${groupCall.id}`;
  if (currentCall) return `call:${[myIp, currentCall].sort().join(":")}`;
  return "";
}
function bindSharesToCall(callId, members) {
  if (!callId) return;
  for (const share of Object.values(myShares)) {
    if (!share.callId && (share.targets || []).some((ip) => members.includes(ip))) share.callId = callId;
  }
}
async function closeScreensForPeer(ip) {
  if (!ip) return;
  for (const [id, share] of Object.entries(myShares)) {
    const connection = share.pcs?.[ip];
    if (!connection) continue;
    invoke("sig_send", { peerIp: ip, payload: { kind: "screen-stop", from: myNick(), share_id: id } }).catch(() => {});
    try { connection.close(); } catch {}
    delete share.pcs[ip];
    delete share.pcsAt?.[ip];
    share.targets = (share.targets || []).filter((target) => target !== ip);
    if (!Object.keys(share.pcs).length) await stopShare(id, false);
  }
  for (const [id, view] of Object.entries(views)) {
    if (view.ip === ip) {
      invoke("sig_send", { peerIp: ip, payload: { kind: "screen-stop", from: myNick(), share_id: id, request: true } }).catch(() => {});
      closeView(id, true);
    }
  }
}
async function closeScreensForCall(callId, peersToClose = []) {
  if (callId) {
    for (const [id, share] of Object.entries(myShares)) if (share.callId === callId) await stopShare(id);
    for (const [id, view] of Object.entries(views)) if (view.callId === callId) closeView(id, true);
  }
  for (const ip of peersToClose) await closeScreensForPeer(ip);
}
function shareTargets() {
  const on = (ip) => peers.find((p) => p.ip === ip)?.online;
  if (groupCall) return groupCall.members.filter(on);
  if (currentCall) return [currentCall];
  if (selGroup) return (curGroup()?.members || []).filter(on);
  if (selectedIp) return [selectedIp];
  return [];
}
async function startScreen() {
  const screens = Object.entries(myShares).filter(([, s]) => s.kind !== "camera");
  if (screens.length) {
    if (!await askConfirm("Остановить все свои демонстрации?", "", "Остановить")) return;
    for (const [id] of screens) await stopShare(id);
    updateScreenBtn(); return;
  }
  const q = shareQ();
  const videoC = { width: { max: q.w }, height: { max: q.h }, frameRate: { max: q.fps } };
  // Эхо-фикс: audio: true захватывает весь системный loopback ПК, включая голоса
  // участников звонка, играющие на динамиках демонстранта. restrictOwnAudio
  // (Chromium 111+, под капотом WASAPI process loopback с исключением своего
  // дерева процессов) отрезает звук самого SafeDrop, игра/видео остаются.
  let stream, withAudio = false, audioMode = "off";
  const picker = (audio) => navigator.mediaDevices.getDisplayMedia({ video: videoC, audio });
  try {
    if (navigator.mediaDevices.getSupportedConstraints?.().restrictOwnAudio) {
      stream = await picker({ restrictOwnAudio: true });
      withAudio = stream.getAudioTracks().length > 0;
      audioMode = withAudio ? "restrict-own" : "no-audio-checked";
      const at = stream.getAudioTracks()[0];
      console.info("[share] аудиотрек демонстрации:", audioMode, at ? at.getSettings() : "нет");
    } else {
      const noAudio = await askConfirm(
        "Звук демонстрации с эхом",
        "Версия WebView2 здесь не умеет вырезать голоса звонка из системного звука демонстрации. Показать без звука? (Отмена → показать со звуком, но собеседников будет слышно в демонстрации — лучше наушники.)",
        "Без звука",
      );
      if (noAudio) {
        stream = await picker(false);
        toast("Без системного звука — в следующий раз включи «Поделиться звуком» в окне выбора");
      } else {
        stream = await picker(true);
        withAudio = stream.getAudioTracks().length > 0;
        audioMode = "loopback-full";
      }
      console.info("[share] аудиотрек демонстрации:", audioMode);
    }
  } catch (e) {
    if (e?.name === "NotAllowedError") return toast("Демонстрация отменена", true);
    console.warn("[share] getDisplayMedia с ограничением аудио не сработал:", e);
    try {
      stream = await picker(false);
      audioMode = "fallback-no-audio";
      withAudio = false;
      toast("Без системного звука — в следующий раз включи «Поделиться звуком» в окне выбора");
    } catch { return toast("Демонстрация отменена", true); }
  }
  await startShareStream(stream, `Экран • ${myNick()}`, withAudio, "screen", audioMode);
}
async function startCamera() {
  const activeCam = Object.entries(myShares).find(([, s]) => s.kind === "camera");
  if (activeCam) { await stopShare(activeCam[0]); updateCameraBtn(); return; }
  const targets = shareTargets();
  if (!targets.length) return toast("Некому показать: выбери контакт/группу или позвони", true);
  let stream;
  try {
    stream = await cameraStream();
  } catch (e) { return toast("Камера недоступна: " + (e?.message || e), true); }
  await startShareStream(stream, `Камера • ${myNick()}`, false, "camera");
  updateCameraBtn();
}
function updateCameraBtn() {
  const b = $("#vb-cam"); if (!b) return;
  const on = Object.values(myShares).some((s) => s.kind === "camera");
  b.disabled = false;
  b.classList.toggle("off", on);
  b.title = on ? "Выключить камеру" : "Включить камеру";
  syncCallScreenControls();
}
async function startShareStream(stream, label, withAudio, kind = "screen", audioMode = "") {
  const targets = shareTargets();
  if (!targets.length) { stream.getTracks().forEach((track) => track.stop()); return toast("Некому показать: выбери контакт/группу или позвони", true); }
  let callId = screenCallId();
  if (!callId && screenCallId() && targets.some((ip) => shareTargets().includes(ip))) callId = screenCallId();
  if (callId && callId !== screenCallId()) {
    stream.getTracks().forEach((track) => track.stop());
    return toast("Звонок завершился, демонстрация не запущена", true);
  }
  const shareId = "sh-" + Date.now().toString(36);
  myShares[shareId] = { stream, pcs: {}, pcsAt: {}, label, targets, callId, kind, audioMode };
  stream.getVideoTracks()[0]?.addEventListener("ended", () => stopShare(shareId));
  for (const ip of targets) {
    try {
      const c = new RTCPeerConnection({ iceServers: [{ urls: "stun:stun.l.google.com:19302" }] });
      myShares[shareId].pcs[ip] = c;
      myShares[shareId].pcsAt[ip] = Date.now();
      const senders = [];
      stream.getTracks().forEach((t) => { try { senders.push(c.addTrack(t, stream)); } catch {} });
      // режем битрейт под качество (на ретрансляции Radmin высокое может висеть)
      const qb = shareQ().br;
      for (const s of senders) {
        try {
          if (s.track?.kind === "video") {
            const p = s.getParameters();
            p.encodings = [{ maxBitrate: qb }];
            if ("degradationPreference" in p) p.degradationPreference = qb >= 2000000 ? "maintain-resolution" : "maintain-framerate";
            await s.setParameters(p);
          }
        } catch {}
      }
      c.onicecandidate = (e) => {
        if (e.candidate) invoke("sig_send", { peerIp: ip, payload: { kind: "screen-ice", from: myNick(), share_id: shareId, candidate: JSON.stringify(e.candidate) } }).catch(() => {});
      };
      const offer = await c.createOffer();
      await c.setLocalDescription(offer);
      await invoke("sig_send", { peerIp: ip, payload: { kind: "screen-offer", from: myNick(), sdp: JSON.stringify(offer), share_id: shareId, label, with_audio: withAudio, call_id: callId } });
    } catch (e) { toast(`${ip}: ${e}`, true); }
  }
  updateScreenBtn(); updateCameraBtn(); renderStages(); renderCallScreen(); updateBusy();
  toast(`<svg class='ic' viewBox='0 0 24 24'><path d='M14,10V22H4a2,2,0,0,1-2-2V10Z'/><path d='M22,10V20a2,2,0,0,1-2,2H16V10Z'/><path d='M22,4V8H2V4A2,2,0,0,1,4,2H20A2,2,0,0,1,22,4Z'/></svg> ${kind === "camera" ? "Камера включена" : "Показываешь экран"} (${targets.length})${withAudio ? " со звуком" : ""}`);
}
function updateScreenBtn() {
  const b = $("#btn-screen"); if (!b) return;
  const onScreen = Object.values(myShares).some((s) => s.kind !== "camera");
  b.innerHTML = onScreen ? '<svg class="ic" viewBox="0 0 24 24" style="vertical-align:-3px;margin-right:4px"><path d="M6,6H18V18H6Z"/></svg> Стоп' : '<svg class="ic" viewBox="0 0 24 24" style="vertical-align:-3px;margin-right:4px"><path d="M14,10V22H4a2,2,0,0,1-2-2V10Z"/><path d="M22,10V20a2,2,0,0,1-2,2H16V10Z"/><path d="M22,4V8H2V4A2,2,0,0,1,4,2H20A2,2,0,0,1,22,4Z"/></svg> Экран';
  syncCallScreenControls();
}
async function stopShare(shareId, notifyPeers = true) {
  const s = myShares[shareId]; if (!s) return;
  // сначала убираем из списков, чтобы плитка исчезла при любом исходе
  delete myShares[shareId];
  delete stageTiles[shareId];
  try {
    if (notifyPeers) for (const ip of Object.keys(s.pcs)) invoke("sig_send", { peerIp: ip, payload: { kind: "screen-stop", from: myNick(), share_id: shareId } }).catch(() => {});
    for (const ip of Object.keys(s.pcs)) { try { s.pcs[ip].close(); } catch {} }
    try { s.stream.getTracks().forEach((t) => t.stop()); } catch {}
  } finally {
    updateScreenBtn(); updateCameraBtn();
    renderStages(); renderShareList(); renderCallStage(); updateBusy();
  }
  toast("Показ остановлен <svg class='ic' viewBox='0 0 24 24'><path d='M9,16.17,4.83,12l-1.42,1.41L9,19,21,7l-1.41-1.41Z'/></svg>");
  renderStages(); renderShareList(); renderCallStage();
  const n = Object.keys(myShares).length + Object.keys(views).length;
  $("#screens-badge").textContent = n ? `• ${n}` : "";
}
$("#btn-close-all-shares").onclick = async () => {
  for (const id of Object.keys(myShares)) await stopShare(id);
  for (const id of Object.keys(views)) closeView(id);
  toast("Все показы закрыты");
};
$("#btn-share-dbg").onclick = () => {
  const lines = [];
  lines.push(`myShares: ${Object.keys(myShares).length}, views: ${Object.keys(views).length}, tiles in map: ${Object.keys(stageTiles).length}, tiles in DOM: ${document.querySelectorAll("#stages > .stage").length}`);
  for (const [sid, s] of Object.entries(myShares)) {
    const pcs = Object.entries(s.pcs).map(([ip, c]) => `${ip}:${c.connectionState}`).join(", ") || "нет зрителей";
    const vt = s.stream ? s.stream.getVideoTracks().map((t) => t.readyState).join(",") : "no stream";
    const at = s.stream ? s.stream.getAudioTracks().map((t) => t.readyState).join(",") : "нет";
    lines.push(`Мой ${sid}${s.audioMode ? ` [аудио: ${s.audioMode}]` : ""}: ${pcs} | видео: ${vt} | звук: ${at}`);
  }
  for (const [sid, v] of Object.entries(views)) {
    const tracks = v.stream ? [...v.stream.getTracks()].map((t) => `${t.kind}:${t.readyState}`).join(",") : "no stream";
    const tile = stageTiles[sid];
    const rs = tile?.querySelector("video")?.readyState;
    lines.push(`Просмотр ${sid} (${v.label}): pc=${v.pc?.connectionState}, треки=[${tracks}], video=${rs}`);
  }
  showHelp("Диагностика экранов", `<div class="muted left">${lines.map((l) => esc(l)).join("<br>")}<br><br>Если плитки висят — пришли скрин этого окна.</div>`);
};
async function acceptScreenView({ ip, sdp, share_id, label, call_id }) {
  if (views[share_id]) return;
  const c = new RTCPeerConnection({ iceServers: [{ urls: "stun:stun.l.google.com:19302" }] });
  const stream = new MediaStream();
  views[share_id] = { pc: c, ip, label: label || `Экран • ${ip}`, callId: call_id || "", stream, volume: 100, muted: false, inbuf: [], createdAt: Date.now(), firstTrackAt: 0 };
  c.ontrack = (e) => {
    try { stream.addTrack(e.track); } catch {}
    if (e.track.kind === "audio") {
      attachShareAudio(share_id, e.track);
    }
    try {
      // трек замолчал (показ убит на той стороне, а сессия жива) — через 6с закрываем
      e.track.onmute = () => {
        setTimeout(() => {
          const vv = views[share_id];
          if (!vv) return;
          const stillMuted = [...vv.stream.getTracks()].every((t) => t.muted || t.readyState === "ended");
          if (stillMuted) { closeView(share_id); toast("<svg class='ic' viewBox='0 0 24 24'><path d='M14,10V22H4a2,2,0,0,1-2-2V10Z'/><path d='M22,10V20a2,2,0,0,1-2,2H16V10Z'/><path d='M22,4V8H2V4A2,2,0,0,1,4,2H20A2,2,0,0,1,22,4Z'/></svg> Поток заглох — показ закрыт", true); }
        }, 6000);
      };
    } catch {}
    const vv = views[share_id]; if (vv && !vv.firstTrackAt) vv.firstTrackAt = Date.now(); renderStages();
  };
  c.onicecandidate = (e) => {
    if (e.candidate) invoke("sig_send", { peerIp: ip, payload: { kind: "screen-ice", from: myNick(), share_id, candidate: JSON.stringify(e.candidate) } }).catch(() => {});
  };
  await c.setRemoteDescription(JSON.parse(sdp));
  const buf = views[share_id].inbuf; views[share_id].inbuf = [];
  for (const cd of buf) { try { await c.addIceCandidate(JSON.parse(cd)); } catch {} }
  const ans = await c.createAnswer();
  await c.setLocalDescription(ans);
  await invoke("sig_send", { peerIp: ip, payload: { kind: "screen-answer", from: myNick(), share_id, sdp: JSON.stringify(ans) } });
  renderStages();
  renderCallStage(); updateBusy();
  toast(`<svg class='ic' viewBox='0 0 24 24'><path d='M14,10V22H4a2,2,0,0,1-2-2V10Z'/><path d='M22,10V20a2,2,0,0,1-2,2H16V10Z'/><path d='M22,4V8H2V4A2,2,0,0,1,4,2H20A2,2,0,0,1,22,4Z'/></svg> ${views[share_id].label} — нажми <svg class='ic' viewBox='0 0 24 24'><path d='M8,5v14l11-7z'/></svg> на плитке, чтобы смотреть со звуком`);
}
function closeView(shareId, silent) {
  try { if (watchId === shareId) closeWatchModal(); } catch {}
  const v = views[shareId];
  delete views[shareId]; delete stageTiles[shareId];
  dropShareAudio(shareId);
  try {
    if (v) { try { v.pc.close(); } catch {} }
  } finally {
    renderStages(); renderShareList(); renderCallStage(); updateBusy();
  }
  if (!silent) toast("Просмотр закрыт");
}
// ---------- просмотр как в Discord: клик ▶ → большой оверлей со звуком ----------
let watchId = null;
let detachedWatchId = null;
const shareAudioGains = {};
function attachShareAudio(id, track) {
  const ctx = ampEnsure();
  if (!ctx || !track || track.readyState === "ended") return;
  dropShareAudio(id);
  try {
    // Build the gain directly from the received audio track. This avoids the
    // media-element routing that caused the stream slider to act like mute.
    const source = ctx.createMediaStreamSource(new MediaStream([track]));
    const gain = ctx.createGain();
    source.connect(gain); gain.connect(ctx.destination);
    shareAudioGains[id] = { source, gain };
    applyViewVolume(id);
  } catch (e) { console.warn("Не удалось подключить регулировку звука экрана", e); }
}
function dropShareAudio(id) {
  const rec = shareAudioGains[id];
  if (rec) {
    try { rec.source.disconnect(); } catch {}
    try { rec.gain.disconnect(); } catch {}
    delete shareAudioGains[id];
  }
}
function openWatch(id, fullscreen) {
  const V = views[id]; if (!V) return;
  const previousWatchId = watchId;
  const video = $("#watch-video");
  if (document.pictureInPictureElement === video) {
    detachedWatchId = null;
    document.exitPictureInPicture().catch(() => {});
  }
  watchId = id;
  $("#mini-watch").classList.add("hidden");
  $("#mini-watch-video").srcObject = null;
  if (previousWatchId && previousWatchId !== id) applyViewVolume(previousWatchId);
  $("#watch-label").textContent = V.label;
  const mv = video;
  mv.muted = true;
  mv.srcObject = V.stream;
  applyViewVolume(id);
  const modal = $("#watch-modal");
  modal.classList.remove("hidden");
  if (fullscreen !== undefined) {
    modal.classList.toggle("watch-fullscreen", !!fullscreen);
    $("#watch-fs").title = fullscreen ? "Выйти из полноэкранного" : "На весь экран";
  }
  mv.play().catch((e) => toast("Не удалось воспроизвести: " + e, true));
  const ctx = ampEnsure();
  try { if (settings.speaker_id && typeof ctx?.setSinkId === "function") ctx.setSinkId(settings.speaker_id).catch(() => {}); } catch {}
  if (ctx?.state === "suspended") ctx.resume().catch(() => {});
}
function closeWatchModal() {
  const previousWatchId = watchId;
  $("#watch-modal").classList.add("hidden");
  $("#watch-modal").classList.remove("watch-fullscreen");
  $("#watch-fs").title = "На весь экран";
  $("#mini-watch").classList.add("hidden");
  const mini = $("#mini-watch-video");
  try { mini.pause(); } catch {}
  mini.srcObject = null;
  const mv = $("#watch-video");
  if (document.pictureInPictureElement === mv) document.exitPictureInPicture().catch(() => {});
  try { mv.pause(); } catch {}
  mv.removeAttribute("src"); mv.srcObject = null;
  watchId = null;
  if (previousWatchId) applyViewVolume(previousWatchId);
  renderCallStage();
}
function minimizeWatch() {
  const id = watchId, V = views[id];
  if (!id || !V) return;
  const mv = $("#watch-video"), mini = $("#mini-watch-video");
  try { mv.pause(); } catch {}
  mini.srcObject = V.stream;
  mini.play().catch(() => {});
  $("#mini-watch-label").textContent = V.label;
  $("#mini-watch").classList.remove("hidden");
  $("#watch-modal").classList.add("hidden");
  $("#watch-modal").classList.remove("watch-fullscreen");
  applyViewVolume(id);
}
$("#mini-watch-restore").onclick = () => { if (watchId) openWatch(watchId); };
$("#mini-watch-close").onclick = closeWatchModal;
$("#mini-watch-video").onclick = () => { if (watchId) openWatch(watchId); };
async function detachWatchWindow() {
  const id = watchId;
  const video = $("#watch-video");
  if (!id || !views[id]) return;
  if (!document.pictureInPictureEnabled || typeof video.requestPictureInPicture !== "function") {
    toast("Отдельное окно просмотра не поддерживается этой версией WebView", true);
    return;
  }
  try {
    await video.requestPictureInPicture();
    detachedWatchId = id;
    $("#watch-modal").classList.add("hidden");
    toast("Просмотр открыт отдельно — окно можно перетаскивать между экранами");
  } catch (e) { toast("Не удалось открыть отдельное окно: " + (e?.message || e), true); }
}
$("#watch-video").addEventListener("leavepictureinpicture", () => {
  const id = detachedWatchId;
  detachedWatchId = null;
  if (!id || watchId !== id) return;
  const video = $("#watch-video");
  try { video.pause(); } catch {}
  video.removeAttribute("src"); video.srcObject = null;
  watchId = null;
  applyViewVolume(id);
});
function applyViewVolume(id) {
  const V = views[id]; if (!V) return;
  // Only the currently opened screen viewer should produce stream audio.
  // Keep stream gain independent from every participant's voice gain.
  const pct = (watchId !== id || V.muted || deafened) ? 0 : V.volume;
  const audioGain = shareAudioGains[id]?.gain;
  if (audioGain) {
    try {
      audioGain.gain.setTargetAtTime(pct / 100, ampCtx.currentTime, 0.025);
      if (ampCtx.state === "suspended") ampCtx.resume().catch(() => {});
    } catch {}
  }
  if (watchId === id) {
    const mv = $("#watch-video");
    mv.muted = true;
    const wv = $("#watch-vol");
    if (document.activeElement !== wv) wv.value = V.muted ? 0 : V.volume;
    $("#watch-mute").innerHTML = V.muted ? '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><line x1="23" y1="9" x2="17" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><line x1="17" y1="9" x2="23" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>' : '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><path d="M16.5,12a4.5,4.5,0,0,0-2.5-4v8A4.5,4.5,0,0,0,16.5,12Z"/><path d="M14,3.23v2.06a7,7,0,0,1,0,13.42v2.06a9,9,0,0,0,0-17.54Z"/></svg>';
  }
  const row = document.querySelector(`#cp-shares [data-view="${id}"]`);
  if (row) {
    const r = row.querySelector("input");
    if (document.activeElement !== r) r.value = V.muted ? 0 : V.volume;
    const b = row.querySelector("button");
    if (b) b.innerHTML = V.muted ? '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><line x1="23" y1="9" x2="17" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><line x1="17" y1="9" x2="23" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>' : '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><path d="M16.5,12a4.5,4.5,0,0,0-2.5-4v8A4.5,4.5,0,0,0,16.5,12Z"/><path d="M14,3.23v2.06a7,7,0,0,1,0,13.42v2.06a9,9,0,0,0,0-17.54Z"/></svg>';
  }
}
$("#watch-vol").addEventListener("input", (e) => {
  const V = views[watchId]; if (!V) return;
  V.volume = Number(e.target.value); V.muted = V.volume === 0;
  applyViewVolume(watchId);
});
$("#watch-mute").onclick = () => {
  const V = views[watchId]; if (!V) return;
  V.muted = !V.muted;
  applyViewVolume(watchId);
};
$("#watch-detach").onclick = detachWatchWindow;
$("#watch-min").onclick = minimizeWatch;
$("#watch-min").title = "Свернуть в мини-плеер";
$("#watch-app-min").onclick = () => invoke("win_min").catch((e) => toast("" + e, true));
// Close the large viewer and return to the tile. The tile's close button ends
// the stream; keeping those actions separate makes the stream reopenable.
$("#watch-x").onclick = closeWatchModal;
$("#watch-x").title = "Вернуться к экрану";
$("#watch-fs").onclick = () => {
  const modal = $("#watch-modal");
  const active = modal.classList.toggle("watch-fullscreen");
  $("#watch-fs").title = active ? "Оконный режим" : "На весь экран";
};
function renderStages() {
  const box = $("#stages"); if (!box) return;
  const ids = [...Object.keys(myShares), ...Object.keys(views)];
  for (const id of Object.keys(stageTiles)) {
    if (!ids.includes(id)) { stageTiles[id].remove(); delete stageTiles[id]; }
  }
  for (const id of ids) {
    let tile = stageTiles[id];
    if (!tile) {
      tile = document.createElement("div");
      tile.className = "stage";
      tile.innerHTML = `<video class="tile-v" playsinline muted></video><div class="srow"><b></b><input type="range" min="0" max="200" value="100" /><button class="mute"><svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><path d="M16.5,12a4.5,4.5,0,0,0-2.5-4v8A4.5,4.5,0,0,0,16.5,12Z"/><path d="M14,3.23v2.06a7,7,0,0,1,0,13.42v2.06a9,9,0,0,0,0-17.54Z"/></svg></button><button class="fs"><svg class="ic" viewBox="0 0 24 24"><path d="M7,14H5v5h5V17H7Z"/><path d="M5,10H7V7h3V5H5Z"/><path d="M17,17h-3v2h5V14H17Z"/><path d="M14,5v2h3v3h2V5Z"/></svg></button><button class="x"><svg class="ic" viewBox="0 0 24 24"><path d="M13.41,12l4.29-4.29a1,1,0,0,0-1.41-1.41L12,10.59,7.71,6.29A1,1,0,0,0,6.29,7.71L10.59,12,6.29,16.29a1,1,0,0,0,1.41,1.41L12,13.41l4.29,4.29a1,1,0,0,0,1.41-1.41Z"/></svg></button></div>`;
      stageTiles[id] = tile; box.appendChild(tile);
    }
    const mine = !!myShares[id];
    const V = mine ? myShares[id] : views[id];
    tile.querySelector("b").innerHTML = (mine ? "<svg class='ic' viewBox='0 0 24 24' style='vertical-align:-3px;margin-right:4px'><path d='M14,10V22H4a2,2,0,0,1-2-2V10Z'/><path d='M22,10V20a2,2,0,0,1-2,2H16V10Z'/><path d='M22,4V8H2V4A2,2,0,0,1,4,2H20A2,2,0,0,1,22,4Z'/></svg> мой • " : "") + esc(V.label) + (mine ? ` • ${Object.keys(V.pcs).length} зрит.` : "");
    const vid = tile.querySelector("video");
    // превью всегда без звука (автплей разрешён) — звук включается в оверлее по клику
    vid.muted = true;
    if (vid.srcObject !== V.stream) vid.srcObject = V.stream;
    vid.play().catch(() => {});
    vid.onclick = () => { if (!mine) openWatch(id); };
    vid.style.cursor = mine ? "default" : "pointer";
    const vol = tile.querySelector('input[type="range"]');
    if (document.activeElement !== vol) vol.value = mine ? 100 : (V.muted ? 0 : V.volume);
    vol.disabled = !!mine;
    vol.oninput = () => {
      if (mine) return;
      V.volume = Number(vol.value); V.muted = V.volume === 0;
      tile.querySelector(".mute").innerHTML = V.muted ? '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><line x1="23" y1="9" x2="17" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><line x1="17" y1="9" x2="23" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>' : '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><path d="M16.5,12a4.5,4.5,0,0,0-2.5-4v8A4.5,4.5,0,0,0,16.5,12Z"/><path d="M14,3.23v2.06a7,7,0,0,1,0,13.42v2.06a9,9,0,0,0,0-17.54Z"/></svg>';
      applyViewVolume(id);
    };
    tile.querySelector(".mute").innerHTML = mine ? "—" : (V.muted ? '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><line x1="23" y1="9" x2="17" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><line x1="17" y1="9" x2="23" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>' : '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><path d="M16.5,12a4.5,4.5,0,0,0-2.5-4v8A4.5,4.5,0,0,0,16.5,12Z"/><path d="M14,3.23v2.06a7,7,0,0,1,0,13.42v2.06a9,9,0,0,0,0-17.54Z"/></svg>');
    tile.querySelector(".mute").onclick = () => {
      if (mine) return;
      V.muted = !V.muted;
      tile.querySelector(".mute").innerHTML = V.muted ? '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><line x1="23" y1="9" x2="17" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><line x1="17" y1="9" x2="23" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>' : '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><path d="M16.5,12a4.5,4.5,0,0,0-2.5-4v8A4.5,4.5,0,0,0,16.5,12Z"/><path d="M14,3.23v2.06a7,7,0,0,1,0,13.42v2.06a9,9,0,0,0,0-17.54Z"/></svg>';
      applyViewVolume(id);
    };
    tile.querySelector(".fs").onclick = () => { if (!mine) openWatch(id); };
    tile.querySelector(".fs").style.display = mine ? "none" : "";
    tile.querySelector(".x").onclick = () => { mine ? stopShare(id) : closeView(id); };
    tile.querySelector(".x").title = mine ? "Остановить показ" : "Закрыть просмотр";
  }
  const n = ids.length;
  $("#screens-badge").textContent = n ? `• ${n}` : "";
  updateScreenBtn();
  renderShareList();
  renderCallScreenStage();
}
function renderShareList() {
  const box = $("#cp-shares"); if (!box) return; box.innerHTML = "";
  for (const [id, s] of Object.entries(myShares)) {
    const d = document.createElement("div");
    d.className = "cp-share";
    d.innerHTML = `<b><svg class="ic" viewBox="0 0 24 24" style="vertical-align:-3px;margin-right:4px"><path d="M14,10V22H4a2,2,0,0,1-2-2V10Z"/><path d="M22,10V20a2,2,0,0,1-2,2H16V10Z"/><path d="M22,4V8H2V4A2,2,0,0,1,4,2H20A2,2,0,0,1,22,4Z"/></svg> мой экран • ${Object.keys(s.pcs).length} зрит.</b>`;
    const b = document.createElement("button"); b.innerHTML = '<svg class="ic" viewBox="0 0 24 24"><path d="M6,6H18V18H6Z"/></svg>'; b.title = "Остановить";
    b.onclick = () => stopShare(id);
    d.appendChild(b); box.appendChild(d);
  }
  for (const [id, v] of Object.entries(views)) {
    const d = document.createElement("div");
    d.className = "cp-share";
    d.dataset.view = id;
    d.innerHTML = `<b>${esc(v.label)}</b>`;
    const r = document.createElement("input");
    r.type = "range"; r.min = 0; r.max = 200; r.value = v.muted ? 0 : v.volume;
    r.oninput = () => {
      v.volume = Number(r.value); v.muted = v.volume === 0;
      applyViewVolume(id);
    };
    const b = document.createElement("button");
    b.innerHTML = v.muted ? '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><line x1="23" y1="9" x2="17" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><line x1="17" y1="9" x2="23" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>' : '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><path d="M16.5,12a4.5,4.5,0,0,0-2.5-4v8A4.5,4.5,0,0,0,16.5,12Z"/><path d="M14,3.23v2.06a7,7,0,0,1,0,13.42v2.06a9,9,0,0,0,0-17.54Z"/></svg>';
    b.onclick = () => {
      v.muted = !v.muted;
      applyViewVolume(id);
    };
    d.appendChild(r); d.appendChild(b); box.appendChild(d);
  }
  if (!box.children.length) box.innerHTML = `<div class="muted">Нет демонстраций.</div>`;
}

// ---------- wire UI ----------
$("#peer-search").addEventListener("input", renderContacts);
$("#btn-details").onclick = () => {
  const open = document.body.classList.toggle("details-open");
  if (open) previewCamera(); else stopCameraPreview();
};
try {
  navigator.mediaDevices?.addEventListener("devicechange", () => {
    refreshCameraList();
    if (document.body.classList.contains("details-open")) previewCamera();
  });
} catch {}
async function quietCtxRefresh() {
  if (settings?.ctx_menu) { try { await invoke("refresh_ctx_menu"); } catch {} }
}
async function doScan() {
  toast("Сканирую сеть Radmin…");
    try { peers = await invoke("scan_network"); renderAll(); quietCtxRefresh(); }
  catch (e) { toast("Скан: " + e, true); }
}
$("#btn-scan").onclick = doScan;
async function addPeerDlg() {
  const ip = await askPrompt("IP контакта (Radmin)", "", "например 26.1.2.3"); if (!ip?.trim()) return;
  const name = await askPrompt("Имя контакта", "", "можно пустым") || "";
  try { await invoke("add_peer", { ip: ip.trim(), name }); peers = await invoke("get_peers"); renderAll(); quietCtxRefresh(); selectPeer(ip.trim()); toast("Контакт добавлен, запрашиваю его профиль…"); }
  catch (e) { toast("" + e, true); }
}
$("#btn-add").onclick = addPeerDlg;
$("#btn-call").onclick = () => { if (selGroup) startGroupCall(curGroup()); else startCall(selectedIp); };
$("#rc-call").onclick = () => { if (selGroup) startGroupCall(curGroup()); else startCall(selectedIp); };
$("#btn-screen").onclick = startScreen;
$("#btn-hangup").onclick = () => hangup();
$("#btn-hangup").onclick = () => { if (groupCall) hangupGroup(); else hangup(); };
$("#me-mic").onclick = toggleMic;
async function pickAndSend() {
  if (!selectedIp && !selGroup) return toast("Сначала выбери контакт или группу", true);
  try {
    const paths = await invoke("pick_files");
    await sendPaths(selectedIp, paths);
  } catch (e) { toast("" + e, true); }
}
$("#btn-sendfile").onclick = pickAndSend;
$("#rc-send").onclick = pickAndSend;
$("#btn-pick-files").onclick = pickAndSend;
$("#btn-attach").onclick = pickAndSend;
$("#btn-image").onclick = pickAndSendImage;
$("#chat-input").addEventListener("paste", async (e) => {
  const files = [...(e.clipboardData?.files || [])].filter((x) => x.type.startsWith("image/"));
  if (!files.length) return;
  e.preventDefault();
  for (const f of files) {
    const raw = await new Promise((res, rej) => { const r = new FileReader(); r.onload = () => res(r.result); r.onerror = rej; r.readAsDataURL(f); });
    await sendImageDataUrl(raw);
  }
});
$("#btn-send").onclick = () => { if (editingId) saveEdit(); else sendChat(); };
$("#chat-input").addEventListener("keydown", (e) => {
  if (e.key === "Enter") { if (editingId) saveEdit(); else sendChat(); }
  else if (e.key === "Escape" && (replyToId || editingId)) setReply(null);
});
// правка своего последнего сообщения по стрелке вверх в пустом поле
$("#chat-input").addEventListener("keydown", (e) => {
  if (editingId || replyToId || e.key !== "ArrowUp") return;
  const inp = e.target;
  if (inp.selectionStart !== null && inp.selectionStart !== 0) return;
  const rows = chats[curKey()] || [];
  const last = [...rows].reverse().find((m) => m.from_me && m.text && m.text.trim());
  if (!last) return;
  e.preventDefault();
  setEdit({ id: last.id, text: last.text });
});
// reply-bar
$("#reply-cancel").onclick = () => setReply(null);

// ---------- «печатает…» ----------
const typingPeers = new Map(); // key -> Map(nickname -> expiry)
const typingTimers = new Map(); // key -> render timer id
let typingLastSent = 0;

function stopTypingLocal() {
  // после отправки своего сообщения собеседник всё равно получит его;
  // просто сбрасываем счётчик throttle
  typingLastSent = 0;
}

function setTypingState(key, nick, active) {
  if (!typingPeers.has(key)) typingPeers.set(key, new Map());
  const m = typingPeers.get(key);
  if (active) m.set(nick, Date.now() + 4000);
  else m.delete(nick);
  renderTypingBadge(key);
}

function typingLabel(key) {
  const m = typingPeers.get(key);
  if (!m) return "";
  const now = Date.now();
  for (const [k, exp] of m) { if (exp < now) m.delete(k); }
  const names = [...m.keys()];
  if (!names.length) return "";
  if (names.length === 1) return `${names[0]} печатает…`;
  if (names.length <= 3) return `${names.join(", ")} печатают…`;
  return `${names.length} печатают…`;
}

function renderTypingBadge(key) {
  const el = $("#conv-typing");
  if (!el) return;
  if (key !== curKey()) return;
  const label = typingLabel(key);
  el.textContent = label;
  el.classList.toggle("hidden", !label);
  const m = typingPeers.get(key);
  if (m && m.size) {
    if (!typingTimers.has(key)) {
      // перерисовываем через 1.5с — индикаторы протухают/меняются
      typingTimers.set(key, setTimeout(() => { typingTimers.delete(key); renderTypingBadge(key); }, 1500));
    }
  } else {
    const t = typingTimers.get(key);
    if (t) { clearTimeout(t); typingTimers.delete(key); }
  }
}

$("#chat-input").addEventListener("input", () => {
  // эмитим «печатает» не чаще раза в 2.5с, только когда есть контакт/группа
  if (!selectedIp && !selGroup) return;
  const now = Date.now();
  if (now - typingLastSent < 2500) return;
  const inp = $("#chat-input");
  if (!inp.value.trim()) return;
  typingLastSent = now;
  const key = curKey();
  invoke("chat_typing", { peerIp: key }).catch(() => {});
});
async function renameSel() {
  const g = curGroup();
  if (g) { openGroupModal(g); return; }
  const p = sel(); if (!p) return;
  const n = await askPrompt("Локальное имя", p.name); if (n == null) return;
  try { await invoke("rename_peer", { ip: p.ip, name: n }); peers = await invoke("get_peers"); renderAll(); toast("Имя сохранено"); }
  catch (e) { toast("" + e, true); }
}
$("#rc-rename").onclick = renameSel;
$("#rc-avatar-btn").onclick = pickPeerAvatar;
async function toggleTrustSel() {
  if (!selectedIp) return;
  try { await invoke("toggle_trust", { ip: selectedIp }); peers = await invoke("get_peers"); renderAll(); }
  catch (e) { toast("" + e, true); }
}
$("#rc-trust").onclick = toggleTrustSel;
let confirmRes = null;
function askConfirm(title, body, okText = "Да") {
  $("#confirm-title").textContent = title;
  $("#confirm-body").textContent = body || "";
  $("#confirm-ok").textContent = okText;
  $("#confirm-modal").classList.remove("hidden");
  return new Promise((res) => { confirmRes = res; });
}
$("#confirm-ok").onclick = () => { $("#confirm-modal").classList.add("hidden"); confirmRes?.(true); confirmRes = null; };
$("#confirm-no").onclick = () => { $("#confirm-modal").classList.add("hidden"); confirmRes?.(false); confirmRes = null; };
let promptRes = null;
function askPrompt(title, initial = "", placeholder = "") {
  $("#prompt-title").textContent = title;
  const inp = $("#prompt-input");
  inp.value = initial; inp.placeholder = placeholder;
  $("#prompt-modal").classList.remove("hidden");
  setTimeout(() => inp.focus(), 50);
  return new Promise((res) => { promptRes = res; });
}
function promptDone(val) {
  $("#prompt-modal").classList.add("hidden");
  promptRes?.(val); promptRes = null;
}
$("#prompt-ok").onclick = () => promptDone($("#prompt-input").value);
$("#prompt-no").onclick = () => promptDone(null);
$("#prompt-input").addEventListener("keydown", (e) => {
  if (e.key === "Enter") promptDone($("#prompt-input").value);
  if (e.key === "Escape") promptDone(null);
});
async function delSel() {
  const g = curGroup();
  if (g) {
    if (!await askConfirm(`Удалить группу «${g.name}»?`, "Переписка группы тоже удалится.", "Удалить")) return;
    try {
      await invoke("delete_group", { id: g.id });
      if (selGroup === g.id) selGroup = null;
      groups = await invoke("get_groups"); normalizeGroups(); renderAll(); renderChat(); toast("Группа удалена");
    } catch (e) { toast("" + e, true); }
    return;
  }
  if (!selectedIp || !confirm("Удалить контакт?")) return;
  try { await invoke("remove_peer", { ip: selectedIp }); peers = await invoke("get_peers"); selectedIp = null; renderAll(); quietCtxRefresh(); }
  catch (e) { toast("" + e, true); }
}
$("#rc-del").onclick = delSel;
$("#btn-avatar-pick").onclick = pickMyAvatar;
$("#btn-profile-save").onclick = async () => {
  try {
    profile = await invoke("save_profile", { nickname: $("#set-nick").value.trim(), avatar: profile.avatar || "" });
    $("#me-nick").textContent = profile.nickname || settings.device_name;
    renderMyAvatar(); renderAll(); toast("Профиль сохранён и разослан всем <svg class='ic' viewBox='0 0 24 24'><path d='M9,16.17,4.83,12l-1.42,1.41L9,19,21,7l-1.41-1.41Z'/></svg>");
  } catch (e) { toast("" + e, true); }
};
$("#btn-open-loaddrop").onclick = () => invoke("open_loaddrop").catch((e) => toast("" + e, true));
$("#btn-open-loaddrop2").onclick = $("#btn-open-loaddrop").onclick;
async function moveLoaddrop() {
  try {
    const dir = await invoke("pick_folder");
    if (!dir) return;
    await invoke("move_loaddrop", { newPath: dir });
    $("#loaddrop-path").textContent = dir; $("#set-loaddrop-view").textContent = dir;
    toast("Папка приёма: " + dir);
  } catch (e) { toast("" + e, true); }
}
$("#btn-move-loaddrop").onclick = moveLoaddrop;
$("#btn-move-loaddrop2").onclick = moveLoaddrop;
$("#btn-save").onclick = async () => {
  const oldPort = settings.tcp_port;
  const oldUdp = settings.udp_port;
  settings = { ...settings, device_name: $("#set-device").value, tcp_port: parseInt($("#set-port").value) || 47831, udp_enabled: $("#set-udp").checked, udp_port: parseInt($("#set-udpport").value) || 47833, auto_accept_trusted: $("#set-autotrust").checked, auto_accept_limit_mb: parseInt($("#set-limit").value) || 500, autostart: $("#set-autostart").checked, minimize_to_tray: $("#set-tray").checked, ctx_menu: $("#set-ctxmenu").checked, theme: collectTheme() };
  try {
    await invoke("save_settings", { settings });
    if ($("#set-ctxmenu").checked !== undefined) { try { await invoke("set_ctx_menu", { enabled: $("#set-ctxmenu").checked }); } catch (e) { toast("Меню проводника: " + e, true); } }
    toast((oldPort !== settings.tcp_port || oldUdp !== settings.udp_port) ? "Сохранено. Порты сменятся после перезапуска." : "Настройки сохранены <svg class='ic' viewBox='0 0 24 24'><path d='M9,16.17,4.83,12l-1.42,1.41L9,19,21,7l-1.41-1.41Z'/></svg>");
  } catch (e) { toast("" + e, true); }
};
$("#set-udp").onchange = (e) => { $("#set-udpport").disabled = !e.target.checked; };
$("#btn-firewall").onclick = async () => {
  toast("Открываю порты… (если спросит — разреши права администратора)");
  try { const log = await invoke("open_firewall"); toast("Firewall: " + String(log).replace(/\n/g, " ")); }
  catch (e) { showHelp("Нужно разрешение администратора", `<div class="muted left">Автоматически не вышло: ${esc(e)}<br><br>Вручную в CMD <b>от администратора</b>:<br><code>netsh advfirewall firewall add rule name="SafeDrop files" dir=in action=allow protocol=TCP localport=${settings.tcp_port}</code><br><code>netsh advfirewall firewall add rule name="SafeDrop signal" dir=in action=allow protocol=TCP localport=${settings.sig_port}</code>${settings.udp_enabled ? `<br><code>netsh advfirewall firewall add rule name="SafeDrop QUIC" dir=in action=allow protocol=UDP localport=${settings.udp_port}</code>` : ""}</div>`); }
};
$("#set-ctxmenu").onchange = async (e) => {
  try { await invoke("set_ctx_menu", { enabled: e.target.checked }); settings.ctx_menu = e.target.checked; await invoke("save_settings", { settings }); toast(e.target.checked ? "Пункт в проводнике включён" : "Пункт в проводнике убран"); }
  catch (err) { toast("" + err, true); e.target.checked = false; }
};
$("#btn-clear-peers").onclick = async () => {
  if (!await askConfirm("Удалить ВСЕ контакты?", "Дебаг-действие.", "Удалить")) return;
  try { await invoke("clear_peers"); peers = []; selectedIp = null; renderAll(); renderChat(); toast("Контакты очищены"); }
  catch (e) { toast("" + e, true); }
};
$("#btn-factory-reset").onclick = async () => {
  if (!await askConfirm("ПОЛНЫЙ СБРОС?", "Стереть контакты, чаты, настройки, оформление. Папка LoadDrop с файлами останется.", "Сбросить")) return;
  if (!await askConfirm("Точно-точно?", "Всё вернётся как при первом запуске.", "Да, сбросить")) return;
  try { await invoke("factory_reset"); } catch (e) { toast("" + e, true); return; }
  location.reload();
};
$("#btn-clear-cache").onclick = async () => {
  try { toast(await invoke("clear_cache")); }
  catch (e) { toast("" + e, true); }
};
$("#btn-test-notify").onclick = () => invoke("test_notify").then((r) => toast(r)).catch((e) => toast("" + e, true));
$("#btn-notif-win").onclick = () => invoke("open_notif_settings").catch((e) => toast("" + e, true));
$("#btn-ctx-refresh").onclick = async () => {
  try {
    const n = await invoke("refresh_ctx_menu");
    toast(settings.ctx_menu ? `Контактов в подменю: ${n}` : "Меню выключено — включи галочку выше");
  }
  catch (e) { toast("" + e, true); }
};
$("#btn-netdbg").onclick = async () => {
  try {
    const d = await invoke("scan_debug");
    showHelp("Диагностика сети", `<div class="muted left">Мои IP: <b>${esc(d.my_ips.join(", ") || "—")}</b><br>Radmin IP (26.*): <b>${esc(d.radmin_ips.join(", ") || "НЕТ — Radmin не подключён!")}</b><br>Сканируются подсети:<br><b>${(d.scan_nets || []).map(esc).join("<br>") || "—"}</b><br>Порты: ${d.tcp_port} / ${d.sig_port} / UDP ${d.udp_enabled ? "вкл :" + d.udp_port : "выкл"}<br>Контактов: ${d.contacts_total}, в сети: ${d.online.map((o) => `${esc(o.ip)} (${o.ping} мс)`).join(", ") || "—"}<br><br>Если Radmin IP нет — проверь, что Radmin VPN запущен и ты в сети.<br>Если IP есть, а контактов нет — жми «Открыть порты в Firewall».</div>`);
  } catch (e) { toast("" + e, true); }
};
// Custom ringtone selection is handled by the visible [data-ringback="custom"] option below.
$("#btn-ringback-test").onclick = () => { ringbackStart(); setTimeout(ringbackStop, 3000); };
$("#btn-ringback-clear").onclick = async () => {
  settings.ringback_melody = "default";
  settings.ringback_file = "";
  await invoke("save_settings", { settings });
  $("#ringback-name").textContent = "Стандартный гудок";
  document.querySelectorAll("[data-ringback]").forEach((b) => b.classList.toggle("active", b.dataset.ringback === "default"));
};
document.querySelectorAll("[data-ringback]").forEach((b) => {
  b.onclick = async () => {
    const m = b.dataset.ringback;
    if (m === "custom") {
      try {
        const path = await invoke("pick_audio");
        if (!path) return;
        const bad = await checkAudioFile(path);
        if (bad) return toast("Гудок не подойдёт: " + bad, true);
        settings.ringback_file = path; settings.ringback_melody = "custom";
        await invoke("save_settings", { settings });
        $("#ringback-name").textContent = fileName(path);
        toast("Гудок установлен");
      } catch (e) { toast("" + e, true); }
      return;
    }
    settings.ringback_melody = m;
    settings.ringback_file = "";
    await invoke("save_settings", { settings });
    $("#ringback-name").textContent = b.textContent;
    document.querySelectorAll("[data-ringback]").forEach((x) => x.classList.toggle("active", x.dataset.ringback === m));
    playRingMelody(m);
  };
});
document.querySelectorAll("[data-incoming-ring]").forEach((b) => {
  b.onclick = async () => {
    settings.incoming_ringtone_melody = b.dataset.incomingRing;
    settings.ringtone = "";
    try {
      await invoke("save_settings", { settings });
      $("#incoming-ring-name").textContent = b.textContent;
      document.querySelectorAll("[data-incoming-ring]").forEach((x) => x.classList.toggle("active", x === b));
      playRingMelody(b.dataset.incomingRing);
    } catch (e) { toast("" + e, true); }
  };
});
$("#btn-incoming-ring-pick").onclick = async () => {
  try {
    const path = await invoke("pick_audio");
    if (!path) return;
    const bad = await checkAudioFile(path);
    if (bad) return toast("Рингтон не подойдёт: " + bad, true);
    settings.ringtone = path;
    await invoke("save_settings", { settings });
    $("#incoming-ring-name").textContent = fileName(path);
    toast("Рингтон входящего звонка установлен");
    playOneShot(path);
  } catch (e) { toast("" + e, true); }
};
$("#btn-incoming-ring-clear").onclick = async () => {
  settings.ringtone = "";
  settings.incoming_ringtone_melody = "default";
  try {
    await invoke("save_settings", { settings });
    $("#incoming-ring-name").textContent = "Стандартный";
    document.querySelectorAll("[data-incoming-ring]").forEach((b) => b.classList.toggle("active", b.dataset.incomingRing === "default"));
    toast("Стандартный рингтон включён");
  } catch (e) { toast("" + e, true); }
};
$("#btn-incoming-ring-test").onclick = () => {
  if (settings.ringtone) playOneShot(settings.ringtone);
  else playRingMelody(settings.incoming_ringtone_melody || "default");
};
document.querySelectorAll("[data-call-join]").forEach((b) => {
  b.onclick = async () => {
    settings.call_join_melody = b.dataset.callJoin;
    settings.call_join_sound = "";
    try {
      await invoke("save_settings", { settings });
      $("#call-join-sound-name").textContent = b.textContent;
      document.querySelectorAll("[data-call-join]").forEach((x) => x.classList.toggle("active", x === b));
      playCallJoinSound();
    } catch (e) { toast("" + e, true); }
  };
});
$("#btn-call-join-pick").onclick = async () => {
  try {
    const path = await invoke("pick_audio");
    if (!path) return;
    const bad = await checkAudioFile(path);
    if (bad) return toast("Звук не подойдёт: " + bad, true);
    settings.call_join_sound = path;
    await invoke("save_settings", { settings });
    $("#call-join-sound-name").textContent = fileName(path);
    document.querySelectorAll("[data-call-join]").forEach((b) => b.classList.remove("active"));
    toast("Звук ответа участника установлен");
    playCallJoinSound();
  } catch (e) { toast("" + e, true); }
};
$("#btn-call-join-test").onclick = playCallJoinSound;
$("#btn-call-join-clear").onclick = async () => {
  settings.call_join_sound = "";
  settings.call_join_melody = "classic";
  try {
    await invoke("save_settings", { settings });
    $("#call-join-sound-name").textContent = "Классика";
    document.querySelectorAll("[data-call-join]").forEach((b) => b.classList.toggle("active", b.dataset.callJoin === "classic"));
    toast("Стандартный звук ответа включён");
  } catch (e) { toast("" + e, true); }
};
document.querySelectorAll("[data-call-leave]").forEach((b) => {
  b.onclick = async () => {
    settings.call_leave_melody = b.dataset.callLeave;
    settings.call_leave_sound = "";
    try {
      await invoke("save_settings", { settings });
      $("#call-leave-sound-name").textContent = b.textContent;
      document.querySelectorAll("[data-call-leave]").forEach((x) => x.classList.toggle("active", x === b));
      playCallLeaveSound();
    } catch (e) { toast("" + e, true); }
  };
});
$("#btn-call-leave-pick").onclick = async () => {
  try {
    const path = await invoke("pick_audio");
    if (!path) return;
    const bad = await checkAudioFile(path);
    if (bad) return toast("Звук не подойдёт: " + bad, true);
    settings.call_leave_sound = path;
    await invoke("save_settings", { settings });
    $("#call-leave-sound-name").textContent = fileName(path);
    document.querySelectorAll("[data-call-leave]").forEach((b) => b.classList.remove("active"));
    toast("Звук выхода участника установлен");
    playCallLeaveSound();
  } catch (e) { toast("" + e, true); }
};
$("#btn-call-leave-test").onclick = playCallLeaveSound;
$("#btn-call-leave-clear").onclick = async () => {
  settings.call_leave_sound = "";
  settings.call_leave_melody = "default";
  try {
    await invoke("save_settings", { settings });
    $("#call-leave-sound-name").textContent = "Стандартный";
    document.querySelectorAll("[data-call-leave]").forEach((b) => b.classList.toggle("active", b.dataset.callLeave === "default"));
    toast("Стандартный звук выхода включён");
  } catch (e) { toast("" + e, true); }
};
$("#btn-msg-pick").onclick = async () => {
  try {
    const path = await invoke("pick_audio");
    if (!path) return;
    const bad = await checkAudioFile(path);
    if (bad) return toast("Звук не подойдёт: " + bad, true);
    settings.message_sound = path; await invoke("save_settings", { settings });
    $("#msg-name").textContent = fileName(path); toast("Звук сообщений установлен <svg class='ic' viewBox='0 0 24 24'><path d='M9,16.17,4.83,12l-1.42,1.41L9,19,21,7l-1.41-1.41Z'/></svg>"); playMsgSound();
  } catch (e) { toast("" + e, true); }
};
$("#btn-msg-test").onclick = () => playMsgSound();
$("#btn-msg-clear").onclick = async () => {
  settings.message_sound = ""; await invoke("save_settings", { settings });
  $("#msg-name").textContent = "Стандартный «блуп»"; toast("Стандартный звук");
};
$("#btn-snd-file-pick").onclick = async () => {
  try {
    const path = await invoke("pick_audio");
    if (!path) return;
    const bad = await checkAudioFile(path);
    if (bad) return toast("Звук не подойдёт: " + bad, true);
    settings.file_sound = path; await invoke("save_settings", { settings });
    $("#snd-file-name").textContent = fileName(path); toast("Звук файлов установлен <svg class='ic' viewBox='0 0 24 24'><path d='M9,16.17,4.83,12l-1.42,1.41L9,19,21,7l-1.41-1.41Z'/></svg>"); playFileSound();
  } catch (e) { toast("" + e, true); }
};
$("#btn-snd-file-test").onclick = () => playFileSound();
$("#btn-snd-file-clear").onclick = async () => {
  settings.file_sound = ""; await invoke("save_settings", { settings });
  $("#snd-file-name").textContent = "Стандартный сигнал"; toast("Стандартный звук");
};
$("#btn-clear-history").onclick = async () => {
  if (!await askConfirm("Очистить всю историю передач?", "Сами файлы не тронуты.", "Очистить")) return;
  try { await invoke("clear_transfers"); refreshTransfers(); toast("История очищена"); }
  catch (e) { toast("" + e, true); }
};
$("#mic-vol").addEventListener("input", (e) => { settings.mic_volume = Number(e.target.value); applyMicVolume(); });
$("#mic-vol").addEventListener("change", () => invoke("save_settings", { settings }).catch(() => {}));
$("#gate-vol").addEventListener("input", (e) => {
  settings.noise_gate = Number(e.target.value);
  const el = $("#gate-vol-v"); if (el) el.textContent = `${settings.noise_gate}`;
  gateNode?.port?.postMessage({ type: "settings", threshold: settings.noise_gate / 3000 });
});
$("#gate-vol").addEventListener("change", () => invoke("save_settings", { settings }).catch(() => {}));
async function saveMicProcessingSettings() {
  try {
    await invoke("save_settings", { settings });
    const constraints = {
      echoCancellation: { ideal: settings.echo_cancellation !== false },
      noiseSuppression: { ideal: (settings.noise_filter || "rnnoise") === "webrtc" },
      autoGainControl: { ideal: settings.auto_gain_control !== false },
    };
    await Promise.all(localStream?.getAudioTracks().map((track) => track.applyConstraints(constraints).catch(() => {})) || []);
  } catch (e) { toast(`Настройки микрофона: ${e}`, true); }
}
$("#noise-engine").addEventListener("change", async (e) => {
  settings.noise_filter = e.target.value;
  settings.noise_suppression = e.target.value === "webrtc";
  await saveMicProcessingSettings();
  toast(inVoice() ? "Шумодав сохранён — новый алгоритм включится после звонка" : "Алгоритм шумоподавления сохранён");
});
$("#noise-strength").addEventListener("input", (e) => {
  settings.noise_strength = Number(e.target.value);
  const strength = settings.noise_strength / 100;
  if (rnnoiseWetGain && rnnoiseDryGain) {
    rnnoiseWetGain.gain.setTargetAtTime(strength, micCtx.currentTime, 0.025);
    rnnoiseDryGain.gain.setTargetAtTime(1 - strength, micCtx.currentTime, 0.025);
  }
  const label = $("#noise-strength-v"); if (label) label.textContent = `${settings.noise_strength}%`;
});
$("#noise-strength").addEventListener("change", () => invoke("save_settings", { settings }).catch(() => {}));
$("#echo-cancel").addEventListener("change", (e) => { settings.echo_cancellation = e.target.checked; saveMicProcessingSettings(); });
$("#auto-gain").addEventListener("change", (e) => { settings.auto_gain_control = e.target.checked; saveMicProcessingSettings(); });
$("#noise-smooth").addEventListener("input", (e) => {
  settings.noise_smooth = Number(e.target.value);
  const el = $("#noise-smooth-v"); if (el) el.textContent = `${settings.noise_smooth}%`;
  gateNode?.port?.postMessage({ type: "settings", smooth: settings.noise_smooth });
});
$("#noise-smooth").addEventListener("change", () => invoke("save_settings", { settings }).catch(() => {}));
function collectTheme() {
  return { bg_mode: document.querySelector(".seg-theme .seg-btn.active")?.dataset.bg || "gradient", bg_color: $("#th-color").value, bg_gradient: settings.theme?.bg_gradient || "linear-gradient(135deg,#1a2138,#0d1020)", bg_image: settings.theme?.bg_image || "", blur: parseInt($("#th-blur").value) || 0, opacity: (parseInt($("#th-opacity").value) || 85) / 100, accent: $("#th-accent").value, button_gradient_from: $("#th-button-gradient-from").value, button_gradient_to: $("#th-button-gradient-to").value, button_gradient_angle: Number($("#th-button-gradient-angle").value) || 0, radius: parseInt($("#th-radius").value) || 18, font: $("#th-font").value || "Segoe UI", custom_css: $("#th-css").value || "", text_color: $("#th-text")?.value || "#eef1f8", muted_color: $("#th-muted")?.value || "#9aa3b8", btn_color: $("#th-btn")?.value || "rgba(255,255,255,.1)", btn_text: $("#th-btntext")?.value || "#eef1f8", link_color: $("#th-link")?.value || "#7aa2ff" };
}
["th-color","th-button-gradient-from","th-button-gradient-to","th-button-gradient-angle","th-blur","th-opacity","th-accent","th-radius","th-font","th-text","th-muted","th-btn","th-btntext","th-link"].forEach((id) => document.getElementById(id)?.addEventListener("input", () => {
  settings.theme = { ...collectTheme(), bg_image: settings.theme?.bg_image || "" };
  $("#th-button-gradient-angle-v").textContent = `${$("#th-button-gradient-angle").value}°`;
  $("#button-gradient-preview").style.background = `linear-gradient(${settings.theme.button_gradient_angle}deg,${settings.theme.button_gradient_from},${settings.theme.button_gradient_to})`;
  applyTheme();
}));
document.querySelectorAll(".seg-theme .seg-btn").forEach((b) => b.onclick = () => { settings.theme = { ...collectTheme(), bg_mode: b.dataset.bg, bg_image: settings.theme?.bg_image || "" }; applyTheme(); });
$("#th-css")?.addEventListener("input", () => { settings.theme.custom_css = $("#th-css").value; $("#custom-css").textContent = settings.theme.custom_css; });
$("#btn-bg-pick").onclick = async () => {
  try {
    const path = await invoke("pick_image");
    if (!path) return;
    settings.theme = { ...collectTheme(), bg_mode: "image", bg_image: path };
    applyTheme(); await invoke("save_settings", { settings }); toast("Фон установлен <svg class='ic' viewBox='0 0 24 24'><path d='M9,16.17,4.83,12l-1.42,1.41L9,19,21,7l-1.41-1.41Z'/></svg>");
  } catch (e) { toast("" + e, true); }
};
$("#btn-bg-clear").onclick = async () => {
  settings.theme = { ...collectTheme(), bg_mode: "gradient", bg_image: "" };
  applyTheme(); await invoke("save_settings", { settings }); toast("Фон убран");
};
$("#btn-theme-save").onclick = async () => {
  settings.theme = collectTheme();
  try { await invoke("save_settings", { settings }); toast("Оформление сохранено <svg class='ic' viewBox='0 0 24 24'><path d='M9,16.17,4.83,12l-1.42,1.41L9,19,21,7l-1.41-1.41Z'/></svg>"); } catch (e) { toast("" + e, true); }
};
$("#btn-theme-export").onclick = () => {
  const blob = new Blob([JSON.stringify(settings.theme, null, 2)], { type: "application/json" });
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob); a.download = "safedrop-theme.json"; a.click();
};
$("#btn-theme-reset").onclick = async () => {
  settings.theme = { bg_mode: "gradient", bg_color: "#141824", bg_gradient: "linear-gradient(135deg,#1a2138,#0d1020)", bg_image: "", blur: 14, opacity: 0.85, accent: "#5b8cff", button_gradient_from: "#5b8cff", button_gradient_to: "#9b59ff", button_gradient_angle: 135, radius: 18, font: "Segoe UI", custom_css: "", text_color: "#eef1f8", muted_color: "#9aa3b8", btn_color: "rgba(255,255,255,.1)", btn_text: "#eef1f8", link_color: "#7aa2ff" };
  try { await invoke("save_settings", { settings }); loadAll(); } catch (e) { toast("" + e, true); }
};
document.querySelectorAll("#seg .seg-btn").forEach((b) => b.onclick = () => switchSeg(b.dataset.seg));

// ---------- обновления: раздача установщика (.exe) контактам ----------
function cmpVer(a, b) {
  const pa = String(a).split(".").map(Number), pb = String(b).split(".").map(Number);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const x = pa[i] || 0, y = pb[i] || 0;
    if (x !== y) return x > y ? 1 : -1;
  }
  return 0;
}
let updPath = null, updVer = "", updOffer = null;
const updSkip = new Set();
async function refreshMyVer() {
  try {
    const b = await invoke("get_build_info");
    $("#upd-myver").textContent = "v" + b.version;
    return b.version;
  } catch { return "0.0.0"; }
}
$("#btn-upd-pick").onclick = async () => {
  try {
    const paths = await invoke("pick_files");
    const p = (paths || [])[0]; if (!p) return;
    updPath = p;
    $("#upd-file").textContent = fileName(p);
  } catch (e) { toast("" + e, true); }
};
$("#btn-upd-send").onclick = async () => {
  if (!updPath) return toast("Сначала выбери установщик SafeDrop (.exe)", true);
  if (!/\.exe$/i.test(fileName(updPath))) return toast("Для обновления выбери установщик SafeDrop (.exe)", true);
  const targets = peers.filter((p) => p.online);
  if (!targets.length) return toast("Никого нет в сети", true);
  const log = $("#upd-log"); log.innerHTML = "";
  for (const t of targets) {
    const d = document.createElement("div"); d.className = "tcard";
    d.dataset.updatePeer = t.ip; d.dataset.updateFile = fileName(updPath);
    const title = document.createElement("b"); title.textContent = `${dispName(t)} · ${fileName(updPath)}`;
    const details = document.createElement("div"); details.className = "muted update-details"; details.textContent = "Подключение к получателю…";
    const bar = document.createElement("div"); bar.className = "bar"; bar.innerHTML = "<div style='width:0%'></div>";
    d.append(title, details, bar); log.appendChild(d);
    try {
      await invoke("send_update", { peerIp: t.ip, filePath: updPath });
      details.textContent = "Установщик доставлен";
      bar.firstElementChild.style.width = "100%";
    } catch (e) { details.textContent = `Ошибка: ${String(e)}`; }
  }
};
$("#upd-no").onclick = () => { if (updOffer) updSkip.add(updOffer.version + updOffer.path); $("#update-modal").classList.add("hidden"); updOffer = null; };
$("#upd-yes").onclick = async () => {
  const o = updOffer; $("#update-modal").classList.add("hidden"); updOffer = null;
  if (!o) return;
  toast("Запускаю установщик…");
  try { await invoke("apply_update", { path: o.path, version: o.version }); }
  catch (e) { toast("Не вышло: " + e, true); }
};
// окно: drag по топбару + кнопки через движок (надёжно)
(function () {
  const tb = $("#topbar");
  const interactive = (e) => e.target.closest("input,button,select,textarea,a");
  const bindDrag = (el) => {
    if (!el) return;
    el.addEventListener("mousedown", (e) => {
      if (e.button !== 0 || interactive(e)) return;
      invoke("win_drag").catch(() => {});
    });
    el.addEventListener("dblclick", (e) => {
      if (interactive(e)) return;
      invoke("win_max_toggle").catch(() => {});
    });
  };
  bindDrag(tb);
  bindDrag($("#call-screen .cs-top"));
})();
$("#btn-min").onclick = () => invoke("win_min").catch((e) => toast("" + e, true));
$("#btn-close").onclick = () => closeAppWindow(false);
$("#btn-max").onclick = () => invoke("win_max_toggle").catch((e) => toast("" + e, true));
$("#cm-accept").onclick = async () => {
  if (pendingOffer?.group) {
    const o = pendingOffer; pendingOffer = null;
    $("#call-modal").classList.add("hidden"); ringbackStop();
    await joinGroupCall(o.group, o.ip, o.sdp);
    return;
  }
  acceptCall();
};
$("#cm-decline").onclick = async () => {
  $("#call-modal").classList.add("hidden"); ringbackStop();
  if (pendingOffer) await invoke("sig_send", { peerIp: pendingOffer.ip, payload: { kind: "call-hangup", from: profile.nickname || "" } }).catch(() => {});
  pendingOffer = null;
};
// ---------- presence: кто в звонке / кто показывает ----------
function myPresence() {
  const call = !!(currentCall || groupCall);
  const sh = Object.keys(myShares).length > 0;
  return call && sh ? "incall+sharing" : call ? "incall" : sh ? "sharing" : "idle";
}
function stateBadge(st) {
  if (!st || st === "idle") return "";
  let s = "";
  if (st.includes("incall")) s += ' <svg class="ic" viewBox="0 0 24 24"><path d="M21,15v3.93a2,2,0,0,1-2.29,2A18,18,0,0,1,3.14,5.29,2,2,0,0,1,5.13,3H9a1,1,0,0,1,1,.89,10.74,10.74,0,0,0,1,3.78,1,1,0,0,1-.42,1.26l-.86.49a1,1,0,0,0-.33,1.46,14.08,14.08,0,0,0,3.69,3.69,1,1,0,0,0,1.46-.33l.49-.86A1,1,0,0,1,16.33,13a10.74,10.74,0,0,0,3.78,1A1,1,0,0,1,21,15Z"/></svg>';
  if (st.includes("sharing")) s += ' <svg class="ic" viewBox="0 0 24 24"><path d="M14,10V22H4a2,2,0,0,1-2-2V10Z"/><path d="M22,10V20a2,2,0,0,1-2,2H16V10Z"/><path d="M22,4V8H2V4A2,2,0,0,1,4,2H20A2,2,0,0,1,22,4Z"/></svg>';
  return s;
}
function myPresenceDetail() {
  if (groupCall) return `группа ${groupCall.name}`;
  if (currentCall) { const p = peers.find((x) => x.ip === currentCall); return p ? dispName(p) : currentCall; }
  if (Object.keys(myShares).length) return "показывает экран";
  return "";
}
setInterval(() => {
  if (isDemo || !peers.length) return;
  const st = myPresence(), detail = myPresenceDetail();
  for (const p of peers) {
    if (!p.online) continue;
    invoke("sig_send", { peerIp: p.ip, payload: { kind: "presence", state: st, detail } }).catch(() => {});
  }
}, 15000);
// перезагрузка страницы = смерть звонка/показа: тормозим её во время голоса
window.addEventListener("beforeunload", (e) => {
  if (inVoice() || Object.keys(myShares).length) { e.preventDefault(); e.returnValue = ""; }
});
window.addEventListener("keydown", (e) => {
  if (!inVoice() && !Object.keys(myShares).length) return;
  if (e.key === "F5" || ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "r")) {
    e.preventDefault();
    toast("<svg class='ic' viewBox='0 0 24 24'><path d='M12,2A10,10,0,1,0,22,12,10,10,0,0,0,12,2Zm0,18a8,8,0,1,1,8-8A8,8,0,0,1,12,20Z'/><path d='M12,6a1,1,0,0,0-1,1v4.59l-2.29,2.29a1,1,0,0,0,1.41,1.41l2.59-2.59A1,1,0,0,0,13,12V7A1,1,0,0,0,12,6Z'/></svg> Обновление во время звонка/показа всё оборвёт — сначала заверши", true);
  }
});

// ---------- удаление сообщений ----------
let delTarget = null; // {mode:'msg'|'clear', id, mine}
function openDel(t) {
  delTarget = t;
  $("#del-title").textContent = t.mode === "clear" ? "Очистить весь чат?" : "Удалить сообщение?";
  const all = $("#del-all");
  const canAll = !selGroup && (t.mode === "clear" || t.mine);
  all.style.display = canAll ? "" : "none";
  all.textContent = t.mode === "clear" ? "У обоих" : "У всех";
  $("#del-modal").classList.remove("hidden");
}
$("#del-cancel").onclick = () => { $("#del-modal").classList.add("hidden"); delTarget = null; };
$("#del-self").onclick = async () => {
  const t = delTarget; $("#del-modal").classList.add("hidden"); delTarget = null;
  const key = curKey();
  if (!t || !key) return;
  try {
    if (t.mode === "clear") await invoke("clear_chat", { peerIp: key, forPeer: false });
    else await invoke("delete_msgs", { peerIp: key, ids: [t.id], forPeer: false });
    chats[key] = await invoke("get_chat", { peerIp: key }); renderChat();
  } catch (e) { toast("" + e, true); }
};
$("#del-all").onclick = async () => {
  const t = delTarget; $("#del-modal").classList.add("hidden"); delTarget = null;
  const key = curKey();
  if (!t || !key) return;
  try {
    if (t.mode === "clear") await invoke("clear_chat", { peerIp: key, forPeer: true });
    else await invoke("delete_msgs", { peerIp: key, ids: [t.id], forPeer: true });
    chats[key] = await invoke("get_chat", { peerIp: key }); renderChat();
    toast("Удалено у обоих <svg class='ic' viewBox='0 0 24 24'><path d='M9,16.17,4.83,12l-1.42,1.41L9,19,21,7l-1.41-1.41Z'/></svg>");
  } catch (e) { toast("" + e, true); }
};
$("#btn-clear-chat").onclick = () => { if (curKey()) openDel({ mode: "clear" }); };
$("#btn-mic-test").onclick = () => startMic().then(() => toast("Говори — полоска должна двигаться")).catch(() => {});
let micMonitorNode = null;
$("#btn-mic-monitor").onclick = () => {
  if (micMonitorNode) {
    try { micMonitorNode.disconnect(); } catch {}
    micMonitorNode = null;
    toast("Мониторинг выключен");
    return;
  }
  if (!micCtx || !localStream) return toast("Сначала включи микрофон", true);
  try {
    const src = micCtx.createMediaStreamSource(micDest?.stream || localStream);
    const gain = micCtx.createGain();
    gain.gain.value = 0.5;
    src.connect(gain); gain.connect(micCtx.destination);
    micMonitorNode = gain;
    toast("Мониторинг включён — слышишь себя");
  } catch (e) { toast("Не вышло: " + e, true); }
};
const _micMonitorBtn = $("#btn-mic-monitor");
if (_micMonitorBtn && !_micMonitorBtn.onclick) {
  _micMonitorBtn.onclick = () => {};
}
$("#btn-mic-win").onclick = () => invoke("open_mic_settings").catch((e) => toast("" + e, true));
$("#btn-mic-req").onclick = requestMic;

// ---------- аудио в чате ----------
const AUDIO_EXT = /\.(mp3|wav|ogg|m4a|flac)$/i;
const IMAGE_EXT = /\.(png|jpe?g|gif|webp|bmp)$/i;
document.addEventListener("play", (e) => {
  if (e.target?.tagName === "AUDIO" && e.target.closest?.("#messages")) {
    document.querySelectorAll("#messages audio").forEach((a) => {
      if (a !== e.target) { try { a.pause(); } catch {} }
    });
  }
}, true);
async function sendAudioToChat(path) {
  if (!selectedIp && !selGroup) return toast("Сначала выбери контакт или группу", true);
  const name = fileName(path);
  const caption = ($("#chat-input")?.value || "").trim();
  const targets = selGroup
    ? (curGroup()?.members || []).filter((ip) => peers.find((p) => p.ip === ip)?.online)
    : (selectedIp ? [selectedIp] : []);
  if (!targets.length) return toast("Нет получателей в сети", true);
  toast(`Отправка аудио ${name}…`);
  let ok = 0;
  for (const t of targets) {
    try { await invoke("send_file", { peerIp: t, filePath: path }); ok++; }
    catch (e) { toast("Не отправлено: " + e, true); }
  }
  if (!ok) return;
  const m = selGroup
    ? await invoke("send_group_chat", { groupId: selGroup, text: caption, image: "", images: [], audio: path })
    : await invoke("send_chat", { peerIp: selectedIp, text: caption, image: "", images: [], audio: path });
  if ($("#chat-input")) $("#chat-input").value = "";
  if (pushMsg(m)) renderChat();
  stopTypingLocal();
}
function dropOnChat(pos) {
  if (!pos || typeof pos.x !== "number" || typeof pos.y !== "number") return false;
  const dpr = window.devicePixelRatio || 1;
  const hit = document.elementFromPoint(pos.x / dpr, pos.y / dpr);
  const pane = $("#chat-pane");
  return !!(hit && pane && (hit === pane || pane.contains(hit)));
}

// перетаскивание файлов на окно: в Tauri приходят настоящие пути
try {
  await listen("tauri://drag-drop", async (e) => {
    const paths = e.payload?.paths || [];
    if (!selectedIp && !selGroup) return toast("Сначала выбери контакт или группу", true);
    if (!paths.length) return;
    const onChat = dropOnChat(e.payload?.position);
    const audioFiles = onChat ? paths.filter((p) => AUDIO_EXT.test(p)) : [];
    const imageFiles = onChat ? paths.filter((p) => IMAGE_EXT.test(p)) : [];
    const others = paths.filter((p) => !audioFiles.includes(p) && !imageFiles.includes(p));
    for (const p of audioFiles) {
      try { await sendAudioToChat(p); } catch (err) { toast("" + err, true); }
    }
    for (const p of imageFiles) {
      try {
        const raw = await invoke("read_image_dataurl", { path: p });
        await sendImageDataUrl(raw);
      } catch (err) { toast("" + err, true); }
    }
    if (others.length) await sendPaths(selectedIp, others);
  });
} catch {}
const dz = $("#dropzone");
if (dz) {
  dz.ondragover = (e) => { e.preventDefault(); dz.classList.add("dragover"); };
  dz.ondragleave = () => dz.classList.remove("dragover");
  dz.ondrop = () => { dz.classList.remove("dragover"); if (!isDemo) toast("Кидай файлы прямо на окно приложения"); };
}

// presets
const PRESETS = [
  { name: "SafeDrop", theme: { bg_mode: "gradient", bg_color: "#141824", bg_gradient: "linear-gradient(135deg,#1a2138,#0d1020)", bg_image: "", blur: 14, opacity: 0.85, accent: "#5b8cff", radius: 18, font: "Segoe UI", custom_css: "", text_color: "#eef1f8", muted_color: "#9aa3b8", btn_color: "#333a4d", btn_text: "#eef1f8" } },
  { name: "Неон", theme: { bg_mode: "gradient", bg_color: "#0b0020", bg_gradient: "linear-gradient(135deg,#2b0066,#00b3ff)", bg_image: "", blur: 8, opacity: 0.8, accent: "#00e5ff", radius: 20, font: "Segoe UI", custom_css: "", text_color: "#eef1f8", muted_color: "#9aa3b8", btn_color: "#333a4d", btn_text: "#eef1f8", link_color: "#7aa2ff" } },
  { name: "Графит", theme: { bg_mode: "color", bg_color: "#1b1e27", bg_gradient: "", bg_image: "", blur: 0, opacity: 1, accent: "#5b8cff", radius: 14, font: "Segoe UI", custom_css: "", text_color: "#eef1f8", muted_color: "#9aa3b8", btn_color: "#333a4d", btn_text: "#eef1f8", link_color: "#7aa2ff" } },
];
const pr = $("#presets");
PRESETS.forEach((p) => { const b = document.createElement("button"); b.textContent = p.name; b.onclick = async () => { settings.theme = p.theme; try { await invoke("save_settings", { settings }); loadAll(); } catch (e) { toast("" + e, true); } }; pr.appendChild(b); });

// ---------- аккордеон настроек: 9 секций, состояние запоминается ----------
const ACC_SECTION_IDS = ["sec-profile", "sec-net", "sec-call", "sec-notif", "sec-chat", "sec-files", "sec-appearance", "sec-system", "sec-about"];
function sectionsOpenMap() { return (settings.sections_open && typeof settings.sections_open === "object") ? settings.sections_open : {}; }
let accProgrammatic = false;
function renderAccordionState() {
  const open = sectionsOpenMap();
  accProgrammatic = true;
  ACC_SECTION_IDS.forEach((id) => {
    const d = document.getElementById(id);
    if (!d) return;
    if (id in open) d.open = !!open[id];
    if (!(id in open)) d.open = false; // дефолт: всё закрыто, первый запуск не шумит
  });
  setTimeout(() => { accProgrammatic = false; }, 0);
}
let accSaveTimer = null;
function bindAccordion() {
  const acc = $("#settings-acc");
  if (!acc) return;
  acc.querySelectorAll("details.set-sec").forEach((d) => {
    d.addEventListener("toggle", () => {
      // поиск/восстановление состояния не должны перезаписывать настройку
      if (accProgrammatic || ($("#set-search")?.value || "").trim()) return;
      const map = sectionsOpenMap();
      const was = map[d.id];
      if (was === d.open) return;
      map[d.id] = d.open;
      settings.sections_open = map;
      clearTimeout(accSaveTimer);
      accSaveTimer = setTimeout(() => { invoke("save_settings", { settings }).catch(() => {}); }, 400);
    });
  });
}

// ---------- поиск по настройкам ----------
let setSearchTimer = null;
const setSearchOriginal = new Map(); // element -> original html
function restoreSettingsSearch() {
  document.querySelectorAll(".set-empty-hint").forEach((el) => el.remove());
  for (const [el, saved] of setSearchOriginal) el.innerHTML = saved;
  setSearchOriginal.clear();
  document.querySelectorAll("#settings-acc .set-hit, #settings-acc .set-dim, .set-sec.set-nohits")
    .forEach((el) => el.classList.remove("set-hit", "set-dim", "set-nohits"));
  renderAccordionState(); // вернуть запомненное состояние секций
}
$("#set-search").addEventListener("input", () => {
  clearTimeout(setSearchTimer);
  setSearchTimer = setTimeout(() => runSettingsSearch(), 250);
});
function runSettingsSearch() {
  const inp = $("#set-search");
  if (!inp) return;
  const q = inp.value.trim().toLowerCase();
  if (!q) { restoreSettingsSearch(); return; }
  restoreSettingsSearch();
  const acc = $("#settings-acc");
  if (!acc) return;
  const rowSel = "label[data-set-label], label.check, button[data-set-label], div.muted:not([id])";
  let any = false;
  acc.querySelectorAll("details.set-sec").forEach((sec) => {
    let secHit = false;
    const rows = [...sec.querySelectorAll(rowSel)]
      .filter((el) => !el.parentElement?.closest(rowSel)); // только верхние строки
    for (const el of rows) {
      const labelText = (el.dataset.setLabel || "").toLowerCase();
      const bodyText = el.textContent.toLowerCase();
      if (labelText.includes(q) || bodyText.includes(q)) {
        secHit = true; any = true;
        el.classList.add("set-hit");
        markTextMatches(el, q);
      } else {
        el.classList.add("set-dim");
      }
    }
    // заголовки: только подсветка, не скрываем
    sec.querySelectorAll("h3").forEach((h) => {
      if (h.textContent.toLowerCase().includes(q)) { secHit = true; any = true; markTextMatches(h, q); }
    });
    if (secHit) sec.open = true;
    else sec.classList.add("set-nohits");
  });
  if (!any) {
    const hint = document.createElement("div");
    hint.className = "muted set-empty-hint";
    hint.textContent = "Ничего не найдено";
    acc.prepend(hint);
  }
}
function markTextMatches(el, q) {
  // обёртка совпадений тегом <mark>; сохраняем оригинал ДО любых изменений
  if (!setSearchOriginal.has(el)) setSearchOriginal.set(el, el.innerHTML);
  const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT, null);
  const targets = [];
  while (walker.nextNode()) {
    const n = walker.currentNode;
    if (n.nodeValue.toLowerCase().includes(q)) targets.push(n);
  }
  for (const n of targets) {
    const text = n.nodeValue;
    const lower = text.toLowerCase();
    let idx = lower.indexOf(q);
    const frag = document.createDocumentFragment();
    let pos = 0;
    while (idx !== -1) {
      if (idx > pos) frag.appendChild(document.createTextNode(text.slice(pos, idx)));
      const mark = document.createElement("mark");
      mark.textContent = text.slice(idx, idx + q.length);
      frag.appendChild(mark);
      pos = idx + q.length;
      idx = lower.indexOf(q, pos);
    }
    if (pos < text.length) frag.appendChild(document.createTextNode(text.slice(pos)));
    n.parentNode.replaceChild(frag, n);
  }
}

// ---------- рендеринг сети: мои адаптеры + скан ----------
function adapterKeyOf(name, ip) { return `${name}|${ip}`; }
async function renderMyAdapters() {
  const host = $("#my-adapters-list");
  if (!host) return;
  try {
    const ads = await invoke("get_my_adapters");
    if (!ads.length) { host.innerHTML = "<span class=\"muted\">Адаптеры не найдены</span>"; return; }
    host.innerHTML = ads.map((a) => {
      const st = a.up ? '<span style="color:#3ddc84">●</span>' : '<span style="color:#666">○</span>';
      return `<div class="adapter-row" data-ip="${esc(a.ip)}" title="Клик — скопировать адрес">${st} <b>${esc(a.name)}</b><span class="adapter-ip">${esc(a.ip)}</span><span class="muted">/${a.prefix}</span></div>`;
    }).join("");
    host.querySelectorAll(".adapter-row").forEach((row) => {
      row.onclick = async () => {
        try {
          await navigator.clipboard.writeText(row.dataset.ip);
          toast(`Скопировано: ${row.dataset.ip}`);
        } catch { toast("Не скопировалось", true); }
      };
    });
  } catch (e) {
    host.textContent = "Адаптеры: " + e;
  }
}
function renderScanUI() {
  const host = $("#scan-adapters-list");
  const manual = $("#scan-manual-list");
  if (manual) {
    const list = settings.scan_manual || [];
    manual.innerHTML = list.map((s, i) => `<div class="scan-manual-row"><span>${esc(s)}</span><button data-i="${i}" title="Убрать">×</button></div>`).join("");
    manual.querySelectorAll("button[data-i]").forEach((b) => {
      b.onclick = () => {
        const arr = settings.scan_manual || [];
        arr.splice(Number(b.dataset.i), 1);
        settings.scan_manual = arr;
        invoke("save_settings", { settings }).catch(() => {});
        renderScanUI();
      };
    });
  }
  if (!host) return;
  const allowed = settings.scan_adapters || [];
  invoke("get_my_adapters").then((ads) => {
    if (!ads.length) { host.innerHTML = "<div class=\"muted\">Адаптеры не найдены</div>"; return; }
    host.innerHTML = ads.map((a) => {
      const key = adapterKeyOf(a.name, a.ip);
      const on = allowed.length === 0 ? a.up : allowed.includes(key);
      return `<label class="check scan-adapter-check"><input type="checkbox" data-key="${esc(key)}" ${on ? "checked" : ""} /> ${a.up ? '<span style="color:#3ddc84">●</span>' : '<span style="color:#666">○</span>'} ${esc(a.name)} <span class="muted">${esc(a.ip)}</span></label>`;
    }).join("");
    host.querySelectorAll("input[data-key]").forEach((box) => {
      box.onchange = () => {
        const activeKeys = ads.filter((a) => a.up).map((a) => adapterKeyOf(a.name, a.ip));
        // рабочий список: явный, если задан; иначе все активные
        let list = ((settings.scan_adapters || []).length ? settings.scan_adapters.slice() : activeKeys.slice());
        const k = box.dataset.key;
        if (box.checked) {
          if (!list.includes(k)) list.push(k);
        } else {
          list = list.filter((x) => x !== k);
        }
        // все активные включены → авто-режим (пустой список)
        const allOn = activeKeys.length && activeKeys.every((x) => list.includes(x));
        settings.scan_adapters = allOn ? [] : list;
        invoke("save_settings", { settings }).catch(() => {});
        renderScanUI();
        toast("Скан: " + (allOn ? "все активные адаптеры" : `${(settings.scan_adapters || []).length} адаптеров`));
      };
    });
  }).catch(() => {});
}
$("#btn-scan-manual-add").onclick = () => {
  const inp = $("#scan-manual-input");
  if (!inp) return;
  const v = inp.value.trim();
  if (!v) return;
  // базовая проверка: IP или CIDR
  const re = /^\d{1,3}(\.\d{1,3}){3}(\/\d{1,2})?$/;
  if (!re.test(v)) { toast("Формат: 10.99.16.0/22 или 1.2.3.4", true); return; }
  const arr = settings.scan_manual || [];
  if (arr.includes(v)) { toast("Уже добавлено", true); return; }
  arr.push(v);
  settings.scan_manual = arr;
  invoke("save_settings", { settings }).catch(() => {});
  inp.value = "";
  renderScanUI();
};
let scanRunning = false;
$("#btn-scan-now").onclick = async () => {
  if (scanRunning) return toast("Скан уже идёт", true);
  scanRunning = true;
  const btn = $("#btn-scan-now");
  if (btn) btn.disabled = true;
  const prog = $("#scan-progress");
  if (prog) { prog.classList.remove("hidden"); prog.textContent = "Скан…"; }
  try {
    await invoke("scan_network");
  } catch (e) { toast("" + e, true); }
  await new Promise((r) => setTimeout(r, 300));
  renderMyAdapters();
  toast("Скан завершён");
  scanRunning = false;
  if (btn) btn.disabled = false;
};

// биндинг настроек чата
{
  const st = $("#set-show-typing");
  if (st) st.onchange = () => {
    settings.show_typing = st.checked;
    invoke("save_settings", { settings }).catch((e) => toast("" + e, true));
  };
}

bindAccordion();
try {
  await listen("safedrop-scan-progress", (e) => {
    const { done = 0, total = 0 } = e.payload || {};
    const prog = $("#scan-progress");
    if (!prog) return;
    if (total && done < total) {
      prog.classList.remove("hidden");
      prog.textContent = `Скан: ${done}/${total}`;
    } else {
      prog.textContent = "Готово";
      setTimeout(() => prog.classList.add("hidden"), 1500);
    }
  });
} catch {}

// ---------- события из Rust ----------
try {
  await listen("safedrop-quit-requested", () => closeAppWindow(true));
  await listen("safedrop-peers", async (e) => {
    peers = Array.isArray(e.payload) ? e.payload : await invoke("get_peers").catch(() => peers);
    if (!sel() && !selGroup && peers.length) { selectedIp = peers[0].ip; try { chats[selectedIp] = await invoke("get_chat", { peerIp: selectedIp }); chats[selectedIp].forEach((m) => seenIds.add(m.id)); } catch {} }
    renderAll(); renderChat(); renderCallStage();
  });
  await listen("safedrop-chat", (e) => {
    const m = e.payload;
    if (!pushMsg(m)) return; // дубль
    const key = m.peer || curKey();
    setTypingState(key, m.from_nick, false); // сообщение пришло — «печатает» снимаем
    const sameChat = key === curKey();
    if (sameChat) renderChat();
    else { unread[m.peer] = (unread[m.peer] || 0) + 1; renderContacts(); renderGroups(); toast(`<svg class='ic' viewBox='0 0 24 24'><path d="M20,2H4A2,2,0,0,0,2,4V22l4-4H20a2,2,0,0,0,2-2V4A2,2,0,0,0,20,2Z"/></svg> ${m.from_nick}: ${(m.text || "<svg class='ic' viewBox='0 0 24 24'><path d='M20,3H4A2,2,0,0,0,2,5V16.76l4.88-4.88A3,3,0,0,1,11,11.76l.87-.88A3,3,0,0,1,14,10a3,3,0,0,1,2.12.88L22,16.76V5A2,2,0,0,0,20,3ZM11,9a2,2,0,1,1,2-2A2,2,0,0,1,11,9ZM21.93,19.51a2,2,0,0,1-1.42,1.42A1.7,1.7,0,0,1,20,21H4a1.7,1.7,0,0,1-.51-.07,2,2,0,0,1-1.42-1.42L4,17.58l4.29-4.29a1,1,0,0,1,1.42,0L11,14.59l2.29-2.3a1,1,0,0,1,1.42,0L20,17.58Z'/></svg> картинка").slice(0, 80)}`); }
    // Sound only when the app is not in the foreground. In a focused window
    // the open chat stays silent; any other chat still plays the sound.
    const foreground = !document.hidden && document.hasFocus();
    if (!sameChat || !foreground) playMsgSound();
    if (!foreground) pingHidden();
  });
  await listen("safedrop-ctx-send", (e) => handlePending(e.payload));
  await listen("safedrop-peer-lost", (e) => {
    const ip = e.payload?.ip; if (!ip) return;
    toast(`<span style="color:#666">●</span> ${ip} отключился`);
    if (currentCall === ip) { playCallLeaveSound(); hangup(); }
    if (gpc[ip]) {
      try { gpc[ip].pc.close(); } catch {}
      delete gpc[ip]; dropRemoteVoice(ip);
      if (groupCall) {
        groupCall.members = groupCall.members.filter((m) => m !== ip);
        playCallLeaveSound();
      }
      renderCallPanel(); renderCallStage();
      if (groupCall && !Object.keys(gpc).length) endGroupCall(true);
    }
    for (const [sid, v] of Object.entries(views)) if (v.ip === ip) closeView(sid);
    for (const [sid, s] of Object.entries(myShares)) {
      if (s.pcs[ip]) { try { s.pcs[ip].close(); } catch {} delete s.pcs[ip]; }
    }
    for (const [sid, s] of Object.entries(myShares)) {
      if (!Object.keys(s.pcs).length) stopShare(sid, false);
    }
    renderStages();
  });
  await listen("safedrop-chat-changed", async (e) => {
    const key = e.payload?.peer;
    if (allChatsCache && key && key in allChatsCache) {
      try { allChatsCache[key] = await invoke("get_chat", { peerIp: key }); } catch {}
    }
    if (key && (key === selectedIp || key === curKey())) {
      try { chats[key] = await invoke("get_chat", { peerIp: key }); chats[key].forEach((m) => seenIds.add(m.id)); } catch {}
      renderChat();
    }
  });
  // Прямое обновление галочек по ack — гарантия отображения «доставлено».
  await listen("safedrop-chat-ack", async (e) => {
    const p = e.payload || {};
    const ids = new Set(p.ids || []);
    if (!ids.size) return;
    let hit = false;
    for (const key of Object.keys(chats)) {
      const rows = chats[key] || [];
      for (const m of rows) {
        if (m && m.from_me && ids.has(m.id) && m.status !== "delivered") {
          m.status = "delivered";
          hit = true;
        }
      }
    }
    if (hit) renderChat();
  });
  await listen("safedrop-typing", (e) => {
    const v = e.payload || {};
    const key = v.group ? `g:${v.group}` : (v.ip || "");
    if (!key) return;
    const nick = v.nickname || v.from || "";
    if (!nick) return;
    setTypingState(key, nick, true);
  });
  await listen("safedrop-transfers", () => refreshTransfers());
  await listen("safedrop-file-offer", (e) => { incomingFileOffers.push(e.payload); showNextIncomingFileOffer(); });
  await listen("safedrop-received", (e) => { toast(`<svg class='ic' viewBox='0 0 24 24'><path d='M20,11H7.83l5.59-5.59L12,4l-8,8,8,8,1.41-1.41L7.83,13H20Z'/></svg> Получен файл: ${e.payload.filename}`); playFileSound(); pingHidden(); refreshTransfers(); });
  await listen("safedrop-cancelled", (e) => { toast(`<svg class='ic' viewBox='0 0 24 24'><path d='M6,6H18V18H6Z'/></svg> Отменено: ${e.payload.filename} (хвост удалён)`); refreshTransfers(); });
  await listen("safedrop-call-offer", async (e) => {
    const { ip, sdp, from, group } = e.payload;
    if (group?.id) {
      if (groupCall?.id === group.id) {
        // mesh: ещё один участник дозванивается — принимаем тихо
        try { await acceptMeshOffer(ip, sdp); } catch (err) { toast(`${ip}: ${err}`, true); }
        return;
      }
      if (currentCall || groupCall || pendingOffer || callSetupPeer) {
        await invoke("sig_send", { peerIp: ip, payload: { kind: "call-hangup", from: myNick() } }).catch(() => {});
        return;
      }
      pendingOffer = { ip, sdp, group };
      $("#cm-name").textContent = `${group.name || "Группа"} — звонит ${from} · ${ip}`;
      $("#cm-avatar").outerHTML = `<span class="ava lg"><span><svg class="ic" viewBox="0 0 24 24" style="width:28px;height:28px"><circle cx="9" cy="8" r="3"/><path d="M14,11H4A2,2,0,0,0,2,13v5H6V15H4V13A1,1,0,0,1,5,12H13a1,1,0,0,1,1,1v2H12v3h4V13A2,2,0,0,0,14,11Z"/><circle cx="17" cy="9" r="2.5"/><path d="M20,11.5A1.5,1.5,0,0,0,18.5,10H15a3,3,0,0,1,2,2.82V18H15v3h4V14.82A3,3,0,0,1,20,13Z"/></svg></span></span>`.replace('class="ava lg', 'id="cm-avatar" class="ava lg');
      $("#call-modal").classList.remove("hidden");
      incomingRingtoneStart(); pingHidden();
      return;
    }
    if (currentCall || groupCall || pendingOffer || callSetupPeer) {
      if (currentCall === ip) return;
      await invoke("sig_send", { peerIp: ip, payload: { kind: "call-hangup", from: myNick() } }).catch(() => {});
      return;
    }
    pendingOffer = { ip, sdp };
    const p = peers.find((x) => x.ip === ip);
    $("#cm-name").textContent = `${from || (p ? dispName(p) : ip)} · ${ip}`;
    $("#cm-avatar").outerHTML = (p ? avaHTML(p, "lg") : `<span class="ava lg"><span>?</span></span>`).replace('class="ava lg', 'id="cm-avatar" class="ava lg');
    $("#call-modal").classList.remove("hidden");
    incomingRingtoneStart(); pingHidden();
  });
  await listen("safedrop-call-answer", async (e) => {
    const t = e.payload;
    if (pc && currentCall === t.ip) {
      try {
        await pc.setRemoteDescription(JSON.parse(t.sdp));
        await flushInIce();
        ringbackStop();
        playCallJoinSound();
      } catch {}
      return;
    }
    const g = gpc[t.ip];
    if (g) {
      try {
        await g.pc.setRemoteDescription(JSON.parse(t.sdp));
        await flushGin(t.ip);
        ringbackStop();
        playCallJoinSound();
      } catch {}
    }
  });
  await listen("safedrop-ice", async (e) => {
    const t = e.payload;
    if (pc && currentCall === t.ip) {
      if (pc.remoteDescription) { try { await pc.addIceCandidate(JSON.parse(t.candidate)); } catch {} }
      else iceInBuf.push(t.candidate); // придёт раньше answer — подождёт
      return;
    }
    const g = gpc[t.ip];
    if (g) {
      if (g.pc.remoteDescription) { try { await g.pc.addIceCandidate(JSON.parse(t.candidate)); } catch {} }
      else { ginBuf[t.ip] = ginBuf[t.ip] || []; ginBuf[t.ip].push(t.candidate); }
    }
  });
  await listen("safedrop-hangup", (e) => {
    const ip = e.payload?.ip;
    if (pendingOffer?.ip === ip) {
      pendingOffer = null;
      $("#call-modal").classList.add("hidden");
      ringbackStop();
      toast("Вызов отклонён");
      return;
    }
    if (groupCall && ip && groupCall.members.includes(ip)) {
      void closeScreensForPeer(ip);
      try { gpc[ip]?.pc.close(); } catch {}
      delete gpc[ip]; dropRemoteVoice(ip);
      groupCall.members = groupCall.members.filter((m) => m !== ip);
      playCallLeaveSound();
      toast("Участник вышел из звонка");
      renderCallPanel(); renderCallStage();
      if (!Object.keys(gpc).length) endGroupCall(true);
      return;
    }
    if (groupCall || currentCall !== ip) return; // чужой или устаревший hangup
    $("#call-modal").classList.add("hidden"); pendingOffer = null; ringbackStop();
    void closeScreensForPeer(ip);
    playCallLeaveSound();
    toast("Звонок завершён"); hideCallUI();
  });
  await listen("safedrop-groups", (e) => {
    groups = e.payload || []; normalizeGroups();
    renderGroups(); renderConv(); renderPeerCard();
    if (selGroup && !curGroup()) { selGroup = null; renderConv(); renderPeerCard(); renderChat(); }
  });
  await listen("safedrop-update-offer", async (e) => {
    const { version, path, peer, filename } = e.payload;
    let my = "0.0.0";
    try { my = (await invoke("get_build_info")).version; } catch {}
    if (!version) { toast(`<svg class='ic' viewBox='0 0 24 24'><path d='M21,8H17.17A5,5,0,0,0,13,4.17V3H11V4.17A5,5,0,0,0,6.83,8H3V20a2,2,0,0,0,2,2H19a2,2,0,0,0,2-2ZM12,6a3,3,0,0,1,3,3H9A3,3,0,0,1,12,6Z'/></svg> Пришёл пакет обновлений без версии от ${peer} — пропущен`, true); return; }
    if (updSkip.has(version + path)) return;
    const cmp = cmpVer(version, my);
    if (cmp < 0) { toast(`<svg class='ic' viewBox='0 0 24 24'><path d='M21,8H17.17A5,5,0,0,0,13,4.17V3H11V4.17A5,5,0,0,0,6.83,8H3V20a2,2,0,0,0,2,2H19a2,2,0,0,0,2-2ZM12,6a3,3,0,0,1,3,3H9A3,3,0,0,1,12,6Z'/></svg> Пакет v${version} от ${peer} старее твоей v${my} — пропущен`); return; }
    updOffer = { version, path };
    if (cmp === 0) {
      $("#upd-ver").textContent = `v${version} (у тебя v${my}) — та же версия, переустановить?`;
    } else {
      $("#upd-ver").textContent = `v${version} (у тебя v${my})`;
    }
    $("#upd-from").textContent = `Прислал: ${peer} · ${filename || ""}`;
    $("#update-modal").classList.remove("hidden");
  });
  await listen("safedrop-screen-offer", (e) => { pingHidden(); acceptScreenView(e.payload).catch((err) => toast(`Экран: ${err}`, true)); });
  await listen("safedrop-screen-answer", async (e) => {
    const t = e.payload;
    const c = myShares[t.share_id]?.pcs[t.ip];
    if (c && !c.remoteDescription) { try { await c.setRemoteDescription(JSON.parse(t.sdp)); } catch {} }
    const buf = sinBuf[t.share_id + t.ip] || []; delete sinBuf[t.share_id + t.ip];
    for (const cd of buf) { try { await c?.addIceCandidate(JSON.parse(cd)); } catch {} }
  });
  await listen("safedrop-screen-ice", async (e) => {
    const t = e.payload;
    const mine = myShares[t.share_id]?.pcs[t.ip];
    if (mine) {
      if (mine.remoteDescription) { try { await mine.addIceCandidate(JSON.parse(t.candidate)); } catch {} }
      else { const k = t.share_id + t.ip; sinBuf[k] = sinBuf[k] || []; sinBuf[k].push(t.candidate); }
      return;
    }
    const v = views[t.share_id];
    if (v) {
      if (v.pc.remoteDescription) { try { await v.pc.addIceCandidate(JSON.parse(t.candidate)); } catch {} }
      else v.inbuf.push(t.candidate);
    }
  });
  await listen("safedrop-screen-stop", async (e) => {
    const t = e.payload;
    if (t.request && myShares[t.share_id]?.pcs?.[t.ip]) {
      const share = myShares[t.share_id];
      try { share.pcs[t.ip].close(); } catch {}
      delete share.pcs[t.ip];
      delete share.pcsAt?.[t.ip];
      share.targets = (share.targets || []).filter((target) => target !== t.ip);
      if (!Object.keys(share.pcs).length) await stopShare(t.share_id, false);
      else { renderStages(); renderShareList(); renderCallStage(); updateBusy(); }
      return;
    }
    if (views[t.share_id]) { closeView(t.share_id); renderCallStage(); toast("Демонстрация завершена"); }
  });
} catch {}

// ---------- совместный просмотр YouTube ----------
// Сессия живёт, пока идёт звонок. Плеер создаётся один раз и переиспользуется
// для большого окна, мини-окна и повторного входа.
const WT_POS_KEY = "sd.wt.mini";
const WT_VOL_KEY = "sd.wt.vol";
const wtState = {
  player: null, apiStarted: false, apiPoll: null, playerReady: false,
  videoId: null, title: "", by: "", vertical: false,
  session: false, open: false, mini: false, joining: false,
  applyingUntil: 0, justLoadedUntil: 0, joinedUntil: 0, contentDuration: 0,
  // seq от времени запуска: после перезапуска приложения номера не начинаются
  // заново с нуля и не отбрасываются как старые у остальных участников.
  seen: {}, lastAt: {}, seq: Date.now(),
  syncTimer: null, uiTimer: null, readyTimer: null, nowTimer: null,
  seeking: false, vol: 70, muted: false,
  pending: null, remotePos: 0, remotePaused: true,
  viewers: {},
};
window.__wtId = window.__wtId || null;
let wtUiSig = "";

function myNick() { return profile.nickname || settings?.device_name || "Я"; }
function wtQ(id) { return document.getElementById(id); }
function wtOn(id, ev, fn) { const el = wtQ(id); if (el) el.addEventListener(ev, fn); return el; }
function wtFmt(s) {
  s = Math.max(0, Number(s) || 0);
  const m = Math.floor(s / 60), r = Math.floor(s % 60);
  return m + ":" + String(r).padStart(2, "0");
}
function wtNick(ip, fallback) {
  try {
    const p = typeof callPeerByIp === "function" ? callPeerByIp(ip) : null;
    if (p) return dispName(p);
  } catch {}
  return fallback || ip || "Участник";
}

function parseYT(url) {
  const raw = String(url || "");
  const m = raw.match(/(?:youtube\.com\/(?:watch\?[^ ]*v=|shorts\/|embed\/|live\/)|youtu\.be\/)([A-Za-z0-9_-]{6,})/);
  const t = raw.match(/[?&#]t=(\d+)/);
  return { id: m && m[1], start: t ? Number(t[1]) : 0, vertical: /\/shorts\//.test(raw) };
}

function wtHasSession() { return !!wtState.session && !!wtState.videoId; }
function wtIsWatching() { return !!(wtState.open || wtState.mini); }
function wtSessionInfo() {
  if (!wtHasSession()) return null;
  return { id: wtState.videoId, title: wtState.title || "Видео", by: wtState.by || "", vertical: !!wtState.vertical };
}

function wtTargets() {
  if (typeof inVoice !== "function" || !inVoice()) return [];
  let list = [];
  if (currentCall) list = [currentCall];
  else if (groupCall && Array.isArray(groupCall.members)) list = groupCall.members.slice();
  else if (typeof gpc !== "undefined") list = Object.keys(gpc);
  list = [...new Set(list)].filter((ip) => ip && ip !== myIp);
  return list.filter((ip) => {
    const p = (typeof peers !== "undefined" ? peers : []).find((x) => x.ip === ip);
    return !p || p.supports_watch !== false;
  });
}

function wtSetSession(videoId, title, by, vertical) {
  if (wtState.videoId !== videoId) wtState.contentDuration = 0;
  wtState.videoId = videoId;
  wtState.session = true;
  wtState.title = title || "Видео";
  wtState.by = by || wtState.by || myNick();
  wtState.vertical = !!vertical;
  window.__wtId = videoId;
  wtQ("watch-together")?.classList.toggle("wt-vert", !!vertical);
  // размытая обложка по бокам вертикального видео (id только [A-Za-z0-9_-])
  wtQ("wt-stage")?.style.setProperty("--wt-bg", `url("https://i.ytimg.com/vi/${videoId}/hqdefault.jpg")`);
  wtRefreshUI();
}

function wtClearSession() {
  wtState.session = false; wtState.videoId = null; wtState.title = ""; wtState.by = "";
  wtState.vertical = false; wtState.pending = null;
  wtState.remotePos = 0; wtState.remotePaused = true; wtState.viewers = {};
  window.__wtId = null;
  wtQ("watch-together")?.classList.remove("wt-vert");
  wtQ("wt-stage")?.style.removeProperty("--wt-bg");
  wtRefreshUI();
}

function wtAdopt(p) {
  if (!p.video) return;
  wtSetSession(p.video, p.title || "Видео", p.by || p.from || wtState.by, !!p.vertical);
  wtState.remotePos = Number(p.pos) || 0;
  wtState.remotePaused = !!p.paused;
}

function wtSend(payload, directIp) {
  if (typeof inVoice !== "function" || !inVoice()) return;
  const targets = directIp ? [directIp] : wtTargets();
  if (!targets.length) return;
  const msg = Object.assign({
    kind: "yt-sync",
    from: myNick(),
    at: Date.now(),
    seq: ++wtState.seq,
    video: wtState.videoId || "",
    title: wtState.title || "",
    by: wtState.by || "",
    vertical: !!wtState.vertical,
    watching: wtIsWatching(),
  }, payload || {});
  for (const ip of targets) invoke("sig_send", { peerIp: ip, payload: msg }).catch(() => {});
}

function wtSendState(directIp) {
  if (!wtHasSession() || !wtState.playerReady || !wtState.player) return;
  let pos = wtState.remotePos, paused = true, dur = 0;
  try {
    pos = wtState.player.getCurrentTime() || 0;
    dur = wtState.player.getDuration() || 0;
    paused = wtState.player.getPlayerState() !== 1;
    const vd = wtState.player.getVideoData ? wtState.player.getVideoData() : null;
    if (vd && vd.title && wtState.title === "Видео") wtState.title = vd.title;
  } catch {}
  // Реклама: не рассылаем позицию рекламного ролика как позицию видео.
  if (!directIp && dur > 0 && dur < 30 && wtState.contentDuration > 60) return;
  wtSend({ action: "state", pos, paused, watching: wtIsWatching(), dur }, directIp);
}

function wtMarkViewer(ip, watching) {
  if (!ip || !watching || ip === myIp) return;
  wtState.viewers[ip] = Date.now();
}
// IP смотрящих участников (без себя), устаревшие отметки выкидываются.
function wtViewerIps() {
  const now = Date.now();
  for (const [ip, ts] of Object.entries(wtState.viewers)) {
    if (now - ts > 12000 || ip === myIp) delete wtState.viewers[ip];
  }
  return Object.keys(wtState.viewers);
}
function wtViewerCount() { return (wtIsWatching() ? 1 : 0) + wtViewerIps().length; }

function wtStatus(text) {
  const el = wtQ("wt-now");
  if (el) el.textContent = text || "";
  if (wtState.nowTimer) clearTimeout(wtState.nowTimer);
  if (text) wtState.nowTimer = setTimeout(() => {
    const e2 = wtQ("wt-now");
    if (e2) e2.textContent = wtHasSession() ? "Идёт совместный просмотр" : "";
  }, 8000);
}
function wtLoadText(text) { const el = wtQ("wt-load-text"); if (el) el.textContent = text || ""; }
function wtShowLoader(on, text) {
  const l = wtQ("wt-loader"); if (!l) return;
  l.classList.toggle("hidden", !on);
  if (text) wtLoadText(text);
}
function wtShowError(text, retry = true) {
  wtShowLoader(false);
  const box = wtQ("wt-error"), t = wtQ("wt-error-text"), b = wtQ("wt-error-retry");
  if (t) t.textContent = text || "Ошибка плеера";
  if (b) b.classList.toggle("hidden", !retry);
  box?.classList.remove("hidden");
}
function wtHideError() { wtQ("wt-error")?.classList.add("hidden"); }
function wtShowEmpty(on) { wtQ("wt-empty")?.classList.toggle("hidden", !on); }
function wtHideMenu() { wtQ("wt-menu")?.classList.add("hidden"); }

function wtSyncEntryButtons() {
  const on = wtHasSession();
  for (const id of ["vb-watch", "cs-yt"]) wtQ(id)?.classList.toggle("on", on);
}
let wtViewersSig = "";
function wtRefreshViewers() {
  const el = wtQ("wt-viewers");
  if (!el) return;
  const ips = wtViewerIps();
  const me = wtIsWatching();
  const n = ips.length + (me ? 1 : 0);
  const sig = wtHasSession() && n > 0 ? (me ? "me," : "") + ips.join(",") : "";
  if (sig === wtViewersSig) return;
  wtViewersSig = sig;
  if (!sig) { el.innerHTML = ""; return; }
  const faces = [];
  if (me) faces.push(`<span class="ava" title="Ты"><span>${avaInner(profile.avatar, (myNick()[0] || "?").toUpperCase())}</span></span>`);
  for (const ip of ips.slice(0, me ? 3 : 4)) {
    const p = callPeerByIp(ip);
    const nm = p ? dispName(p) : ip;
    faces.push(`<span class="ava" title="${esc(nm)}"><span>${avaInner(p ? dispAva(p) : "", (nm[0] || "?").toUpperCase())}</span></span>`);
  }
  el.innerHTML = `<span class="wt-faces">${faces.join("")}</span><span>смотрят: ${n}</span>`;
}
function wtRefreshUI() {
  const info = wtSessionInfo();
  const title = wtQ("wt-title");
  if (title) title.textContent = info ? (info.title || "Видео") : "Смотреть вместе";
  const by = wtQ("wt-by");
  if (by) by.textContent = info && info.by ? "включил(а) " + info.by : "";
  wtSyncEntryButtons();
  wtRefreshViewers();
  const sig = [info ? info.id : "", info ? info.title : "", info ? info.by : ""].join("|");
  if (sig !== wtUiSig) {
    wtUiSig = sig;
    if (typeof inVoice === "function" && inVoice() && typeof renderCallStage === "function") renderCallStage();
  }
}

function wtTileHTML(info) {
  return `<img class="wt-thumb" src="https://i.ytimg.com/vi/${esc(info.id)}/mqdefault.jpg" alt="">` +
    `<b>Смотреть вместе</b><small>${esc(info.title || "Видео")}</small>` +
    (info.by ? `<small class="wt-tile-by">${esc(info.by)}</small>` : "") +
    `<button class="watch-btn">Смотреть</button>`;
}

// ---------- плеер ----------
function wtEnsureApi() {
  if (wtState.apiStarted) return;
  wtState.apiStarted = true;
  if (!document.querySelector('script[src*="youtube.com/iframe_api"]')) {
    const tag = document.createElement("script");
    tag.src = "https://www.youtube.com/iframe_api";
    tag.onerror = () => wtShowError("Не удалось загрузить YouTube. Проверьте подключение к интернету.", true);
    document.head.appendChild(tag);
  }
  wtState.apiPoll = setInterval(() => {
    if (window.YT && window.YT.Player) {
      clearInterval(wtState.apiPoll); wtState.apiPoll = null;
      wtCreatePlayer();
    }
  }, 250);
}
window.onYouTubeIframeAPIReady = function () { wtCreatePlayer(); };

function wtCreatePlayer() {
  if (wtState.player) { if (wtState.playerReady) wtPlayerReady(); return; }
  if (!(window.YT && window.YT.Player)) return;
  const root = wtQ("yt-player");
  if (!root) return;
  try {
    let saved = Number(localStorage.getItem(WT_VOL_KEY));
    wtState.vol = isFinite(saved) && saved >= 0 ? Math.min(100, Math.max(0, saved)) : 70;
    wtState.player = new window.YT.Player("yt-player", {
      host: "https://www.youtube-nocookie.com",
      width: "100%", height: "100%",
      playerVars: {
        enablejsapi: 1, rel: 0, modestbranding: 1, playsinline: 1,
        controls: 0, disablekb: 1, fs: 0, origin: location.origin,
      },
      events: { onReady: wtPlayerReady, onStateChange: wtOnPlayerState, onError: wtOnPlayerError },
    });
    if (wtState.readyTimer) clearTimeout(wtState.readyTimer);
    wtState.readyTimer = setTimeout(() => {
      if (!wtState.playerReady) wtShowError("Плеер YouTube не ответил за 10 секунд.", true);
    }, 10000);
  } catch (e) {
    wtShowError("Ошибка инициализации плеера: " + (e && e.message ? e.message : e), true);
  }
}

function wtPlayerReady() {
  if (wtState.playerReady) return;
  wtState.playerReady = true;
  if (wtState.readyTimer) { clearTimeout(wtState.readyTimer); wtState.readyTimer = null; }
  try { wtState.player.setVolume(wtState.vol || 70); } catch {}
  const volEl = wtQ("wt-vol");
  if (volEl) volEl.value = String(wtState.vol || 70);
  wtShowLoader(false); wtHideError(); wtShowEmpty(false);
  if (!wtState.uiTimer) wtState.uiTimer = setInterval(wtUpdateControls, 500);
  wtApplyPending();
  wtUpdateControls();
  wtPokeControls();
}

function wtApplyPending() {
  if (!wtState.playerReady || !wtState.player) return;
  const p = wtState.pending;
  wtState.pending = null;
  if (p && p.id) {
    try {
      wtState.applyingUntil = Date.now() + 900;
      wtState.justLoadedUntil = Date.now() + 1800;
      wtState.player.loadVideoById(p.id, Number(p.pos) || 0);
      if (p.paused) wtState.player.pauseVideo(); else wtState.player.playVideo();
    } catch {}
    return;
  }
  if (wtHasSession() && wtState.videoId && wtIsWatching()) {
    try {
      const vid = wtState.player.getVideoData ? wtState.player.getVideoData()?.video_id : "";
      if (vid !== wtState.videoId) {
        wtState.applyingUntil = Date.now() + 900;
        wtState.justLoadedUntil = Date.now() + 1800;
        wtState.player.loadVideoById(wtState.videoId, Number(wtState.remotePos) || 0);
        if (wtState.remotePaused) wtState.player.pauseVideo(); else wtState.player.playVideo();
      }
    } catch {}
  }
}

function wtOnPlayerState(e) {
  const st = e && e.data;
  if (st === 3) { if (wtIsWatching()) wtShowLoader(true, "Буферизация…"); }
  else if (st === 1 || st === 2 || st === 5) wtShowLoader(false);
  // видео пошло без движения мыши (команда участника, загрузка) — панель тоже прячется
  if (st === 1) wtPokeControls();
  wtUpdateControls();
  // Команды шлём только из наших кнопок: так реклама и служебные
  // переключения YouTube не рассылаются участникам как действия пользователя.
}

function wtOnPlayerError(e) {
  const code = e && e.data;
  const map = {
    2: "Неверная ссылка на видео.",
    5: "Ошибка HTML5-плеера.",
    100: "Видео недоступно или удалено.",
    101: "Автор запретил встраивание этого видео.",
    150: "Автор запретил встраивание этого видео.",
    153: "Автор запретил встраивание этого видео.",
  };
  wtShowError((map[code] || "Не удалось воспроизвести видео (ошибка " + code + ").") + " У остальных участников сессия продолжается.", true);
}

function wtUpdateControls() {
  const p = wtState.player;
  if (!p || !wtState.playerReady) return;
  let cur = 0, dur = 0, st = -1;
  try {
    cur = p.getCurrentTime() || 0; dur = p.getDuration() || 0; st = p.getPlayerState();
    if (dur > 30 && dur > (wtState.contentDuration || 0)) wtState.contentDuration = dur;
    if (dur > 30 && wtState.contentDuration === 0) wtState.contentDuration = dur;
    const vd = p.getVideoData ? p.getVideoData() : null;
    if (vd && vd.title && wtHasSession() && vd.title !== wtState.title) {
      wtState.title = vd.title;
      wtRefreshUI();
    }
  } catch {}
  const seek = wtQ("wt-seek");
  if (seek && !wtState.seeking && document.activeElement !== seek) {
    seek.max = String(Math.max(1, Math.round(dur || 1)));
    seek.value = String(Math.min(cur, dur || cur || 1));
  }
  const time = wtQ("wt-time");
  if (time) time.textContent = wtFmt(cur) + " / " + wtFmt(dur);
  const toggle = wtQ("wt-toggle");
  if (toggle) toggle.innerHTML = st === 1
    ? '<svg class="ic" viewBox="0 0 24 24"><path d="M6 5h4v14H6zM14 5h4v14h-4z"/></svg>'
    : '<svg class="ic" viewBox="0 0 24 24"><path d="M8 5v14l11-7z"/></svg>';
  wtSyncMuteIcon();
}

function wtSyncMuteIcon() {
  const b = wtQ("wt-mute");
  if (!b) return;
  let muted = false, vol = wtState.vol;
  try { muted = !!wtState.player?.isMuted?.(); vol = wtState.player?.getVolume?.() ?? vol; } catch {}
  b.innerHTML = (muted || vol === 0)
    ? '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><line x1="23" y1="9" x2="17" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><line x1="17" y1="9" x2="23" y2="15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>'
    : '<svg class="ic" viewBox="0 0 24 24"><path d="M3,9v6h4l5,5V4L7,9Z"/><path d="M16.5,12a4.5,4.5,0,0,0-2.5-4v8A4.5,4.5,0,0,0,16.5,12Z"/></svg>';
}

function wtTogglePlay() {
  const p = wtState.player;
  if (!p || !wtState.playerReady) return;
  let playing = false, pos = 0;
  try { playing = p.getPlayerState() === 1; pos = p.getCurrentTime() || 0; } catch {}
  try { if (playing) p.pauseVideo(); else p.playVideo(); } catch {}
  if (wtHasSession()) {
    wtSend({ action: playing ? "pause" : "play", pos, paused: playing });
    wtStatus(myNick() + (playing ? ": поставил на паузу" : ": включил"));
  }
  setTimeout(wtUpdateControls, 120);
}

function wtSeekBy(delta) {
  const p = wtState.player;
  if (!p || !wtState.playerReady) return;
  let cur = 0, dur = 0;
  try { cur = p.getCurrentTime() || 0; dur = p.getDuration() || 0; } catch {}
  const pos = Math.max(0, Math.min(dur || Infinity, cur + delta));
  try { wtState.applyingUntil = Date.now() + 500; p.seekTo(pos, true); } catch {}
  wtSend({ action: "seek", pos, paused: p.getPlayerState() !== 1 });
  wtStatus(myNick() + ": перемотал на " + wtFmt(pos));
  wtUpdateControls();
}

// ---------- применение удалённых команд ----------
function wtAccept(p) {
  const from = p.ip || p.from || "";
  const seq = Number(p.seq || 0);
  if (seq > 0) {
    if (seq <= (wtState.seen[from] || 0)) return false;
    wtState.seen[from] = seq;
  }
  // Время сравниваем только с пакетами того же отправителя: часы на разных
  // ПК расходятся, общий порог отбрасывал бы всё от участника с отстающими часами.
  const at = Number(p.at || 0), last = wtState.lastAt[from] || 0;
  if (at > 0 && last && at < last - 2500) return false;
  if (at > last) wtState.lastAt[from] = at;
  return true;
}

function wtApplyState(p) {
  wtAdopt(p);
  // Поправка на время в пути. Считается по чужим часам, поэтому не больше 1 с:
  // иначе расхождение часов на несколько секунд давало бы вечную перемотку.
  const lag = p.paused ? 0 : Math.min(1, Math.max(0, (Date.now() - Number(p.at || Date.now())) / 1000));
  const expected = Number(p.pos || 0) + lag;
  wtState.remotePos = expected;
  wtState.remotePaused = !!p.paused;
  if (!wtIsWatching() || !wtState.playerReady || !wtState.player) { wtRefreshUI(); return; }
  try {
    const vid = wtState.player.getVideoData ? wtState.player.getVideoData()?.video_id : "";
    const isNewVideo = !!p.video && p.video !== vid;
    const joining = Date.now() < (wtState.joinedUntil || 0);
    if (isNewVideo || joining) {
      wtState.applyingUntil = Date.now() + 1100;
      wtState.justLoadedUntil = Date.now() + 1900;
      if (isNewVideo) wtState.player.loadVideoById(p.video, expected);
      if (p.paused) wtState.player.pauseVideo(); else wtState.player.playVideo();
    } else {
      // Периодическая коррекция: только позиция, без команд паузы/плея,
      // и без прыжков на десятки секунд (так реклама одного участника
      // не перематывает видео у остальных).
      const cur = wtState.player.getCurrentTime() || 0;
      const diff = Math.abs(expected - cur);
      if (diff > 1.5 && diff < 15) {
        wtState.applyingUntil = Date.now() + 700;
        wtState.player.seekTo(expected, true);
      }
    }
  } catch {}
  wtShowLoader(false);
  wtUpdateControls();
  wtRefreshUI();
}

function wtApplyCommand(p) {
  if (!p.video && p.action !== "stop") return;
  if (p.video) wtAdopt(p);
  if (!wtIsWatching() || !wtState.playerReady || !wtState.player) { wtRefreshUI(); return; }
  const who = wtNick(p.ip || "", p.from);
  const pos = Number(p.pos || 0);
  try {
    if (p.action === "load") {
      wtState.applyingUntil = Date.now() + 1100;
      wtState.justLoadedUntil = Date.now() + 1900;
      wtState.player.loadVideoById(p.video, pos);
      if (p.paused) wtState.player.pauseVideo(); else wtState.player.playVideo();
      wtStatus(who + ": включил видео");
      wtShowLoader(false);
    } else if (p.action === "seek") {
      wtState.applyingUntil = Date.now() + 700;
      wtState.player.seekTo(pos, true);
      wtStatus(who + ": перемотал на " + wtFmt(pos));
    } else if (p.action === "play" || p.action === "pause") {
      const cur = wtState.player.getCurrentTime() || 0;
      if (Math.abs(pos - cur) > 1.5) {
        wtState.applyingUntil = Date.now() + 700;
        wtState.player.seekTo(pos, true);
      }
      wtState.applyingUntil = Date.now() + 700;
      if (p.action === "play") wtState.player.playVideo(); else wtState.player.pauseVideo();
      wtStatus(who + (p.action === "play" ? ": включил" : ": поставил на паузу"));
    }
  } catch {}
  wtUpdateControls();
  wtRefreshUI();
}

function wtRemoteStop() {
  try { wtState.player?.stopVideo(); } catch {}
  wtCloseMe(true);
  wtClearSession();
  wtStatus("Сессия просмотра остановлена");
  toast("Смотреть вместе: сессия остановлена");
}

function wtStopAll() {
  askConfirm("Остановить просмотр для всех?", "Плеер закроется у всех участников звонка.", "Остановить").then((ok) => {
    if (!ok) return;
    wtSend({ action: "stop" });
    try { wtState.player?.stopVideo(); } catch {}
    wtCloseMe(true);
    wtClearSession();
    toast("Смотреть вместе остановлено");
  });
}

// ---------- открытие / сворачивание ----------
function wtOpenChange() {
  const row = wtQ("wt-change-row");
  if (!row) return;
  row.classList.remove("hidden");
  const input = wtQ("wt-url");
  if (input) {
    if (!input.value && wtState.videoId) input.value = "https://youtu.be/" + wtState.videoId;
    setTimeout(() => input.focus(), 30);
  }
}
function wtCloseChange() { wtQ("wt-change-row")?.classList.add("hidden"); }

function wtLoadVideo(url) {
  const pr = parseYT(url);
  if (!pr.id) {
    wtStatus("Не ссылка на YouTube. Пример: youtu.be/… или youtube.com/watch?v=…");
    toast("Не ссылка на YouTube", true);
    return;
  }
  wtSetSession(pr.id, "Видео YouTube", myNick(), !!pr.vertical);
  wtState.remotePos = pr.start || 0;
  wtState.remotePaused = false;
  wtHideError(); wtShowEmpty(false); wtShowLoader(true, "Загрузка видео…");
  if (wtState.playerReady && wtState.player) {
    try {
      wtState.applyingUntil = Date.now() + 1100;
      wtState.justLoadedUntil = Date.now() + 1900;
      wtState.player.loadVideoById(pr.id, pr.start || 0);
      wtState.player.playVideo();
    } catch {}
  } else {
    wtState.pending = { id: pr.id, pos: pr.start || 0, paused: false };
    wtEnsureApi();
  }
  wtSend({ action: "load", video: pr.id, pos: pr.start || 0, paused: false });
  wtStatus(myNick() + ": включил видео");
  wtCloseChange();
  wtUpdateControls();
}

function wtSubmitChange() {
  const input = wtQ("wt-url");
  wtLoadVideo(input ? input.value.trim() : "");
}

function wtOpen() {
  if (typeof inVoice === "function" && !inVoice()) return toast("Смотреть вместе доступно во время звонка", true);
  const root = wtQ("watch-together");
  if (!root) return;
  root.classList.remove("hidden", "wt-min", "wt-fullscreen");
  root.classList.toggle("wt-vert", !!wtState.vertical);
  wtState.open = true; wtState.mini = false; wtState.joining = true;
  wtState.joinedUntil = Date.now() + 6000;
  wtSyncMiniButton();
  wtHideMenu(); wtCloseChange();
  if (wtState.playerReady) wtShowLoader(false); else wtShowLoader(true, "Загрузка плеера…");
  wtShowEmpty(false); wtHideError();
  wtStatus("");
  wtRefreshUI();
  wtEnsureApi();

  if (wtHasSession()) {
    if (wtState.playerReady && wtState.player) {
      try {
        const vid = wtState.player.getVideoData ? wtState.player.getVideoData()?.video_id : "";
        if (vid !== wtState.videoId) {
          wtState.applyingUntil = Date.now() + 1000;
          wtState.justLoadedUntil = Date.now() + 1800;
          wtState.player.loadVideoById(wtState.videoId, Number(wtState.remotePos) || 0);
          if (wtState.remotePaused) wtState.player.pauseVideo(); else wtState.player.playVideo();
        }
      } catch {}
      wtSend({ action: "state-request" });
      wtState.joining = false;
    } else {
      wtState.pending = { id: wtState.videoId, pos: wtState.remotePos || 0, paused: wtState.remotePaused !== false };
    }
  } else {
    wtState.joining = false;
    wtSend({ action: "state-request" });
  }

  setTimeout(() => {
    wtState.joining = false;
    if (!wtState.open) return;
    if (!wtHasSession()) { wtShowLoader(false); wtShowEmpty(true); }
    else wtShowLoader(false);
    wtUpdateControls();
    wtPokeControls();
  }, 1600);
}

function wtCloseMe(silent) {
  const root = wtQ("watch-together");
  root?.classList.add("hidden");
  root?.classList.remove("wt-min", "wt-fullscreen");
  root?.style.removeProperty("left"); root?.style.removeProperty("top");
  root?.style.removeProperty("right"); root?.style.removeProperty("bottom");
  root?.style.removeProperty("width");
  wtState.open = false; wtState.mini = false;
  try { wtState.player?.pauseVideo(); } catch {}
  wtHideMenu(); wtCloseChange(); wtHideError(); wtShowEmpty(false);
  wtShowLoader(true, "Загрузка плеера…");
  if (!silent) wtStatus(wtHasSession() ? "Вы не смотрите. Вернуться можно по плитке в звонке." : "");
  wtSyncMiniButton();
  wtSyncEntryButtons();
  wtRefreshUI();
}

function wtSyncMiniButton() {
  const b = wtQ("wt-min");
  if (!b) return;
  b.innerHTML = wtState.mini
    ? '<svg class="ic" viewBox="0 0 24 24"><path d="M5,5H11V7H7V11H5Z"/><path d="M13,17h4V13h2v6H13Z"/></svg>'
    : '<svg class="ic" viewBox="0 0 24 24"><path d="M5 12h14"/></svg>';
  b.title = wtState.mini ? "Развернуть" : "Свернуть в мини-окно";
}

function wtSetMini(on) {
  const root = wtQ("watch-together");
  if (!root) return;
  if (on) {
    root.classList.remove("hidden", "wt-fullscreen");
    root.classList.add("wt-min");
    wtState.mini = true; wtState.open = true;
    wtRestoreMiniPos();
    wtHideMenu(); wtCloseChange();
  } else {
    root.classList.remove("wt-min", "wt-fullscreen");
    wtState.mini = false; wtState.open = true;
    for (const k of ["left", "top", "right", "bottom", "width"]) root.style.removeProperty(k);
    wtUpdateControls();
    wtPokeControls();
  }
  wtSyncMiniButton();
  wtRefreshUI();
}

function wtToggleMini() { wtSetMini(!wtState.mini); }

function wtToggleFull() {
  const root = wtQ("watch-together");
  if (!root) return;
  const on = !root.classList.contains("wt-fullscreen");
  root.classList.toggle("wt-fullscreen", on);
  if (on) { root.classList.remove("wt-min"); wtState.mini = false; wtState.open = true; }
  wtUpdateControls();
}

function wtPokeControls() {
  const st = wtQ("wt-stage");
  if (!st) return;
  st.classList.remove("idle");
  if (wtState._idleTimer) clearTimeout(wtState._idleTimer);
  wtState._idleTimer = setTimeout(() => {
    if (wtState.mini) return;
    let playing = false;
    try { playing = wtState.player?.getPlayerState?.() === 1; } catch {}
    if (playing) st.classList.add("idle");
  }, 2500);
}

function wtRetryPlayer() {
  wtHideError();
  wtShowLoader(true, "Перезапуск плеера…");
  if (wtState.player && wtState.playerReady) { wtApplyPending(); return; }
  try { wtState.player?.destroy?.(); } catch {}
  wtState.player = null; wtState.playerReady = false;
  if (wtState.apiPoll) { clearInterval(wtState.apiPoll); wtState.apiPoll = null; }
  wtState.apiStarted = false;
  wtEnsureApi();
}

function wtEndCall() {
  try { wtState.player?.stopVideo(); } catch {}
  wtCloseMe(true);
  wtClearSession();
  wtState.seen = {}; wtState.lastAt = {};
}

// ---------- мини-окно: перетаскивание и размер ----------
function wtBindDrag(el, handleSel, key, enabled) {
  const handle = el?.querySelector(handleSel);
  if (!handle || handle.dataset.wtDrag) return;
  handle.dataset.wtDrag = "1";
  let start = null;
  handle.addEventListener("pointerdown", (e) => {
    if (enabled && !enabled()) return;
    if (e.button !== 0 || e.target.closest("button, input, a")) return;
    const r = el.getBoundingClientRect();
    start = { x: e.clientX, y: e.clientY, left: r.left, top: r.top };
    try { handle.setPointerCapture(e.pointerId); } catch {}
    e.preventDefault();
  });
  handle.addEventListener("pointermove", (e) => {
    if (!start) return;
    const r = el.getBoundingClientRect();
    let left = start.left + (e.clientX - start.x);
    let top = start.top + (e.clientY - start.y);
    left = Math.max(4, Math.min(left, innerWidth - r.width - 4));
    top = Math.max(4, Math.min(top, innerHeight - r.height - 4));
    el.style.left = left + "px"; el.style.top = top + "px";
    el.style.right = "auto"; el.style.bottom = "auto";
  });
  const stop = () => {
    if (!start) return;
    start = null;
    try { localStorage.setItem(key, JSON.stringify({ left: el.style.left, top: el.style.top, width: el.style.width })); } catch {}
  };
  handle.addEventListener("pointerup", stop);
  handle.addEventListener("pointercancel", stop);
}

function wtBindResize(el, handleSel, key, enabled, min = 240) {
  const handle = el?.querySelector(handleSel);
  if (!handle || handle.dataset.wtResize) return;
  handle.dataset.wtResize = "1";
  let start = null;
  handle.addEventListener("pointerdown", (e) => {
    if (enabled && !enabled()) return;
    const r = el.getBoundingClientRect();
    start = { x: e.clientX, w: r.width };
    try { handle.setPointerCapture(e.pointerId); } catch {}
    e.preventDefault(); e.stopPropagation();
  });
  handle.addEventListener("pointermove", (e) => {
    if (!start) return;
    const w = Math.max(min, Math.min(innerWidth - 20, start.w + (e.clientX - start.x)));
    el.style.width = w + "px";
  });
  const stop = () => {
    if (!start) return;
    start = null;
    try { localStorage.setItem(key, JSON.stringify({ left: el.style.left, top: el.style.top, width: el.style.width })); } catch {}
  };
  handle.addEventListener("pointerup", stop);
  handle.addEventListener("pointercancel", stop);
}

function wtRestoreMiniPos() {
  const root = wtQ("watch-together");
  if (!root) return;
  try {
    const saved = JSON.parse(localStorage.getItem(WT_POS_KEY) || "null");
    if (!saved) return;
    const savedW = parseFloat(saved.width);
    if (savedW > 0) root.style.width = Math.min(savedW, innerWidth - 20) + "px";
    if (saved.left) root.style.left = saved.left;
    if (saved.top) root.style.top = saved.top;
    if (saved.left || saved.top) { root.style.right = "auto"; root.style.bottom = "auto"; }
    requestAnimationFrame(() => {
      const rr = root.getBoundingClientRect();
      let l = parseFloat(root.style.left || "NaN");
      let t = parseFloat(root.style.top || "NaN");
      if (!isFinite(l)) l = rr.left;
      if (!isFinite(t)) t = rr.top;
      l = Math.max(4, Math.min(l, innerWidth - rr.width - 4));
      t = Math.max(4, Math.min(t, innerHeight - rr.height - 4));
      root.style.left = l + "px"; root.style.top = t + "px";
    });
  } catch {}
}

// ---------- синхронизация ----------
function wtStartSync() {
  if (wtState.syncTimer) return;
  wtState.syncTimer = setInterval(() => {
    if (!wtHasSession() || !wtIsWatching()) { wtRefreshViewers(); return; }
    wtSendState();
    wtRefreshViewers();
  }, 4000);
}

if (typeof listen === "function") {
  listen("safedrop-watch", (e) => {
    try {
      const t = e.payload || {}, p = t.payload || {};
      if (p.kind !== "yt-sync") return;
      const srcIp = t.ip || p.ip || "";
      if (srcIp && myIp && srcIp === myIp) return;
      if (srcIp) p.ip = srcIp;
      if (p.action === "state-request") {
        wtMarkViewer(srcIp, !!p.watching);
        if (wtHasSession() && wtIsWatching() && !wtState.joining) wtSendState(srcIp);
        return;
      }
      if (p.action === "stop") {
        if (wtAccept(p)) wtRemoteStop();
        return;
      }
      if (!wtAccept(p)) return;
      if (p.action === "state") {
        // Состояние от участника, который сам не смотрит, не применяем:
        // он мог закрыть окно и поставить видео на паузу только у себя.
        if (p.watching === false) return;
        wtMarkViewer(srcIp, !!p.watching);
        wtApplyState(p);
        return;
      }
      if (p.watching) wtMarkViewer(srcIp, true);
      wtApplyCommand(p);
    } catch (err) { console.error("watch listener", err); }
  }).catch((err) => console.error("watch listen", err));
}

// ---------- обработчики UI ----------
wtOn("vb-watch", "click", () => wtOpen());
wtOn("cs-yt", "click", () => wtOpen());
wtOn("wt-min", "click", () => wtToggleMini());
wtOn("wt-min2", "click", () => wtToggleMini());
wtOn("wt-close", "click", () => wtCloseMe());
wtOn("wt-close-me", "click", () => { wtHideMenu(); wtCloseMe(); });
wtOn("wt-stop-all", "click", () => { wtHideMenu(); wtStopAll(); });
wtOn("wt-more", "click", (e) => { e.stopPropagation(); wtQ("wt-menu")?.classList.toggle("hidden"); });
wtOn("wt-change", "click", () => wtOpenChange());
wtOn("wt-empty-pick", "click", () => wtOpenChange());
wtOn("wt-change-cancel", "click", () => wtCloseChange());
wtOn("wt-load", "click", () => wtSubmitChange());
wtOn("wt-url", "keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); wtSubmitChange(); } });
wtOn("wt-toggle", "click", () => wtTogglePlay());
wtOn("wt-back10", "click", () => wtSeekBy(-10));
wtOn("wt-fwd10", "click", () => wtSeekBy(10));
wtOn("wt-fs", "click", () => wtToggleFull());
wtOn("wt-retry", "click", () => wtRetryPlayer());
wtOn("wt-error-retry", "click", () => wtRetryPlayer());
wtOn("wt-mute", "click", () => {
  const p = wtState.player;
  if (!p) return;
  try { if (p.isMuted()) p.unMute(); else p.mute(); } catch {}
  wtSyncMuteIcon();
});
wtQ("watch-together")?.addEventListener("pointerdown", (e) => {
  if (e.target !== e.currentTarget || wtState.mini) return;
  if (wtHasSession()) wtSetMini(true); else wtCloseMe();
});
document.addEventListener("keydown", (e) => {
  if (e.key !== "Escape") return;
  const root = wtQ("watch-together");
  if (!root || root.classList.contains("hidden")) return;
  if (!wtState.mini) {
    e.preventDefault(); e.stopPropagation();
    if (root.classList.contains("wt-fullscreen")) wtToggleFull();
    else wtSetMini(true);
  }
}, true);
wtQ("wt-stage")?.addEventListener("pointermove", () => wtPokeControls());
wtQ("wt-stage")?.addEventListener("pointerdown", () => wtPokeControls());

// Щит поверх iframe: без него клик по видео ставил паузу только у себя
// (YouTube не сообщает об этом нашим кнопкам), а движение мыши над видео
// уходило в iframe и панель управления не появлялась.
// Большое окно: клик — плей/пауза, двойной клик — весь экран.
// Мини-окно: перетаскивание, двойной клик — развернуть.
const wtShield = wtQ("wt-shield");
if (wtShield) {
  let clickTimer = null;
  wtShield.addEventListener("click", () => {
    if (wtState.mini || clickTimer) return;
    clickTimer = setTimeout(() => { clickTimer = null; wtTogglePlay(); }, 230);
  });
  wtShield.addEventListener("dblclick", () => {
    if (clickTimer) { clearTimeout(clickTimer); clickTimer = null; }
    if (wtState.mini) wtSetMini(false); else wtToggleFull();
  });
}

const wtSeekEl = wtQ("wt-seek");
if (wtSeekEl) {
  const beginSeek = () => { wtState.seeking = true; };
  const endSeek = () => {
    if (!wtState.seeking) return;
    wtState.seeking = false;
    const pos = Number(wtSeekEl.value) || 0;
    try { wtState.applyingUntil = Date.now() + 600; wtState.player?.seekTo(pos, true); } catch {}
    wtSend({ action: "seek", pos, paused: wtState.player?.getPlayerState?.() !== 1 });
    wtStatus(myNick() + ": перемотал на " + wtFmt(pos));
    wtUpdateControls();
  };
  wtSeekEl.addEventListener("pointerdown", beginSeek);
  wtSeekEl.addEventListener("touchstart", beginSeek, { passive: true });
  wtSeekEl.addEventListener("input", () => {
    if (!wtState.seeking) return;
    const t = wtQ("wt-time");
    if (t) t.textContent = wtFmt(wtSeekEl.value) + " / " + wtFmt(wtState.player?.getDuration?.() || 0);
  });
  wtSeekEl.addEventListener("change", endSeek);
  wtSeekEl.addEventListener("pointerup", endSeek);
}

const wtVolEl = wtQ("wt-vol");
if (wtVolEl) {
  try {
    const saved = localStorage.getItem(WT_VOL_KEY);
    if (saved != null) wtVolEl.value = String(Math.min(100, Math.max(0, Number(saved) || 70)));
  } catch {}
  wtState.vol = Number(wtVolEl.value) || 70;
  wtVolEl.addEventListener("input", () => {
    wtState.vol = Number(wtVolEl.value) || 0;
    try { wtState.player?.setVolume(wtState.vol); } catch {}
    try { localStorage.setItem(WT_VOL_KEY, String(wtState.vol)); } catch {}
    wtSyncMuteIcon();
  });
}

wtBindDrag(wtQ("watch-together"), "#wt-head", WT_POS_KEY, () => wtState.mini);
wtBindDrag(wtQ("watch-together"), "#wt-shield", WT_POS_KEY, () => wtState.mini);
wtBindResize(wtQ("watch-together"), "#wt-resize", WT_POS_KEY, () => wtState.mini, 260);
wtBindDrag(wtQ("mini-watch"), ".mw-head", "sd.mini-watch", () => true);
wtBindResize(wtQ("mini-watch"), "#mini-watch-resize", "sd.mini-watch", () => true, 220);

wtSyncEntryButtons();
wtSyncMiniButton();
wtStartSync();
window.wtSessionInfo = wtSessionInfo;
window.__wtSend = wtSend;

// catch all JS errors so the user sees them instead of a blank window
window.addEventListener("error", (e) => { try { console.error("[SafeDrop] JS error:", e.message, e.filename, e.lineno); } catch {} });
window.addEventListener("unhandledrejection", (e) => { try { console.error("[SafeDrop] unhandled:", e && e.reason); } catch {} });
if (isDemo) {
  $("#demo-banner").classList.remove("hidden");
  if (new URLSearchParams(location.search).has("demo-call")) {
    setTimeout(() => {
      currentCall = "26.0.0.2";
      callScreenOpen = true;
      updateVoiceBar(); renderCallStage();
    }, 500);
  }
} else {
  console.log("[SafeDrop] движок подключён:", bridgeInfo());
}
try {
  await loadAll();
  if (!selectedIp && peers.length) { selectedIp = peers[0].ip; try { chats[selectedIp] = await invoke("get_chat", { peerIp: selectedIp }); chats[selectedIp].forEach((m) => seenIds.add(m.id)); } catch {} }
  renderAll(); renderChat(); refreshTransfers(); refreshMyVer();
  if (!isDemo && !peers.length) toast("Контактов нет — идёт автоскан сети Radmin…");
  if (!isDemo) {
    try {
      const pend = await invoke("get_pending_send");
      if (pend) handlePending(pend);
    } catch {}
    try {
      if (await invoke("take_mic_reset_flag")) toast("Запрет микрофона сброшен <svg class='ic' viewBox='0 0 24 24'><path d='M9,16.17,4.83,12l-1.42,1.41L9,19,21,7l-1.41-1.41Z'/></svg> Нажми «Запросить доступ»");
    } catch {}
  }
} catch (e) { toast("Ошибка запуска: " + e, true); }
// чистка зависших показов: соединение умерло — плитку долой, без чёрных экранов
const discCount = {};
function sweepStale() {
  const now = Date.now();
  // самозалечивание DOM: узлы без живых записей удаляем в любом случае
  const liveIds = new Set([...Object.keys(myShares), ...Object.keys(views)]);
  let orphans = 0;
  document.querySelectorAll("#stages > .stage").forEach((el) => {
    let known = false;
    for (const id of Object.keys(stageTiles)) {
      if (stageTiles[id] === el && liveIds.has(id)) { known = true; break; }
    }
    if (!known) { try { el.remove(); } catch {} orphans++; }
  });
  if (orphans) {
    for (const id of Object.keys(stageTiles)) if (!liveIds.has(id)) delete stageTiles[id];
    toast(`Убрано зависших плиток: ${orphans}`);
  }
  for (const [sid, v] of Object.entries(views)) {
    const st = v.pc?.connectionState;
    if (st === "failed" || st === "closed") {
      closeView(sid);
      toast("<svg class='ic' viewBox='0 0 24 24'><path d='M14,10V22H4a2,2,0,0,1-2-2V10Z'/><path d='M22,10V20a2,2,0,0,1-2,2H16V10Z'/><path d='M22,4V8H2V4A2,2,0,0,1,4,2H20A2,2,0,0,1,22,4Z'/></svg> Сигнал потерян — показ закрыт", true);
      continue;
    }
    // треки умерли, а сессия жива — тоже чёрный квадрат
    try {
      const tracks = v.stream ? [...v.stream.getTracks()] : [];
      if (tracks.length && tracks.every((t) => t.readyState === "ended")) {
        closeView(sid);
        toast("<svg class='ic' viewBox='0 0 24 24'><path d='M14,10V22H4a2,2,0,0,1-2-2V10Z'/><path d='M22,10V20a2,2,0,0,1-2,2H16V10Z'/><path d='M22,4V8H2V4A2,2,0,0,1,4,2H20A2,2,0,0,1,22,4Z'/></svg> Поток завершён — показ закрыт");
        continue;
      }
    } catch {}
    if (st === "disconnected") {
      discCount[sid] = (discCount[sid] || 0) + 1;
      if (discCount[sid] >= 8) {
        delete discCount[sid];
        closeView(sid);
        toast("<svg class='ic' viewBox='0 0 24 24'><path d='M14,10V22H4a2,2,0,0,1-2-2V10Z'/><path d='M22,10V20a2,2,0,0,1-2,2H16V10Z'/><path d='M22,4V8H2V4A2,2,0,0,1,4,2H20A2,2,0,0,1,22,4Z'/></svg> Нет данных 12+ сек — показ закрыт", true);
      }
      continue;
    }
    delete discCount[sid];
    // рука не пожата / треков нет 15+ сек — тоже чёрный квадрат, долой
    if (!v.firstTrackAt && now - (v.createdAt || now) > 15000) {
      closeView(sid);
      toast("<svg class='ic' viewBox='0 0 24 24'><path d='M14,10V22H4a2,2,0,0,1-2-2V10Z'/><path d='M22,10V20a2,2,0,0,1-2,2H16V10Z'/><path d='M22,4V8H2V4A2,2,0,0,1,4,2H20A2,2,0,0,1,22,4Z'/></svg> Поток не пришёл — показ закрыт", true);
      continue;
    }
    // соединение есть, треки есть, а картинки нет 25+ сек — зависший кадр
    const tile = stageTiles[sid];
    const vid = tile?.querySelector("video");
    if (watchId === sid && v.pc?.connectionState === "connected" && vid && vid.readyState < 2) {
      v.stallSince = v.stallSince || now;
      if (now - v.stallSince > 25000) {
        closeView(sid);
        toast("<svg class='ic' viewBox='0 0 24 24'><path d='M14,10V22H4a2,2,0,0,1-2-2V10Z'/><path d='M22,10V20a2,2,0,0,1-2,2H16V10Z'/><path d='M22,4V8H2V4A2,2,0,0,1,4,2H20A2,2,0,0,1,22,4Z'/></svg> Картинка не идёт 25+ сек — показ закрыт", true);
      }
    } else {
      v.stallSince = 0;
    }
  }
  for (const [sid, v] of Object.entries(views)) {
    const st = v.pc?.connectionState;
    if (st === "failed" || st === "closed") {
      closeView(sid);
      toast("<svg class='ic' viewBox='0 0 24 24'><path d='M14,10V22H4a2,2,0,0,1-2-2V10Z'/><path d='M22,10V20a2,2,0,0,1-2,2H16V10Z'/><path d='M22,4V8H2V4A2,2,0,0,1,4,2H20A2,2,0,0,1,22,4Z'/></svg> Сигнал потерян — показ закрыт", true);
    }
  }
  for (const [sid, s] of Object.entries(myShares)) {
    // свой источник умер (закрыли окно/экран мимо кнопки) — гасим показ
    try {
      const vt = s.stream ? s.stream.getVideoTracks() : [];
      if (vt.length && vt.every((t) => t.readyState === "ended")) {
        stopShare(sid);
        continue;
      }
    } catch {}
    let changed = false;
    const now2 = Date.now();
    for (const [ip, c] of Object.entries(s.pcs)) {
      const st = c.connectionState;
      const age = now2 - ((s.pcsAt && s.pcsAt[ip]) || now2);
      if (st === "failed" || st === "closed" || ((st === "new" || st === "connecting") && age > 30000)) {
        try { c.close(); } catch {}
        delete s.pcs[ip]; if (s.pcsAt) delete s.pcsAt[ip]; changed = true;
      }
    }
    if (!Object.keys(s.pcs).length) {
      stopShare(sid, false);
      toast("<svg class='ic' viewBox='0 0 24 24'><path d='M14,10V22H4a2,2,0,0,1-2-2V10Z'/><path d='M22,10V20a2,2,0,0,1-2,2H16V10Z'/><path d='M22,4V8H2V4A2,2,0,0,1,4,2H20A2,2,0,0,1,22,4Z'/></svg> Зрителей не осталось — показ остановлен");
    } else if (changed) renderStages();
  }
}
setInterval(() => { refreshTransfers(); sweepStale(); }, 1500);
setInterval(async () => { try { peers = await invoke("get_peers"); renderContacts(); renderConv(); renderPeerCard(); } catch {} }, 10000);

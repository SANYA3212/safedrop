// SafeDrop для Android: раскладка телефона поверх общего интерфейса (app.js).
// Работает только на Android (html.mobile ставит index.html). Экраны:
// вход → список чатов → чат → настройки / профиль контакта; системная
// кнопка «назад» закрывает текущий экран (через историю страницы).
// Файлы выбирает WebView (<input type=file>), а Rust получает их по частям
// (m_stage_chunk) — дальше они уходят так же, как на компьютере.
import { invoke, listen, overrideInvoke, isDemo } from "./tauri-shim.js";

const root = document.documentElement;
const $ = (s) => document.querySelector(s);
let app = null; // mobileApi из app.js, когда он загрузится
let detailsTitle = null; // заголовок панели настроек / профиля

if (root.classList.contains("mobile")) start().catch((e) => {
  console.error("SafeDrop mobile:", e);
  root.classList.add("m-ready");
});

function el(tag, props = {}, ...kids) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(props)) {
    if (k === "class") e.className = v;
    else if (k === "text") e.textContent = v;
    else if (k.startsWith("on")) e.addEventListener(k.slice(2), v);
    else e.setAttribute(k, v);
  }
  for (const k of kids) if (k) e.append(k);
  return e;
}
// иконки-затычки лежат в icons/m/<имя>.svg — заменить файл, и иконка сменится
function ic(name) {
  const i = el("i", { class: "m-ic", "aria-hidden": "true" });
  i.style.setProperty("--m-icon", `url(icons/m/${name}.svg)`);
  return i;
}
function iconBtn(name, title, onclick, id) {
  const b = el("button", { class: "m-icon-btn", title, "aria-label": title, onclick });
  if (id) b.id = id;
  b.append(ic(name));
  return b;
}

// ---------- короткие сообщения внизу экрана ----------
let noteTimer = 0;
function note(text, ms = 3500) {
  let n = $("#m-note");
  if (!n) { n = el("div", { id: "m-note", class: "hidden" }); document.body.append(n); }
  n.textContent = text;
  n.classList.remove("hidden");
  clearTimeout(noteTimer);
  if (ms) noteTimer = setTimeout(() => n.classList.add("hidden"), ms);
}

// ---------- меню снизу (как в Telegram) ----------
function sheet(title, items) {
  const ov = el("div", { class: "m-sheet-ov" });
  const box = el("div", { class: "m-sheet" });
  if (title) box.append(el("h4", { text: title }));
  const close = () => ov.remove();
  for (const it of items) {
    if (!it) continue;
    const b = el("button", { class: it.danger ? "danger" : "" }, ic(it.icon), el("span", { text: it.text }));
    b.onclick = () => { close(); it.run(); };
    box.append(b);
  }
  ov.onclick = (e) => { if (e.target === ov) close(); };
  ov.append(box);
  document.body.append(ov);
}

// ---------- экраны и кнопка «назад» ----------
const SCREENS = ["chat", "settings", "profile"];
let screen = "list";
function applyScreen(name) {
  const prev = screen;
  screen = name;
  for (const s of SCREENS) document.body.classList.toggle("m-" + s, s === name || (s === "chat" && name === "profile"));
  if (name === "list" && prev !== "list") {
    app?.closeChat();
    refreshPreviews();
  }
  if (name === "chat") document.body.classList.toggle("m-direct", !app?.isGroupOpen());
  if (detailsTitle) detailsTitle.textContent = name === "profile" ? "Профиль" : "Настройки";
}
function go(name) {
  if (name === screen) return;
  history.pushState({ m: name }, "");
  applyScreen(name);
}
// «назад» сначала закрывает то, что открыто поверх экрана: меню, окно, экран звонка
function closeTopmost() {
  if (scanStop) { scanStop(); return true; }
  const ov = document.querySelector(".m-sheet-ov");
  if (ov) { ov.remove(); return true; }
  const shown = (sel) => { const n = $(sel); return n && !n.classList.contains("hidden") ? n : null; };
  const closers = [
    ["#img-modal", () => $("#img-modal").classList.add("hidden")],
    ["#group-modal", () => $("#group-cancel").click()],
    ["#pick-modal", () => $("#pick-cancel").click()],
    ["#help-modal", () => $("#help-ok").click()],
    ["#search-modal", () => $("#search-modal").classList.add("hidden")],
    ["#call-panel", () => $("#cp-close").click()],
    ["#call-screen", () => $("#cs-chat").click()],
  ];
  for (const [sel, close] of closers) if (shown(sel)) { close(); return true; }
  return false;
}
window.addEventListener("popstate", (e) => {
  if (closeTopmost()) { history.pushState({ m: screen }, ""); return; }
  applyScreen(e.state?.m || "list");
});

// ---------- вход ----------
async function serverStatus() {
  try { return await invoke("server_status"); } catch { return null; }
}
function loggedIn(st) { return !!st && st.mode === "server" && !!st.addr; }

function buildLogin() {
  const box = el("div", { class: "m-login-box" });
  const addr = el("input", { id: "m-l-addr", placeholder: "IP сервера, например 203.0.113.5", autocomplete: "off", spellcheck: "false", inputmode: "url" });
  const pass = el("input", { id: "m-l-pass", type: "password", placeholder: "если сервер с паролем", autocomplete: "off" });
  const code = el("input", { id: "m-l-code", placeholder: "1234-567890", autocomplete: "off", spellcheck: "false", inputmode: "numeric", maxlength: "13" });
  const nick = el("input", { id: "m-l-nick", placeholder: "как тебя будут видеть друзья", maxlength: "32" });
  const state = el("div", { id: "m-l-state" });
  const loginBtn = el("button", { id: "m-l-login", text: "Войти в свой аккаунт" });
  const newBtn = el("button", { id: "m-l-new", class: "primary", text: "Нет ПК версии — новый аккаунт" });
  box.append(
    el("img", { class: "m-logo", src: "logo.png", alt: "" }),
    el("h1", { text: "SafeDrop" }),
    el("p", { text: "Чаты, файлы и звонки через твой сервер SafeDrop" }),
    // есть SafeDrop на компьютере — проще всего войти по QR-коду с его экрана
    el("button", { class: "primary m-qr-btn", id: "m-l-scan", onclick: () => scanQR((pair) => {
      addr.value = pair.addr; pass.value = pair.pass; code.value = pair.code;
      loginByCode(pair.addr, pair.pass, pair.code);
    }) }, ic("qr"), el("span", { text: "Сканировать QR-код" })),
    el("p", { class: "m-hint", text: "На компьютере: Настройки → Сервер → «Подключить телефон», и наведи камеру на QR-код. Или впиши всё вручную:" }),
    el("label", {}, el("span", { text: "Адрес сервера" }), addr),
    el("label", {}, el("span", { text: "Пароль сервера" }), pass),
    el("div", { class: "m-sep", text: "есть SafeDrop на компьютере" }),
    el("label", {}, el("span", { text: "Код с компьютера" }), code),
    loginBtn,
    el("div", { class: "m-sep", text: "или" }),
    el("label", {}, el("span", { text: "Ник" }), nick),
    newBtn,
    state,
  );
  const page = el("div", { id: "m-login" }, box);
  document.body.append(page);
  loginBtn.onclick = () => loginByCode(addr.value.trim(), pass.value, code.value.trim());
  code.onkeydown = (e) => { if (e.key === "Enter") loginBtn.click(); };
  newBtn.onclick = () => createAccount(addr.value.trim(), pass.value, nick.value.trim());
  addr.onkeydown = (e) => { if (e.key === "Enter") pass.focus(); };
  return page;
}
function say(text, bad = false, ok = false) {
  const s = $("#m-l-state");
  if (!s) return;
  s.textContent = text;
  s.className = bad ? "err" : ok ? "on" : "";
}
function waitServer(ms) {
  return new Promise((resolve) => {
    let done = false, un = null;
    const finish = (st) => { if (done) return; done = true; clearInterval(t); clearTimeout(to); un?.(); resolve(st); };
    const check = (st) => {
      if (!st) return;
      if (st.state === "online" && st.my_id) finish(st);
      // отказ сервера (пароль) и чужой ключ сами не пройдут — ответить сразу
      else if (st.state === "error" && (st.pending_fp || /отказал/i.test(st.error || ""))) finish(st);
    };
    listen("safedrop-server", (e) => check(e.payload)).then((u) => { if (done) u?.(); else un = u; }).catch(() => {});
    const t = setInterval(async () => check(await serverStatus()), 700);
    const to = setTimeout(async () => finish(await serverStatus()), ms);
  });
}
let creating = false;
// один аккаунт с компьютером: код с его экрана, ключ аккаунта приходит с него
async function loginByCode(addr, pass, code) {
  if (creating) return;
  if (!addr) { say("Впиши адрес сервера — тот же, что на компьютере", true); $("#m-l-addr").focus(); return; }
  if (!/^\d{4}[- ]?\d{3} ?\d{3}$/.test(code)) { say("Код — 10 цифр с экрана компьютера, например 1234-567890", true); $("#m-l-code").focus(); return; }
  creating = true;
  for (const id of ["#m-l-login", "#m-l-new", "#m-l-scan"]) { const b = $(id); if (b) b.disabled = true; }
  say("Вхожу… подтверждаю код с компьютером");
  try {
    await invoke("pair_join", { addr, pass, code });
    say("Готово! Переписка сейчас придёт с компьютера.", false, true);
    await askNotifications();
    setTimeout(() => location.reload(), 900);
  } catch (e) {
    say(String(e), true);
  } finally {
    creating = false;
    for (const id of ["#m-l-login", "#m-l-new", "#m-l-scan"]) { const b = $(id); if (b) b.disabled = false; }
  }
}
async function createAccount(addr, pass, nick) {
  if (creating) return;
  if (!addr) { say("Впиши адрес сервера", true); $("#m-l-addr").focus(); return; }
  creating = true;
  $("#m-l-new").disabled = true;
  say("Подключаюсь к серверу…");
  try {
    await invoke("server_configure", { mode: "server", addr, pass });
    let st = await waitServer(20000);
    if (st?.pending_fp) {
      // этот телефон уже знал другой ключ этого сервера
      const ok = confirm(`Сервер ${addr} показал другой ключ, чем раньше. Подтверждай, только если сервер переустанавливали.`);
      if (ok) { await invoke("server_trust_key", { fp: st.pending_fp }); st = await waitServer(15000); }
    }
    if (st?.state !== "online" || !st.my_id) {
      // неудача: остаться на экране входа и при следующем запуске
      await invoke("server_configure", { mode: "p2p", addr, pass }).catch(() => {});
      say(st?.error ? `Не вышло: ${st.error}` : "Сервер не ответил. Проверь адрес и что сервер запущен.", true);
      return;
    }
    if (nick) {
      const prof = await invoke("get_profile").catch(() => ({}));
      await invoke("save_profile", { nickname: nick, avatar: prof?.avatar || "" }).catch((e) => console.warn(e));
    }
    await askNotifications();
    showDone(st.my_id);
  } catch (e) {
    say(String(e), true);
  } finally {
    creating = false;
    const b = $("#m-l-new");
    if (b) b.disabled = false;
  }
}
function showDone(id) {
  const box = $("#m-login .m-login-box");
  box.replaceChildren(
    el("img", { class: "m-logo", src: "logo.png", alt: "" }),
    el("h1", { text: "Готово!" }),
    el("p", { text: "Твой код на сервере — по нему тебя узнают друзья. Его можно посмотреть потом в настройках." }),
    el("div", { class: "m-code", text: id }),
    el("button", { text: "Скопировать код", onclick: () => copy(id) }),
    el("button", { class: "primary", text: "Продолжить", onclick: () => location.reload() }),
  );
}
async function copy(text) {
  try { await navigator.clipboard.writeText(text); note("Скопировано"); } catch { note("Не удалось скопировать"); }
}
async function askNotifications() {
  if (isDemo) return;
  try {
    const granted = await invoke("plugin:notification|is_permission_granted");
    if (!granted) await invoke("plugin:notification|request_permission");
  } catch (e) { console.warn("notifications:", e); }
}

// ---------- звонки и сообщения, когда приложение закрыто ----------
// Пока выполнен вход, работает служба «SafeDrop на связи» (KeepAliveService):
// окно можно закрыть, а звонок придёт уведомлением на весь экран. Звонок,
// пришедший без страницы, она забирает при открытии (m_replay_call).
function answerWhenReady(ip) {
  let n = 0;
  const t = setInterval(() => {
    const cur = app?.pendingIp?.() || "";
    if (cur && (!ip || cur === ip)) { clearInterval(t); $("#cm-accept")?.click(); }
    else if (++n > 50) clearInterval(t);
  }, 150);
}
async function registerBackgroundCalls(st) {
  if (isDemo || !loggedIn(st)) return;
  invoke("m_keep_alive", { on: true }).catch((e) => console.warn("keep-alive:", e));
  listen("safedrop-answer-call", (e) => answerWhenReady(e.payload?.ip || "")).catch(() => {});
  // окно снова на экране, а звонок ещё звонит — теперь звонит страница
  const ring = () => { if (!document.hidden) app?.ringIfPending?.(); };
  document.addEventListener("visibilitychange", ring);
  window.addEventListener("focus", ring);
  try { if (await invoke("m_replay_call")) answerWhenReady(""); } catch {}
  ring();
}
function batteryCard() {
  const state = el("div", { class: "muted", text: "" });
  const btn = el("button", { text: "Разрешить работу в фоне", onclick: async () => {
    try { await invoke("m_battery", { open: true }); } catch (e) { note(String(e)); }
    setTimeout(fill, 1500);
  } });
  const card = el("div", { class: "card", id: "m-bg-card" },
    el("b", { text: "Звонки, когда приложение закрыто" }),
    el("div", { class: "muted", text: "SafeDrop остаётся на связи в фоне (уведомление «SafeDrop на связи»): звонок приходит, даже если окно закрыто или телефон заблокирован. Чтобы Android его не выключал, разреши работу без ограничений батареи. На Xiaomi ещё: Настройки → Приложения → SafeDrop → «Автозапуск» — включить, «Контроль активности» — «Нет ограничений»." }),
    state, btn,
  );
  async function fill() {
    if (isDemo) return;
    try {
      const free = await invoke("m_battery", { open: false });
      state.textContent = free ? "Батарея: без ограничений ✓" : "Батарея: Android может выключать SafeDrop в фоне";
      btn.classList.toggle("hidden", !!free);
    } catch {}
  }
  card.addEventListener("m-open", fill);
  return card;
}

// ---------- QR-код с компьютера: вход в его аккаунт ----------
// В QR: safedrop://pair?v=1&s=<адрес сервера>&p=<пароль сервера>&c=<код 1234-567890>
let scanStop = null;
function parsePairQR(text) {
  let u;
  try { u = new URL(String(text).trim()); } catch { return null; }
  if (u.protocol !== "safedrop:" || !/pair/.test(u.host + u.pathname)) return null;
  const q = u.searchParams;
  const s = (q.get("s") || "").trim(), p = q.get("p") || "", c = (q.get("c") || "").trim();
  if (!s || !/^\d{4}-?\d{6}$/.test(c)) return null;
  return { addr: s, pass: p, code: c };
}
function loadJsQR() {
  if (window.jsQR) return Promise.resolve(window.jsQR);
  return new Promise((resolve, reject) => {
    const s = el("script", { src: "vendor/jsQR.js" });
    s.onload = () => (window.jsQR ? resolve(window.jsQR) : reject(new Error("сканер не загрузился")));
    s.onerror = () => reject(new Error("сканер не загрузился"));
    document.head.append(s);
  });
}
async function qrDetector() {
  // встроенный в WebView распознаватель (быстрее); нет его — jsQR
  try {
    if ("BarcodeDetector" in window && (await BarcodeDetector.getSupportedFormats()).includes("qr_code")) {
      const bd = new BarcodeDetector({ formats: ["qr_code"] });
      return async (canvas) => (await bd.detect(canvas))[0]?.rawValue || "";
    }
  } catch {}
  const jsQR = await loadJsQR();
  return async (canvas, ctx) => {
    const img = ctx.getImageData(0, 0, canvas.width, canvas.height);
    return jsQR(img.data, img.width, img.height, { inversionAttempts: "attemptBoth" })?.data || "";
  };
}
// onPair — что делать с прочитанным кодом (по умолчанию — войти из настроек)
async function scanQR(onPair) {
  if (scanStop) return;
  const done = typeof onPair === "function" ? onPair : joinFromQR;
  let stream;
  try {
    stream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: { ideal: "environment" }, width: { ideal: 1280 }, height: { ideal: 720 } }, audio: false });
  } catch (e) { note("Нет доступа к камере: " + (e?.message || e)); return; }
  let detect;
  try { detect = await qrDetector(); } catch (e) { stream.getTracks().forEach((t) => t.stop()); note(String(e?.message || e)); return; }
  const video = el("video", { playsinline: "", muted: "", autoplay: "" });
  video.muted = true;
  video.srcObject = stream;
  const hint = el("div", { class: "m-scan-hint", text: "Наведи камеру на QR-код на экране компьютера" });
  const ov = el("div", { id: "m-scan" }, video, el("div", { class: "m-scan-frame" }), hint,
    el("button", { class: "m-scan-close", onclick: () => scanStop?.() }, ic("close")));
  document.body.append(ov);
  const canvas = document.createElement("canvas");
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  let alive = true, busy = false;
  const stop = () => {
    alive = false; scanStop = null;
    clearInterval(timer);
    stream.getTracks().forEach((t) => t.stop());
    ov.remove();
  };
  scanStop = stop;
  try { await video.play(); } catch {}
  const timer = setInterval(async () => {
    if (!alive || busy || !video.videoWidth) return;
    busy = true;
    try {
      const k = Math.min(1, 800 / Math.max(video.videoWidth, video.videoHeight));
      canvas.width = Math.round(video.videoWidth * k);
      canvas.height = Math.round(video.videoHeight * k);
      ctx.drawImage(video, 0, 0, canvas.width, canvas.height);
      const text = await detect(canvas, ctx);
      if (!alive || !text) return;
      const pair = parsePairQR(text);
      if (!pair) { hint.textContent = "Это не QR-код SafeDrop. Нужен код из «Подключить телефон» на компьютере"; return; }
      stop();
      await done(pair);
    } catch {} finally { busy = false; }
  }, 250);
}
async function joinFromQR({ addr, pass, code }) {
  if (!confirm(`Войти в аккаунт компьютера через сервер ${addr}?\n\nПереписка, которая сейчас на телефоне, заменится перепиской с компьютера.`)) return;
  note("Вхожу… подтверждаю код с компьютером", 60000);
  try {
    await invoke("pair_join", { addr, pass, code });
    note("Готово! Переписка сейчас придёт с компьютера.");
    setTimeout(() => location.reload(), 900);
  } catch (e) { note(String(e), 8000); }
}

// ---------- выбор файлов (WebView) → Rust по частям ----------
const CHUNK = 2 * 1024 * 1024;
function chooseFiles({ accept = "", multiple = false } = {}) {
  return new Promise((resolve) => {
    const inp = el("input", { type: "file" });
    if (accept) inp.accept = accept;
    inp.multiple = multiple;
    inp.style.display = "none";
    let done = false;
    const finish = (files) => { if (done) return; done = true; inp.remove(); resolve(files); };
    inp.addEventListener("change", () => finish([...inp.files]));
    inp.addEventListener("cancel", () => finish([]));
    document.body.append(inp);
    inp.click();
  });
}
function chunkB64(blob) {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result).split(",")[1] || "");
    r.onerror = () => reject(r.error);
    r.readAsDataURL(blob);
  });
}
function stageId() {
  try { return crypto.randomUUID().replace(/-/g, ""); }
  catch { return Array.from({ length: 32 }, () => "0123456789abcdef"[Math.floor(Math.random() * 16)]).join(""); }
}
async function stageFiles(files, keep = false) {
  const paths = [];
  for (const f of files) {
    const id = stageId();
    let path = "";
    if (!f.size) {
      path = await invoke("m_stage_chunk", { id, name: f.name, offset: 0, data: "", keep });
    }
    for (let off = 0; off < f.size; off += CHUNK) {
      const data = await chunkB64(f.slice(off, off + CHUNK));
      path = await invoke("m_stage_chunk", { id, name: f.name, offset: off, data, keep });
      if (f.size > 4 * CHUNK) note(`Готовлю «${f.name}»… ${Math.min(100, Math.round(((off + CHUNK) / f.size) * 100))}%`, 0);
    }
    if (f.size > 4 * CHUNK) note(`«${f.name}» готов к отправке`);
    paths.push(path);
  }
  return paths;
}
// скрепка: «Фото или видео» (галерея) или «Файл» — что выбрали, то и спросить у системы
let pickAccept = "";
function registerPickers() {
  if (isDemo) return;
  const one = async (accept, keep) => (await stageFiles(await chooseFiles({ accept }), keep))[0] || null;
  overrideInvoke("pick_files", async () => {
    const accept = pickAccept; pickAccept = "";
    return stageFiles(await chooseFiles({ accept, multiple: true }));
  });
  overrideInvoke("pick_image", () => one("image/*", false));
  overrideInvoke("pick_audio", () => one("audio/*", true));
  overrideInvoke("pick_bg_media", () => one("image/*,video/*", true));
  overrideInvoke("pick_folder", async () => {
    note("На телефоне полученные файлы всегда сохраняются в «Загрузки/SafeDrop»");
    return null;
  });
}

// ---------- список чатов ----------
let previews = {};
let previewTimer = 0;
async function refreshPreviews() {
  clearTimeout(previewTimer);
  previewTimer = setTimeout(async () => {
    if (isDemo) return;
    try { previews = (await invoke("m_chat_previews")) || {}; decorate(); } catch (e) { console.warn(e); }
  }, 250);
}
function previewText(p, isGroup) {
  if (!p) return "";
  let body = p.text || "";
  if (p.kind === "image") body = body ? "Фото · " + body : "Фото";
  else if (p.kind === "media") {
    const voice = /^safedrop-voice-/i.test(p.media || "");
    const video = !voice && /\.(webm|mp4|mov|m4v|mkv)$/i.test(p.media || "");
    const label = voice ? "Голосовое сообщение" : video ? "Видео" : "Аудио";
    body = body ? `${label} · ${body}` : label;
  }
  const who = p.me ? "Вы" : isGroup ? (p.nick || "") : "";
  return { who, body };
}
function timeText(ms) {
  if (!ms) return "";
  const d = new Date(ms), now = new Date();
  const two = (n) => String(n).padStart(2, "0");
  if (d.toDateString() === now.toDateString()) return `${two(d.getHours())}:${two(d.getMinutes())}`;
  return `${two(d.getDate())}.${two(d.getMonth() + 1)}`;
}
function setText(node, text) { if (node.textContent !== text) node.textContent = text; }
function decorate() {
  const rows = [...document.querySelectorAll("#m-list .contact")];
  const keyed = rows.map((row, i) => {
    const key = row.dataset.ip || (row.dataset.gid ? `g:${row.dataset.gid}` : "");
    return { row, key, i, at: previews[key]?.at || 0 };
  });
  // как в Telegram: сверху чаты с самыми свежими сообщениями
  const sorted = [...keyed].sort((a, b) => b.at - a.at || a.i - b.i);
  const empty = $("#m-empty");
  if (empty) empty.classList.toggle("hidden", rows.length > 0);
  sorted.forEach((k, n) => { if (k.row.style.order !== String(n)) k.row.style.order = String(n); });
  for (const { row, key } of keyed) {
    if (!key) continue;
    const isGroup = key.startsWith("g:");
    const p = previews[key];
    const pv = previewText(p, isGroup);
    row.classList.toggle("m-has-prev", !!p);
    let prev = row.querySelector(":scope > .m-prev");
    let time = row.querySelector(":scope > .m-time");
    if (p) {
      if (!prev) { prev = el("span", { class: "m-prev" }, el("i"), el("span")); row.append(prev); }
      if (!time) { time = el("span", { class: "m-time" }); row.append(time); }
      setText(prev.firstChild, pv.who ? pv.who + ": " : "");
      setText(prev.lastChild, pv.body);
      setText(time, timeText(p.at));
    } else { prev?.remove(); time?.remove(); }
    if (!isGroup) row.classList.toggle("m-on", !!app?.isOnline(key));
    if (isGroup) {
      const n = app?.unreadOf(key) || 0;
      let badge = row.querySelector(":scope > .m-badge");
      if (n) {
        if (!badge) { badge = el("span", { class: "m-badge" }); row.append(badge); }
        setText(badge, String(n));
      } else badge?.remove();
    }
  }
}
let decoratePending = false;
function watchList() {
  const list = $("#m-list");
  // app.js перерисовывает строки сам; дорисовываем своё после него. Второй
  // проход ничего не меняет (текст пишется, только если другой), и всё стихает.
  new MutationObserver(() => {
    if (decoratePending) return;
    decoratePending = true;
    requestAnimationFrame(() => { decoratePending = false; decorate(); });
  }).observe(list, { childList: true, subtree: true, characterData: true });
  // открыть чат: строку уже обработал app.js (selectPeer/selectGroup)
  list.addEventListener("click", (e) => { if (e.target.closest(".contact")) go("chat"); });
}

// ---------- сборка экранов ----------
function buildList() {
  const contacts = $("#contacts");
  const status = el("small", { text: "" });
  const bar = el("div", { class: "m-bar", id: "m-bar-list" },
    iconBtn("menu", "Настройки", () => openSettings()),
    el("div", { class: "m-title" }, el("b", { text: "SafeDrop" }), status),
    iconBtn("search", "Поиск", () => {
      const row = $("#m-search-row");
      row.classList.toggle("hidden");
      if (!row.classList.contains("hidden")) $("#peer-search").focus();
      else { $("#peer-search").value = ""; $("#peer-search").dispatchEvent(new Event("input")); }
    }),
  );
  const search = el("div", { id: "m-search-row", class: "hidden" });
  const list = el("div", { id: "m-list" });
  const empty = el("div", { id: "m-empty", class: "empty-hint" },
    el("b", { text: "Пока никого нет." }),
    el("br"),
    document.createTextNode("Все, кто подключён к этому серверу, появятся здесь сами. Дай друзьям адрес и пароль сервера."));
  const fab = el("button", { id: "m-fab", title: "Новая группа", "aria-label": "Новая группа" }, ic("pencil"));
  fab.onclick = () => sheet("Создать", [
    { icon: "group", text: "Новая группа", run: () => $("#btn-group-add").click() },
  ]);
  // app.js ещё загружается и ищет элементы по id: переносить их можно только
  // внутри страницы, поэтому сначала новые контейнеры встают в страницу
  contacts.prepend(bar, search, list);
  contacts.append(fab);
  const peerSearch = $("#peer-search");
  peerSearch.placeholder = "Поиск чатов";
  search.append(peerSearch);
  list.append($("#contact-list"), $("#group-list"), empty);
  const renderStatus = (st) => {
    const labels = { online: "в сети", connecting: "подключение…", error: "нет связи с сервером", off: "нет связи" };
    status.textContent = st ? (labels[st.state] || st.state || "") : "";
    status.className = st?.state === "online" ? "on" : st?.state === "error" ? "err" : "";
  };
  serverStatus().then(renderStatus);
  listen("safedrop-server", (e) => renderStatus(e.payload)).catch(() => {});
}
function buildChat() {
  const head = $("#conv-head");
  head.prepend(iconBtn("back", "Назад", () => history.back()));
  // нажатие на имя или аватарку — профиль контакта / настройки группы
  head.addEventListener("click", (e) => {
    if (!e.target.closest(".conv-meta, #conv-ava")) return;
    if (app?.isGroupOpen()) openGroupSettings(); else go("profile");
  });
  const more = iconBtn("more", "Ещё", () => chatMenu(), "m-chat-more");
  $(".conv-actions").append(more);
  // «Экраны» (чужие демонстрации) — только когда они есть
  const badge = $("#screens-badge");
  const syncScreens = () => document.body.classList.toggle("m-screens", !!badge.textContent.trim());
  new MutationObserver(syncScreens).observe(badge, { childList: true, characterData: true, subtree: true });
  syncScreens();
  // голосовой звонок: полоска над всеми экранами, а не только в чате
  const vb = $("#voice-bar");
  $("#app").insertBefore(vb, $("#layout"));
  // открыть полученный файл приложением телефона
  $("#transfers").addEventListener("click", (e) => {
    const card = e.target.closest(".tcard.m-openable");
    if (card && !e.target.closest("button")) openReceived(card.dataset.mfile);
  });
  new MutationObserver(markTransfers).observe($("#transfers"), { childList: true });
}
function chatMenu() {
  const group = app?.isGroupOpen();
  const filesOpen = !$("#files-pane").classList.contains("hidden");
  sheet("", [
    { icon: "file", text: "Отправить файл", run: () => $("#btn-sendfile").click() },
    { icon: "folder", text: filesOpen ? "Вернуться к сообщениям" : "Файлы и передачи", run: () => $("#btn-files-pane").click() },
    { icon: "search", text: "Поиск по сообщениям", run: () => $("#btn-msg-search").click() },
    group ? { icon: "group", text: "Изменить группу", run: openGroupSettings } : { icon: "user", text: "Профиль контакта", run: () => go("profile") },
    { icon: "trash", text: "Очистить чат", danger: true, run: () => $("#btn-clear-chat").click() },
  ]);
}
function openGroupSettings() { $("#rc-rename").click(); }
async function markTransfers() {
  const cards = [...document.querySelectorAll('#transfers .tcard[data-dir="down"][data-status="done"]:not(.m-openable)')];
  if (!cards.length) return;
  let dir = "";
  try { dir = await invoke("get_loaddrop"); } catch { return; }
  for (const card of cards) {
    card.dataset.mfile = dir.replace(/\/+$/, "") + "/" + card.dataset.file;
    card.classList.add("m-openable");
  }
}
async function openReceived(path) {
  try { await invoke("m_open_file", { path }); } catch (e) { note(String(e)); }
}
function buildSettings() {
  const det = $("#details");
  const title = el("b", { text: "Настройки" });
  const bar = el("div", { class: "m-bar", id: "m-bar-set" }, iconBtn("back", "Назад", () => history.back()), el("div", { class: "m-title" }, title));
  det.prepend(bar);
  // самое видное место: войти в аккаунт компьютера по QR-коду с его экрана
  const qr = el("div", { class: "card", id: "m-qr-card" },
    el("button", { class: "primary m-qr-btn", id: "m-qr-scan", onclick: scanQR }, ic("qr"), el("span", { text: "Сканировать QR-код" })),
    el("div", { class: "muted", text: "Один аккаунт с компьютером: на ПК — Настройки → Сервер → «Подключить телефон», и наведи камеру на QR-код." }),
  );
  det.insertBefore(qr, $(".settings-search"));
  const bg = batteryCard();
  det.insertBefore(bg, $(".settings-search"));
  det.addEventListener("m-open", () => bg.dispatchEvent(new Event("m-open")));
  // карточка аккаунта: ник, код на сервере, выход
  const card = el("div", { class: "card", id: "m-account" });
  det.insertBefore(card, $(".settings-search"));
  const fill = async () => {
    const [st, prof] = await Promise.all([serverStatus(), invoke("get_profile").catch(() => ({}))]);
    const id = st?.my_id || "";
    const info = el("div", { class: "grow" },
      el("b", { text: prof?.nickname || "Без ника" }),
      el("div", { class: "muted", text: st?.addr ? `Сервер: ${st.addr}` : "" }),
      id ? el("code", { text: id }) : el("div", { class: "muted", text: "Код появится, когда будет связь с сервером" }),
      el("div", { class: "row" },
        id ? el("button", { text: "Скопировать код", onclick: () => copy(id) }) : null,
        el("button", { class: "danger", text: "Выйти", onclick: logout }),
      ),
    );
    card.replaceChildren(info);
  };
  det.addEventListener("m-open", fill);
  detailsTitle = title;
}
function openSettings() {
  $("#details").dispatchEvent(new Event("m-open"));
  go("settings");
}
async function logout() {
  const st = await serverStatus();
  if (st?.linked) {
    // телефон вошёл в аккаунт компьютера: выход стирает с телефона ключ и переписку
    if (!confirm("Выйти из аккаунта на этом телефоне? Переписка с телефона удалится — на компьютере она останется. Войти снова можно по новому коду с компьютера.")) return;
    await invoke("m_keep_alive", { on: false }).catch(() => {});
    try { await invoke("factory_reset"); } catch (e) { note(String(e)); return; }
    location.reload();
    return;
  }
  if (!confirm("Выйти с сервера? Переписка останется на телефоне, при следующем входе на этот же сервер твой код не изменится.")) return;
  await invoke("m_keep_alive", { on: false }).catch(() => {});
  try { await invoke("server_configure", { mode: "p2p", addr: st?.addr || "", pass: st?.pass || "" }); } catch (e) { note(String(e)); return; }
  location.reload();
}
// чат открыли не из списка (поиск, звонок, уведомление) — показать его
function followSelection() {
  let last = "";
  const check = () => {
    const key = app?.openKey() || "";
    if (key && key !== last && screen === "list") go("chat");
    last = key;
  };
  new MutationObserver(check).observe($("#conv-name"), { childList: true, characterData: true, subtree: true });
}

// ---------- цвета пузырей из темы ----------
// На компьютере «Цвет своих / чужих сообщений» по умолчанию прозрачный (строки
// как в Discord). У пузырей свой цвет по умолчанию, а заданный в теме — главнее.
function alphaOf(color) {
  const probe = el("i");
  probe.style.backgroundColor = color;
  if (!probe.style.backgroundColor) return 0; // не цвет
  document.body.append(probe);
  const c = getComputedStyle(probe).backgroundColor;
  probe.remove();
  const m = c.match(/rgba\([^)]*,\s*([\d.]+)\s*\)/);
  return m ? parseFloat(m[1]) : 1;
}
function syncBubbleColors() {
  const cs = getComputedStyle(root);
  for (const [from, to] of [["--message-own", "--m-own"], ["--message-peer", "--m-peer"]]) {
    const v = cs.getPropertyValue(from).trim();
    if (v && alphaOf(v) > 0) { if (root.style.getPropertyValue(to) !== v) root.style.setProperty(to, v); }
    else if (root.style.getPropertyValue(to)) root.style.removeProperty(to);
  }
}
function watchTheme() {
  syncBubbleColors();
  // applyTheme (app.js) пишет переменные темы в style у <html>
  new MutationObserver(syncBubbleColors).observe(root, { attributes: true, attributeFilter: ["style"] });
}

// ---------- выбор цвета ----------
// Android WebView не открывает окно выбора цвета для <input type="color">:
// на телефоне — своя палитра. Значение уходит в поле как обычно (события
// input и change), дальше тему сохраняет app.js.
const PALETTE = [
  "#000000", "#141824", "#1a2138", "#0d1020", "#2b2d31", "#4b5563", "#9aa3b8", "#ffffff",
  "#5b8cff", "#3b82f6", "#06b6d4", "#14b8a6", "#22c55e", "#84cc16", "#eab308", "#f97316",
  "#ef4444", "#ec4899", "#d946ef", "#a855f7", "#7c3aed", "#9b59ff", "#6366f1", "#0ea5e9",
];
function hexToHsl(hex) {
  const n = parseInt(hex.slice(1), 16);
  const r = (n >> 16 & 255) / 255, g = (n >> 8 & 255) / 255, b = (n & 255) / 255;
  const max = Math.max(r, g, b), min = Math.min(r, g, b), l = (max + min) / 2;
  let h = 0, sat = 0;
  if (max !== min) {
    const d = max - min;
    sat = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    h = max === r ? (g - b) / d + (g < b ? 6 : 0) : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
    h *= 60;
  }
  return [Math.round(h), Math.round(sat * 100), Math.round(l * 100)];
}
function hslToHex(h, sat, l) {
  sat /= 100; l /= 100;
  const k = (n) => (n + h / 30) % 12, a = sat * Math.min(l, 1 - l);
  const f = (n) => Math.round(255 * (l - a * Math.max(-1, Math.min(k(n) - 3, 9 - k(n), 1))));
  return "#" + [f(0), f(8), f(4)].map((v) => v.toString(16).padStart(2, "0")).join("");
}
function colorPicker(input) {
  let value = /^#[0-9a-f]{6}$/i.test(input.value) ? input.value.toLowerCase() : "#5b8cff";
  const ov = el("div", { class: "m-sheet-ov" });
  const box = el("div", { class: "m-sheet m-color" });
  const title = input.closest("label")?.querySelector("span")?.textContent?.trim() || "Цвет";
  const preview = el("div", { class: "m-color-preview" });
  const hex = el("input", { class: "m-color-hex", maxlength: "7", spellcheck: "false", autocomplete: "off" });
  const sliders = ["Оттенок", "Насыщенность", "Яркость"].map((name, i) => {
    const r = el("input", { type: "range", min: "0", max: i ? "100" : "360" });
    return { r, row: el("label", { class: "m-color-row" }, el("span", { text: name }), r) };
  });
  const grid = el("div", { class: "m-color-grid" });
  const show = (v, from) => {
    value = v;
    preview.style.background = v;
    if (from !== "hex") hex.value = v;
    if (from !== "hsl") hexToHsl(v).forEach((n, i) => { sliders[i].r.value = String(n); });
    sliders[0].row.style.setProperty("--m-hue", sliders[0].r.value);
  };
  for (const c of PALETTE) {
    const b = el("button", { class: "m-swatch", title: c, "aria-label": c });
    b.style.background = c;
    b.onclick = () => show(c);
    grid.append(b);
  }
  sliders.forEach(({ r }) => { r.oninput = () => show(hslToHex(...sliders.map((x) => Number(x.r.value))), "hsl"); });
  hex.oninput = () => { const v = hex.value.trim(); if (/^#[0-9a-f]{6}$/i.test(v)) show(v.toLowerCase(), "hex"); };
  const close = () => ov.remove();
  const done = el("button", { class: "primary", text: "Готово", onclick: () => {
    close();
    if (input.value.toLowerCase() === value) return;
    input.value = value;
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new Event("change", { bubbles: true }));
  } });
  box.append(
    el("h4", { text: title }),
    el("div", { class: "m-color-top" }, preview, hex),
    grid,
    ...sliders.map((x) => x.row),
    el("div", { class: "row" }, el("button", { text: "Отмена", onclick: close }), done),
  );
  ov.onclick = (e) => { if (e.target === ov) close(); };
  ov.append(box);
  document.body.append(ov);
  show(value);
}
function registerColorPicker() {
  document.addEventListener("click", (e) => {
    const input = e.target.closest?.('input[type="color"]');
    if (!input || input.disabled) return;
    e.preventDefault();
    e.stopPropagation();
    colorPicker(input);
  }, true);
}

// ---------- голосовые как в Telegram ----------
// Держишь микрофон — идёт запись, отпустил — ушло. Пока держишь: влево —
// отмена, вверх — запись закрепляется (дальше «Отправить» или «Отмена»).
function registerAttachMenu() {
  const attach = $("#btn-attach");
  if (!attach) return;
  $("#composer").addEventListener("click", (e) => {
    if (!e.target.closest("#btn-attach")) return;
    e.stopPropagation(); e.preventDefault();
    sheet("Отправить", [
      { icon: "image", text: "Фото или видео", run: () => { pickAccept = "image/*,video/*"; attach.onclick?.(); } },
      { icon: "file", text: "Файл", run: () => { pickAccept = ""; attach.onclick?.(); } },
    ]);
  }, true);
}

// ---------- демонстрация экрана ----------
// Экран снимает Android (ScreenCaptureService.kt), кадры приходят событием
// safedrop-screen-frame (JPEG). Из них — видеодорожка, которую app.js шлёт как
// обычную демонстрацию. MediaStreamTrackGenerator пишет кадры без отрисовки
// на экране и работает, пока SafeDrop в фоне; холст — запасной путь.
const SCREEN_FPS = 12;
let screenSession = null;
function b64ToBlob(b64) {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return new Blob([bytes], { type: "image/jpeg" });
}
async function startPhoneScreen() {
  if (screenSession) return null;
  await invoke("m_screen_start", { fps: SCREEN_FPS, max: 1280 }); // отказ — исключение
  let track, draw;
  if (typeof window.MediaStreamTrackGenerator === "function" && typeof window.VideoFrame === "function") {
    const gen = new MediaStreamTrackGenerator({ kind: "video" });
    const writer = gen.writable.getWriter();
    const t0 = performance.now();
    track = gen;
    draw = async (bmp) => {
      const frame = new VideoFrame(bmp, { timestamp: Math.round((performance.now() - t0) * 1000) });
      try { await writer.write(frame); } finally { frame.close(); }
    };
  } else {
    const canvas = el("canvas");
    canvas.width = 720; canvas.height = 1280;
    const ctx = canvas.getContext("2d");
    track = canvas.captureStream(SCREEN_FPS).getVideoTracks()[0];
    draw = async (bmp) => {
      if (canvas.width !== bmp.width || canvas.height !== bmp.height) { canvas.width = bmp.width; canvas.height = bmp.height; }
      ctx.drawImage(bmp, 0, 0);
    };
  }
  const session = { alive: true, unlisten: null };
  screenSession = session;
  const stopNative = () => {
    if (!session.alive) return;
    session.alive = false;
    if (screenSession === session) screenSession = null;
    session.unlisten?.();
    invoke("m_screen_stop").catch(() => {});
  };
  // app.js заканчивает показ через track.stop() — тогда и Android перестаёт снимать
  const origStop = track.stop.bind(track);
  track.stop = () => { stopNative(); origStop(); };
  session.unlisten = await listen("safedrop-screen-frame", async (e) => {
    const f = e.payload;
    try {
      if (session.alive && f?.data) {
        const bmp = await createImageBitmap(b64ToBlob(f.data));
        try { await draw(bmp); } finally { bmp.close(); }
      }
    } catch (err) {
      console.warn("screen frame:", err);
    } finally {
      invoke("m_screen_ack").catch(() => {});
    }
  });
  invoke("m_screen_ack").catch(() => {});
  return new MediaStream([track]);
}
function registerScreenShare() {
  if (isDemo || !app?.setScreenSource) return;
  app.setScreenSource(startPhoneScreen);
  // показ остановили из уведомления Android или система его прервала
  listen("safedrop-screen-stopped", () => { if (screenSession) app.stopScreenShares(); }).catch(() => {});
}

// ---------- полоска звонка ----------
// Нажатие на полоску (не на кнопки) — вернуться на экран звонка, как в Telegram
function registerCallBar() {
  const vb = $("#voice-bar");
  if (!vb) return;
  vb.addEventListener("click", (e) => { if (!e.target.closest("button")) $("#vb-open")?.click(); });
}

function registerVoiceGesture() {
  const btn = $("#btn-record-audio");
  if (!btn) return;
  const bar = el("div", { id: "m-rec", class: "hidden" });
  const timer = el("b", { text: "0:00" });
  const hint = el("span", { class: "m-rec-hint", text: "‹ влево — отмена" });
  const cancelBtn = el("button", { class: "m-rec-cancel", text: "Отмена" });
  const sendBtn = el("button", { class: "m-rec-send primary", text: "Отправить" });
  const lock = el("div", { id: "m-rec-lock", class: "hidden" }, ic("lock"), el("span", { text: "▲" }));
  bar.append(el("i", { class: "m-rec-dot" }), timer, hint, cancelBtn, sendBtn);
  $("#composer").append(bar, lock);
  // клик после касания app.js не должен принять за «начать / остановить»
  $("#composer").addEventListener("click", (e) => {
    if (e.target.closest("#btn-record-audio")) { e.stopPropagation(); e.preventDefault(); }
  }, true);
  btn.addEventListener("contextmenu", (e) => e.preventDefault());
  let st = null; // { x, y, t0, locked, cancelled, timerId, pointerId }
  const fmt = (ms) => { const sec = Math.floor(ms / 1000); return `${Math.floor(sec / 60)}:${String(sec % 60).padStart(2, "0")}`; };
  const ui = (on) => {
    bar.classList.toggle("hidden", !on);
    lock.classList.toggle("hidden", !on || !!st?.locked);
    $("#composer").classList.toggle("m-recording", on);
    bar.classList.toggle("locked", !!st?.locked);
  };
  const finish = async (send) => {
    const s = st; st = null;
    if (!s) return;
    clearInterval(s.timerId);
    ui(false);
    if (!send || Date.now() - s.t0 < 600) {
      app?.voiceStop(true);
      if (send) note("Удерживай кнопку, чтобы записать голосовое");
      return;
    }
    if (!app?.voiceRecording()) { app?.voiceStop(true); return; }
    const pending = $("#pending-audio");
    app.voiceStop(false);
    // запись сохраняется (app.js) и встаёт в «к отправке» — сразу отправить
    for (let i = 0; i < 60; i++) {
      if (pending && !pending.classList.contains("hidden")) { $("#btn-send").click(); return; }
      await new Promise((r) => setTimeout(r, 100));
    }
    note("Голосовое не успело сохраниться — оно ждёт над полем ввода");
  };
  btn.addEventListener("pointerdown", (e) => {
    if (st || !app) return;
    e.preventDefault();
    try { btn.setPointerCapture(e.pointerId); } catch {}
    st = { x: e.clientX, y: e.clientY, t0: Date.now(), locked: false, pointerId: e.pointerId };
    app.voiceStart();
    timer.textContent = "0:00";
    st.timerId = setInterval(() => { if (st) timer.textContent = fmt(Date.now() - st.t0); }, 250);
    ui(true);
  });
  btn.addEventListener("pointermove", (e) => {
    if (!st || st.locked || e.pointerId !== st.pointerId) return;
    const dx = e.clientX - st.x, dy = e.clientY - st.y;
    if (dx < -90) { finish(false); return; }
    if (dy < -70) { st.locked = true; ui(true); }
  });
  const up = (e) => { if (st && !st.locked && e.pointerId === st.pointerId) finish(true); };
  btn.addEventListener("pointerup", up);
  btn.addEventListener("pointercancel", (e) => { if (st && !st.locked) finish(false); });
  cancelBtn.onclick = () => finish(false);
  sendBtn.onclick = () => finish(true);
}

// ---------- камера: фронтальная ↔ основная ----------
// Сторону переключает app.js (flipCamera): телефон сначала отпускает текущую
// камеру — две сразу он не открывает — и просит нужную по facingMode.
async function flipCamera() {
  if (!app?.flipCamera) return;
  try {
    const side = await app.flipCamera();
    note(side === "environment" ? "Основная камера" : "Фронтальная камера", 1800);
  } catch (e) { note("Не удалось переключить камеру: " + (e?.message || e)); }
}
function registerCameraFlip() {
  const cs = $("#cs-cam");
  if (cs) {
    const b = el("button", { id: "m-cs-flip", class: "cs-btn", title: "Перевернуть камеру", "aria-label": "Перевернуть камеру" }, ic("flip"));
    b.onclick = flipCamera;
    cs.after(b);
  }
  const vb = $("#vb-cam");
  if (vb) {
    const b = el("button", { id: "m-vb-flip", title: "Перевернуть камеру", "aria-label": "Перевернуть камеру" }, ic("flip"));
    b.onclick = flipCamera;
    vb.after(b);
  }
}

// ---------- запуск ----------
async function start() {
  const vp = document.querySelector('meta[name="viewport"]');
  if (vp) vp.content = "width=device-width, initial-scale=1, maximum-scale=1, user-scalable=no, interactive-widget=resizes-content";
  history.replaceState({ m: "list" }, "");
  registerPickers();
  buildList();
  buildChat();
  buildSettings();
  watchList();
  watchTheme();
  registerColorPicker();
  const st = await serverStatus();
  if (!loggedIn(st)) buildLogin();
  root.classList.add("m-ready");
  // остальное — когда app.js загрузит контакты и чаты
  try {
    app = (await import("./app.js")).mobileApi || null;
  } catch (e) { console.error("SafeDrop mobile: app.js", e); }
  followSelection();
  registerVoiceGesture();
  registerCameraFlip();
  registerAttachMenu();
  registerCallBar();
  registerScreenShare();
  decorate();
  refreshPreviews();
  if (loggedIn(st)) askNotifications();
  registerBackgroundCalls(st);
  for (const ev of ["safedrop-chat", "safedrop-chat-changed", "safedrop-chat-ack", "safedrop-peers"]) {
    listen(ev, () => refreshPreviews()).catch(() => {});
  }
  // свои отправленные сообщения событий не дают — обновлять список, пока он виден
  setInterval(() => { if (screen === "list" && !document.hidden) refreshPreviews(); }, 8000);
}

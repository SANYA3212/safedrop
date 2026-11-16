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
  const code = el("input", { id: "m-l-code", placeholder: "код с компьютера (вкладка «Сервер» → Мой ID)", autocomplete: "off", spellcheck: "false" });
  const nick = el("input", { id: "m-l-nick", placeholder: "как тебя будут видеть друзья", maxlength: "32" });
  const state = el("div", { id: "m-l-state" });
  const loginBtn = el("button", { id: "m-l-login", text: "Войти в свой аккаунт" });
  const newBtn = el("button", { id: "m-l-new", class: "primary", text: "Нет ПК версии — новый аккаунт" });
  box.append(
    el("img", { class: "m-logo", src: "logo.png", alt: "" }),
    el("h1", { text: "SafeDrop" }),
    el("p", { text: "Чаты, файлы и звонки через твой сервер SafeDrop" }),
    el("label", {}, el("span", { text: "Адрес сервера" }), addr),
    el("label", {}, el("span", { text: "Пароль сервера" }), pass),
    el("div", { class: "m-sep", text: "есть SafeDrop на компьютере" }),
    el("label", {}, el("span", { text: "Код устройства" }), code),
    loginBtn,
    el("div", { class: "m-sep", text: "или" }),
    el("label", {}, el("span", { text: "Ник" }), nick),
    newBtn,
    state,
  );
  const page = el("div", { id: "m-login" }, box);
  document.body.append(page);
  loginBtn.onclick = () => {
    const c = code.value.trim();
    if (!c) { say("Впиши код устройства с компьютера", true); code.focus(); return; }
    // Вход в аккаунт компьютера = одно и то же ID на двух устройствах сразу.
    // Сейчас сервер держит одно подключение на ID, а сам ID — отпечаток ключа
    // компьютера: по одному коду войти нельзя, иначе войдёт любой, кто код знает.
    say("Вход в аккаунт компьютера будет в следующей версии: компьютер должен подтвердить телефон, а сервер — доставлять сообщения на оба устройства. Пока зайди новым аккаунтом.", true);
  };
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
function registerPickers() {
  if (isDemo) return;
  const one = async (accept, keep) => (await stageFiles(await chooseFiles({ accept }), keep))[0] || null;
  overrideInvoke("pick_files", async () => stageFiles(await chooseFiles({ multiple: true })));
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
  if (!confirm("Выйти с сервера? Переписка останется на телефоне, при следующем входе на этот же сервер твой код не изменится.")) return;
  const st = await serverStatus();
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
  const st = await serverStatus();
  if (!loggedIn(st)) buildLogin();
  root.classList.add("m-ready");
  // остальное — когда app.js загрузит контакты и чаты
  try {
    app = (await import("./app.js")).mobileApi || null;
  } catch (e) { console.error("SafeDrop mobile: app.js", e); }
  followSelection();
  decorate();
  refreshPreviews();
  if (loggedIn(st)) askNotifications();
  for (const ev of ["safedrop-chat", "safedrop-chat-changed", "safedrop-chat-ack", "safedrop-peers"]) {
    listen(ev, () => refreshPreviews()).catch(() => {});
  }
  // свои отправленные сообщения событий не дают — обновлять список, пока он виден
  setInterval(() => { if (screen === "list" && !document.hidden) refreshPreviews(); }, 8000);
}

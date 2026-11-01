//! Окно «Браузер»: любые сайты внутри приложения (по умолчанию YouTube).
//!
//! Окно `ytb` из двух webview:
//! - `ytb-bar` — наша панель сверху (локальный `yt-browser.html`, есть IPC);
//! - `ytb-view` — сам сайт. Ни одна capability не выдаётся этой метке,
//!   поэтому удалённая страница не может вызывать команды приложения.
//!
//! Текущий адрес и заголовок страницы Rust читает сам и рассылает событием
//! `ytb-url` панели и главному окну. Режим «Листать вместе» (`SHARE`):
//! главное окно отправляет переходы участникам звонка, а чужие переходы
//! применяет через `ytb_remote_nav`. Чтобы не было эха, адрес, открытый по
//! команде участника, помечается в событии как `remote`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::webview::{NewWindowResponse, WebviewBuilder};
use tauri::window::WindowBuilder;
use tauri::{
    AppHandle, Emitter, EventTarget, LogicalPosition, LogicalSize, Manager, PhysicalPosition,
    PhysicalSize, Rect, Url, WebviewUrl, WindowEvent,
};

const WIN: &str = "ytb";
const BAR: &str = "ytb-bar";
const VIEW: &str = "ytb-view";
const BAR_H: f64 = 48.0;
/// Сколько после перехода по команде участника считать изменения адреса
/// (редиректы сайта) чужими и не отправлять обратно.
const REMOTE_QUIET: Duration = Duration::from_millis(2500);

/// Последний заголовок страницы (приходит из on_document_title_changed).
static TITLE: Mutex<String> = Mutex::new(String::new());
/// Режим «Листать вместе».
static SHARE: AtomicBool = AtomicBool::new(false);
/// Последний адрес, открытый по команде участника, и когда.
static REMOTE: Mutex<Option<(String, Instant)>> = Mutex::new(None);

fn web_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

/// Стартовая страница: плитки сайтов. Отдаётся как data: URL — у неё нет
/// доступа к приложению (как и у любого сайта в этом окне).
const START_HTML: &str = r#"<!doctype html><html lang="ru"><head><meta charset="utf-8"><title>Начальная страница</title>
<style>
body{margin:0;min-height:100vh;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:26px;background:radial-gradient(1200px 700px at 50% 30%,#2a2150,#0f1220 70%);color:#eef1f8;font:15px "Segoe UI",system-ui,sans-serif}
h1{margin:0;font-size:22px;font-weight:600}
p{margin:0;color:#9aa3b8;font-size:13px}
.grid{display:grid;grid-template-columns:repeat(4,150px);gap:14px}
a{display:flex;flex-direction:column;align-items:center;justify-content:center;gap:10px;height:110px;border-radius:16px;background:rgba(255,255,255,.06);border:1px solid rgba(255,255,255,.09);color:#eef1f8;text-decoration:none;transition:.15s}
a:hover{background:rgba(255,255,255,.12);transform:translateY(-2px)}
b{width:44px;height:44px;border-radius:12px;display:flex;align-items:center;justify-content:center;font-size:20px;color:#fff}
@media(max-width:700px){.grid{grid-template-columns:repeat(2,150px)}}
</style></head><body>
<h1>Куда пойдём?</h1>
<div class="grid">
<a href="https://www.youtube.com/"><b style="background:#ff0033">▶</b>YouTube</a>
<a href="https://www.google.com/"><b style="background:#4285f4">G</b>Google</a>
<a href="https://www.reddit.com/"><b style="background:#ff4500">r/</b>Reddit</a>
<a href="https://www.twitch.tv/"><b style="background:#9146ff">T</b>Twitch</a>
<a href="https://vkvideo.ru/"><b style="background:#0077ff">VK</b>VK Видео</a>
<a href="https://rutube.ru/"><b style="background:#100943">R</b>Rutube</a>
<a href="https://www.kinopoisk.ru/"><b style="background:#ff6600">K</b>Кинопоиск</a>
<a href="https://ru.wikipedia.org/"><b style="background:#636466">W</b>Википедия</a>
</div>
<p>Любой сайт — введите адрес или запрос в строку сверху. «Листать вместе» — друг увидит, что вы открываете.</p>
</body></html>"#;

fn start_url() -> Url {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(START_HTML.as_bytes());
    Url::parse(&format!("data:text/html;charset=utf-8;base64,{b64}")).expect("data url")
}

/// Строка из поля ввода: адрес — открываем, иначе ищем в Google.
/// (Раньше на YouTube искали поиском YouTube — «reddit» без .com так и не
/// открывал Reddit, поэтому поиск теперь всегда один.)
fn input_to_url(input: &str) -> Url {
    let s = input.trim();
    if let Ok(u) = Url::parse(s) {
        if web_url(&u) {
            return u;
        }
    }
    let looks_like_host = !s.contains(' ') && s.contains('.') && !s.starts_with('.') && !s.ends_with('.');
    if looks_like_host {
        if let Ok(u) = Url::parse(&format!("https://{s}")) {
            return u;
        }
    }
    let mut u = Url::parse("https://www.google.com/search").expect("static url");
    u.query_pairs_mut().append_pair("q", s);
    u
}

fn current_title() -> String {
    TITLE.lock().map(|t| t.clone()).unwrap_or_default()
}

fn is_remote(url: &str) -> bool {
    REMOTE
        .lock()
        .ok()
        .and_then(|r| r.clone())
        .map(|(u, at)| u == url || at.elapsed() < REMOTE_QUIET)
        .unwrap_or(false)
}

fn state_json(url: &str) -> serde_json::Value {
    serde_json::json!({
        "url": url,
        "title": current_title(),
        "share": SHARE.load(Ordering::Relaxed),
        "remote": is_remote(url),
    })
}

fn emit_state(app: &AppHandle, url: &str) {
    let payload = state_json(url);
    let _ = app.emit_to(EventTarget::webview(BAR), "ytb-url", payload.clone());
    let _ = app.emit_to(EventTarget::webview_window("main"), "ytb-url", payload);
}

fn emit_current(app: &AppHandle) {
    let url = app.get_webview(VIEW).and_then(|v| v.url().ok()).map(|u| u.to_string()).unwrap_or_default();
    emit_state(app, &url);
}

fn mark_remote(url: &Url) {
    if let Ok(mut r) = REMOTE.lock() {
        *r = Some((url.to_string(), Instant::now()));
    }
}

/// Раскладка: панель сверху фиксированной высоты, сайт — всё остальное.
fn layout(app: &AppHandle) {
    let Some(win) = app.get_window(WIN) else { return };
    let (Ok(size), Ok(scale)) = (win.inner_size(), win.scale_factor()) else { return };
    let bar_h = (BAR_H * scale).round() as u32;
    if let Some(bar) = app.get_webview(BAR) {
        let _ = bar.set_bounds(Rect {
            position: PhysicalPosition::new(0, 0).into(),
            size: PhysicalSize::new(size.width, bar_h).into(),
        });
    }
    if let Some(view) = app.get_webview(VIEW) {
        let _ = view.set_bounds(Rect {
            position: PhysicalPosition::new(0, bar_h as i32).into(),
            size: PhysicalSize::new(size.width, size.height.saturating_sub(bar_h)).into(),
        });
    }
}

/// Следим за адресом: многие сайты (YouTube, Reddit) — одностраничные,
/// переходы идут через history API без настоящей навигации, поэтому опрашиваем.
fn spawn_url_watch(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut last = String::new();
        let mut last_title = String::new();
        loop {
            tokio::time::sleep(Duration::from_millis(600)).await;
            let Some(view) = app.get_webview(VIEW) else { break };
            let url = match view.url() {
                Ok(u) => u.to_string(),
                Err(_) => continue,
            };
            let title = current_title();
            if url != last || title != last_title {
                last = url.clone();
                last_title = title;
                emit_state(&app, &url);
            }
        }
        // окно закрыли: режим «вместе» выключается и здесь, и в главном окне
        SHARE.store(false, Ordering::Relaxed);
        let _ = app.emit_to(EventTarget::webview_window("main"), "ytb-closed", ());
    });
}

fn build_window(app: &AppHandle, start: Url) -> Result<(), String> {
    let win = WindowBuilder::new(app, WIN)
        .title("Браузер — SafeDrop")
        .inner_size(1180.0, 780.0)
        .min_inner_size(640.0, 420.0)
        .build()
        .map_err(|e| e.to_string())?;
    let size = win
        .inner_size()
        .map_err(|e| e.to_string())?
        .to_logical::<f64>(win.scale_factor().unwrap_or(1.0));

    let bar = WebviewBuilder::new(BAR, WebviewUrl::App("yt-browser.html".into()));
    win.add_child(bar, LogicalPosition::new(0.0, 0.0), LogicalSize::new(size.width, BAR_H))
        .map_err(|e| e.to_string())?;

    let app_title = app.clone();
    let app_popup = app.clone();
    let view = WebviewBuilder::new(VIEW, WebviewUrl::External(start))
        .on_navigation(|u| {
            // обычные сайты — здесь; mailto:, tel: и прочие схемы — системе
            if web_url(u) || matches!(u.scheme(), "about" | "blob" | "data") {
                return true;
            }
            let _ = open::that(u.as_str());
            false
        })
        .on_new_window(move |u, _| {
            // ссылки «в новой вкладке» открываем в этом же окне
            if web_url(&u) {
                let app = app_popup.clone();
                tauri::async_runtime::spawn(async move {
                    if let Some(v) = app.get_webview(VIEW) {
                        let _ = v.navigate(u);
                    }
                });
            }
            NewWindowResponse::Deny
        })
        .on_document_title_changed(move |_, title| {
            let clean = title.trim_end_matches(" - YouTube").to_string();
            if let Ok(mut t) = TITLE.lock() {
                *t = clean;
            }
            emit_current(&app_title);
        });
    win.add_child(
        view,
        LogicalPosition::new(0.0, BAR_H),
        LogicalSize::new(size.width, (size.height - BAR_H).max(100.0)),
    )
    .map_err(|e| e.to_string())?;

    let app_ev = app.clone();
    win.on_window_event(move |e| match e {
        WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => layout(&app_ev),
        _ => {}
    });
    layout(app);
    spawn_url_watch(app.clone());
    spawn_media_watch(app.clone());
    Ok(())
}

/// Открыть окно (или показать уже открытое). `remote`: адрес пришёл от
/// участника звонка — его не нужно рассылать обратно.
#[tauri::command]
pub async fn ytb_open(app: AppHandle, url: Option<String>, remote: Option<bool>) -> Result<(), String> {
    let target = url
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(input_to_url)
        .filter(web_url);
    if remote.unwrap_or(false) {
        if let Some(u) = &target {
            mark_remote(u);
        }
    }
    if let Some(win) = app.get_window(WIN) {
        if let (Some(u), Some(view)) = (target, app.get_webview(VIEW)) {
            view.navigate(u).map_err(|e| e.to_string())?;
        }
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(());
    }
    build_window(&app, target.unwrap_or_else(start_url))
}

/// Переход по строке из панели: адрес или поисковый запрос.
#[tauri::command]
pub async fn ytb_go(app: AppHandle, input: String) -> Result<(), String> {
    let view = app.get_webview(VIEW).ok_or("окно браузера закрыто")?;
    view.navigate(input_to_url(&input)).map_err(|e| e.to_string())
}

/// Кнопки панели. В страницу уходят только фиксированные строки
/// (назад/вперёд/пауза), страница ничего не получает в ответ.
#[tauri::command]
pub async fn ytb_nav(app: AppHandle, action: String) -> Result<(), String> {
    let view = app.get_webview(VIEW).ok_or("окно браузера закрыто")?;
    match action.as_str() {
        "back" => view.eval("history.back()"),
        "forward" => view.eval("history.forward()"),
        "reload" => view.reload(),
        "home" => view.navigate(start_url()),
        _ => return Err("unknown action".into()),
    }
    .map_err(|e| e.to_string())
}

/// «Смотреть вместе» для YouTube: ставим видео в окне на паузу и отдаём
/// адрес главному окну, где запускается синхронный плеер.
#[tauri::command]
pub async fn ytb_watch(app: AppHandle, queue: Option<bool>) -> Result<(), String> {
    let view = app.get_webview(VIEW).ok_or("окно браузера закрыто")?;
    let url = view.url().map_err(|e| e.to_string())?.to_string();
    let queue = queue.unwrap_or(false);
    // в очередь — видео в браузере не трогаем, пользователь листает дальше
    if !queue {
        pause_media(&app);
    }
    app.emit_to(
        EventTarget::webview_window("main"),
        "ytb-watch",
        serde_json::json!({ "url": url, "title": current_title(), "queue": queue }),
    )
    .map_err(|e| e.to_string())?;
    if queue {
        return Ok(());
    }
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.unminimize();
        let _ = main.show();
        let _ = main.set_focus();
    }
    Ok(())
}

/// Пауза всего видео и звука на странице браузера. Фиксированная строка,
/// страница ничего не получает в ответ. Второй проход через секунду: YouTube
/// после паузы иногда сам продолжает (автовоспроизведение, смена ролика).
fn pause_media(app: &AppHandle) {
    const JS: &str = "document.querySelectorAll('video,audio').forEach(function(m){try{m.pause()}catch(e){}})";
    let Some(view) = app.get_webview(VIEW) else { return };
    let _ = view.eval(JS);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1000)).await;
        if let Some(v) = app.get_webview(VIEW) {
            let _ = v.eval(JS);
        }
    });
}

/// «Остановить для всех» и подобное: браузер тоже замолкает.
#[tauri::command]
pub async fn ytb_pause_media(app: AppHandle) -> Result<(), String> {
    pause_media(&app);
    Ok(())
}

/// Включить/выключить «Листать вместе». Главное окно получает `ytb-share`
/// и заново текущий адрес, чтобы сразу показать участникам эту страницу.
#[tauri::command]
pub async fn ytb_set_share(app: AppHandle, on: bool) -> Result<(), String> {
    SHARE.store(on, Ordering::Relaxed);
    let _ = app.emit_to(EventTarget::webview_window("main"), "ytb-share", serde_json::json!({ "on": on }));
    let _ = app.emit_to(EventTarget::webview(BAR), "ytb-share", serde_json::json!({ "on": on }));
    if on {
        emit_current(&app);
    }
    Ok(())
}

/// Переход по команде участника. Возвращает false, если окно закрыто или
/// «Листать вместе» выключено — тогда главное окно предлагает присоединиться.
#[tauri::command]
pub async fn ytb_remote_nav(app: AppHandle, url: String, by: String) -> Result<bool, String> {
    if !SHARE.load(Ordering::Relaxed) {
        return Ok(false);
    }
    let Some(view) = app.get_webview(VIEW) else { return Ok(false) };
    let u = Url::parse(&url).map_err(|e| e.to_string())?;
    if !web_url(&u) {
        return Err("неподдерживаемый адрес".into());
    }
    if view.url().map(|cur| cur == u).unwrap_or(false) {
        return Ok(true);
    }
    mark_remote(&u);
    view.navigate(u).map_err(|e| e.to_string())?;
    let who = if by.trim().is_empty() { "Участник".to_string() } else { by };
    let _ = app.emit_to(
        EventTarget::webview(BAR),
        "ytb-notice",
        serde_json::json!({ "text": format!("{who} открыл страницу"), "ok": true }),
    );
    Ok(true)
}

/// Приглушение видео и звука на странице браузера, пока в звонке говорят.
/// `k` — доля обычной громкости (1 = вернуть как было). Исходная громкость
/// каждого элемента запоминается в data-атрибуте и восстанавливается.
#[tauri::command]
pub async fn ytb_duck(app: AppHandle, k: f64) -> Result<(), String> {
    let Some(view) = app.get_webview(VIEW) else { return Ok(()) };
    if !k.is_finite() {
        return Err("bad level".into());
    }
    let k = k.clamp(0.0, 1.0);
    let js = if k >= 0.999 {
        "document.querySelectorAll('video,audio').forEach(function(m){if(m.dataset.sdv!==undefined){m.volume=parseFloat(m.dataset.sdv);delete m.dataset.sdv}})".to_string()
    } else {
        format!("document.querySelectorAll('video,audio').forEach(function(m){{if(m.dataset.sdv===undefined)m.dataset.sdv=m.volume;m.volume=Math.max(0,Math.min(1,parseFloat(m.dataset.sdv)*{k}))}})")
    };
    view.eval(js).map_err(|e| e.to_string())
}

/// Сообщение в чат звонка из панели браузера: главное окно отправит его.
#[tauri::command]
pub async fn ytb_chat_send(app: AppHandle, text: String) -> Result<(), String> {
    let text = text.trim().to_string();
    if text.is_empty() || text.chars().count() > 2000 {
        return Err("пустое или слишком длинное сообщение".into());
    }
    app.emit_to(EventTarget::webview_window("main"), "ytb-chat-send", serde_json::json!({ "text": text }))
        .map_err(|e| e.to_string())
}

/// Новое сообщение звонка — показать в панели браузера.
#[tauri::command]
pub async fn ytb_chat_show(app: AppHandle, from: String, text: String) -> Result<(), String> {
    if app.get_webview(BAR).is_none() {
        return Ok(());
    }
    app.emit_to(EventTarget::webview(BAR), "ytb-chat", serde_json::json!({ "from": from, "text": text }))
        .map_err(|e| e.to_string())
}

/// Ответ главного окна для панели (например, «сначала начните звонок»).
#[tauri::command]
pub async fn ytb_notify(app: AppHandle, text: String, ok: bool) -> Result<(), String> {
    app.emit_to(EventTarget::webview(BAR), "ytb-notice", serde_json::json!({ "text": text, "ok": ok }))
        .map_err(|e| e.to_string())
}

/// Текущее состояние для панели при загрузке.
#[tauri::command]
pub async fn ytb_state(app: AppHandle) -> Result<serde_json::Value, String> {
    let url = app.get_webview(VIEW).and_then(|v| v.url().ok()).map(|u| u.to_string()).unwrap_or_default();
    Ok(state_json(&url))
}

// ---------- синхронное видео на любых сайтах ----------
//
// В режиме «Листать вместе» раз в 0,7 с читаем состояние главного <video>
// страницы (самого большого на экране). Заметили, что пользователь нажал
// паузу/плей или перемотал, — отдаём главному окну `ytb-media`, оно шлёт
// участникам. Чужие команды применяются через `ytb_media_apply`, после чего
// на 1,5 с наши собственные «изменения» не рассылаются (защита от эха).
// Работает только для видео прямо на странице: плееры во встроенных окнах
// (iframe с другого сайта) отсюда не видны.

const MEDIA_TICK: Duration = Duration::from_millis(700);
const MEDIA_QUIET: Duration = Duration::from_millis(1500);
/// Тот, кто последним управлял, раз в 5 с шлёт позицию для мягкой подгонки.
const MEDIA_TIME_EVERY: Duration = Duration::from_secs(5);

/// Чтение состояния главного видео. Возвращает объект — WebView2 сам
/// сериализует его в JSON для колбэка.
const MEDIA_READ_JS: &str = "(function(){var b=null,ba=0;document.querySelectorAll('video').forEach(function(v){var r=v.getBoundingClientRect(),a=r.width*r.height;if(a>ba){ba=a;b=v}});if(!b||ba<40000)return {has:false};return {has:true,paused:b.paused,t:b.currentTime,dur:isFinite(b.duration)?b.duration:0}})()";

#[derive(Clone)]
struct MediaSnap {
    url: String,
    paused: bool,
    t: f64,
    at: Instant,
}

#[derive(Default)]
struct MediaSync {
    prev: Option<MediaSnap>,
    quiet_until: Option<Instant>,
    leader: bool,
    last_time_sent: Option<Instant>,
}

static MEDIA: Mutex<MediaSync> = Mutex::new(MediaSync { prev: None, quiet_until: None, leader: false, last_time_sent: None });

fn url_no_fragment(u: &str) -> &str {
    u.split('#').next().unwrap_or(u)
}

fn clean_time(t: f64) -> Option<f64> {
    (t.is_finite() && (0.0..=1_000_000.0).contains(&t)).then_some(t)
}

/// Разбор ответа страницы. Страница может подсунуть что угодно, поэтому
/// берём только нужные поля и проверяем числа.
fn parse_media(raw: &str) -> Option<(bool, f64)> {
    let mut v: serde_json::Value = serde_json::from_str(raw).ok()?;
    if let serde_json::Value::String(s) = &v {
        v = serde_json::from_str(s).ok()?;
    }
    if !v.get("has")?.as_bool()? {
        return None;
    }
    let paused = v.get("paused")?.as_bool()?;
    let t = clean_time(v.get("t")?.as_f64()?)?;
    Some((paused, t))
}

fn on_media_read(app: &AppHandle, url: String, raw: String) {
    let now = Instant::now();
    let Ok(mut m) = MEDIA.lock() else { return };
    let Some((paused, t)) = parse_media(&raw) else {
        m.prev = None;
        return;
    };
    let snap = MediaSnap { url: url.clone(), paused, t, at: now };
    let prev = match m.prev.replace(snap) {
        Some(p) if url_no_fragment(&p.url) == url_no_fragment(&url) => p,
        _ => return, // новая страница или первое чтение — только запоминаем
    };
    if m.quiet_until.map(|q| now < q).unwrap_or(false) {
        return;
    }
    let elapsed = now.duration_since(prev.at).as_secs_f64();
    let expected = prev.t + if prev.paused { 0.0 } else { elapsed };
    let action = if paused != prev.paused {
        Some(if paused { "pause" } else { "play" })
    } else if (t - expected).abs() > 2.0 {
        Some("seek")
    } else if m.leader && !paused && m.last_time_sent.map(|s| now.duration_since(s) >= MEDIA_TIME_EVERY).unwrap_or(true) {
        Some("time")
    } else {
        None
    };
    let Some(action) = action else { return };
    if action != "time" {
        m.leader = true;
    }
    m.last_time_sent = Some(now);
    drop(m);
    let _ = app.emit_to(
        EventTarget::webview_window("main"),
        "ytb-media",
        serde_json::json!({ "action": action, "t": t, "paused": paused, "url": url }),
    );
}

fn spawn_media_watch(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(MEDIA_TICK).await;
            let Some(view) = app.get_webview(VIEW) else { break };
            if !SHARE.load(Ordering::Relaxed) {
                if let Ok(mut m) = MEDIA.lock() {
                    *m = MediaSync::default();
                }
                continue;
            }
            let Ok(url) = view.url().map(|u| u.to_string()) else { continue };
            if !url.starts_with("http") {
                continue;
            }
            let app_cb = app.clone();
            let _ = view.eval_with_callback(MEDIA_READ_JS, move |raw| on_media_read(&app_cb, url.clone(), raw));
        }
    });
}

/// Команда участника для видео на той же странице. Возвращает false, если
/// у нас другая страница или «Листать вместе» выключено.
#[tauri::command]
pub async fn ytb_media_apply(app: AppHandle, action: String, t: f64, url: String, by: String) -> Result<bool, String> {
    if !SHARE.load(Ordering::Relaxed) {
        return Ok(false);
    }
    let view = app.get_webview(VIEW).ok_or("окно браузера закрыто")?;
    let cur = view.url().map_err(|e| e.to_string())?.to_string();
    if url_no_fragment(&cur) != url_no_fragment(&url) {
        return Ok(false);
    }
    let t = clean_time(t).ok_or("bad time")?;
    // фиксированные шаблоны, в страницу попадает только проверенное число
    let body = match action.as_str() {
        "play" => format!("if(Math.abs(b.currentTime-{t})>1.5)b.currentTime={t};b.play();"),
        "pause" => format!("b.pause();if(Math.abs(b.currentTime-{t})>1.5)b.currentTime={t};"),
        "seek" => format!("b.currentTime={t};"),
        "time" => format!("if(!b.paused&&Math.abs(b.currentTime-{t})>2.5)b.currentTime={t};"),
        _ => return Err("unknown action".into()),
    };
    let js = format!("(function(){{var b=null,ba=0;document.querySelectorAll('video').forEach(function(v){{var r=v.getBoundingClientRect(),a=r.width*r.height;if(a>ba){{ba=a;b=v}}}});if(!b)return;try{{{body}}}catch(e){{}}}})()");
    if let Ok(mut m) = MEDIA.lock() {
        m.quiet_until = Some(Instant::now() + MEDIA_QUIET);
        m.leader = false;
        m.prev = None;
    }
    view.eval(js).map_err(|e| e.to_string())?;
    if action != "time" {
        let who = if by.trim().is_empty() { "Участник".to_string() } else { by };
        let what = match action.as_str() {
            "play" => "включил видео".to_string(),
            "pause" => "поставил на паузу".to_string(),
            _ => format!("перемотал на {}:{:02}", (t / 60.0) as u64, (t % 60.0) as u64),
        };
        let _ = app.emit_to(EventTarget::webview(BAR), "ytb-notice", serde_json::json!({ "text": format!("{who} {what}"), "ok": true }));
    }
    Ok(true)
}

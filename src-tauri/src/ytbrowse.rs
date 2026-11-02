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
//!
//! Крестик окно не уничтожает, а прячет (`shut`): повторное открытие —
//! мгновенное, без создания нового WebView2.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::webview::{NewWindowResponse, PageLoadEvent, Webview, WebviewBuilder};
use tauri::window::WindowBuilder;
use tauri::{
    AppHandle, Emitter, EventTarget, LogicalPosition, LogicalSize, Manager, PhysicalPosition,
    PhysicalSize, Rect, Url, WebviewUrl, WindowEvent,
};

const WIN: &str = "ytb";
const BAR: &str = "ytb-bar";
const VIEW: &str = "ytb-view";
const BAR_H: f64 = 48.0;
const CHAT: &str = "ytb-chat";
const CHAT_W: f64 = 340.0;
/// Открыта ли панель чата справа.
static CHAT_OPEN: AtomicBool = AtomicBool::new(false);
/// Сколько после перехода по команде участника считать изменения адреса
/// (редиректы сайта) чужими и не отправлять обратно — если пользователь
/// за это время ничего не нажимал на странице.
const REMOTE_QUIET: Duration = Duration::from_secs(6);
/// Окно браузера показано (спрятанное крестиком считается закрытым).
static OPEN: AtomicBool = AtomicBool::new(false);
/// Пользователь сам выключил «Листать вместе»: чужие переходы не подхватываем.
static OPT_OUT: AtomicBool = AtomicBool::new(false);
/// Время последнего клика/нажатия клавиши на странице (мс, как Date.now()).
static LAST_INPUT: AtomicU64 = AtomicU64::new(0);

/// Последний заголовок страницы (приходит из on_document_title_changed).
static TITLE: Mutex<String> = Mutex::new(String::new());
/// Режим «Листать вместе».
static SHARE: AtomicBool = AtomicBool::new(false);
/// Последний адрес, открытый по команде участника, и когда (Instant и мс).
static REMOTE: Mutex<Option<(String, Instant, u64)>> = Mutex::new(None);

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Страница сайта, если окно браузера открыто.
fn open_view(app: &AppHandle) -> Option<Webview> {
    if OPEN.load(Ordering::Relaxed) {
        app.get_webview(VIEW)
    } else {
        None
    }
}

fn web_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

/// Стартовая страница с плитками сайтов — локальный файл `start.html`.
/// (Раньше был data:-адрес: без особой опции Tauri такой webview не создаётся,
/// и в окне оставалась только панель — «окно браузера закрыто».)
fn start_url() -> Url {
    let base = if cfg!(windows) { "http://tauri.localhost/start.html" } else { "tauri://localhost/start.html" };
    Url::parse(base).expect("static url")
}

/// Служебные адреса самого приложения (tauri.localhost, ipc.localhost …).
/// Сайт или собеседник не должен уводить окно браузера на них: страницы
/// приложения имеют доступ к его командам. Разрешена только стартовая страница.
fn is_app_host(u: &Url) -> bool {
    u.scheme() == "tauri" || u.host_str().map(|h| h.to_ascii_lowercase().ends_with(".localhost")).unwrap_or(false)
}

fn is_start_page(u: &Url) -> bool {
    let s = start_url();
    u.scheme() == s.scheme() && u.host_str() == s.host_str() && u.path() == s.path()
}

/// Куда можно переходить внутри окна браузера.
fn nav_allowed(u: &Url) -> bool {
    if is_start_page(u) {
        return true;
    }
    web_url(u) && !is_app_host(u)
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

/// Адрес открыт по команде участника (или это редирект после неё)? После
/// любого клика пользователя на странице — уже нет: это его собственный переход.
fn is_remote(url: &str) -> bool {
    REMOTE
        .lock()
        .ok()
        .and_then(|r| r.clone())
        .map(|(u, at, ms)| LAST_INPUT.load(Ordering::Relaxed) <= ms && (u == url || at.elapsed() < REMOTE_QUIET))
        .unwrap_or(false)
}

fn clear_remote() {
    if let Ok(mut r) = REMOTE.lock() {
        *r = None;
    }
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
        *r = Some((url.to_string(), Instant::now(), now_ms()));
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
    let below = size.height.saturating_sub(bar_h);
    let chat_open = CHAT_OPEN.load(Ordering::Relaxed) && app.get_webview(CHAT).is_some();
    let chat_w = if chat_open { ((CHAT_W * scale).round() as u32).min(size.width / 2) } else { 0 };
    if let Some(view) = app.get_webview(VIEW) {
        let _ = view.set_bounds(Rect {
            position: PhysicalPosition::new(0, bar_h as i32).into(),
            size: PhysicalSize::new(size.width - chat_w, below).into(),
        });
    }
    if let Some(chat) = app.get_webview(CHAT) {
        if chat_open {
            let _ = chat.set_bounds(Rect {
                position: PhysicalPosition::new((size.width - chat_w) as i32, bar_h as i32).into(),
                size: PhysicalSize::new(chat_w, below).into(),
            });
            let _ = chat.show();
        } else {
            let _ = chat.hide();
        }
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
            if SHARE.load(Ordering::Relaxed) {
                // в режиме «вместе» адрес отслеживает spawn_agent_watch
                last.clear();
                continue;
            }
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
        CHAT_OPEN.store(false, Ordering::Relaxed);
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
    let app_load = app.clone();
    let view_url = if is_start_page(&start) { WebviewUrl::App("start.html".into()) } else { WebviewUrl::External(start) };
    let view = WebviewBuilder::new(VIEW, view_url)
        .initialization_script(AGENT_JS)
        .on_page_load(move |_, p| {
            // полоска загрузки в панели
            let on = matches!(p.event(), PageLoadEvent::Started);
            let _ = app_load.emit_to(EventTarget::webview(BAR), "ytb-loading", serde_json::json!({ "on": on }));
        })
        .on_navigation(|u| {
            // обычные сайты — здесь; служебные адреса приложения — нельзя;
            // системе отдаём только mailto: и tel:. Остальные схемы (file:,
            // search-ms:, ms-*:) сайт мог бы использовать, чтобы запустить
            // программу на компьютере без вопроса.
            if nav_allowed(u) || matches!(u.scheme(), "about" | "blob") {
                return true;
            }
            if matches!(u.scheme(), "mailto" | "tel") {
                let _ = open::that(u.as_str());
            }
            false
        })
        .on_new_window(move |u, _| {
            // ссылки «в новой вкладке» открываем в этом же окне
            if nav_allowed(&u) {
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
    if let Err(e) = win.add_child(
        view,
        LogicalPosition::new(0.0, BAR_H),
        LogicalSize::new(size.width, (size.height - BAR_H).max(100.0)),
    ) {
        // без страницы окно бесполезно — закрываем, чтобы следующее открытие
        // создало его заново, и показываем причину
        let _ = win.close();
        return Err(format!("не удалось открыть страницу: {e}"));
    }

    let app_ev = app.clone();
    win.on_window_event(move |e| match e {
        WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => layout(&app_ev),
        WindowEvent::CloseRequested { api, .. } => {
            // не уничтожаем: следующее открытие будет мгновенным
            api.prevent_close();
            shut(&app_ev);
        }
        _ => {}
    });
    OPEN.store(true, Ordering::Relaxed);
    layout(app);
    spawn_url_watch(app.clone());
    spawn_media_watch(app.clone());
    spawn_agent_watch(app.clone());
    Ok(())
}

/// «Закрыть» окно браузера: спрятать, остановить страницу (уходим на
/// стартовую — звук и видео замолкают), выключить «вместе» и чат.
fn shut(app: &AppHandle) {
    OPEN.store(false, Ordering::Relaxed);
    LAST_INPUT.store(0, Ordering::Relaxed);
    SHARE.store(false, Ordering::Relaxed);
    CHAT_OPEN.store(false, Ordering::Relaxed);
    OPT_OUT.store(false, Ordering::Relaxed);
    clear_remote();
    if let Ok(mut m) = MEDIA.lock() {
        *m = MediaSync::default();
    }
    if let Some(w) = app.get_window(WIN) {
        let _ = w.hide();
    }
    if let Some(v) = app.get_webview(VIEW) {
        let _ = v.navigate(start_url());
    }
    layout(app);
    let _ = app.emit_to(EventTarget::webview(BAR), "ytb-share", serde_json::json!({ "on": false }));
    let _ = app.emit_to(EventTarget::webview(BAR), "ytb-chat-state", serde_json::json!({ "open": false }));
    let _ = app.emit_to(EventTarget::webview(BAR), "ytb-people", serde_json::json!([]));
    let _ = app.emit_to(EventTarget::webview_window("main"), "ytb-closed", ());
}

/// Закрыть окно браузера (кнопка «Закрыть у всех» у участника звонка).
/// Возвращает, было ли окно открыто.
#[tauri::command]
pub async fn ytb_close(app: AppHandle) -> Result<bool, String> {
    let was = OPEN.load(Ordering::Relaxed);
    if app.get_window(WIN).is_some() {
        shut(&app);
    }
    Ok(was)
}

/// Кнопка панели «Закрыть у всех»: главное окно разошлёт команду и закроет наше.
#[tauri::command]
pub async fn ytb_close_all(app: AppHandle) -> Result<(), String> {
    app.emit_to(EventTarget::webview_window("main"), "ytb-close-all", ()).map_err(|e| e.to_string())
}

/// Открыть окно (или показать уже открытое). `remote`: адрес пришёл от
/// участника звонка — его не нужно рассылать обратно.
#[tauri::command]
pub async fn ytb_open(app: AppHandle, url: Option<String>, remote: Option<bool>) -> Result<(), String> {
    let target = url
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(input_to_url)
        .filter(nav_allowed);
    if remote.unwrap_or(false) {
        if let Some(u) = &target {
            mark_remote(u);
        }
    }
    if let Some(win) = app.get_window(WIN) {
        if let (Some(u), Some(view)) = (target, app.get_webview(VIEW)) {
            view.navigate(u).map_err(|e| e.to_string())?;
        }
        OPEN.store(true, Ordering::Relaxed);
        layout(&app);
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(());
    }
    static BUILDING: AtomicBool = AtomicBool::new(false);
    if BUILDING.swap(true, Ordering::SeqCst) {
        return Ok(()); // окно уже создаётся
    }
    let r = build_window(&app, target.unwrap_or_else(start_url));
    BUILDING.store(false, Ordering::SeqCst);
    r
}

/// Переход по строке из панели: адрес или поисковый запрос.
#[tauri::command]
pub async fn ytb_go(app: AppHandle, input: String) -> Result<(), String> {
    let view = app.get_webview(VIEW).ok_or("окно браузера закрыто")?;
    let u = input_to_url(&input);
    if !nav_allowed(&u) {
        return Err("этот адрес нельзя открыть".into());
    }
    clear_remote();
    view.navigate(u).map_err(|e| e.to_string())
}

/// Кнопки панели. В страницу уходят только фиксированные строки
/// (назад/вперёд/пауза), страница ничего не получает в ответ.
#[tauri::command]
pub async fn ytb_nav(app: AppHandle, action: String) -> Result<(), String> {
    let view = app.get_webview(VIEW).ok_or("окно браузера закрыто")?;
    clear_remote();
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
/// `manual` — кнопка в панели: выключил сам — чужие переходы не подхватываем.
#[tauri::command]
pub async fn ytb_set_share(app: AppHandle, on: bool, manual: Option<bool>) -> Result<(), String> {
    if manual.unwrap_or(false) {
        OPT_OUT.store(!on, Ordering::Relaxed);
    }
    SHARE.store(on, Ordering::Relaxed);
    let _ = app.emit_to(EventTarget::webview_window("main"), "ytb-share", serde_json::json!({ "on": on }));
    let _ = app.emit_to(EventTarget::webview(BAR), "ytb-share", serde_json::json!({ "on": on }));
    if on {
        emit_current(&app);
    }
    Ok(())
}

/// Переход по команде участника. Окно открыто, а «Листать вместе» выключено
/// не вручную (друг открыл браузер сам) — включаем и присоединяемся.
/// Возвращает false, если окно закрыто или пользователь сам выключил режим —
/// тогда главное окно предлагает присоединиться.
#[tauri::command]
pub async fn ytb_remote_nav(app: AppHandle, url: String, by: String) -> Result<bool, String> {
    let Some(view) = open_view(&app) else { return Ok(false) };
    let u = Url::parse(&url).map_err(|e| e.to_string())?;
    if !nav_allowed(&u) {
        return Err("неподдерживаемый адрес".into());
    }
    let joined = !SHARE.load(Ordering::Relaxed);
    if joined {
        if OPT_OUT.load(Ordering::Relaxed) {
            return Ok(false);
        }
        SHARE.store(true, Ordering::Relaxed);
        // auto: главное окно не рассылает в ответ свою старую страницу
        let on = serde_json::json!({ "on": true, "auto": true });
        let _ = app.emit_to(EventTarget::webview_window("main"), "ytb-share", on.clone());
        let _ = app.emit_to(EventTarget::webview(BAR), "ytb-share", on);
    }
    let who = if by.trim().is_empty() { "Участник".to_string() } else { by };
    let same = view.url().map(|cur| cur == u).unwrap_or(false);
    if !same {
        mark_remote(&u);
        view.navigate(u).map_err(|e| e.to_string())?;
    }
    if joined || !same {
        let text = if joined { format!("{who} листает — вы присоединились") } else { format!("{who} открыл страницу") };
        let _ = app.emit_to(EventTarget::webview(BAR), "ytb-notice", serde_json::json!({ "text": text, "ok": true }));
    }
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

/// Новое сообщение звонка: в открытую панель чата, иначе — всплывашка в панели браузера.
#[tauri::command]
pub async fn ytb_chat_show(app: AppHandle, from: String, text: String, mine: Option<bool>) -> Result<(), String> {
    if !OPEN.load(Ordering::Relaxed) || app.get_webview(BAR).is_none() {
        return Ok(());
    }
    let payload = serde_json::json!({ "from": from, "text": text, "mine": mine.unwrap_or(false) });
    if CHAT_OPEN.load(Ordering::Relaxed) && app.get_webview(CHAT).is_some() {
        return app.emit_to(EventTarget::webview(CHAT), "ytb-chat", payload).map_err(|e| e.to_string());
    }
    if mine.unwrap_or(false) {
        return Ok(());
    }
    app.emit_to(EventTarget::webview(BAR), "ytb-chat", payload).map_err(|e| e.to_string())
}

/// Открыть/закрыть панель чата справа (во всю высоту окна под панелью).
#[tauri::command]
pub async fn ytb_chat_toggle(app: AppHandle, open: Option<bool>) -> Result<bool, String> {
    let win = app.get_window(WIN).ok_or("окно браузера закрыто")?;
    let on = open.unwrap_or(!CHAT_OPEN.load(Ordering::Relaxed));
    if on && app.get_webview(CHAT).is_none() {
        let chat = WebviewBuilder::new(CHAT, WebviewUrl::App("yt-chat.html".into()));
        win.add_child(chat, LogicalPosition::new(0.0, BAR_H), LogicalSize::new(CHAT_W, 300.0))
            .map_err(|e| e.to_string())?;
    }
    CHAT_OPEN.store(on, Ordering::Relaxed);
    layout(&app);
    let _ = app.emit_to(EventTarget::webview(BAR), "ytb-chat-state", serde_json::json!({ "open": on }));
    if on {
        let _ = app.emit_to(EventTarget::webview_window("main"), "ytb-chat-request", ());
    }
    Ok(on)
}

/// Панель чата просит историю — главное окно ответит ytb_chat_history.
#[tauri::command]
pub async fn ytb_chat_request(app: AppHandle) -> Result<(), String> {
    app.emit_to(EventTarget::webview_window("main"), "ytb-chat-request", ()).map_err(|e| e.to_string())
}

/// История чата звонка от главного окна — в панель чата.
#[tauri::command]
pub async fn ytb_chat_history(app: AppHandle, data: serde_json::Value) -> Result<(), String> {
    if app.get_webview(CHAT).is_none() {
        return Ok(());
    }
    app.emit_to(EventTarget::webview(CHAT), "ytb-chat-history", data).map_err(|e| e.to_string())
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
const MEDIA_READ_JS: &str = "(function(){var b=null,ba=0;document.querySelectorAll('video').forEach(function(v){var r=v.getBoundingClientRect(),a=r.width*r.height;if(a>ba){ba=a;b=v}});if(!b||ba<40000)return {has:false};return {has:true,paused:b.paused,t:b.currentTime,dur:isFinite(b.duration)?b.duration:0,ad:!!document.querySelector('.ad-showing')}})()";

#[derive(Clone)]
struct MediaSnap {
    url: String,
    paused: bool,
    t: f64,
    dur: f64,
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
fn parse_media(raw: &str) -> Option<(bool, f64, f64)> {
    let mut v: serde_json::Value = serde_json::from_str(raw).ok()?;
    if let serde_json::Value::String(s) = &v {
        v = serde_json::from_str(s).ok()?;
    }
    if !v.get("has")?.as_bool()? {
        return None;
    }
    if v.get("ad").and_then(|x| x.as_bool()).unwrap_or(false) {
        return None; // реклама: ни паузы, ни перемотки не рассылаем
    }
    let paused = v.get("paused")?.as_bool()?;
    let t = clean_time(v.get("t")?.as_f64()?)?;
    let dur = v.get("dur").and_then(|x| x.as_f64()).filter(|x| x.is_finite() && *x >= 0.0).unwrap_or(0.0);
    Some((paused, t, dur))
}

fn on_media_read(app: &AppHandle, url: String, raw: String) {
    let now = Instant::now();
    let Ok(mut m) = MEDIA.lock() else { return };
    let Some((paused, t, dur)) = parse_media(&raw) else {
        m.prev = None;
        return;
    };
    let snap = MediaSnap { url: url.clone(), paused, t, dur, at: now };
    let prev = match m.prev.replace(snap) {
        // другая длительность — сменился ролик в том же <video> (реклама, плейлист)
        Some(p) if url_no_fragment(&p.url) == url_no_fragment(&url) && (p.dur - dur).abs() < 1.0 => p,
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
    let Some(view) = open_view(&app) else { return Ok(false) };
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

// ---------- «Листать вместе»: курсоры и прокрутка ----------
//
// В каждую страницу окна встраивается маленький скрипт (AGENT_JS): он
// запоминает положение мыши, прокрутку и время последнего клика, а также
// рисует курсоры участников. IPC у страницы нет — Rust сам читает её
// состояние (spawn_agent_watch) и отдаёт главному окну `ytb-cursor` /
// `ytb-scroll`; чужие курсоры и прокрутку применяют ytb_remote_cursor /
// ytb_remote_scroll. Имена участников в страницу не попадают — только
// цвет; кто какого цвета, видно в панели браузера.

/// Цвета курсоров (тот же список — WEB_COLORS в app.js и в yt-browser.html).
const PALETTE: [&str; 8] = ["#3fbf6f", "#ff5c6c", "#ffd23f", "#4aa8ff", "#b36bff", "#ff9f43", "#ff6bcb", "#2ee6d6"];
const AGENT_TICK: Duration = Duration::from_millis(120);
const CURSOR_EVERY: Duration = Duration::from_millis(150);
const SCROLL_EVERY: Duration = Duration::from_millis(200);

/// Скрипт страницы. Без innerHTML (YouTube запрещает его через Trusted Types),
/// курсоры — в закрытом shadow root поверх страницы, мышь сквозь них проходит.
const AGENT_JS: &str = r#"(function(){
if (window.top !== window || window.__sdA) return;
var st = {i:0, x:-1, y:-1, t:0, u:0, d:false, ap:0, ws:0};
var now = function(){ return Date.now(); };
var opt = {capture:true, passive:true};
var se = function(){ return document.scrollingElement || document.documentElement; };
addEventListener('pointerdown', function(){ st.i = now(); st.d = true; }, opt);
addEventListener('pointerup', function(){ st.d = false; }, opt);
addEventListener('keydown', function(){ st.i = now(); st.ws = now(); }, opt);
addEventListener('wheel', function(){ st.ws = now(); }, opt);
addEventListener('touchmove', function(){ st.ws = now(); }, opt);
addEventListener('mousemove', function(e){ st.x = e.clientX; st.y = e.clientY; st.t = now(); }, opt);
addEventListener('mouseout', function(e){ if (!e.relatedTarget) { st.x = -1; st.t = now(); } }, opt);
var host = null, root = null, curs = {};
function place(el){ var e = se(), sh = e ? e.scrollHeight : innerHeight; el.style.transform = 'translate(' + (el._x * innerWidth) + 'px,' + (el._y * sh - scrollY) + 'px)'; }
addEventListener('scroll', function(){
  var n = now();
  for (var k in curs) place(curs[k]);
  if (n - st.ap < 300) return;
  if (st.d || n - st.ws < 800) st.u = n;
}, opt);
function ensure(){
  if (host && host.isConnected) return true;
  var d = document.documentElement; if (!d) return false;
  host = document.createElement('div');
  host.style.cssText = 'position:fixed;left:0;top:0;width:0;height:0;overflow:visible;z-index:2147483647;pointer-events:none';
  root = host.attachShadow({mode:'closed'});
  curs = {};
  d.appendChild(host);
  return true;
}
function arrow(c){
  var ns = 'http://www.w3.org/2000/svg', el = document.createElement('div');
  el.style.cssText = 'position:fixed;left:0;top:0;width:22px;height:22px;pointer-events:none;transition:transform .16s linear;will-change:transform;filter:drop-shadow(0 1px 2px rgba(0,0,0,.55))';
  var svg = document.createElementNS(ns, 'svg'); svg.setAttribute('width', '22'); svg.setAttribute('height', '22'); svg.setAttribute('viewBox', '0 0 24 24');
  var p = document.createElementNS(ns, 'path'); p.setAttribute('d', 'M4 2l16 9-7 2-3 7z'); p.setAttribute('fill', c); p.setAttribute('stroke', '#fff'); p.setAttribute('stroke-width', '1.6'); p.setAttribute('stroke-linejoin', 'round');
  svg.appendChild(p); el.appendChild(svg); return el;
}
function def(n, f){ try { Object.defineProperty(window, n, {value: f}); } catch (e) {} }
def('__sdA', function(){ var e = se(); return {i:st.i, x:st.x, y:st.y, t:st.t, u:st.u, ap:st.ap, sy:scrollY, sh:e ? e.scrollHeight : 0, vw:innerWidth, vh:innerHeight}; });
def('__sdCur', function(id, c, fx, fy){
  if (!ensure()) return;
  var el = curs[id];
  if (fx < 0) { if (el) { el.remove(); delete curs[id]; } return; }
  if (!el) { el = arrow(c); root.appendChild(el); curs[id] = el; }
  el._x = fx; el._y = fy; el._t = now(); place(el);
});
def('__sdScroll', function(y, h){
  var e = se(); if (!e) return;
  var sh = e.scrollHeight, top = (h > 0 && Math.abs(sh - h) / h > 0.1) ? y / h * sh : y;
  st.ap = now();
  scrollTo({top: top, behavior: 'instant'});
});
setInterval(function(){ var n = now(); for (var k in curs) { if (n - curs[k]._t > 6000) { curs[k].remove(); delete curs[k]; } } }, 2000);
})();"#;

const AGENT_READ_JS: &str = "(window.__sdA?window.__sdA():null)";

struct AgentRead {
    input: f64,
    x: f64,
    y: f64,
    t: f64,
    user_scroll: f64,
    applied: f64,
    sy: f64,
    sh: f64,
    vw: f64,
}

/// Ответ страницы: берём только числа и проверяем их.
fn parse_agent(raw: &str) -> Option<AgentRead> {
    let mut v: serde_json::Value = serde_json::from_str(raw).ok()?;
    if let serde_json::Value::String(s) = &v {
        v = serde_json::from_str(s).ok()?;
    }
    let n = |k: &str| v.get(k).and_then(|x| x.as_f64()).filter(|x| x.is_finite());
    Some(AgentRead {
        input: n("i")?,
        x: n("x")?,
        y: n("y")?,
        t: n("t")?,
        user_scroll: n("u")?,
        applied: n("ap").unwrap_or(0.0),
        sy: n("sy")?.max(0.0),
        sh: n("sh")?.max(0.0),
        vw: n("vw")?,
    })
}

#[derive(Default)]
struct AgentState {
    url: String,
    t: f64,
    cursor_on: bool,
    cursor_at: Option<Instant>,
    sy: f64,
    scroll_at: Option<Instant>,
}

fn agent_step(app: &AppHandle, s: &mut AgentState, url: &str, a: Option<AgentRead>) {
    if let Some(a) = &a {
        if a.input > 0.0 {
            LAST_INPUT.fetch_max((a.input as u64).min(now_ms()), Ordering::Relaxed);
        }
    }
    // адрес — уже после отметки клика: переход по ссылке не сочтём чужим
    if url != s.url {
        s.url = url.to_string();
        s.cursor_on = false;
        emit_state(app, url);
    }
    let Some(a) = a else { return };
    if !url.starts_with("http") {
        return;
    }
    let now = Instant::now();
    let main = EventTarget::webview_window("main");
    if a.t != s.t && s.cursor_at.map(|t| now.duration_since(t) >= CURSOR_EVERY).unwrap_or(true) {
        s.t = a.t;
        let inside = a.x >= 0.0 && a.vw > 0.0 && a.sh > 0.0;
        if inside || s.cursor_on {
            let (x, y) = if inside { ((a.x / a.vw).clamp(0.0, 1.0), ((a.y + a.sy) / a.sh).clamp(0.0, 1.0)) } else { (-1.0, -1.0) };
            s.cursor_on = inside;
            s.cursor_at = Some(now);
            let _ = app.emit_to(main.clone(), "ytb-cursor", serde_json::json!({ "x": x, "y": y, "url": url }));
        }
    }
    if a.applied > 0.0 && (now_ms() as f64 - a.applied) < 600.0 {
        // прокрутку только что применили от участника — не отсылаем её обратно
        s.sy = a.sy;
        return;
    }
    let user_recent = a.user_scroll > 0.0 && (now_ms() as f64 - a.user_scroll) < 1500.0;
    if user_recent && (a.sy - s.sy).abs() >= 4.0 && s.scroll_at.map(|t| now.duration_since(t) >= SCROLL_EVERY).unwrap_or(true) {
        s.sy = a.sy;
        s.scroll_at = Some(now);
        let _ = app.emit_to(main, "ytb-scroll", serde_json::json!({ "y": a.sy, "h": a.sh, "url": url }));
    }
}

/// В режиме «вместе» 8 раз в секунду читаем страницу: адрес, мышь, прокрутку.
fn spawn_agent_watch(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let mut s = AgentState::default();
        loop {
            tokio::time::sleep(AGENT_TICK).await;
            let Some(view) = app.get_webview(VIEW) else { break };
            if !SHARE.load(Ordering::Relaxed) || !OPEN.load(Ordering::Relaxed) {
                s = AgentState::default();
                continue;
            }
            while rx.try_recv().is_ok() {} // запоздавшие ответы
            let reply = tx.clone();
            if view.eval_with_callback(AGENT_READ_JS, move |raw| { let _ = reply.send(raw); }).is_err() {
                continue;
            }
            let raw = match tokio::time::timeout(Duration::from_millis(600), rx.recv()).await {
                Ok(Some(r)) => r,
                _ => continue,
            };
            let Ok(url) = view.url().map(|u| u.to_string()) else { continue };
            agent_step(&app, &mut s, &url, parse_agent(&raw));
        }
    });
}

fn same_page(view: &Webview, url: &str) -> bool {
    view.url().map(|u| url_no_fragment(u.as_str()) == url_no_fragment(url)).unwrap_or(false)
}

/// Курсор участника (`slot` — номер цвета). x < 0 — убрать курсор.
#[tauri::command]
pub async fn ytb_remote_cursor(app: AppHandle, slot: usize, x: f64, y: f64, url: String) -> Result<(), String> {
    if !SHARE.load(Ordering::Relaxed) {
        return Ok(());
    }
    let Some(view) = open_view(&app) else { return Ok(()) };
    let color = PALETTE.get(slot).ok_or("bad slot")?;
    if !x.is_finite() || !y.is_finite() {
        return Err("bad position".into());
    }
    // на другой странице курсор не показываем
    let (x, y) = if x >= 0.0 && same_page(&view, &url) { (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)) } else { (-1.0, -1.0) };
    view.eval(format!("window.__sdCur&&window.__sdCur({slot},'{color}',{x},{y})")).map_err(|e| e.to_string())
}

/// Прокрутка участника: y — отступ сверху, h — высота его страницы.
#[tauri::command]
pub async fn ytb_remote_scroll(app: AppHandle, y: f64, h: f64, url: String) -> Result<(), String> {
    if !SHARE.load(Ordering::Relaxed) {
        return Ok(());
    }
    let Some(view) = open_view(&app) else { return Ok(()) };
    let ok = |v: f64| v.is_finite() && (0.0..=50_000_000.0).contains(&v);
    if !ok(y) || !ok(h) {
        return Err("bad scroll".into());
    }
    if !same_page(&view, &url) {
        return Ok(());
    }
    view.eval(format!("window.__sdScroll&&window.__sdScroll({y},{h})")).map_err(|e| e.to_string())
}

/// Кто листает вместе и каким цветом — для панели браузера.
#[tauri::command]
pub async fn ytb_people(app: AppHandle, data: serde_json::Value) -> Result<(), String> {
    if app.get_webview(BAR).is_none() {
        return Ok(());
    }
    app.emit_to(EventTarget::webview(BAR), "ytb-people", data).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_reply_is_validated() {
        let ok = r#"{"i":1,"x":10,"y":20,"t":3,"u":0,"sy":100,"sh":2000,"vw":800,"vh":600}"#;
        let a = parse_agent(ok).expect("parsed");
        assert_eq!(a.sy, 100.0);
        assert!(parse_agent("null").is_none());
        assert!(parse_agent(r#"{"i":"x"}"#).is_none());
    }

    #[test]
    fn app_hosts_are_blocked() {
        assert!(!nav_allowed(&Url::parse("http://ipc.localhost/x").unwrap()));
        assert!(nav_allowed(&Url::parse("https://www.youtube.com/").unwrap()));
        assert!(nav_allowed(&start_url()));
    }
}

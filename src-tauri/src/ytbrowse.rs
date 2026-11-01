//! Окно «YouTube»: полноценный youtube.com внутри приложения.
//!
//! Окно `ytb` из двух webview:
//! - `ytb-bar` — наша панель сверху (локальный `yt-browser.html`, есть IPC);
//! - `ytb-view` — сам youtube.com. Ни одна capability не выдаётся этой
//!   метке, поэтому удалённая страница не может вызывать команды приложения.
//!
//! Текущий адрес и заголовок страницы Rust читает сам (без внедрения скриптов)
//! и рассылает событием `ytb-url` панели и главному окну. Кнопка
//! «Смотреть вместе» в панели шлёт главному окну `ytb-watch` с адресом,
//! а дальше работает обычная сессия совместного просмотра.

use std::sync::Mutex;
use std::time::Duration;
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
const HOME: &str = "https://www.youtube.com/";

/// Последний заголовок страницы YouTube (приходит из on_document_title_changed).
static TITLE: Mutex<String> = Mutex::new(String::new());

/// Домены, по которым можно ходить внутри окна: сам YouTube и вход в Google.
/// Остальные ссылки (реклама, описания) открываются в обычном браузере.
fn host_allowed(url: &Url) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    let host = match url.host_str() {
        Some(h) => h.to_ascii_lowercase(),
        None => return false,
    };
    let ends = |d: &str| host == d || host.ends_with(&format!(".{d}"));
    ends("youtube.com")
        || ends("youtu.be")
        || ends("youtube-nocookie.com")
        || ends("google.com")
        || ends("gstatic.com")
        || host.starts_with("google.")
        || host.contains(".google.")
}

fn open_external(url: &Url) {
    if matches!(url.scheme(), "http" | "https") {
        let _ = open::that(url.as_str());
    }
}

/// Строка из поля ввода: ссылка YouTube — открываем её, иначе ищем по тексту.
fn input_to_url(input: &str) -> Url {
    let s = input.trim();
    let with_scheme = if s.starts_with("http://") || s.starts_with("https://") {
        s.to_string()
    } else if s.contains("youtube.com/") || s.contains("youtu.be/") {
        format!("https://{s}")
    } else {
        String::new()
    };
    if let Ok(u) = Url::parse(&with_scheme) {
        if host_allowed(&u) {
            return u;
        }
    }
    let mut u = Url::parse("https://www.youtube.com/results").expect("static url");
    u.query_pairs_mut().append_pair("search_query", s);
    u
}

fn emit_state(app: &AppHandle, url: &str) {
    let title = TITLE.lock().map(|t| t.clone()).unwrap_or_default();
    let payload = serde_json::json!({ "url": url, "title": title });
    let _ = app.emit_to(EventTarget::webview(BAR), "ytb-url", payload.clone());
    let _ = app.emit_to(EventTarget::webview_window("main"), "ytb-url", payload);
}

/// Раскладка: панель сверху фиксированной высоты, YouTube — всё остальное.
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

/// Следим за адресом: YouTube — одностраничное приложение, переходы между
/// видео идут через history API без настоящей навигации, поэтому опрашиваем.
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
            let title = TITLE.lock().map(|t| t.clone()).unwrap_or_default();
            if url != last || title != last_title {
                last = url.clone();
                last_title = title;
                emit_state(&app, &url);
            }
        }
    });
}

#[tauri::command]
pub async fn ytb_open(app: AppHandle, url: Option<String>) -> Result<(), String> {
    let target = url.as_deref().filter(|s| !s.trim().is_empty()).map(input_to_url);
    if let Some(win) = app.get_window(WIN) {
        if let (Some(u), Some(view)) = (target, app.get_webview(VIEW)) {
            view.navigate(u).map_err(|e| e.to_string())?;
        }
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(());
    }

    let start = target.unwrap_or_else(|| Url::parse(HOME).expect("static url"));
    let win = WindowBuilder::new(&app, WIN)
        .title("YouTube — SafeDrop")
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
    let view = WebviewBuilder::new(VIEW, WebviewUrl::External(start))
        .on_navigation(|u| {
            if host_allowed(u) || u.scheme() == "about" || u.scheme() == "blob" {
                return true;
            }
            open_external(u);
            false
        })
        .on_new_window(|u, _| {
            // target=_blank: YouTube открываем здесь же, чужое — в браузере
            if host_allowed(&u) {
                return NewWindowResponse::Deny;
            }
            open_external(&u);
            NewWindowResponse::Deny
        })
        .on_document_title_changed(move |_, title| {
            let clean = title.trim_end_matches(" - YouTube").to_string();
            if let Ok(mut t) = TITLE.lock() {
                *t = clean;
            }
            if let Some(v) = app_title.get_webview(VIEW) {
                if let Ok(u) = v.url() {
                    emit_state(&app_title, u.as_str());
                }
            }
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
    layout(&app);
    spawn_url_watch(app.clone());
    Ok(())
}

/// Переход по строке из панели: ссылка или поисковый запрос.
#[tauri::command]
pub async fn ytb_go(app: AppHandle, input: String) -> Result<(), String> {
    let view = app.get_webview(VIEW).ok_or("окно YouTube закрыто")?;
    view.navigate(input_to_url(&input)).map_err(|e| e.to_string())
}

/// Кнопки панели. Назад/вперёд — единственное место, где в страницу уходит
/// скрипт: фиксированная строка, страница ничего не получает в ответ.
#[tauri::command]
pub async fn ytb_nav(app: AppHandle, action: String) -> Result<(), String> {
    let view = app.get_webview(VIEW).ok_or("окно YouTube закрыто")?;
    match action.as_str() {
        "back" => view.eval("history.back()"),
        "forward" => view.eval("history.forward()"),
        "reload" => view.reload(),
        "home" => view.navigate(Url::parse(HOME).expect("static url")),
        _ => return Err("unknown action".into()),
    }
    .map_err(|e| e.to_string())
}

/// «Смотреть вместе»: ставим видео в окне на паузу и отдаём адрес главному окну.
#[tauri::command]
pub async fn ytb_watch(app: AppHandle) -> Result<(), String> {
    let view = app.get_webview(VIEW).ok_or("окно YouTube закрыто")?;
    let url = view.url().map_err(|e| e.to_string())?.to_string();
    let title = TITLE.lock().map(|t| t.clone()).unwrap_or_default();
    let _ = view.eval("document.querySelectorAll('video').forEach(function(v){v.pause()})");
    app.emit_to(
        EventTarget::webview_window("main"),
        "ytb-watch",
        serde_json::json!({ "url": url, "title": title }),
    )
    .map_err(|e| e.to_string())?;
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.unminimize();
        let _ = main.show();
        let _ = main.set_focus();
    }
    Ok(())
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
    let title = TITLE.lock().map(|t| t.clone()).unwrap_or_default();
    Ok(serde_json::json!({ "url": url, "title": title }))
}

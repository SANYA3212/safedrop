//! Linux: интерфейс показывает Electron (Chromium), а это Rust-ядро работает без
//! своего окна. WebRTC в WebKitGTK плохо совместим с Chromium в Windows (звук,
//! RNNoise, демонстрация экрана), в Electron он тот же, что в WebView2.
//!
//! Electron запускает ядро с `SAFEDROP_ELECTRON_CORE=1` и двумя каналами:
//! fd 3 — запросы `{"id":1,"cmd":"get_peers","args":{...}}`, по строке JSON;
//! fd 4 — ответы `{"id":1,"ok":...}` / `{"id":1,"err":...}`, события
//! `{"event":"safedrop-peers","payload":...}` и команды окну `{"ctl":"focus"}`.
//! Запросы идут через обычный путь команд Tauri (`on_message`) скрытого окна
//! "main": те же команды, права и проверки, что у окна WebKit.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::FromRawFd;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};
use tauri::ipc::{CallbackFn, InvokeBody, InvokeError, InvokeResponse, InvokeResponseBody};
use tauri::webview::InvokeRequest;
use tauri::{AppHandle, Listener, Manager};

use crate::state::AppState;

/// Все события, которые слушает интерфейс (`listen(...)` в app.js).
const EVENTS: &[&str] = &[
    "safedrop-call-answer", "safedrop-call-offer", "safedrop-cancelled", "safedrop-chat",
    "safedrop-chat-ack", "safedrop-chat-changed", "safedrop-ctx-send", "safedrop-file-offer",
    "safedrop-groups", "safedrop-hangup", "safedrop-ice", "safedrop-key-changed",
    "safedrop-linux-update-available", "safedrop-peer-lost", "safedrop-peers", "safedrop-profile",
    "safedrop-quit-requested", "safedrop-received", "safedrop-reset", "safedrop-scan-progress",
    "safedrop-screen-answer", "safedrop-screen-ice", "safedrop-screen-offer", "safedrop-screen-stop",
    "safedrop-settings", "safedrop-transfers", "safedrop-typing", "safedrop-update-offer",
    "safedrop-watch", "safedrop-web",
    "ytb-chat-request", "ytb-chat-send", "ytb-close-all", "ytb-cursor", "ytb-media", "ytb-scroll",
    "ytb-watch",
];

/// События, которые Rust шлёт отдельно окну "main" и панели браузера
/// (`emit_to`): через `listen_any` пришли бы дважды — слушаем скрытое окно.
const MAIN_ONLY_EVENTS: &[&str] = &["ytb-url", "ytb-share", "ytb-closed"];

/// Самая длинная строка запроса: картинка в чате приходит data-URL
const MAX_LINE: u64 = 64 * 1024 * 1024;

static OUT: OnceLock<Mutex<std::fs::File>> = OnceLock::new();

/// Ядро запущено из Electron (а не как обычное окно Tauri).
pub fn active() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("SAFEDROP_ELECTRON_CORE").as_deref() == Ok("1")
            // каналы должен открыть Electron; без них не трогаем чужие fd
            && std::path::Path::new("/proc/self/fd/3").exists()
            && std::path::Path::new("/proc/self/fd/4").exists()
    })
}

fn send(v: &Value) {
    let Some(out) = OUT.get() else { return };
    let mut line = v.to_string();
    line.push('\n');
    if let Ok(mut f) = out.lock() {
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
    }
}

/// Показать/скрыть окно Electron (трей, уведомления, второй запуск).
pub fn control(what: &str) {
    send(&json!({ "ctl": what }));
}

/// Спрятать окно WebKit (в нём не должен работать второй интерфейс) и начать
/// обслуживать Electron. Вызывать в начале `setup`, до запуска цикла событий.
pub fn start(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
        // пустая страница вместо index.html: app.js в скрытом окне не запустится
        if let Ok(mut url) = w.url() {
            url.set_path("/core.html");
            url.set_query(None);
            let _ = w.navigate(url);
        }
    }
    // SAFETY: active() проверил, что fd 3 и 4 открыты; их создал Electron
    // только для ядра, больше ими никто в процессе не владеет.
    let input = unsafe { std::fs::File::from_raw_fd(3) };
    let output = unsafe { std::fs::File::from_raw_fd(4) };
    let _ = OUT.set(Mutex::new(output));

    let forward = |name: &'static str| {
        move |ev: tauri::Event| {
            let payload: Value = serde_json::from_str(ev.payload()).unwrap_or(Value::Null);
            send(&json!({ "event": name, "payload": payload }));
        }
    };
    for name in EVENTS {
        app.listen_any(*name, forward(name));
    }
    if let Some(w) = app.get_webview_window("main") {
        for name in MAIN_ONLY_EVENTS {
            w.listen(*name, forward(name));
        }
    }

    let app = app.clone();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(input);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.by_ref().take(MAX_LINE).read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if !line.ends_with('\n') {
                // строка длиннее предела или оборвана: канал больше не разобрать
                eprintln!("SafeDrop: слишком большой или оборванный запрос от Electron");
                break;
            }
            handle(&app, line.trim_end());
        }
        // Electron закрылся — ядро тоже выходит, дописав историю
        crate::settings::save_chats_now(&app.state::<AppState>());
        app.exit(0);
    });
}

fn handle(app: &AppHandle, line: &str) {
    if line.is_empty() {
        return;
    }
    let Ok(req) = serde_json::from_str::<Value>(line) else {
        return;
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let Some(cmd) = req.get("cmd").and_then(Value::as_str).map(str::to_owned) else {
        send(&json!({ "id": id, "err": "нет команды" }));
        return;
    };
    let args = match req.get("args") {
        Some(a @ Value::Object(_)) => a.clone(),
        _ => json!({}),
    };
    let Some(w) = app.get_webview_window("main") else {
        send(&json!({ "id": id, "err": "ядро не готово" }));
        return;
    };
    let url = match w.url() {
        Ok(u) => u,
        Err(e) => {
            send(&json!({ "id": id, "err": e.to_string() }));
            return;
        }
    };
    let request = InvokeRequest {
        cmd,
        callback: CallbackFn(0),
        error: CallbackFn(1),
        url,
        body: InvokeBody::Json(args),
        headers: Default::default(),
        invoke_key: app.invoke_key().to_string(),
    };
    let responder = Box::new(move |_wv, _cmd, response: InvokeResponse, _cb, _err| {
        let msg = match response {
            InvokeResponse::Ok(InvokeResponseBody::Json(s)) => {
                json!({ "id": id, "ok": serde_json::from_str::<Value>(&s).unwrap_or(Value::Null) })
            }
            InvokeResponse::Ok(InvokeResponseBody::Raw(bytes)) => json!({ "id": id, "ok": bytes }),
            InvokeResponse::Err(InvokeError(v)) => json!({ "id": id, "err": v }),
        };
        send(&msg);
    });
    // как настоящий IPC: команда стартует в главном потоке
    let _ = app.run_on_main_thread(move || w.on_message(request, responder));
}

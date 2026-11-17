//! Android: то, что на компьютере дают Windows и окна выбора файлов, здесь
//! берётся у Android — папки приложения и «Загрузок» (через JNI), уведомления
//! (плагин уведомлений Tauri), открытие ссылок и файлов (свой небольшой плагин
//! `SdAndroidPlugin` на Kotlin в `gen/android`). Файлы для отправки выбирает
//! интерфейс (`mobile.js`) и по частям складывает сюда, в папку `outbox`.
//!
//! Сеть, ключи, чаты и протокол — общие с Windows-версией, здесь их нет.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tauri::{AppHandle, Manager, Runtime};

const PACKAGE: &str = "com.safedrop.app";
/// одна часть файла из интерфейса (base64): чуть больше 2 МБ на часть
const MAX_CHUNK_B64: usize = 3 * 1024 * 1024;
/// файлы, выбранные для отправки, живут в `outbox` столько дней
const OUTBOX_DAYS: u64 = 3;

// ---------- папки (JNI) ----------

/// Вызвать Java через контекст приложения. До запуска Activity контекста нет —
/// тогда `None`, и берутся запасные пути.
fn with_context<T>(
    f: impl FnOnce(&mut jni::JNIEnv, &jni::objects::JObject) -> jni::errors::Result<T>,
) -> Option<T> {
    let ctx = std::panic::catch_unwind(ndk_context::android_context).ok()?;
    if ctx.vm().is_null() || ctx.context().is_null() {
        return None;
    }
    // SAFETY: указатели JavaVM и Context приложения кладёт ndk-context при
    // запуске Activity; они живут всё время работы процесса
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.ok()?;
    let mut env = vm.attach_current_thread().ok()?;
    let context = unsafe { jni::objects::JObject::from_raw(ctx.context().cast()) };
    let r = f(&mut env, &context);
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
    r.ok()
}

fn file_path(env: &mut jni::JNIEnv, file: jni::objects::JObject) -> jni::errors::Result<Option<PathBuf>> {
    if file.is_null() {
        return Ok(None);
    }
    let s = env.call_method(&file, "getAbsolutePath", "()Ljava/lang/String;", &[])?.l()?;
    let s: String = env.get_string(&jni::objects::JString::from(s))?.into();
    Ok(Some(PathBuf::from(s)))
}

/// Закрытая папка приложения (`/data/user/0/com.safedrop.app/files`).
pub fn data_dir() -> PathBuf {
    static D: OnceLock<PathBuf> = OnceLock::new();
    D.get_or_init(|| {
        with_context(|env, ctx| {
            let f = env.call_method(ctx, "getFilesDir", "()Ljava/io/File;", &[])?.l()?;
            file_path(env, f)
        })
        .flatten()
        .unwrap_or_else(|| PathBuf::from(format!("/data/data/{PACKAGE}/files")))
    })
    .clone()
}

fn public_downloads() -> Option<PathBuf> {
    with_context(|env, _| {
        let name = env.new_string("Download")?;
        let f = env
            .call_static_method(
                "android/os/Environment",
                "getExternalStoragePublicDirectory",
                "(Ljava/lang/String;)Ljava/io/File;",
                &[jni::objects::JValue::Object(&name)],
            )?
            .l()?;
        file_path(env, f)
    })
    .flatten()
}

fn external_files() -> Option<PathBuf> {
    with_context(|env, ctx| {
        let f = env
            .call_method(
                ctx,
                "getExternalFilesDir",
                "(Ljava/lang/String;)Ljava/io/File;",
                &[jni::objects::JValue::Object(&jni::objects::JObject::null())],
            )?
            .l()?;
        file_path(env, f)
    })
    .flatten()
}

fn writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".safedrop-probe");
    let ok = std::fs::write(&probe, b"1").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Куда сохранять полученные файлы: «Загрузки/SafeDrop» (видно в файлах
/// телефона), а если туда писать нельзя (старый Android без разрешения) —
/// папка приложения на памяти телефона.
pub fn loaddrop_dir() -> PathBuf {
    if let Some(d) = public_downloads().map(|d| d.join("SafeDrop")) {
        if writable(&d) {
            return d;
        }
    }
    external_files()
        .map(|d| d.join("LoadDrop"))
        .unwrap_or_else(|| data_dir().join("LoadDrop"))
}

// ---------- уведомления ----------

static APP: OnceLock<AppHandle> = OnceLock::new();

pub fn init(app: &AppHandle) {
    let _ = APP.set(app.clone());
    clean_outbox();
}

/// Уведомление Android (разрешение спрашивает интерфейс при входе).
pub fn notify(title: &str, body: &str) {
    use tauri_plugin_notification::NotificationExt;
    if let Some(app) = APP.get() {
        // значок — res/drawable/ic_notification.xml (настроек у плагина в Rust нет:
        // `plugins.notification` в tauri.conf ломает запуск)
        if let Err(e) = app.notification().builder().title(title).body(body).icon("ic_notification").show() {
            eprintln!("SafeDrop notify failed: {e}");
        }
    }
}

// ---------- окна выбора файлов ----------

/// Окон выбора файлов из Rust на Android нет: файлы выбирает интерфейс
/// (`mobile.js`), а этот заменитель только сохраняет код компьютера как есть.
pub struct NoDialog;

impl NoDialog {
    pub fn set_title(self, _: &str) -> Self {
        self
    }
    pub fn add_filter(self, _: &str, _: &[&str]) -> Self {
        self
    }
    pub fn pick_files(self) -> Option<Vec<PathBuf>> {
        None
    }
    pub fn pick_file(self) -> Option<PathBuf> {
        None
    }
    pub fn pick_folder(self) -> Option<PathBuf> {
        None
    }
}

// ---------- файлы для отправки ----------

fn outbox() -> PathBuf {
    data_dir().join("outbox")
}

/// Мелодии и фон окна, выбранные в настройках, — навсегда (до «Сбросить всё»).
fn media_dir() -> PathBuf {
    data_dir().join("media")
}

/// Старые выбранные файлы не копить: при запуске убрать всё старше трёх дней.
fn clean_outbox() {
    let Ok(rd) = std::fs::read_dir(outbox()) else { return };
    let limit = std::time::Duration::from_secs(OUTBOX_DAYS * 24 * 3600);
    for e in rd.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > limit);
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Убрать все выбранные файлы, мелодии и фон («Сбросить всё»).
pub fn clear_outbox() {
    let _ = std::fs::remove_dir_all(outbox());
    let _ = std::fs::remove_dir_all(media_dir());
}

fn valid_stage_id(id: &str) -> bool {
    (8..=40).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// Имя файла от интерфейса: только последнее звено, без управляющих символов.
fn safe_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = last
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .take(150)
        .collect();
    let clean = clean.trim().trim_start_matches('.').to_string();
    if clean.is_empty() { "file".into() } else { clean }
}

/// Часть выбранного файла: `offset` 0 начинает файл заново, следующие части
/// дописываются строго по порядку. `keep` — мелодия или фон из настроек: они
/// лежат отдельно и старыми не удаляются. Возвращает путь к файлу.
#[tauri::command]
pub async fn m_stage_chunk(id: String, name: String, offset: u64, data: String, keep: Option<bool>) -> Result<String, String> {
    use base64::Engine;
    use std::io::Write;
    if !valid_stage_id(&id) {
        return Err("bad id".into());
    }
    if data.len() > MAX_CHUNK_B64 {
        return Err("chunk too big".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(data.as_bytes()).map_err(|e| e.to_string())?;
    let dir = if keep.unwrap_or(false) { media_dir() } else { outbox() }.join(&id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(safe_name(&name));
    tokio::task::spawn_blocking(move || {
        let mut f = if offset == 0 {
            std::fs::File::create(&path)
        } else {
            std::fs::OpenOptions::new().append(true).open(&path)
        }
        .map_err(|e| e.to_string())?;
        let len = f.metadata().map_err(|e| e.to_string())?.len();
        if len != offset {
            return Err(format!("часть не по порядку: {len} ≠ {offset}"));
        }
        f.write_all(&bytes).map_err(|e| e.to_string())?;
        Ok(path.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- список чатов ----------

/// Последнее сообщение каждого чата — для списка чатов, как в Telegram:
/// без картинок (они тяжёлые), только текст, вид вложения и время.
#[tauri::command]
pub async fn m_chat_previews(state: tauri::State<'_, crate::state::AppState>) -> Result<serde_json::Value, String> {
    let chats = state.chats.lock().await;
    let mut out = serde_json::Map::new();
    for (key, rows) in chats.iter() {
        let Some(m) = rows.iter().max_by_key(|m| m.sent_at) else { continue };
        let kind = if !m.images.is_empty() || !m.image.is_empty() {
            "image"
        } else if !m.audio.is_empty() {
            "media"
        } else {
            "text"
        };
        let media = Path::new(&m.audio).file_name().and_then(|n| n.to_str()).unwrap_or("");
        out.insert(
            key.clone(),
            serde_json::json!({
                "text": m.text.chars().take(160).collect::<String>(),
                "at": m.sent_at,
                "me": m.from_me,
                "nick": m.from_nick,
                "kind": kind,
                "media": media,
                "status": m.status,
            }),
        );
    }
    Ok(serde_json::Value::Object(out))
}

// ---------- открыть ссылку или файл (плагин на Kotlin) ----------

struct Native<R: Runtime>(tauri::plugin::PluginHandle<R>);

pub fn plugin<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("sdandroid")
        .setup(|app, api| {
            let h = api.register_android_plugin(PACKAGE, "SdAndroidPlugin")?;
            app.manage(Native(h));
            Ok(())
        })
        .build()
}

fn run_native(app: &AppHandle, method: &str, args: serde_json::Value) -> Result<(), String> {
    let n = app.try_state::<Native<tauri::Wry>>().ok_or("нет плагина Android")?;
    n.0.run_mobile_plugin::<serde_json::Value>(method, args)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub async fn open_url(app: AppHandle, url: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || run_native(&app, "openUrl", serde_json::json!({ "url": url })))
        .await
        .map_err(|e| e.to_string())?
}

/// Открыть полученный файл подходящим приложением телефона. Только файлы из
/// папки полученных (LoadDrop) и записей голосовых — не произвольный путь.
#[tauri::command]
pub async fn m_open_file(app: AppHandle, state: tauri::State<'_, crate::state::AppState>, path: String) -> Result<(), String> {
    let loaddrop = PathBuf::from(state.settings.lock().await.loaddrop_dir.clone());
    let p = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
    let allowed = [loaddrop, state.app_dir.clone(), outbox(), media_dir()]
        .iter()
        .filter_map(|d| std::fs::canonicalize(d).ok())
        .any(|d| p.starts_with(&d));
    if !allowed || !p.is_file() {
        return Err("этот файл открыть нельзя".into());
    }
    let p = p.to_string_lossy().to_string();
    tokio::task::spawn_blocking(move || run_native(&app, "openFile", serde_json::json!({ "path": p })))
        .await
        .map_err(|e| e.to_string())?
}

// ---------- демонстрация экрана ----------
// Кадры снимает Android (ScreenCaptureService.kt) и отдаёт сюда JPEG-ами, а
// отсюда они сразу уходят в страницу событием `safedrop-screen-frame`. Не по
// таймеру страницы: пока показываешь другое приложение, SafeDrop в фоне, и
// таймеры страницы Android замедляет до раза в секунду. Следующий кадр уходит,
// только когда страница забрала прошлый (m_screen_ack), — иначе пропускается.

static FRAME_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static FRAME_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Кадр больше этого — не кадр экрана (страховка от мусора по JNI).
const MAX_FRAME: usize = 4 * 1024 * 1024;

#[no_mangle]
pub extern "system" fn Java_com_safedrop_app_ScreenCaptureService_nativeFrame(
    env: jni::JNIEnv,
    _class: jni::objects::JClass,
    data: jni::objects::JByteArray,
    w: jni::sys::jint,
    h: jni::sys::jint,
) {
    use std::sync::atomic::Ordering;
    use tauri::Emitter;
    if FRAME_BUSY.swap(true, Ordering::AcqRel) {
        return; // страница ещё рисует прошлый кадр
    }
    let Some(app) = APP.get() else { FRAME_BUSY.store(false, Ordering::Release); return };
    let jpeg = match env.convert_byte_array(&data) {
        Ok(j) if !j.is_empty() && j.len() <= MAX_FRAME && w > 0 && h > 0 => j,
        _ => { FRAME_BUSY.store(false, Ordering::Release); return; }
    };
    use base64::Engine;
    let seq = FRAME_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    let payload = serde_json::json!({
        "seq": seq, "w": w, "h": h,
        "data": base64::engine::general_purpose::STANDARD.encode(&jpeg),
    });
    if app.emit("safedrop-screen-frame", payload).is_err() {
        FRAME_BUSY.store(false, Ordering::Release);
    }
}

#[no_mangle]
pub extern "system" fn Java_com_safedrop_app_ScreenCaptureService_nativeStopped(
    _env: jni::JNIEnv,
    _class: jni::objects::JClass,
) {
    FRAME_BUSY.store(false, std::sync::atomic::Ordering::Release);
    if let Some(app) = APP.get() {
        use tauri::Emitter;
        let _ = app.emit("safedrop-screen-stopped", ());
    }
}

/// Начать показ экрана: Android спросит разрешение, потом запустит службу.
#[tauri::command]
pub async fn m_screen_start(app: AppHandle, fps: u32, max: u32) -> Result<(), String> {
    FRAME_BUSY.store(false, std::sync::atomic::Ordering::Release);
    let args = serde_json::json!({ "fps": fps.clamp(1, 30), "max": max.clamp(320, 1920) });
    tokio::task::spawn_blocking(move || run_native(&app, "startScreenCapture", args))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn m_screen_stop(app: AppHandle) -> Result<(), String> {
    tokio::task::spawn_blocking(move || run_native(&app, "stopScreenCapture", serde_json::json!({})))
        .await
        .map_err(|e| e.to_string())?
}

/// Страница нарисовала кадр — можно слать следующий.
#[tauri::command]
pub fn m_screen_ack() {
    FRAME_BUSY.store(false, std::sync::atomic::Ordering::Release);
}

// ---------- работа в фоне и звонок, когда окна нет ----------
//
// Окно закрыли (смахнули, «назад»), а процесс живёт: держит его служба
// `KeepAliveService`, а `lib.rs` не даёт Tauri завершиться вместе с окном.
// Связь с сервером остаётся. Входящий звонок, пока окна не видно, показывает
// Android (`CallNotifier.kt`) — на весь экран, со звонком. Сам звонок (offer и
// кандидаты ICE) ждёт здесь: новая страница забирает его (`m_replay_call`) и
// обрабатывает так, будто он только что пришёл.

/// Звонок дольше этого без ответа — уже не звонок.
const CALL_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
const CALL_ICE_MAX: usize = 64;

struct PendingCall {
    ip: String,
    offer: serde_json::Value,
    ice: Vec<serde_json::Value>,
    at: std::time::Instant,
    answer: bool,
    title: String,
}

fn pending_call() -> &'static std::sync::Mutex<Option<PendingCall>> {
    static P: std::sync::Mutex<Option<PendingCall>> = std::sync::Mutex::new(None);
    &P
}

/// Вызвать статический метод Kotlin-класса приложения (из любого потока:
/// класс берётся через загрузчик классов приложения).
fn call_static(class: &str, method: &str, sig: &str, args: &[jni::objects::JValue]) {
    let _ = with_context(|env, ctx| {
        let loader = env.call_method(ctx, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?.l()?;
        let name = env.new_string(format!("{PACKAGE}.{class}"))?;
        let cls = env.call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[(&name).into()])?.l()?;
        let cls = jni::objects::JClass::from(cls);
        let mut full: Vec<jni::objects::JValue> = vec![jni::objects::JValue::Object(ctx)];
        full.extend_from_slice(args);
        env.call_static_method(&cls, method, sig, &full)?;
        Ok(())
    });
}

fn notify_call(ip: &str, title: &str, text: &str) {
    let _ = with_context(|env, ctx| {
        let loader = env.call_method(ctx, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?.l()?;
        let name = env.new_string(format!("{PACKAGE}.CallNotifier"))?;
        let cls = env.call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[(&name).into()])?.l()?;
        let cls = jni::objects::JClass::from(cls);
        let (ip, title, text) = (env.new_string(ip)?, env.new_string(title)?, env.new_string(text)?);
        env.call_static_method(
            &cls,
            "show",
            "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
            &[ctx.into(), (&ip).into(), (&title).into(), (&text).into()],
        )?;
        Ok(())
    });
}

fn missed_call(title: &str, text: &str) {
    let _ = with_context(|env, ctx| {
        let loader = env.call_method(ctx, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?.l()?;
        let name = env.new_string(format!("{PACKAGE}.CallNotifier"))?;
        let cls = env.call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[(&name).into()])?.l()?;
        let cls = jni::objects::JClass::from(cls);
        let (title, text) = (env.new_string(title)?, env.new_string(text)?);
        env.call_static_method(
            &cls,
            "missed",
            "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;)V",
            &[ctx.into(), (&title).into(), (&text).into()],
        )?;
        Ok(())
    });
}

/// Входящий звонок: запомнить для страницы и позвонить уведомлением (если
/// окна не видно — это решает `CallNotifier`).
pub fn call_offer(ip: &str, offer: serde_json::Value, title: &str, text: &str) {
    {
        let mut g = pending_call().lock().unwrap_or_else(|e| e.into_inner());
        *g = Some(PendingCall {
            ip: ip.to_string(),
            offer,
            ice: Vec::new(),
            at: std::time::Instant::now(),
            answer: false,
            title: title.to_string(),
        });
    }
    notify_call(ip, title, text);
}

/// Кандидат ICE звонящего: пока звонок ждёт страницу — тоже ждёт.
pub fn call_ice(ip: &str, ice: serde_json::Value) {
    let mut g = pending_call().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = g.as_mut().filter(|p| p.ip == ip && p.ice.len() < CALL_ICE_MAX) {
        p.ice.push(ice);
    }
}

/// Звонок закончился до ответа здесь: звонящий сбросил (`missed`), ответили
/// или отклонили здесь или на другом своём устройстве.
pub fn call_gone(ip: &str, missed: bool) {
    let taken = {
        let mut g = pending_call().lock().unwrap_or_else(|e| e.into_inner());
        match g.as_ref() {
            Some(p) if p.ip == ip => g.take(),
            _ => None,
        }
    };
    let Some(p) = taken else { return };
    if missed && !p.answer {
        missed_call("Пропущенный звонок", &p.title);
    } else {
        call_static("CallNotifier", "cancel", "(Landroid/content/Context;)V", &[]);
    }
}

/// Новая страница: отдать ей звонок, который пришёл без неё. true — его уже
/// приняли кнопкой «Ответить» в уведомлении (страница сразу отвечает).
#[tauri::command]
pub fn m_replay_call(app: AppHandle) -> bool {
    use tauri::Emitter;
    let p = {
        let mut g = pending_call().lock().unwrap_or_else(|e| e.into_inner());
        match g.as_ref() {
            Some(p) if p.at.elapsed() < CALL_WAIT => {}
            _ => {
                *g = None;
                return false;
            }
        }
        g.take()
    };
    let Some(p) = p else { return false };
    let _ = app.emit("safedrop-call-offer", p.offer.clone());
    for ice in &p.ice {
        let _ = app.emit("safedrop-ice", ice.clone());
    }
    // ответ ещё может прийти: звонок остаётся «ждущим» до ответа страницы
    let answer = p.answer;
    *pending_call().lock().unwrap_or_else(|e| e.into_inner()) = Some(PendingCall { ice: Vec::new(), ..p });
    answer
}

/// Включить / выключить работу в фоне (служба с уведомлением).
#[tauri::command]
pub async fn m_keep_alive(app: AppHandle, on: bool) -> Result<(), String> {
    tokio::task::spawn_blocking(move || run_native(&app, "keepAlive", serde_json::json!({ "on": on })))
        .await
        .map_err(|e| e.to_string())?
}

/// Без ограничений ли батареи; `open` — попросить Android снять ограничения.
#[tauri::command]
pub async fn m_battery(app: AppHandle, open: bool) -> Result<bool, String> {
    tokio::task::spawn_blocking(move || {
        let n = app.try_state::<Native<tauri::Wry>>().ok_or("нет плагина Android")?;
        let v = n.0.run_mobile_plugin::<serde_json::Value>("battery", serde_json::json!({ "on": open })).map_err(|e| e.to_string())?;
        Ok(v.get("free").and_then(|x| x.as_bool()).unwrap_or(true))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Окно открыли снова, а SafeDrop всё это время работал в фоне: создать
/// страницу для нового окна (как при запуске).
#[no_mangle]
pub extern "system" fn Java_com_safedrop_app_MainActivity_nativeReopen(_env: jni::JNIEnv, _class: jni::objects::JClass) {
    let Some(app) = APP.get().cloned() else { return };
    std::thread::spawn(move || {
        if app.get_webview_window("main").is_some() {
            return;
        }
        let Some(cfg) = app.config().app.windows.first().cloned() else { return };
        if let Err(e) = tauri::WebviewWindowBuilder::from_config(&app, &cfg).and_then(|b| b.build()) {
            eprintln!("SafeDrop: окно не создалось заново: {e}");
        }
    });
}

fn jstring(env: &mut jni::JNIEnv, s: &jni::objects::JString) -> String {
    env.get_string(s).map(|x| x.to_string_lossy().chars().take(200).collect()).unwrap_or_default()
}

/// «Ответить» в уведомлении звонка.
#[no_mangle]
pub extern "system" fn Java_com_safedrop_app_MainActivity_nativeAnswer(mut env: jni::JNIEnv, _class: jni::objects::JClass, ip: jni::objects::JString) {
    use tauri::Emitter;
    let ip = jstring(&mut env, &ip);
    if let Some(p) = pending_call().lock().unwrap_or_else(|e| e.into_inner()).as_mut().filter(|p| p.ip == ip) {
        p.answer = true;
    }
    // страница жива (окно было свёрнуто) — пусть отвечает сразу
    if let Some(app) = APP.get() {
        let _ = app.emit("safedrop-answer-call", serde_json::json!({ "ip": ip }));
    }
}

/// «Отклонить» в уведомлении звонка: отбой звонящему, как кнопкой в окне.
#[no_mangle]
pub extern "system" fn Java_com_safedrop_app_MainActivity_nativeDecline(mut env: jni::JNIEnv, _class: jni::objects::JClass, ip: jni::objects::JString) {
    use tauri::Emitter;
    let ip = jstring(&mut env, &ip);
    let ours = pending_call().lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|p| p.ip == ip);
    if !ours {
        return;
    }
    call_gone(&ip, false);
    let Some(app) = APP.get().cloned() else { return };
    let _ = app.emit("safedrop-hangup", serde_json::json!({ "ip": ip, "from": "" }));
    tauri::async_runtime::spawn(async move {
        let state = app.state::<crate::state::AppState>();
        let (port, nick) = {
            let s = state.settings.lock().await;
            (s.sig_port, if s.profile.nickname.is_empty() { s.device_name.clone() } else { s.profile.nickname.clone() })
        };
        let v = serde_json::json!({ "kind": "call-hangup", "from": nick });
        let _ = crate::quic::send_frame_dual(&app, &ip, port, v, false).await;
    });
}

/// «Выключить» в уведомлении «SafeDrop на связи»: сохранить и завершиться.
#[no_mangle]
pub extern "system" fn Java_com_safedrop_app_MainActivity_nativeQuit(_env: jni::JNIEnv, _class: jni::objects::JClass) {
    if let Some(app) = APP.get() {
        let state = app.state::<crate::state::AppState>();
        crate::settings::save_chats_now(&state);
    }
    std::process::exit(0);
}

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
        if let Err(e) = app.notification().builder().title(title).body(body).show() {
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


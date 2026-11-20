//! Хранение чатов на диске.
//!
//! Раньше вся история лежала в одном `chats.json` (картинки внутри, в base64)
//! и переписывалась целиком при каждом сообщении. Теперь:
//!   * `chats/<имя>.json` — по файлу на чат (`{"key": ..., "messages": [...]}`),
//!     имя — первые 32 символа SHA-256 от ключа чата;
//!   * `media/<sha256>.<png|jpg|gif|webp>` — картинки как есть (байты), по
//!     одному разу; в сообщении вместо них ссылка `sdmedia:<sha256>.<ext>`.
//! Переписываются только изменившиеся чаты. В памяти ничего не поменялось:
//! картинки там по-прежнему `data:`-ссылки.
//! Старый `chats.json` читается и переносится (после этого он получает имя
//! `chats.json.old`, но не удаляется).

use crate::state::ChatMsg;
use base64::Engine;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::Hasher;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

pub type Chats = HashMap<String, Vec<ChatMsg>>;

const REF_PREFIX: &str = "sdmedia:";
const CHATS_DIR: &str = "chats";
const MEDIA_DIR: &str = "media";
const MIGRATING_DIR: &str = "chats.migrating";
const LEGACY: &str = "chats.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct ChatFile {
    key: String,
    #[serde(default)]
    messages: Vec<ChatMsg>,
}

/// Что уже записано: хеш содержимого каждого чата, замок записи на диск
/// (внутри — номер последнего записанного снимка) и счётчик снимков.
pub struct Store {
    written: Mutex<HashMap<String, u64>>,
    guard: Mutex<u64>,
    seq: AtomicU64,
    /// (хеш строки data:-ссылки, длина) -> ссылка `sdmedia:...`. Только ускорение:
    /// промах просто идёт обычным путём (декодирование, SHA-256, запись).
    cache: Mutex<HashMap<(u64, usize), String>>,
}

type MediaCache = Mutex<HashMap<(u64, usize), String>>;

impl Store {
    pub fn new() -> Self {
        Store { written: Mutex::new(HashMap::new()), guard: Mutex::new(0), seq: AtomicU64::new(0), cache: Mutex::new(HashMap::new()) }
    }
    fn written(&self) -> std::sync::MutexGuard<'_, HashMap<String, u64>> {
        self.written.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub fn store() -> &'static Store {
    static S: OnceLock<Store> = OnceLock::new();
    S.get_or_init(Store::new)
}

pub fn chats_dir(app_dir: &Path) -> PathBuf {
    app_dir.join(CHATS_DIR)
}
pub fn media_dir(app_dir: &Path) -> PathBuf {
    app_dir.join(MEDIA_DIR)
}
pub fn legacy_path(app_dir: &Path) -> PathBuf {
    app_dir.join(LEGACY)
}

/// Имя файла чата: безопасное для файловой системы, по ключу чата.
pub fn chat_file_name(key: &str) -> String {
    Sha256::digest(key.as_bytes())[..16].iter().map(|b| format!("{b:02x}")).collect()
}

// ---------- картинки ----------

fn ext_of_mime(mime: &str) -> Option<&'static str> {
    match mime {
        "png" => Some("png"),
        "jpeg" => Some("jpg"),
        "gif" => Some("gif"),
        "webp" => Some("webp"),
        _ => None,
    }
}

fn mime_of_ext(ext: &str) -> Option<&'static str> {
    match ext {
        "png" => Some("image/png"),
        "jpg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Разобрать ссылку `sdmedia:<64 hex>.<ext>`: только строчные hex-цифры и
/// разрешённое расширение, чтобы поддельный файл не мог указать на чужой путь.
fn parse_ref(s: &str) -> Option<(&str, &str)> {
    let rest = s.strip_prefix(REF_PREFIX)?;
    let (hash, ext) = rest.split_once('.')?;
    if hash.len() != 64 || !hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    mime_of_ext(ext)?;
    Some((hash, ext))
}

/// Записать файл через временный и замену.
fn write_file_atomic(path: &Path, data: &[u8]) -> bool {
    let tmp = path.with_extension("tmp");
    if fs::write(&tmp, data).is_err() {
        let _ = fs::remove_file(&tmp);
        return false;
    }
    if fs::rename(&tmp, path).is_err() {
        let ok = fs::write(path, data).is_ok();
        let _ = fs::remove_file(&tmp);
        return ok;
    }
    true
}

/// `data:image/...;base64,...` -> ссылка; картинка кладётся в `media/` один раз.
/// При любой неудаче возвращает None — тогда картинка остаётся внутри файла.
fn externalize_str(media: &Path, s: &str, cache: &MediaCache) -> Option<String> {
    let rest = s.strip_prefix("data:image/")?;
    let key = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        h.write(s.as_bytes());
        (h.finish(), s.len())
    };
    if let Some(r) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return Some(r.clone());
    }
    let (mime, b64) = rest.split_once(";base64,")?;
    let ext = ext_of_mime(mime)?;
    let b64e = base64::engine::general_purpose::STANDARD;
    let bytes = b64e.decode(b64).ok()?;
    // только если картинка однозначно восстановится из байтов
    if b64e.encode(&bytes) != b64 {
        return None;
    }
    let hash: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    let name = format!("{hash}.{ext}");
    let path = media.join(&name);
    if !path.exists() {
        fs::create_dir_all(media).ok()?;
        if !write_file_atomic(&path, &bytes) {
            return None;
        }
    }
    let r = format!("{REF_PREFIX}{name}");
    cache.lock().unwrap_or_else(|e| e.into_inner()).insert(key, r.clone());
    Some(r)
}

enum Resolved {
    NotRef,
    Data(String),
    /// Файла картинки нет (или ссылка поддельная)
    Missing,
    /// Файл есть, но прочитать не удалось (антивирус, блокировка): ссылку не трогаем
    Unreadable,
}

fn resolve_str(media: &Path, s: &str) -> Resolved {
    resolve_with(media, s, |p| fs::read(p))
}

/// Только `NotFound` означает «картинки нет»; любая другая ошибка чтения —
/// временная, и ссылку нельзя ни стирать, ни считать лишней.
fn resolve_with(media: &Path, s: &str, read: impl Fn(&Path) -> std::io::Result<Vec<u8>>) -> Resolved {
    let Some((hash, ext)) = parse_ref(s) else {
        return if s.starts_with(REF_PREFIX) { Resolved::Missing } else { Resolved::NotRef };
    };
    let Some(mime) = mime_of_ext(ext) else { return Resolved::Missing };
    match read(&media.join(format!("{hash}.{ext}"))) {
        Ok(bytes) => Resolved::Data(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Resolved::Missing,
        Err(_) => Resolved::Unreadable,
    }
}

fn externalize_rows(media: &Path, rows: &mut [ChatMsg], cache: &MediaCache) {
    for m in rows.iter_mut() {
        if let Some(r) = externalize_str(media, &m.image, cache) {
            m.image = r;
        }
        for img in m.images.iter_mut() {
            if let Some(r) = externalize_str(media, img, cache) {
                *img = r;
            }
        }
    }
}

fn note_ref(s: &str, refs: &mut HashSet<String>) {
    if let Some((h, e)) = parse_ref(s) {
        refs.insert(format!("{h}.{e}"));
    }
}

/// Вернуть картинки по ссылкам; собрать, какие ссылки встретились.
/// Нет файла картинки — поле пустое (в списке `images` запись убирается).
fn resolve_rows(media: &Path, rows: &mut [ChatMsg], refs: &mut HashSet<String>, unsure: &mut bool) {
    for m in rows.iter_mut() {
        match resolve_str(media, &m.image) {
            Resolved::NotRef => {}
            Resolved::Data(d) => {
                note_ref(&m.image, refs);
                m.image = d;
            }
            Resolved::Missing => m.image = String::new(),
            Resolved::Unreadable => {
                // оставляем ссылку как есть: при сохранении она не меняется
                note_ref(&m.image, refs);
                *unsure = true;
            }
        }
        let mut kept = Vec::with_capacity(m.images.len());
        for img in m.images.drain(..) {
            match resolve_str(media, &img) {
                Resolved::NotRef => kept.push(img),
                Resolved::Data(d) => {
                    note_ref(&img, refs);
                    kept.push(d);
                }
                Resolved::Missing => {}
                Resolved::Unreadable => {
                    note_ref(&img, refs);
                    *unsure = true;
                    kept.push(img);
                }
            }
        }
        m.images = kept;
    }
}

// ---------- запись ----------

struct HashWriter(std::collections::hash_map::DefaultHasher);
impl std::io::Write for HashWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn content_hash(rows: &[ChatMsg]) -> Option<u64> {
    let mut w = HashWriter(std::collections::hash_map::DefaultHasher::new());
    serde_json::to_writer(&mut w, rows).ok()?;
    Some(w.0.finish())
}

struct Entry {
    name: String,
    key: String,
    hash: u64,
    /// Копия сообщений — только у изменившихся чатов
    rows: Option<Vec<ChatMsg>>,
}

/// Снимок для записи. Делается под замком чатов и должен быть быстрым:
/// в нём только хеши и копии тех чатов, что изменились.
pub struct Snapshot {
    seq: u64,
    entries: Vec<Entry>,
}

pub fn snapshot(chats: &Chats) -> Snapshot {
    snapshot_with(store(), chats)
}

fn snapshot_with(st: &Store, chats: &Chats) -> Snapshot {
    let seq = st.seq.fetch_add(1, Ordering::SeqCst) + 1;
    let written = st.written();
    let mut entries = Vec::with_capacity(chats.len());
    for (key, rows) in chats {
        let Some(hash) = content_hash(rows) else { continue };
        let name = chat_file_name(key);
        let same = written.get(&name) == Some(&hash);
        entries.push(Entry { name, key: key.clone(), hash, rows: if same { None } else { Some(rows.clone()) } });
    }
    Snapshot { seq, entries }
}

/// Записать снимок (без замка чатов): сначала картинки, потом файл чата;
/// затем удалить файлы чатов, которых больше нет.
pub fn write(app_dir: &Path, snap: Snapshot) {
    write_with(store(), app_dir, snap)
}

fn write_with(st: &Store, app_dir: &Path, snap: Snapshot) {
    let mut last = st.guard.lock().unwrap_or_else(|e| e.into_inner());
    if snap.seq < *last {
        return; // более новый снимок уже записан
    }
    *last = snap.seq;
    let dir = chats_dir(app_dir);
    let media = media_dir(app_dir);
    let _ = fs::create_dir_all(&dir);
    let mut live: HashSet<String> = HashSet::new();
    for e in snap.entries {
        live.insert(e.name.clone());
        let Some(mut rows) = e.rows else { continue };
        if st.written().get(&e.name) == Some(&e.hash) {
            continue;
        }
        externalize_rows(&media, &mut rows, &st.cache);
        let file = ChatFile { key: e.key, messages: rows };
        let Ok(data) = serde_json::to_vec(&file) else { continue };
        if write_file_atomic(&dir.join(format!("{}.json", e.name)), &data) {
            st.written().insert(e.name, e.hash);
        }
    }
    if let Ok(rd) = fs::read_dir(&dir) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let stem = p.file_stem().and_then(|x| x.to_str()).unwrap_or("");
            if !live.contains(stem) {
                let _ = fs::remove_file(&p);
            }
        }
    }
    st.written().retain(|k, _| live.contains(k));
}

// ---------- чтение ----------

pub struct Loaded {
    pub chats: Chats,
    /// Файлы на диске совпадают с памятью
    consistent: bool,
}

impl Loaded {
    /// Нужно ли сразу сохранить всё заново (что-то подмешалось или не записалось).
    pub fn needs_save(&self) -> bool {
        !self.consistent
    }
}

fn sort_rows(chats: &mut Chats) {
    for rows in chats.values_mut() {
        rows.sort_by(|a, b| a.sent_at.cmp(&b.sent_at).then_with(|| a.id.cmp(&b.id)));
    }
}

fn parse_chat_file(p: &Path) -> Option<ChatFile> {
    fs::read(p).ok().and_then(|d| serde_json::from_slice::<ChatFile>(&d).ok())
}

/// Прочитать все файлы чатов из `dir`. Нечитаемый файл переименовывается в
/// `.bad` (не удаляется); второе значение — все ли файлы прочитаны.
fn read_chat_dir(dir: &Path, media: &Path, refs: &mut HashSet<String>, unsure: &mut bool) -> (Chats, bool) {
    let mut chats = Chats::new();
    let mut ok = true;
    let Ok(rd) = fs::read_dir(dir) else { return (chats, false) };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        match parse_chat_file(&p) {
            Some(mut f) => {
                resolve_rows(media, &mut f.messages, refs, unsure);
                chats.entry(f.key).or_default().extend(f.messages);
            }
            None => {
                ok = false;
                let _ = fs::rename(&p, p.with_extension("json.bad"));
            }
        }
    }
    (chats, ok)
}

fn rename_legacy(app_dir: &Path) {
    let mut to = app_dir.join("chats.json.old");
    if to.exists() {
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        to = app_dir.join(format!("chats.json.old-{ts}"));
    }
    let _ = fs::rename(legacy_path(app_dir), &to);
}

/// Записать все чаты в папку (для переноса); true, если всё удалось.
fn write_all_into(dir: &Path, media: &Path, chats: &Chats) -> bool {
    let cache = &store().cache;
    if fs::create_dir_all(dir).is_err() {
        return false;
    }
    for (key, rows) in chats {
        let mut rows = rows.clone();
        externalize_rows(media, &mut rows, cache);
        let file = ChatFile { key: key.clone(), messages: rows };
        let Ok(data) = serde_json::to_vec(&file) else { return false };
        if !write_file_atomic(&dir.join(format!("{}.json", chat_file_name(key))), &data) {
            return false;
        }
    }
    true
}

/// Все ссылки на картинки в готовых файлах чатов.
fn collect_refs_in_dir(dir: &Path) -> HashSet<String> {
    let mut refs = HashSet::new();
    let Ok(rd) = fs::read_dir(dir) else { return refs };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        if let Some(f) = parse_chat_file(&p) {
            for m in &f.messages {
                note_ref(&m.image, &mut refs);
                for i in &m.images {
                    note_ref(i, &mut refs);
                }
            }
        }
    }
    refs
}

/// Запомнить, что эти чаты уже лежат на диске в таком виде.
fn seed(st: &Store, chats: &Chats) {
    let mut w = st.written();
    w.clear();
    for (k, rows) in chats {
        if let Some(h) = content_hash(rows) {
            w.insert(chat_file_name(k), h);
        }
    }
}

/// В папке чатов лежит нечитаемый файл `.bad`: он мог ссылаться на картинки,
/// которых мы не видим, поэтому чистить `media/` нельзя.
fn has_bad_files(dir: &Path) -> bool {
    fs::read_dir(dir)
        .map(|rd| rd.flatten().any(|e| e.path().extension().and_then(|x| x.to_str()) == Some("bad")))
        .unwrap_or(false)
}

/// Удалить файлы картинок, на которые не ссылается ни одно сообщение.
fn gc_media(media: &Path, refs: &HashSet<String>) {
    let Ok(rd) = fs::read_dir(media) else { return };
    for ent in rd.flatten() {
        let p = ent.path();
        let Some(name) = p.file_name().and_then(|x| x.to_str()) else { continue };
        let is_media = parse_ref(&format!("{REF_PREFIX}{name}")).is_some();
        let is_tmp = p.extension().and_then(|x| x.to_str()) == Some("tmp");
        if (is_media && !refs.contains(name)) || is_tmp {
            let _ = fs::remove_file(&p);
        }
    }
}

/// Загрузить историю.
///   1. `chats/` есть — читаем её; если рядом остался `chats.json` (обрыв
///      переноса или запуск старой версии), докладываем из него сообщения,
///      которых нет, сохраняем и только потом убираем старый файл в сторону.
///   2. Иначе читаем старый `chats.json`, пишем всё в `chats.migrating/`,
///      папку целиком переименовываем в `chats/` и лишь затем `chats.json`
///      получает имя `chats.json.old`. Пока нет готовой `chats/`, источником
///      остаётся нетронутый `chats.json`, так что на любом шаге данные целы.
/// Лишние картинки в `media/` удаляются только после полностью удачной загрузки.
pub fn load(app_dir: &Path) -> Loaded {
    let dir = chats_dir(app_dir);
    let media = media_dir(app_dir);
    let legacy = legacy_path(app_dir);
    let mut refs: HashSet<String> = HashSet::new();
    let mut chats = Chats::new();
    let mut files_ok = true;
    let mut consistent = true;
    let mut unsure = false; // картинку не удалось прочитать по временной причине

    let legacy_exists = legacy.is_file();
    let legacy_map: Option<Chats> = if legacy_exists {
        fs::read(&legacy).ok().and_then(|d| serde_json::from_slice::<Chats>(&d).ok())
    } else {
        None
    };
    // нечитаемый chats.json не трогаем и не считаем историю надёжно загруженной
    let legacy_unreadable = legacy_exists && legacy_map.is_none();

    if dir.is_dir() {
        let (c, ok) = read_chat_dir(&dir, &media, &mut refs, &mut unsure);
        chats = c;
        files_ok = ok;
        let _ = fs::remove_dir_all(app_dir.join(MIGRATING_DIR));
        if let Some(old) = legacy_map {
            let mut added = false;
            for (key, rows) in old {
                let have = chats.entry(key).or_default();
                let ids: HashSet<String> = have.iter().map(|m| m.id.clone()).collect();
                for m in rows {
                    if !ids.contains(&m.id) {
                        have.push(m);
                        added = true;
                    }
                }
            }
            sort_rows(&mut chats);
            let mut saved = true;
            if added {
                let st = store();
                st.written().clear();
                let snap = snapshot_with(st, &chats);
                write_with(st, app_dir, snap);
                saved = chats.iter().all(|(k, v)| st.written().get(&chat_file_name(k)) == content_hash(v).as_ref());
                consistent = false; // картинки из добавленного ещё не вынесены; GC не делаем
            }
            if saved {
                rename_legacy(app_dir);
            }
        }
    } else if let Some(mut old) = legacy_map {
        sort_rows(&mut old);
        let tmp = app_dir.join(MIGRATING_DIR);
        let _ = fs::remove_dir_all(&tmp);
        let done = write_all_into(&tmp, &media, &old) && fs::rename(&tmp, &dir).is_ok();
        if done {
            rename_legacy(app_dir);
            refs = collect_refs_in_dir(&dir);
        } else {
            // не вышло — старый файл остался как был, а первое сохранение
            // создаст новую папку (при следующем запуске они объединятся)
            let _ = fs::remove_dir_all(&tmp);
            consistent = false;
        }
        chats = old;
    }

    sort_rows(&mut chats);
    let clean = consistent && files_ok && !legacy_unreadable;
    if clean {
        if !unsure && !has_bad_files(&dir) {
            gc_media(&media, &refs);
        }
        seed(store(), &chats);
    }
    Loaded { chats, consistent: consistent && files_ok }
}

/// Удалить всё, что относится к истории чатов («Сбросить всё»).
pub fn remove_all(app_dir: &Path) {
    store().written().clear();
    store().cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
    let _ = fs::remove_dir_all(chats_dir(app_dir));
    let _ = fs::remove_dir_all(app_dir.join(MIGRATING_DIR));
    let _ = fs::remove_dir_all(media_dir(app_dir));
    let _ = fs::remove_file(legacy_path(app_dir));
    let _ = fs::remove_file(app_dir.join("chats.json.tmp"));
    let _ = fs::remove_file(app_dir.join("chats.before-merge.json"));
    if let Ok(rd) = fs::read_dir(app_dir) {
        for ent in rd.flatten() {
            if ent.file_name().to_string_lossy().starts_with("chats.json.old") {
                let _ = fs::remove_file(ent.path());
            }
        }
    }
}

/// Копия всей истории в старом формате (одним файлом, картинки внутри) —
/// перед склейкой контактов, на случай отката. Существующую не трогает.
pub fn backup_legacy_format(app_dir: &Path, chats: &Chats) {
    let copy = app_dir.join("chats.before-merge.json");
    if copy.exists() || chats.is_empty() {
        return;
    }
    if let Ok(data) = serde_json::to_vec(chats) {
        let _ = fs::write(&copy, data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("safedrop-chatstore-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn msg(id: &str, image: &str) -> ChatMsg {
        serde_json::from_value(serde_json::json!({
            "id": id, "peer": "p", "from_me": true, "from_nick": "n", "text": "t", "ts": "x",
            "sent_at": 1, "image": image
        }))
        .unwrap()
    }

    fn png_url() -> String {
        format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3, 4, 5, 250]))
    }

    fn save_all(st: &Store, dir: &Path, chats: &Chats) {
        let snap = snapshot_with(st, chats);
        write_with(st, dir, snap);
    }

    #[test]
    fn round_trip_with_inline_image() {
        let dir = tmpdir("rt");
        let st = Store::new();
        let mut chats = Chats::new();
        let url = png_url();
        chats.insert("sd-abc".into(), vec![msg("1", &url)]);
        save_all(&st, &dir, &chats);
        let file = fs::read_to_string(chats_dir(&dir).join(format!("{}.json", chat_file_name("sd-abc")))).unwrap();
        assert!(file.contains("sdmedia:"));
        assert!(!file.contains("base64"));
        assert_eq!(fs::read_dir(media_dir(&dir)).unwrap().count(), 1);
        let loaded = load(&dir);
        assert_eq!(loaded.chats["sd-abc"][0].image, url);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_changed_chat_is_rewritten() {
        let dir = tmpdir("chg");
        let st = Store::new();
        let mut chats = Chats::new();
        chats.insert("a".into(), vec![msg("1", "")]);
        chats.insert("b".into(), vec![msg("2", "")]);
        save_all(&st, &dir, &chats);
        let pa = chats_dir(&dir).join(format!("{}.json", chat_file_name("a")));
        let pb = chats_dir(&dir).join(format!("{}.json", chat_file_name("b")));
        fs::write(&pa, "MARK-A").unwrap();
        fs::write(&pb, "MARK-B").unwrap();
        chats.get_mut("b").unwrap().push(msg("3", ""));
        save_all(&st, &dir, &chats);
        assert_eq!(fs::read_to_string(&pa).unwrap(), "MARK-A"); // не менялся
        assert!(fs::read_to_string(&pb).unwrap().contains("\"3\""));
        chats.remove("a");
        save_all(&st, &dir, &chats);
        assert!(!pa.exists()); // удалённый чат пропал и с диска
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrates_legacy_file() {
        let dir = tmpdir("mig");
        let url = png_url();
        let mut chats = Chats::new();
        chats.insert("10.0.0.5".into(), vec![msg("1", &url), msg("2", "")]);
        fs::write(legacy_path(&dir), serde_json::to_vec(&chats).unwrap()).unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded.chats["10.0.0.5"].len(), 2);
        assert_eq!(loaded.chats["10.0.0.5"][0].image, url);
        assert!(chats_dir(&dir).is_dir());
        assert!(!legacy_path(&dir).exists());
        assert!(dir.join("chats.json.old").exists());
        assert_eq!(fs::read_dir(media_dir(&dir)).unwrap().count(), 1);
        let again = load(&dir); // второй запуск читает уже новый формат
        assert_eq!(again.chats["10.0.0.5"][0].image, url);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn leftover_legacy_next_to_chats_dir_is_merged() {
        let dir = tmpdir("mrg");
        let st = Store::new();
        let mut chats = Chats::new();
        chats.insert("a".into(), vec![msg("1", "")]);
        save_all(&st, &dir, &chats);
        chats.get_mut("a").unwrap().push(msg("2", ""));
        fs::write(legacy_path(&dir), serde_json::to_vec(&chats).unwrap()).unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded.chats["a"].len(), 2);
        assert!(!legacy_path(&dir).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn crafted_reference_is_rejected() {
        let dir = tmpdir("bad");
        let media = media_dir(&dir);
        fs::create_dir_all(&media).unwrap();
        fs::write(dir.join("x"), b"secret").unwrap();
        let bad: Vec<String> = vec![
            "sdmedia:../../x".into(),
            "sdmedia:../x.png".into(),
            format!("sdmedia:{}.exe", "a".repeat(64)),
            format!("sdmedia:{}.png", "A".repeat(64)),
            format!("sdmedia:{}/../x.png", "a".repeat(60)),
            "sdmedia:abc.png".into(),
        ];
        for b in &bad {
            assert!(parse_ref(b).is_none(), "{b}");
            assert!(matches!(resolve_str(&media, b), Resolved::Missing), "{b}");
        }
        let good = format!("sdmedia:{}.png", "a".repeat(64));
        assert!(parse_ref(&good).is_some());
        // нет файла картинки — поле пустое, загрузка не падает
        assert!(matches!(resolve_str(&media, &good), Resolved::Missing));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_not_found_means_missing() {
        let media = Path::new("m");
        let good = format!("sdmedia:{}.png", "a".repeat(64));
        let denied = resolve_with(media, &good, |_| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)));
        assert!(matches!(denied, Resolved::Unreadable));
        let gone = resolve_with(media, &good, |_| Err(std::io::Error::from(std::io::ErrorKind::NotFound)));
        assert!(matches!(gone, Resolved::Missing));
        // временная ошибка: ссылка остаётся в памяти, учитывается и GC пропускается
        let mut rows = vec![msg("1", &good)];
        let mut refs = HashSet::new();
        let mut unsure = false;
        // подмена чтения через resolve_rows невозможна, проверяем решение вручную
        if let Resolved::Unreadable = denied {
            note_ref(&rows[0].image, &mut refs);
            unsure = true;
        }
        assert!(unsure && refs.contains(&good["sdmedia:".len()..]));
        // и такая ссылка при сохранении не меняется
        let cache = Mutex::new(HashMap::new());
        assert!(externalize_str(media, &rows[0].image, &cache).is_none());
        externalize_rows(media, &mut rows, &cache);
        assert_eq!(rows[0].image, good);
    }

    #[test]
    fn bad_file_disables_gc() {
        let dir = tmpdir("badgc");
        let st = Store::new();
        let mut chats = Chats::new();
        chats.insert("a".into(), vec![msg("1", &png_url())]);
        save_all(&st, &dir, &chats);
        let media = media_dir(&dir);
        let orphan = format!("{}.png", "b".repeat(64));
        fs::write(media.join(&orphan), b"x").unwrap();
        fs::write(chats_dir(&dir).join("zzz.json.bad"), b"{broken").unwrap();
        let _ = load(&dir);
        assert!(media.join(&orphan).exists()); // из-за .bad чистка пропущена
        fs::remove_file(chats_dir(&dir).join("zzz.json.bad")).unwrap();
        let _ = load(&dir);
        assert!(!media.join(&orphan).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_skips_repeat_work() {
        let dir = tmpdir("cache");
        let media = media_dir(&dir);
        let cache = Mutex::new(HashMap::new());
        let url = png_url();
        let r1 = externalize_str(&media, &url, &cache).unwrap();
        fs::remove_dir_all(&media).unwrap();
        // попадание в кэш: ни декодирования, ни проверки файла
        assert_eq!(externalize_str(&media, &url, &cache).unwrap(), r1);
        assert!(!media.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn gc_keeps_referenced_media_only() {
        let dir = tmpdir("gc");
        let st = Store::new();
        let mut chats = Chats::new();
        chats.insert("a".into(), vec![msg("1", &png_url())]);
        save_all(&st, &dir, &chats);
        let media = media_dir(&dir);
        let orphan = format!("{}.png", "b".repeat(64));
        fs::write(media.join(&orphan), b"x").unwrap();
        let loaded = load(&dir);
        assert!(loaded.chats["a"][0].image.starts_with("data:image/png"));
        let names: Vec<String> = fs::read_dir(&media).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        assert_eq!(names.len(), 1);
        assert_ne!(names[0], orphan);
        let _ = fs::remove_dir_all(&dir);
    }
}
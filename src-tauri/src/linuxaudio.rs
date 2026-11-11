//! Звук на Linux: какой звуковой сервер запущен (PipeWire или PulseAudio) и может ли
//! WebKitGTK через GStreamer найти микрофон.
//!
//! WebKitGTK ищет микрофоны через GstDeviceMonitor. Устройства PipeWire отдаёт плагин
//! pipewire (Arch: gst-plugin-pipewire), PulseAudio и pipewire-pulse — плагин pulse
//! (gst-plugins-good). Если подходящего плагина нет, список микрофонов пуст, и
//! getUserMedia падает с «Invalid constraint» или «Permission denied», хотя запрета нет.
//! Когда плагин pipewire есть и PipeWire запущен, он сам прячет дубли от pulse.
//!
//! Звонкам нужен ещё WebRTC GStreamer: webrtcbin (плагин webrtc, gst-plugins-bad)
//! и nicesrc/nicesink (плагин nice из libnice). Без webrtc WebKit не создаёт
//! RTCPeerConnection: «InvalidAccessError: Bad Configuration Parameters».
//!
//! И совсем основа: gst-plugins-base (opus, audioconvert) и gst-plugins-good (vpx,
//! rtp, v4l2 для камеры). Пакет для Arch тянет их сам, а AppImage берёт плагины
//! из системы — на чистом KDE/GNOME их может не быть, и тогда нет ни звука, ни видео.

use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    /// "pipewire" | "pulseaudio" | "none"
    pub server: &'static str,
    pub pipewire: bool,
    pub pipewire_pulse: bool,
    pub pulseaudio: bool,
    pub pulse_socket: bool,
    pub gst_pipewire: bool,
    pub gst_pulse: bool,
    /// gst-plugins-base (opus, audioconvert) — без него WebKit не работает со звуком вовсе
    pub gst_base: bool,
    /// gst-plugins-good (vpx, rtp, v4l2) — видео, камера и сами звонки
    pub gst_good: bool,
    pub gst_webrtc: bool,
    pub gst_nice: bool,
    /// через что WebKit возьмёт микрофон: "pipewire" | "pulse" | ""
    pub via: &'static str,
    /// пакеты, которых не хватает для звука через этот сервер
    pub missing: Vec<&'static str>,
    /// пакеты, без которых не работают звонки и демонстрация экрана (WebRTC)
    pub call_missing: Vec<&'static str>,
    /// команда установки всего недостающего ("" — ничего не нужно); если
    /// дистрибутив не узнали — просто имена пакетов
    pub install: String,
    /// можно поставить кнопкой: есть pkexec и известный менеджер пакетов
    pub can_install: bool,
    pub distro: String,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Family {
    Arch,
    Debian,
    Fedora,
    Suse,
    Unknown,
}

/// Пакеты с плагином GStreamer: (pipewire, pulse) для семейства дистрибутивов.
fn packages(f: Family) -> (&'static str, &'static str) {
    match f {
        Family::Arch => ("gst-plugin-pipewire", "gst-plugins-good"),
        Family::Debian => ("gstreamer1.0-pipewire", "gstreamer1.0-pulseaudio"),
        Family::Fedora => ("pipewire-gstreamer", "gstreamer1-plugins-good"),
        Family::Suse => ("gstreamer-plugin-pipewire", "gstreamer-plugins-good"),
        Family::Unknown => ("gst-plugin-pipewire", "gst-plugins-good"),
    }
}

/// Пакеты с основными плагинами: (base, good).
fn core_packages(f: Family) -> (&'static str, &'static str) {
    match f {
        Family::Arch | Family::Unknown => ("gst-plugins-base", "gst-plugins-good"),
        Family::Debian => ("gstreamer1.0-plugins-base", "gstreamer1.0-plugins-good"),
        Family::Fedora => ("gstreamer1-plugins-base", "gstreamer1-plugins-good"),
        Family::Suse => ("gstreamer-plugins-base", "gstreamer-plugins-good"),
    }
}

/// Пакеты с плагинами для звонков: (webrtc, nice).
fn call_packages(f: Family) -> (&'static str, &'static str) {
    match f {
        Family::Arch | Family::Unknown => ("gst-plugins-bad", "libnice"),
        Family::Debian => ("gstreamer1.0-plugins-bad", "gstreamer1.0-nice"),
        Family::Fedora => ("gstreamer1-plugins-bad-free", "libnice-gstreamer1"),
        Family::Suse => ("gstreamer-plugins-bad", "gstreamer-plugins-libnice"),
    }
}

/// Программа и аргументы установки (без имён пакетов), запускаются через pkexec.
fn installer(f: Family) -> Option<(&'static str, &'static [&'static str], &'static str)> {
    match f {
        Family::Arch => Some(("/usr/bin/pacman", &["-S", "--needed", "--noconfirm"], "sudo pacman -S --needed")),
        Family::Debian => Some(("/usr/bin/apt-get", &["install", "-y"], "sudo apt install")),
        Family::Fedora => Some(("/usr/bin/dnf", &["install", "-y"], "sudo dnf install")),
        Family::Suse => Some(("/usr/bin/zypper", &["--non-interactive", "install"], "sudo zypper install")),
        Family::Unknown => None,
    }
}

/// Семейство по /etc/os-release (ID и ID_LIKE) и красивое имя дистрибутива.
fn family_of(os_release: &str) -> (Family, String) {
    let mut ids = Vec::new();
    let mut name = String::new();
    for line in os_release.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim().trim_matches('"').to_ascii_lowercase();
        match k.trim() {
            "ID" | "ID_LIKE" => ids.extend(v.split_whitespace().map(str::to_string)),
            "PRETTY_NAME" => name = line.split_once('=').map(|x| x.1.trim().trim_matches('"').to_string()).unwrap_or_default(),
            _ => {}
        }
    }
    let has = |want: &[&str]| ids.iter().any(|i| want.contains(&i.as_str()));
    let f = if has(&["arch", "archlinux", "manjaro", "endeavouros", "cachyos", "garuda"]) {
        Family::Arch
    } else if has(&["debian", "ubuntu", "linuxmint", "pop"]) {
        Family::Debian
    } else if has(&["fedora", "rhel", "centos", "nobara"]) {
        Family::Fedora
    } else if has(&["suse", "opensuse", "opensuse-tumbleweed", "opensuse-leap"]) {
        Family::Suse
    } else {
        Family::Unknown
    };
    (f, name)
}

fn runtime_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let uid = std::fs::metadata("/proc/self").ok()?.uid();
        return Some(PathBuf::from(format!("/run/user/{uid}")));
    }
    #[allow(unreachable_code)]
    None
}

/// Имена запущенных процессов (из /proc/<pid>/comm).
fn running(names: &[&str]) -> Vec<bool> {
    let mut found = vec![false; names.len()];
    let Ok(dir) = std::fs::read_dir("/proc") else { return found };
    for e in dir.flatten() {
        let n = e.file_name();
        if !n.to_string_lossy().bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(comm) = std::fs::read_to_string(e.path().join("comm")) else { continue };
        let comm = comm.trim();
        for (i, want) in names.iter().enumerate() {
            if comm == *want {
                found[i] = true;
            }
        }
    }
    found
}

/// Обычные системные папки плагинов GStreamer (Arch, Fedora, Debian/Ubuntu).
const SYSTEM_PLUGIN_DIRS: [&str; 5] = [
    "/usr/lib/gstreamer-1.0",
    "/usr/lib64/gstreamer-1.0",
    "/usr/lib/x86_64-linux-gnu/gstreamer-1.0",
    "/usr/lib/aarch64-linux-gnu/gstreamer-1.0",
    "/usr/local/lib/gstreamer-1.0",
];

/// Системный gst-plugin-scanner (Arch, Fedora, Debian/Ubuntu).
const SYSTEM_SCANNERS: [&str; 4] = [
    "/usr/lib/gstreamer-1.0/gst-plugin-scanner",
    "/usr/libexec/gstreamer-1.0/gst-plugin-scanner",
    "/usr/lib/x86_64-linux-gnu/gstreamer1.0/gstreamer-1.0/gst-plugin-scanner",
    "/usr/lib/aarch64-linux-gnu/gstreamer1.0/gstreamer-1.0/gst-plugin-scanner",
];

/// Папки, где GStreamer ищет плагины: переменные окружения и обычные системные пути.
fn plugin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for var in ["GST_PLUGIN_PATH_1_0", "GST_PLUGIN_PATH", "GST_PLUGIN_SYSTEM_PATH_1_0", "GST_PLUGIN_SYSTEM_PATH"] {
        if let Some(v) = std::env::var_os(var) {
            dirs.extend(std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()));
        }
    }
    dirs.extend(SYSTEM_PLUGIN_DIRS.iter().map(PathBuf::from));
    dirs
}

/// Есть ли в папке хоть один плагин GStreamer.
fn dir_has_plugins(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten().any(|e| {
                let n = e.file_name();
                let n = n.to_string_lossy();
                n.starts_with("libgst") && n.ends_with(".so")
            })
        })
        .unwrap_or(false)
}

/// Куда направить GStreamer в AppImage: `None` — ничего не менять (в текущих
/// папках уже есть плагины), иначе — список системных папок с плагинами.
fn appimage_plugin_path(current: &[PathBuf], system: &[PathBuf], has_plugins: impl Fn(&Path) -> bool) -> Option<Vec<PathBuf>> {
    if current.iter().any(|d| has_plugins(d)) {
        return None;
    }
    let found: Vec<PathBuf> = system.iter().filter(|d| has_plugins(d)).cloned().collect();
    if found.is_empty() { None } else { Some(found) }
}

/// AppImage (linuxdeploy) при запуске подменяет GST_PLUGIN_SYSTEM_PATH на папку
/// внутри образа, а плагинов GStreamer в нашем образе нет: WebKit не видел ни
/// микрофона, ни камеры, ни динамиков. Возвращаем системные папки плагинов.
/// Вызывать в самом начале main, пока не запущены другие потоки и WebKit.
pub fn fix_appimage_gstreamer() {
    if std::env::var_os("APPIMAGE").is_none() && std::env::var_os("APPDIR").is_none() {
        return;
    }
    let current = effective_paths(std::env::var_os("GST_PLUGIN_SYSTEM_PATH_1_0"), std::env::var_os("GST_PLUGIN_SYSTEM_PATH"));
    // без дублей (/usr/lib64 на Arch — ссылка на /usr/lib) и без папок чужой архитектуры
    let mut system: Vec<PathBuf> = Vec::new();
    let mut seen = Vec::new();
    for d in SYSTEM_PLUGIN_DIRS.iter().filter(|d| for_this_arch(d)) {
        let real = std::fs::canonicalize(d).unwrap_or_else(|_| PathBuf::from(d));
        if !seen.contains(&real) {
            seen.push(real);
            system.push(PathBuf::from(d));
        }
    }
    let Some(dirs) = appimage_plugin_path(&current, &system, dir_has_plugins) else { return };
    let Ok(joined) = std::env::join_paths(&dirs) else { return };
    std::env::set_var("GST_PLUGIN_SYSTEM_PATH_1_0", &joined);
    std::env::set_var("GST_PLUGIN_SYSTEM_PATH", &joined);
    let scanner_ok = std::env::var_os("GST_PLUGIN_SCANNER_1_0")
        .or_else(|| std::env::var_os("GST_PLUGIN_SCANNER"))
        .map(|s| Path::new(&s).is_file())
        .unwrap_or(false);
    if !scanner_ok {
        if let Some(s) = SYSTEM_SCANNERS.iter().find(|s| for_this_arch(s) && Path::new(s).is_file()) {
            std::env::set_var("GST_PLUGIN_SCANNER_1_0", s);
            std::env::set_var("GST_PLUGIN_SCANNER", s);
        }
    }
    // свой кэш плагинов: системный ~/.cache/gstreamer-1.0 общий с другими программами,
    // и плагин, не загрузившийся под библиотеками AppImage, не должен портить его им
    if std::env::var_os("GST_REGISTRY_1_0").is_none() && std::env::var_os("GST_REGISTRY").is_none() {
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")));
        if let Some(c) = cache {
            let dir = c.join("safedrop");
            if std::fs::create_dir_all(&dir).is_ok() {
                std::env::set_var("GST_REGISTRY_1_0", dir.join(format!("gst-registry-appimage.{}.bin", std::env::consts::ARCH)));
            }
        }
    }
}

/// Как GStreamer: берёт `_1_0`, а обычную переменную — только если `_1_0` не задана.
fn effective_paths(v1_0: Option<std::ffi::OsString>, plain: Option<std::ffi::OsString>) -> Vec<PathBuf> {
    v1_0.or(plain)
        .map(|v| std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()).collect())
        .unwrap_or_default()
}

/// Папки Debian/Ubuntu с триплетом (x86_64-linux-gnu, aarch64-linux-gnu) — только своей архитектуры.
fn for_this_arch(path: &str) -> bool {
    !path.contains("-linux-gnu") || path.contains(&format!("{}-linux-gnu", std::env::consts::ARCH))
}

fn has_plugin(dirs: &[PathBuf], file: &str) -> bool {
    dirs.iter().any(|d| d.join(file).is_file())
}

/// Чистая логика выбора по найденному: удобно проверять тестом.
fn decide(mut s: Status, family: Family) -> Status {
    s.server = if s.pulseaudio {
        "pulseaudio"
    } else if s.pipewire {
        "pipewire"
    } else if s.pulse_socket {
        "pulseaudio"
    } else {
        "none"
    };
    let (pw_pkg, pulse_pkg) = packages(family);
    s.via = match s.server {
        "pipewire" if s.gst_pipewire => "pipewire",
        // pipewire-pulse: PipeWire притворяется PulseAudio, и микрофоны видны через pulse
        "pipewire" if s.gst_pulse && s.pulse_socket => "pulse",
        "pulseaudio" if s.gst_pulse => "pulse",
        _ => "",
    };
    s.missing.clear();
    // основа нужна и микрофону, и звонкам; если её нет, остальное уже не поможет
    let (base_pkg, good_pkg) = core_packages(family);
    if !s.gst_base { s.missing.push(base_pkg); }
    match s.server {
        // при PipeWire микрофон берём через его собственный плагин — даже если
        // pipewire-pulse есть, так надёжнее (и это то, о чём просили)
        "pipewire" if !s.gst_pipewire => s.missing.push(pw_pkg),
        "pulseaudio" if !s.gst_pulse => s.missing.push(pulse_pkg),
        _ => {}
    }
    let (webrtc_pkg, nice_pkg) = call_packages(family);
    s.call_missing.clear();
    // в Arch gst-plugins-good — ещё и плагин pulse: не просим его дважды
    if !s.gst_good && !s.missing.contains(&good_pkg) { s.call_missing.push(good_pkg); }
    if !s.gst_webrtc { s.call_missing.push(webrtc_pkg); }
    if !s.gst_nice { s.call_missing.push(nice_pkg); }
    let all = s.all_missing();
    s.install = match (installer(family), all.is_empty()) {
        (Some((_, _, shown)), false) => format!("{shown} {}", all.join(" ")),
        (None, false) => all.join(" "),
        _ => String::new(),
    };
    s
}

impl Status {
    /// Всё, что надо поставить: для звука и для звонков.
    fn all_missing(&self) -> Vec<&'static str> {
        self.missing.iter().chain(&self.call_missing).copied().collect()
    }
}

pub fn status() -> Status {
    let os = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .unwrap_or_default();
    let (family, distro) = family_of(&os);
    let rt = runtime_dir();
    let pw_dir = std::env::var_os("PIPEWIRE_RUNTIME_DIR").map(PathBuf::from).or_else(|| rt.clone());
    let pw_socket = pw_dir.as_deref().map(|d| d.join("pipewire-0")).is_some_and(|p| p.exists());
    let pulse_socket = std::env::var_os("PULSE_SERVER").is_some_and(|v| !v.is_empty())
        || rt.as_deref().map(|d| d.join("pulse").join("native")).is_some_and(|p| p.exists());
    let procs = running(&["pipewire", "pipewire-pulse", "pulseaudio"]);
    let dirs = plugin_dirs();
    let s = Status {
        pipewire: pw_socket || procs[0],
        pipewire_pulse: procs[1],
        pulseaudio: procs[2],
        pulse_socket,
        gst_pipewire: has_plugin(&dirs, "libgstpipewire.so"),
        gst_pulse: has_plugin(&dirs, "libgstpulseaudio.so"),
        gst_base: has_plugin(&dirs, "libgstopus.so") && has_plugin(&dirs, "libgstaudioconvert.so"),
        gst_good: has_plugin(&dirs, "libgstvpx.so") && has_plugin(&dirs, "libgstrtpmanager.so"),
        gst_webrtc: has_plugin(&dirs, "libgstwebrtc.so"),
        gst_nice: has_plugin(&dirs, "libgstnice.so"),
        distro,
        ..Default::default()
    };
    let mut s = decide(s, family);
    s.can_install = !s.all_missing().is_empty()
        && installer(family).is_some_and(|(pm, _, _)| Path::new(pm).exists())
        && Path::new("/usr/bin/pkexec").exists();
    s
}

/// Ставит недостающие пакеты через pkexec (пароль спросит система). Имена пакетов
/// берутся только из status(), а не из интерфейса.
pub fn install_missing() -> Result<Status, String> {
    let s = status();
    let all = s.all_missing();
    if all.is_empty() {
        return Ok(s);
    }
    let os = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let (family, _) = family_of(&os);
    let (pm, args, _) = installer(family).ok_or("Не знаю, как ставить пакеты в этом дистрибутиве")?;
    if !Path::new("/usr/bin/pkexec").exists() {
        return Err("Нет pkexec — поставь пакет командой в терминале".into());
    }
    let out = std::process::Command::new("/usr/bin/pkexec")
        .arg(pm)
        .args(args)
        .args(&all)
        .output()
        .map_err(|e| format!("pkexec: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        // 126 — закрыли окно пароля; 127 — нет прав или нет агента polkit (тогда в stderr причина)
        if out.status.code() == Some(126) || (out.status.code() == Some(127) && err.contains("dismissed")) {
            return Err("Установка отменена".into());
        }
        let tail: Vec<&str> = err.lines().rev().take(4).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        return Err(format!("Не установилось: {}", tail.join(" ").trim()));
    }
    Ok(status())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// плагины звонков на месте: проверяем только звук
    fn base() -> Status {
        Status { gst_base: true, gst_good: true, gst_webrtc: true, gst_nice: true, ..Default::default() }
    }

    #[test]
    fn distro_family() {
        assert_eq!(family_of("ID=arch\nPRETTY_NAME=\"Arch Linux\"\n").0, Family::Arch);
        assert_eq!(family_of("ID=cachyos\nID_LIKE=arch\n").0, Family::Arch);
        assert_eq!(family_of("ID=ubuntu\nID_LIKE=debian\n").0, Family::Debian);
        assert_eq!(family_of("ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\n").0, Family::Suse);
        assert_eq!(family_of("ID=nixos\n").0, Family::Unknown);
        assert_eq!(family_of("PRETTY_NAME=\"Arch Linux\"\n").1, "Arch Linux");
    }

    #[test]
    fn pipewire_without_plugin_asks_for_it() {
        let s = decide(Status { pipewire: true, ..base() }, Family::Arch);
        assert_eq!(s.server, "pipewire");
        assert_eq!(s.via, "");
        assert_eq!(s.missing, vec!["gst-plugin-pipewire"]);
        assert_eq!(s.install, "sudo pacman -S --needed gst-plugin-pipewire");
    }

    #[test]
    fn pipewire_pulse_works_but_plugin_still_suggested() {
        let s = decide(Status { pipewire: true, pipewire_pulse: true, pulse_socket: true, gst_pulse: true, ..base() }, Family::Debian);
        assert_eq!(s.via, "pulse");
        assert_eq!(s.missing, vec!["gstreamer1.0-pipewire"]);
    }

    #[test]
    fn pipewire_with_plugin_is_fine() {
        let s = decide(Status { pipewire: true, gst_pipewire: true, gst_pulse: true, ..base() }, Family::Arch);
        assert_eq!(s.via, "pipewire");
        assert!(s.missing.is_empty() && s.install.is_empty());
    }

    #[test]
    fn calls_need_webrtc_and_nice() {
        let s = decide(Status { pipewire: true, gst_pipewire: true, gst_webrtc: false, gst_nice: false, ..base() }, Family::Arch);
        assert!(s.missing.is_empty());
        assert_eq!(s.call_missing, vec!["gst-plugins-bad", "libnice"]);
        assert_eq!(s.install, "sudo pacman -S --needed gst-plugins-bad libnice");
        // и звук, и звонки — одной командой
        let s = decide(Status { pipewire: true, gst_nice: false, ..base() }, Family::Debian);
        assert_eq!(s.all_missing(), vec!["gstreamer1.0-pipewire", "gstreamer1.0-nice"]);
        assert_eq!(s.install, "sudo apt install gstreamer1.0-pipewire gstreamer1.0-nice");
    }

    #[test]
    fn bare_gstreamer_asks_for_base_and_good() {
        // AppImage на чистой системе: есть только gstreamer и плагин pipewire
        let s = decide(Status { pipewire: true, gst_pipewire: true, ..Default::default() }, Family::Arch);
        assert_eq!(s.missing, vec!["gst-plugins-base"]);
        assert_eq!(s.call_missing, vec!["gst-plugins-good", "gst-plugins-bad", "libnice"]);
        assert_eq!(s.install, "sudo pacman -S --needed gst-plugins-base gst-plugins-good gst-plugins-bad libnice");
        // Arch + PulseAudio: gst-plugins-good — тот же пакет, что и плагин pulse
        let s = decide(Status { pulseaudio: true, pulse_socket: true, gst_base: true, ..Default::default() }, Family::Arch);
        assert_eq!(s.all_missing(), vec!["gst-plugins-good", "gst-plugins-bad", "libnice"]);
    }

    #[test]
    fn real_pulseaudio_wins_over_idle_pipewire() {
        // PipeWire может крутиться ради видео, а звук держит PulseAudio
        let s = decide(Status { pipewire: true, pulseaudio: true, pulse_socket: true, gst_pulse: true, ..base() }, Family::Arch);
        assert_eq!(s.server, "pulseaudio");
        assert_eq!(s.via, "pulse");
        assert!(s.missing.is_empty());
    }

    /// Что видит SafeDrop на этой машине: cargo test --release print_status -- --ignored --nocapture
    #[test]
    #[ignore]
    fn print_status() {
        println!("{:#?}", status());
    }

    #[test]
    fn nothing_running_asks_nothing() {
        let s = decide(base(), Family::Fedora);
        assert_eq!(s.server, "none");
        assert!(s.missing.is_empty());
    }

    #[test]
    fn appimage_empty_plugin_dir_switches_to_system() {
        let cur = vec![PathBuf::from("/tmp/.mount_x/usr/lib/gstreamer-1.0")];
        let sys = vec![PathBuf::from("/usr/lib/gstreamer-1.0"), PathBuf::from("/usr/lib64/gstreamer-1.0")];
        let r = appimage_plugin_path(&cur, &sys, |d| d == Path::new("/usr/lib/gstreamer-1.0"));
        assert_eq!(r, Some(vec![PathBuf::from("/usr/lib/gstreamer-1.0")]));
    }

    #[test]
    fn appimage_with_bundled_plugins_is_left_alone() {
        let cur = vec![PathBuf::from("/tmp/.mount_x/usr/lib/gstreamer-1.0")];
        let sys = vec![PathBuf::from("/usr/lib/gstreamer-1.0")];
        assert_eq!(appimage_plugin_path(&cur, &sys, |_| true), None);
    }

    #[test]
    fn appimage_without_any_plugins_changes_nothing() {
        let cur = vec![PathBuf::from("/tmp/.mount_x/usr/lib/gstreamer-1.0")];
        let sys = vec![PathBuf::from("/usr/lib/gstreamer-1.0")];
        assert_eq!(appimage_plugin_path(&cur, &sys, |_| false), None);
    }

    #[test]
    fn plugin_path_1_0_wins_over_plain() {
        let r = effective_paths(Some("/tmp/.mount_x/usr/lib/gstreamer-1.0:".into()), Some("/usr/lib/gstreamer-1.0".into()));
        assert_eq!(r, vec![PathBuf::from("/tmp/.mount_x/usr/lib/gstreamer-1.0")]);
        assert_eq!(effective_paths(None, Some("/usr/lib/gstreamer-1.0".into())), vec![PathBuf::from("/usr/lib/gstreamer-1.0")]);
        assert!(effective_paths(None, None).is_empty());
    }

    #[test]
    fn foreign_arch_dirs_skipped() {
        assert!(for_this_arch("/usr/lib/gstreamer-1.0"));
        let other = if std::env::consts::ARCH == "aarch64" { "x86_64" } else { "aarch64" };
        assert!(!for_this_arch(&format!("/usr/lib/{other}-linux-gnu/gstreamer-1.0")));
        assert!(for_this_arch(&format!("/usr/lib/{}-linux-gnu/gstreamer-1.0", std::env::consts::ARCH)));
    }
}

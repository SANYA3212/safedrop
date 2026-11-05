//! Звук на Linux: какой звуковой сервер запущен (PipeWire или PulseAudio) и может ли
//! WebKitGTK через GStreamer найти микрофон.
//!
//! WebKitGTK ищет микрофоны через GstDeviceMonitor. Устройства PipeWire отдаёт плагин
//! pipewire (Arch: gst-plugin-pipewire), PulseAudio и pipewire-pulse — плагин pulse
//! (gst-plugins-good). Если подходящего плагина нет, список микрофонов пуст, и
//! getUserMedia падает с «Invalid constraint» или «Permission denied», хотя запрета нет.
//! Когда плагин pipewire есть и PipeWire запущен, он сам прячет дубли от pulse.

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
    /// через что WebKit возьмёт микрофон: "pipewire" | "pulse" | ""
    pub via: &'static str,
    /// пакеты, которых не хватает для звука через этот сервер
    pub missing: Vec<&'static str>,
    /// команда установки для этого дистрибутива ("" — дистрибутив не узнали)
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

/// Папки, где GStreamer ищет плагины: переменные окружения и обычные системные пути.
fn plugin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for var in ["GST_PLUGIN_PATH_1_0", "GST_PLUGIN_PATH", "GST_PLUGIN_SYSTEM_PATH_1_0", "GST_PLUGIN_SYSTEM_PATH"] {
        if let Some(v) = std::env::var_os(var) {
            dirs.extend(std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()));
        }
    }
    for d in [
        "/usr/lib/gstreamer-1.0",
        "/usr/lib64/gstreamer-1.0",
        "/usr/lib/x86_64-linux-gnu/gstreamer-1.0",
        "/usr/lib/aarch64-linux-gnu/gstreamer-1.0",
        "/usr/local/lib/gstreamer-1.0",
    ] {
        dirs.push(PathBuf::from(d));
    }
    dirs
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
    match s.server {
        // при PipeWire микрофон берём через его собственный плагин — даже если
        // pipewire-pulse есть, так надёжнее (и это то, о чём просили)
        "pipewire" if !s.gst_pipewire => s.missing.push(pw_pkg),
        "pulseaudio" if !s.gst_pulse => s.missing.push(pulse_pkg),
        _ => {}
    }
    s.install = match (installer(family), s.missing.is_empty()) {
        (Some((_, _, shown)), false) => format!("{shown} {}", s.missing.join(" ")),
        (None, false) => s.missing.join(" "),
        _ => String::new(),
    };
    s
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
        distro,
        ..Default::default()
    };
    let mut s = decide(s, family);
    s.can_install = !s.missing.is_empty()
        && installer(family).is_some_and(|(pm, _, _)| Path::new(pm).exists())
        && Path::new("/usr/bin/pkexec").exists();
    s
}

/// Ставит недостающие пакеты через pkexec (пароль спросит система). Имена пакетов
/// берутся только из status(), а не из интерфейса.
pub fn install_missing() -> Result<Status, String> {
    let s = status();
    if s.missing.is_empty() {
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
        .args(&s.missing)
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

    fn base() -> Status {
        Status::default()
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
}

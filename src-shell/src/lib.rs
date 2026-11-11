//! SafeDrop Explorer context menu: IExplorerCommand COM handler.
//! Top verb "Отправить в SafeDrop" with live submenu (contacts from peers.json).
//! Shows in the Windows 11 new context menu AND in classic "more options".

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use windows::{
    core::{implement, Interface, BOOL, GUID, HRESULT, IUnknown, PWSTR},
    Win32::{
        Foundation::{CLASS_E_CLASSNOTAVAILABLE, E_INVALIDARG, E_NOINTERFACE, E_NOTIMPL, HINSTANCE, S_FALSE, S_OK},
        System::{
            Com::{CoTaskMemAlloc, CoTaskMemFree, IClassFactory, IClassFactory_Impl},
            LibraryLoader::GetModuleFileNameW,
        },
        UI::Shell::{
            ECF_DEFAULT, ECF_HASSUBCOMMANDS, ECS_ENABLED, IEnumExplorerCommand,
            IEnumExplorerCommand_Impl, IExplorerCommand, IExplorerCommand_Impl, IShellItemArray,
            SIGDN_FILESYSPATH,
        },
    },
};

// {7D2E9F4A-3B1C-4D5E-8F60-9A1B2C3D4E5F}
const CLSID_TOP: GUID = GUID::from_u128(0x7D2E9F4A3B1C4D5E8F609A1B2C3D4E5F);
// {7D2E9F4A-3B1C-4D5E-8F60-9A1B2C3D4E60}
const CLSID_PICK: GUID = GUID::from_u128(0x7D2E9F4A3B1C4D5E8F609A1B2C3D4E60);
// {7D2E9F4A-3B1C-4D5E-8F60-9A1B2C3D4E61}
const CLSID_CONTACT: GUID = GUID::from_u128(0x7D2E9F4A3B1C4D5E8F609A1B2C3D4E61);

/// Must match the GUID in set_ctx_menu (main app).
pub const CLSID_STR: &str = "{7D2E9F4A-3B1C-4D5E-8F60-9A1B2C3D4E5F}";

static DLL_INSTANCE: AtomicU64 = AtomicU64::new(0);

#[no_mangle]
unsafe extern "system" fn DllMain(hinst: HINSTANCE, reason: u32, _: *mut std::ffi::c_void) -> bool {
    if reason == 1 {
        DLL_INSTANCE.store(hinst.0 as u64, Ordering::SeqCst);
    }
    true
}

#[no_mangle]
unsafe extern "system" fn DllCanUnloadNow() -> HRESULT {
    S_OK
}

#[no_mangle]
unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut std::ffi::c_void,
) -> HRESULT {
    if rclsid.is_null() || riid.is_null() || ppv.is_null() {
        return E_INVALIDARG;
    }
    if *rclsid != CLSID_TOP {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    if *riid != IClassFactory::IID && *riid != IUnknown::IID {
        *ppv = std::ptr::null_mut();
        return E_NOINTERFACE;
    }
    let factory: IClassFactory = Factory.into();
    *ppv = Interface::into_raw(factory) as _;
    S_OK
}

// regsvr32 entry points (our app writes HKCU keys itself; these are no-ops).
#[no_mangle]
unsafe extern "system" fn DllRegisterServer() -> HRESULT {
    S_OK
}
#[no_mangle]
unsafe extern "system" fn DllUnregisterServer() -> HRESULT {
    S_OK
}

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        punkouter: windows::core::Ref<IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut std::ffi::c_void,
    ) -> windows::core::Result<()> {
        if punkouter.as_ref().is_some() {
            return Err(E_NOINTERFACE.into()); // no aggregation
        }
        unsafe {
            if riid.is_null() || ppvobject.is_null() {
                return Err(E_INVALIDARG.into());
            }
            if *riid != IExplorerCommand::IID && *riid != IUnknown::IID {
                *ppvobject = std::ptr::null_mut();
                return Err(E_NOINTERFACE.into());
            }
            let cmd: IExplorerCommand = Command(Kind::Top).into();
            *ppvobject = Interface::into_raw(cmd) as _;
        }
        Ok(())
    }

    fn LockServer(&self, _flock: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
enum Kind {
    Top,
    Pick,
    Contact { name: String, ip: String },
}

#[implement(IExplorerCommand)]
struct Command(Kind);

fn pwstr(s: &str) -> PWSTR {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    unsafe {
        let mem = CoTaskMemAlloc(v.len() * 2) as *mut u16;
        if mem.is_null() {
            return PWSTR::null();
        }
        std::ptr::copy_nonoverlapping(v.as_ptr(), mem, v.len());
        PWSTR(mem)
    }
}

fn exe_path() -> String {
    let h = DLL_INSTANCE.load(Ordering::SeqCst);
    let mut buf = vec![0u16; 1024];
    let n = unsafe {
        GetModuleFileNameW(
            Some(windows::Win32::Foundation::HMODULE(h as *mut std::ffi::c_void)),
            &mut buf,
        )
    };
    let dll = String::from_utf16_lossy(&buf[..n as usize]);
    exe_near(std::path::Path::new(&dll))
}

/// safedrop.exe рядом с DLL или папкой выше: установщик кладёт расширение в
/// `<программа>\shell\`, и раньше меню запускало несуществующий `shell\safedrop.exe` —
/// клик по контакту ничего не делал.
fn exe_near(dll: &std::path::Path) -> String {
    let dir = dll.parent();
    dir.into_iter()
        .chain(dir.and_then(|d| d.parent()))
        .map(|d| d.join("safedrop.exe"))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "safedrop.exe".to_string())
}

fn app_data_dir() -> std::path::PathBuf {
    if let Ok(custom) = std::env::var("SAFEDROP_DATA_DIR") {
        return std::path::PathBuf::from(custom);
    }
    dirs::config_dir()
        .map(|p| p.join("safedrop"))
        .unwrap_or(std::path::PathBuf::from("."))
}

#[derive(Clone, serde::Deserialize)]
struct PeerJson {
    #[serde(default)]
    ip: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    nickname: String,
}

fn contact_label(p: &PeerJson) -> (String, String) {
    let base = if !p.name.is_empty() && p.name != p.ip {
        if p.nickname.is_empty() {
            p.name.clone()
        } else {
            format!("{} · {}", p.name, p.nickname)
        }
    } else if !p.nickname.is_empty() {
        p.nickname.clone()
    } else {
        p.name.clone()
    };
    (format!("{base} ({})", p.ip), p.ip.clone())
}

fn load_contacts() -> Vec<(String, String)> {
    let path = app_data_dir().join("peers.json");
    let data = std::fs::read_to_string(path).unwrap_or_default();
    let mut peers: Vec<PeerJson> = serde_json::from_str(&data).unwrap_or_default();
    peers.sort_by(|a, b| a.name.cmp(&b.name));
    peers.into_iter().take(30).map(|p| contact_label(&p)).collect()
}

fn selected_paths(psia: windows::core::Ref<IShellItemArray>) -> Vec<String> {
    let mut out = Vec::new();
    let Some(arr) = psia.as_ref() else { return out };
    let count = unsafe { arr.GetCount().unwrap_or(0) };
    for i in 0..count.min(50) {
        let item = unsafe { arr.GetItemAt(i) };
        let Ok(item) = item else { continue };
        let r = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) };
        let Ok(pw) = r else { continue };
        let s = unsafe { pw.to_string().unwrap_or_default() };
        unsafe { CoTaskMemFree(Some(pw.0 as *const std::ffi::c_void)) };
        if !s.is_empty() {
            out.push(s);
        }
    }
    out
}

fn launch(exe: &str, args: &[&str]) {
    let _ = std::process::Command::new(exe).args(args).spawn();
}

impl IExplorerCommand_Impl for Command_Impl {
    fn GetTitle(&self, _psiitemarray: windows::core::Ref<IShellItemArray>) -> windows::core::Result<PWSTR> {
        Ok(match &self.0 {
            Kind::Top => pwstr("Отправить в SafeDrop"),
            Kind::Pick => pwstr("Выбрать контакт в окне…"),
            Kind::Contact { name, .. } => pwstr(name),
        })
    }

    fn GetIcon(&self, _psiitemarray: windows::core::Ref<IShellItemArray>) -> windows::core::Result<PWSTR> {
        Ok(pwstr(&exe_path()))
    }

    fn GetToolTip(&self, _psiitemarray: windows::core::Ref<IShellItemArray>) -> windows::core::Result<PWSTR> {
        Ok(match &self.0 {
            Kind::Top => pwstr("Отправить файл через SafeDrop P2P"),
            Kind::Pick => pwstr("Открыть SafeDrop и выбрать контакт"),
            Kind::Contact { name, .. } => pwstr(&format!("Отправить через SafeDrop: {name}")),
        })
    }

    fn GetCanonicalName(&self) -> windows::core::Result<GUID> {
        Ok(match &self.0 {
            Kind::Top => CLSID_TOP,
            Kind::Pick => CLSID_PICK,
            Kind::Contact { .. } => CLSID_CONTACT,
        })
    }

    fn GetState(
        &self,
        _psiitemarray: windows::core::Ref<IShellItemArray>,
        _foktobeslow: windows::core::BOOL,
    ) -> windows::core::Result<u32> {
        Ok(ECS_ENABLED.0 as u32)
    }

    fn Invoke(
        &self,
        psiitemarray: windows::core::Ref<IShellItemArray>,
        _pbc: windows::core::Ref<windows::Win32::System::Com::IBindCtx>,
    ) -> windows::core::Result<()> {
        let exe = exe_path();
        let files = selected_paths(psiitemarray);
        match &self.0 {
            Kind::Top => {
                // top item has subcommands; direct invoke opens picker for first file
                if let Some(f) = files.first() {
                    launch(&exe, &["--send", f]);
                }
            }
            Kind::Pick => {
                for f in &files {
                    launch(&exe, &["--send", f]);
                }
            }
            Kind::Contact { ip, .. } => {
                for f in &files {
                    launch(&exe, &["--send-to", ip, f]);
                }
            }
        }
        Ok(())
    }

    fn GetFlags(&self) -> windows::core::Result<u32> {
        Ok(match &self.0 {
            Kind::Top => ECF_HASSUBCOMMANDS.0 as u32,
            _ => ECF_DEFAULT.0 as u32,
        })
    }

    fn EnumSubCommands(&self) -> windows::core::Result<IEnumExplorerCommand> {
        match &self.0 {
            Kind::Top => {
                let mut items: Vec<IExplorerCommand> = Vec::new();
                let pick: IExplorerCommand = Command(Kind::Pick).into();
                items.push(pick);
                for (label, ip) in load_contacts() {
                    let c: IExplorerCommand = Command(Kind::Contact { name: label, ip }).into();
                    items.push(c);
                }
                let e: IEnumExplorerCommand =
                    EnumCommands(items, AtomicUsize::new(0)).into();
                Ok(e)
            }
            _ => Err(E_NOTIMPL.into()),
        }
    }
}

#[implement(IEnumExplorerCommand)]
struct EnumCommands(Vec<IExplorerCommand>, AtomicUsize);

impl IEnumExplorerCommand_Impl for EnumCommands_Impl {
    fn Next(
        &self,
        celt: u32,
        puicommand: *mut Option<IExplorerCommand>,
        pceltfetched: *mut u32,
    ) -> windows::core::HRESULT {
        let mut fetched: u32 = 0;
        while fetched < celt {
            let idx = self.1.fetch_add(1, Ordering::SeqCst);
            if idx >= self.0.len() {
                break;
            }
            unsafe {
                puicommand
                    .add(fetched as usize)
                    .write(Some(self.0[idx].clone()));
            }
            fetched += 1;
        }
        unsafe {
            if !pceltfetched.is_null() {
                *pceltfetched = fetched;
            }
        }
        if fetched == celt {
            S_OK
        } else {
            S_FALSE
        }
    }

    fn Skip(&self, celt: u32) -> windows::core::Result<()> {
        let idx = self.1.fetch_add(celt as usize, Ordering::SeqCst);
        if idx + celt as usize >= self.0.len() {
            return Err(S_FALSE.into());
        }
        Ok(())
    }

    fn Reset(&self) -> windows::core::Result<()> {
        self.1.store(0, Ordering::SeqCst);
        Ok(())
    }

    fn Clone(&self) -> windows::core::Result<IEnumExplorerCommand> {
        let e: IEnumExplorerCommand = EnumCommands(
            self.0.clone(),
            AtomicUsize::new(self.1.load(Ordering::SeqCst)),
        )
        .into();
        Ok(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn labels_and_sorting() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join("sd-shell-test");
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("SAFEDROP_DATA_DIR", &dir);
        std::fs::write(
            dir.join("peers.json"),
            r#"[{"ip":"26.9.9.9","name":"Zed","nickname":""},
                {"ip":"26.1.2.3","name":"Test-PC","nickname":"Tester"},
                {"ip":"26.5.5.5","name":"","nickname":"NickOnly"}]"#,
        )
        .unwrap();
        let list = load_contacts();
        assert_eq!(list.len(), 3);
        // sorted by name: "" < "Test-PC" < "Zed"
        assert_eq!(list[1], ("Test-PC · Tester (26.1.2.3)".to_string(), "26.1.2.3".to_string()));
        assert_eq!(list[2].1, "26.9.9.9".to_string());
        assert!(list[0].0.contains("NickOnly"));
        std::env::remove_var("SAFEDROP_DATA_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn contact_label_formats() {
        let p = PeerJson { ip: "26.1.1.1".into(), name: "".into(), nickname: "".into() };
        assert_eq!(contact_label(&p).0, " (26.1.1.1)".to_string());
    }

    #[test]
    fn exe_found_above_shell_folder() {
        // установленная раскладка: <dir>\safedrop.exe и <dir>\shell\safedrop-shell-x.dll
        let dir = std::env::temp_dir().join("sd-shell-exe-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("shell")).unwrap();
        std::fs::write(dir.join("safedrop.exe"), b"").unwrap();
        let dll = dir.join("shell").join("safedrop-shell-0.0.0.dll");
        assert_eq!(exe_near(&dll), dir.join("safedrop.exe").to_string_lossy());
        // exe рядом с DLL важнее
        std::fs::write(dir.join("shell").join("safedrop.exe"), b"").unwrap();
        assert_eq!(exe_near(&dll), dir.join("shell").join("safedrop.exe").to_string_lossy());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clsid_format() {
        assert_eq!(CLSID_STR.len(), 38);
        assert!(CLSID_STR.starts_with('{') && CLSID_STR.ends_with('}'));
    }
}

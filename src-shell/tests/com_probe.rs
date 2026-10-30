//! End-to-end COM test: loads the built DLL, activates the class object,
//! reads titles/flags and enumerates the live contact submenu.
#![cfg(windows)]

use std::ffi::c_void;
use windows::{
    core::{s, GUID, HRESULT, Interface, IUnknown, PCSTR, PCWSTR, PWSTR},
    Win32::{
        Foundation::HMODULE,
        System::{
            Com::IClassFactory,
            LibraryLoader::{GetProcAddress, LoadLibraryW},
        },
        UI::Shell::{IEnumExplorerCommand, IExplorerCommand},
    },
};

const CLSID_TOP: GUID = GUID::from_u128(0x7D2E9F4A3B1C4D5E8F609A1B2C3D4E5F);

type DllGetClassObjectFn =
    unsafe extern "system" fn(*const GUID, *const GUID, *mut *mut c_void) -> HRESULT;

fn dll_path() -> std::path::PathBuf {
    let m = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for p in [
        m.join("target/debug/safedrop_shell.dll"),
        m.join("target/release/safedrop_shell.dll"),
    ] {
        if p.exists() {
            return p;
        }
    }
    panic!("build the dll first (cargo build [--release])");
}

unsafe fn get_class_object() -> DllGetClassObjectFn {
    let path = dll_path();
    let w: Vec<u16> = path.to_string_lossy().encode_utf16().chain(Some(0)).collect();
    let h: HMODULE = LoadLibraryW(PCWSTR::from_raw(w.as_ptr())).unwrap();
    assert!(!h.is_invalid());
    let f = GetProcAddress(h, PCSTR::from_raw(s!("DllGetClassObject").as_ptr())).unwrap();
    std::mem::transmute(f)
}

unsafe fn pw(p: PWSTR) -> String {
    p.to_string().unwrap_or_default()
}

#[test]
fn com_end_to_end() {
    let dir = std::env::temp_dir().join("sd-shell-probe");
    let _ = std::fs::create_dir_all(&dir);
    std::env::set_var("SAFEDROP_DATA_DIR", &dir);
    std::fs::write(
        dir.join("peers.json"),
        r#"[{"ip":"26.1.2.3","name":"Beta","nickname":""},{"ip":"26.4.5.6","name":"Alpha","nickname":"Al"}]"#,
    )
    .unwrap();

    unsafe {
        let f = get_class_object();
        // bogus CLSID must fail
        let mut pv: *mut c_void = std::ptr::null_mut();
        let hr = f(&GUID::zeroed(), &IExplorerCommand::IID, &mut pv);
        assert!(hr.is_err() && pv.is_null(), "bogus clsid must fail");

        // real activation: class object must be the class factory
        let mut pv: *mut c_void = std::ptr::null_mut();
        let hr = f(&CLSID_TOP, &IClassFactory::IID, &mut pv);
        eprintln!("hr=0x{:08X} pv={pv:?}", hr.0 as u32);
        assert!(hr.is_ok() && !pv.is_null(), "activation failed");
        let fac = IClassFactory::from_raw(pv);
        let cmd: IExplorerCommand = fac.CreateInstance(None::<&IUnknown>).unwrap();
        assert_eq!(pw(cmd.GetTitle(None).unwrap()), "Отправить в SafeDrop");
        assert_eq!(cmd.GetFlags().unwrap(), 1); // ECF_HASSUBCOMMANDS

        let en: IEnumExplorerCommand = cmd.EnumSubCommands().unwrap();
        let mut arr: [Option<IExplorerCommand>; 8] = Default::default();
        let mut fetched = 0u32;
        en.Next(&mut arr, Some(&mut fetched as *mut u32))
            .unwrap();
        // picker + 2 contacts (sorted: Alpha, Beta)
        assert_eq!(fetched, 3, "picker + 2 contacts expected");
        let t0 = pw(arr[0].as_ref().unwrap().GetTitle(None).unwrap());
        assert_eq!(t0, "Выбрать контакт в окне…");
        let t1 = pw(arr[1].as_ref().unwrap().GetTitle(None).unwrap());
        let t2 = pw(arr[2].as_ref().unwrap().GetTitle(None).unwrap());
        assert!(t1.contains("Alpha") && t1.contains("26.4.5.6"), "got {t1}");
        assert!(t2.contains("Beta") && t2.contains("26.1.2.3"), "got {t2}");
    }

    std::env::remove_var("SAFEDROP_DATA_DIR");
    let _ = std::fs::remove_dir_all(&dir);
}

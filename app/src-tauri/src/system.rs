//! Hooking Linecard into the OS: ssh:// and telnet:// links, and the
//! `linecard` command on PATH. Everything is per user and reversible; a
//! link handler that was there before is remembered and put back.

use std::path::PathBuf;

use serde::Serialize;

pub const SCHEMES: [&str; 2] = ["ssh", "telnet"];

/// The program to register: the AppImage itself on Linux (the running
/// binary lives in a temporary mount), else this executable.
fn program() -> Result<PathBuf, String> {
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        return Ok(PathBuf::from(appimage));
    }
    std::env::current_exe().map_err(|e| e.to_string())
}

/// Whether links open in Linecard; `None` where that can't be changed.
pub fn links_on() -> Option<bool> {
    let exe = program().ok()?;
    imp::links_on(&exe)
}

pub fn set_links(on: bool) -> Result<(), String> {
    imp::set_links(&program()?, on)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliPathDto {
    /// `None`: Linecard can't do it here; follow `manual`.
    pub on: Option<bool>,
    /// A command to run instead (or as well).
    pub manual: Option<String>,
}

pub fn cli_path() -> CliPathDto {
    match program() {
        Ok(exe) => imp::cli_path(&exe),
        Err(_) => CliPathDto { on: None, manual: None },
    }
}

pub fn set_cli_path(on: bool) -> Result<(), String> {
    imp::set_cli_path(&program()?, on)
}

// ---- Windows ---------------------------------------------------------------------

#[cfg(windows)]
mod imp {
    //! URL handlers under HKCU\Software\Classes (a replaced one is kept under
    //! HKCU\Software\Linecard); PATH is the per-user one in HKCU\Environment.

    use std::io;
    use std::path::Path;

    use winreg::enums::{HKEY_CURRENT_USER, KEY_ALL_ACCESS};
    use winreg::{RegKey, RegValue};

    use super::{CliPathDto, SCHEMES};

    fn hkcu() -> RegKey {
        RegKey::predef(HKEY_CURRENT_USER)
    }

    fn classes() -> io::Result<RegKey> {
        hkcu().open_subkey_with_flags("Software\\Classes", KEY_ALL_ACCESS)
    }

    fn command(exe: &Path) -> String {
        format!("\"{}\" \"%1\"", exe.display())
    }

    fn current(scheme: &str) -> Option<String> {
        classes()
            .and_then(|c| c.open_subkey(format!("{scheme}\\shell\\open\\command")))
            .and_then(|k| k.get_value::<String, _>(""))
            .ok()
    }

    const BACKUP: &str = "Software\\Linecard\\PreviousLinkHandlers";

    fn points_to(scheme: &str, exe: &Path) -> bool {
        current(scheme).is_some_and(|v| v.eq_ignore_ascii_case(&command(exe)))
    }

    fn write_handler(scheme: &str, cmd: &str, icon: Option<String>) -> io::Result<()> {
        let (key, _) = classes()?.create_subkey(scheme)?;
        key.set_value("", &format!("URL:{} link", scheme.to_uppercase()))?;
        key.set_value("URL Protocol", &"")?;
        if let Some(icon) = icon {
            key.create_subkey("DefaultIcon")?.0.set_value("", &icon)?;
        }
        key.create_subkey("shell\\open\\command")?.0.set_value("", &cmd)
    }

    fn register(scheme: &str, exe: &Path) -> io::Result<()> {
        if let Some(previous) = current(scheme).filter(|_| !points_to(scheme, exe)) {
            hkcu().create_subkey(BACKUP)?.0.set_value(scheme, &previous)?;
        }
        write_handler(scheme, &command(exe), Some(format!("\"{}\",0", exe.display())))
    }

    fn unregister(scheme: &str, exe: &Path) -> io::Result<()> {
        if !points_to(scheme, exe) {
            return Ok(());
        }
        classes()?.delete_subkey_all(scheme)?;
        let backup = hkcu().open_subkey_with_flags(BACKUP, KEY_ALL_ACCESS);
        if let Some(previous) = backup.as_ref().ok().and_then(|b| b.get_value::<String, _>(scheme).ok()) {
            write_handler(scheme, &previous, None)?;
            let _ = backup?.delete_value(scheme);
        }
        Ok(())
    }

    pub fn links_on(exe: &Path) -> Option<bool> {
        Some(SCHEMES.iter().all(|s| points_to(s, exe)))
    }

    pub fn set_links(exe: &Path, on: bool) -> Result<(), String> {
        for s in SCHEMES {
            let r = if on { register(s, exe) } else { unregister(s, exe) };
            r.map_err(|e| format!("couldn't change the {s}:// link handler: {e}"))?;
        }
        Ok(())
    }

    // PATH ---------------------------------------------------------------------

    fn env_key() -> io::Result<RegKey> {
        hkcu().open_subkey_with_flags("Environment", KEY_ALL_ACCESS)
    }

    fn user_path() -> io::Result<(String, winreg::enums::RegType)> {
        match env_key()?.get_raw_value("Path") {
            Ok(v) => {
                let units: Vec<u16> = v
                    .bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes(*c))
                    .collect();
                let s = String::from_utf16_lossy(&units).trim_end_matches('\0').to_string();
                Ok((s, v.vtype))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok((String::new(), winreg::enums::REG_EXPAND_SZ)),
            Err(e) => Err(e),
        }
    }

    /// Keep the value's type: PATH is usually REG_EXPAND_SZ (%USERPROFILE%…),
    /// and writing it back as REG_SZ would break those entries.
    fn set_user_path(value: &str, vtype: winreg::enums::RegType) -> io::Result<()> {
        let mut bytes: Vec<u8> = value.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
        if bytes.is_empty() {
            bytes = vec![0, 0];
        }
        env_key()?.set_raw_value("Path", &RegValue { bytes, vtype })?;
        broadcast_environment_change();
        Ok(())
    }

    /// Tell Explorer (and so new terminals) that the environment changed.
    fn broadcast_environment_change() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
        };
        let name: Vec<u16> = "Environment".encode_utf16().chain([0]).collect();
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                name.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                2000,
                std::ptr::null_mut(),
            );
        }
    }

    fn dir_of(exe: &Path) -> String {
        exe.parent().map(|d| d.display().to_string()).unwrap_or_default()
    }

    fn has(path: &str, dir: &str) -> bool {
        path.split(';').any(|p| {
            p.trim()
                .trim_end_matches('\\')
                .eq_ignore_ascii_case(dir.trim_end_matches('\\'))
        })
    }

    pub fn cli_path(exe: &Path) -> CliPathDto {
        let dir = dir_of(exe);
        CliPathDto {
            on: user_path().ok().map(|(p, _)| has(&p, &dir)),
            manual: None,
        }
    }

    pub fn set_cli_path(exe: &Path, on: bool) -> Result<(), String> {
        let dir = dir_of(exe);
        let (path, vtype) = user_path().map_err(|e| e.to_string())?;
        let next = if on {
            if has(&path, &dir) {
                return Ok(());
            }
            if path.trim().is_empty() {
                dir
            } else {
                format!("{};{dir}", path.trim_end_matches(';'))
            }
        } else {
            path.split(';').filter(|p| !has(p, &dir)).collect::<Vec<_>>().join(";")
        };
        set_user_path(&next, vtype).map_err(|e| format!("couldn't update PATH: {e}"))
    }
}

// ---- Linux -----------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod imp {
    //! Links: a hidden .desktop entry plus `xdg-mime default`; the handler it
    //! replaced is kept in ~/.config/linecard. PATH: a symlink in ~/.local/bin.

    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::{CliPathDto, SCHEMES};

    const DESKTOP: &str = "linecard-links.desktop";

    fn home() -> PathBuf {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
    }

    fn data_dir() -> PathBuf {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/share"))
    }

    fn backup_file(scheme: &str) -> PathBuf {
        home().join(format!(".config/linecard/previous-{scheme}-handler"))
    }

    fn default_for(scheme: &str) -> Option<String> {
        let out = Command::new("xdg-mime")
            .args(["query", "default", &format!("x-scheme-handler/{scheme}")])
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
    }

    fn set_default(scheme: &str, desktop: &str) -> Result<(), String> {
        let ok = Command::new("xdg-mime")
            .args(["default", desktop, &format!("x-scheme-handler/{scheme}")])
            .status()
            .map_err(|e| format!("couldn't run xdg-mime: {e}"))?
            .success();
        ok.then_some(()).ok_or_else(|| "xdg-mime failed".into())
    }

    pub fn links_on(_exe: &Path) -> Option<bool> {
        Command::new("xdg-mime").arg("--version").output().ok()?;
        Some(SCHEMES.iter().all(|s| default_for(s).as_deref() == Some(DESKTOP)))
    }

    pub fn set_links(exe: &Path, on: bool) -> Result<(), String> {
        let apps = data_dir().join("applications");
        if on {
            std::fs::create_dir_all(&apps).map_err(|e| e.to_string())?;
            let entry = format!(
                "[Desktop Entry]\nType=Application\nName=Linecard\nExec=\"{}\" %u\nNoDisplay=true\nMimeType={}\n",
                exe.display(),
                SCHEMES
                    .iter()
                    .map(|s| format!("x-scheme-handler/{s};"))
                    .collect::<String>()
            );
            std::fs::write(apps.join(DESKTOP), entry).map_err(|e| e.to_string())?;
            for s in SCHEMES {
                if let Some(prev) = default_for(s).filter(|p| p != DESKTOP) {
                    let f = backup_file(s);
                    let _ = std::fs::create_dir_all(f.parent().unwrap());
                    let _ = std::fs::write(f, prev);
                }
                set_default(s, DESKTOP)?;
            }
        } else {
            for s in SCHEMES {
                if default_for(s).as_deref() == Some(DESKTOP) {
                    if let Ok(prev) = std::fs::read_to_string(backup_file(s)) {
                        set_default(s, prev.trim())?;
                    }
                }
                let _ = std::fs::remove_file(backup_file(s));
            }
            let _ = std::fs::remove_file(apps.join(DESKTOP));
        }
        Ok(())
    }

    fn link() -> PathBuf {
        home().join(".local/bin/linecard")
    }

    pub fn cli_path(exe: &Path) -> CliPathDto {
        CliPathDto {
            on: Some(std::fs::read_link(link()).is_ok_and(|t| t == exe)),
            manual: Some("~/.local/bin must be on your PATH (it is on most distributions).".into()),
        }
    }

    pub fn set_cli_path(exe: &Path, on: bool) -> Result<(), String> {
        let l = link();
        if std::fs::symlink_metadata(&l).is_ok() {
            if std::fs::read_link(&l).is_ok_and(|t| t == exe) || on {
                std::fs::remove_file(&l).map_err(|e| e.to_string())?;
            }
        }
        if on {
            std::fs::create_dir_all(l.parent().unwrap()).map_err(|e| e.to_string())?;
            std::os::unix::fs::symlink(exe, &l).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

// ---- macOS -----------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod imp {
    //! Links: the app declares ssh/telnet in Info.plist; this sets it as the
    //! default handler with Launch Services (remembering the previous one).
    //! PATH: /usr/local/bin needs admin rights, so the UI shows the command.

    use std::path::{Path, PathBuf};

    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};

    use super::{CliPathDto, SCHEMES};

    const BUNDLE_ID: &str = "dev.linecard.app";

    #[link(name = "CoreServices", kind = "framework")]
    extern "C" {
        fn LSSetDefaultHandlerForURLScheme(scheme: CFStringRef, bundle_id: CFStringRef) -> i32;
        fn LSCopyDefaultHandlerForURLScheme(scheme: CFStringRef) -> CFStringRef;
    }

    fn default_for(scheme: &str) -> Option<String> {
        let s = CFString::new(scheme);
        let r = unsafe { LSCopyDefaultHandlerForURLScheme(s.as_concrete_TypeRef()) };
        (!r.is_null()).then(|| unsafe { CFString::wrap_under_create_rule(r) }.to_string())
    }

    fn set_default(scheme: &str, bundle: &str) -> Result<(), String> {
        let (s, b) = (CFString::new(scheme), CFString::new(bundle));
        match unsafe { LSSetDefaultHandlerForURLScheme(s.as_concrete_TypeRef(), b.as_concrete_TypeRef()) } {
            0 => Ok(()),
            code => Err(format!("Launch Services refused ({code})")),
        }
    }

    fn backup_file(scheme: &str) -> PathBuf {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(format!(
            "Library/Application Support/{BUNDLE_ID}/previous-{scheme}-handler"
        ))
    }

    pub fn links_on(_exe: &Path) -> Option<bool> {
        Some(
            SCHEMES
                .iter()
                .all(|s| default_for(s).is_some_and(|b| b.eq_ignore_ascii_case(BUNDLE_ID))),
        )
    }

    pub fn set_links(_exe: &Path, on: bool) -> Result<(), String> {
        for s in SCHEMES {
            let current = default_for(s);
            let ours = current.as_deref().is_some_and(|b| b.eq_ignore_ascii_case(BUNDLE_ID));
            if on && !ours {
                if let Some(prev) = &current {
                    let f = backup_file(s);
                    let _ = std::fs::create_dir_all(f.parent().unwrap());
                    let _ = std::fs::write(f, prev);
                }
                set_default(s, BUNDLE_ID)?;
            } else if !on && ours {
                // Terminal handles ssh:// out of the box.
                let prev = std::fs::read_to_string(backup_file(s)).unwrap_or_else(|_| "com.apple.Terminal".into());
                set_default(s, prev.trim())?;
                let _ = std::fs::remove_file(backup_file(s));
            }
        }
        Ok(())
    }

    pub fn cli_path(exe: &Path) -> CliPathDto {
        CliPathDto {
            on: None,
            manual: Some(format!("sudo ln -sf \"{}\" /usr/local/bin/linecard", exe.display())),
        }
    }

    pub fn set_cli_path(_exe: &Path, _on: bool) -> Result<(), String> {
        Err("Run the command shown in Terminal (it needs your admin password).".into())
    }
}

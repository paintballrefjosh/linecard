//! What's left behind when Linecard crashes, for a bug report.
//!
//! A Rust panic goes to the debug log. On Windows, a hard crash (an access
//! violation in Linecard or a library it uses) also writes a minidump:
//! Windows' own error reporting keeps only the address, which can't be
//! traced without the threads and stacks. Dumps are written to
//! `crashes/new` and, on the next start, logged and moved up to `crashes`,
//! keeping the newest [`KEEP`].
//!
//! A dump holds part of the app's memory, so it can contain what was on
//! screen; it never leaves the computer unless the user sends it.

use std::fs;
use std::path::{Path, PathBuf};

use crate::debuglog;

/// The folder (next to the debug log) that keeps crash dumps.
pub const DIR: &str = "crashes";
const KEEP: usize = 5;

/// Log panics, report dumps from earlier crashes, and get ready to write
/// one if this run crashes.
pub fn init(data: &Path) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let at = info
            .location()
            .map(|l| format!(" at {}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let what = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".into());
        debuglog::write(
            "panic",
            &format!("thread '{}' panicked{at}: {what}", thread.name().unwrap_or("unnamed")),
        );
        previous(info);
    }));

    let dir = data.join(DIR);
    for (name, when) in collect(&dir) {
        debuglog::write(
            "crash",
            &format!("Linecard crashed at {when}; the dump is {}", dir.join(name).display()),
        );
    }
    #[cfg(windows)]
    dump::arm(&dir.join("new"));
}

/// Move dumps from `dir/new` into `dir` and drop all but the newest
/// [`KEEP`]. Returns the moved ones (name, local time written).
fn collect(dir: &Path) -> Vec<(String, String)> {
    let mut moved = Vec::new();
    for e in fs::read_dir(dir.join("new")).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".dmp") || fs::rename(e.path(), dir.join(&name)).is_err() {
            continue;
        }
        let when = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .or_else(|| fs::metadata(dir.join(&name)).and_then(|m| m.modified()).ok())
            .map(|t| {
                chrono::DateTime::<chrono::Local>::from(t)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_else(|| "an unknown time".into());
        moved.push((name, when));
    }
    let mut kept: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".dmp"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    kept.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, old) in kept.into_iter().skip(KEEP) {
        let _ = fs::remove_file(old);
    }
    moved.sort();
    moved
}

#[cfg(windows)]
mod dump {
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;

    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL};
    use windows_sys::Win32::System::Diagnostics::Debug::{
        MiniDumpWithIndirectlyReferencedMemory, MiniDumpWithThreadInfo, MiniDumpWithUnloadedModules, MiniDumpWriteDump,
        SetUnhandledExceptionFilter, EXCEPTION_POINTERS, MINIDUMP_EXCEPTION_INFORMATION,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId};

    /// Where this run's dump goes, as a NUL-terminated wide string. Built at
    /// start-up: once the process has crashed, its heap may be unusable.
    static PATH: OnceLock<Vec<u16>> = OnceLock::new();

    pub fn arm(dir: &Path) {
        let _ = std::fs::create_dir_all(dir);
        let name = format!(
            "linecard-{}-{}.dmp",
            chrono::Local::now().format("%Y%m%d-%H%M%S"),
            std::process::id()
        );
        let wide = dir.join(name).as_os_str().encode_wide_nul();
        if PATH.set(wide).is_ok() {
            // SAFETY: `on_crash` matches the filter signature and only reads
            // `PATH`, which is never changed after this.
            unsafe { SetUnhandledExceptionFilter(Some(on_crash)) };
        }
    }

    trait EncodeWideNul {
        fn encode_wide_nul(&self) -> Vec<u16>;
    }
    impl EncodeWideNul for std::ffi::OsStr {
        fn encode_wide_nul(&self) -> Vec<u16> {
            use std::os::windows::ffi::OsStrExt;
            self.encode_wide().chain([0]).collect()
        }
    }

    /// Runs on the crashing thread for an exception nothing else handled.
    /// Writes the dump, then lets Windows carry on as before (its error
    /// report and the process ending).
    unsafe extern "system" fn on_crash(info: *const EXCEPTION_POINTERS) -> i32 {
        const CONTINUE_SEARCH: i32 = 0;
        static WRITTEN: AtomicBool = AtomicBool::new(false);
        let Some(path) = PATH.get() else { return CONTINUE_SEARCH };
        if WRITTEN.swap(true, Ordering::SeqCst) {
            return CONTINUE_SEARCH;
        }
        // SAFETY: `path` is NUL-terminated; the handle is closed below; the
        // exception pointers come from Windows and are valid for this call.
        unsafe {
            let file = CreateFileW(
                path.as_ptr(),
                GENERIC_WRITE,
                0,
                std::ptr::null(),
                CREATE_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            );
            if file == INVALID_HANDLE_VALUE {
                return CONTINUE_SEARCH;
            }
            let exception = MINIDUMP_EXCEPTION_INFORMATION {
                ThreadId: GetCurrentThreadId(),
                ExceptionPointers: info as *mut EXCEPTION_POINTERS,
                ClientPointers: 0,
            };
            MiniDumpWriteDump(
                GetCurrentProcess(),
                GetCurrentProcessId(),
                file,
                MiniDumpWithThreadInfo | MiniDumpWithIndirectlyReferencedMemory | MiniDumpWithUnloadedModules,
                &exception,
                std::ptr::null(),
                std::ptr::null(),
            );
            CloseHandle(file);
        }
        CONTINUE_SEARCH
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_dumps_are_reported_once_and_old_ones_pruned() {
        let dir = std::env::temp_dir().join(format!("linecard-crash-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("new")).unwrap();
        for i in 0..KEEP {
            fs::write(dir.join(format!("old-{i}.dmp")), b"x").unwrap();
        }
        // Older than everything above, so it's the one pruned.
        let oldest = dir.join("oldest.dmp");
        fs::write(&oldest, b"x").unwrap();
        fs::File::options()
            .write(true)
            .open(&oldest)
            .unwrap()
            .set_modified(std::time::SystemTime::UNIX_EPOCH)
            .unwrap();
        fs::write(dir.join("new").join("linecard-1.dmp"), b"x").unwrap();
        fs::write(dir.join("new").join("notes.txt"), b"x").unwrap();

        let moved = collect(&dir);
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].0, "linecard-1.dmp");
        assert!(dir.join("linecard-1.dmp").exists());
        assert!(!oldest.exists());
        let dumps = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".dmp"));
        assert_eq!(dumps.count(), KEEP);
        assert!(collect(&dir).is_empty(), "already reported");
        let _ = fs::remove_dir_all(&dir);
    }
}

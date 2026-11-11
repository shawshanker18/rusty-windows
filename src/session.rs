//! Being the Windows shell: the switch (and the way back), and the jobs Explorer does at sign-in that
//! someone must still do when Explorer isn't the shell: starting your startup apps.
//! The shell choice is the per-user value HKCU\Software\Microsoft\Windows NT\CurrentVersion\Winlogon
//! "Shell"; deleting it restores Explorer. Nothing machine-wide is touched.

use std::ffi::c_void;
use std::sync::atomic::AtomicBool;

use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

pub static SHELL: AtomicBool = AtomicBool::new(false);
const WINLOGON: PCWSTR = w!("Software\\Microsoft\\Windows NT\\CurrentVersion\\Winlogon");

pub fn exe() -> String {
    std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default()
}

/// Make shellbits the shell for this user from the next sign-in.
pub fn install() -> windows::core::Result<()> {
    let cmd = format!("\"{}\" --shell", exe());
    let v: Vec<u16> = cmd.encode_utf16().chain([0]).collect();
    unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, WINLOGON, w!("Shell"), REG_SZ.0, Some(v.as_ptr() as *const c_void), (v.len() * 2) as u32).ok() }
}

/// Back to Explorer: remove our value and, if no shell is running, start Explorer now.
pub fn restore() -> windows::core::Result<()> {
    unsafe {
        let r = RegDeleteKeyValueW(HKEY_CURRENT_USER, WINLOGON, w!("Shell"));
        if r != ERROR_SUCCESS && r != ERROR_FILE_NOT_FOUND {
            return Err(windows::core::Error::from(r.to_hresult()));
        }
    }
    Ok(())
}


pub fn installed() -> bool {
    let mut buf = [0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    unsafe {
        RegGetValueW(HKEY_CURRENT_USER, WINLOGON, w!("Shell"), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut c_void), Some(&mut size)).is_ok()
            && String::from_utf16_lossy(&buf[..(size as usize / 2).saturating_sub(1)]).contains("--shell")
    }
}

/// The emergency way back while running as the shell (Ctrl+Alt+Shift+E): restore the setting, start
/// Explorer as the shell once we're gone, and bring shellbits back in its normal (non-shell) mode.
pub fn back_to_explorer_now() {
    let _ = restore();
    let cmd = "cmd.exe /c ping -n 3 127.0.0.1 >nul & start \"\" explorer.exe & ping -n 6 127.0.0.1 >nul & schtasks /run /tn shellbits";
    spawn(cmd);
    std::process::exit(0);
}

fn spawn(cmdline: &str) -> bool {
    let mut cmd: Vec<u16> = cmdline.encode_utf16().chain([0]).collect();
    let si = STARTUPINFOW { cb: size_of::<STARTUPINFOW>() as u32, dwFlags: STARTF_USESHOWWINDOW, wShowWindow: 0, ..Default::default() };
    let mut pi = PROCESS_INFORMATION::default();
    unsafe {
        let ok = CreateProcessW(None, Some(PWSTR(cmd.as_mut_ptr())), None, None, false, CREATE_NO_WINDOW, None, None, &si, &mut pi).is_ok();
        if ok {
            let _ = CloseHandle(pi.hThread);
            let _ = CloseHandle(pi.hProcess);
        }
        ok
    }
}

// ---------------------------------------------------------------- startup apps

/// StartupApproved: first byte 2/6 = enabled, 3/7 = turned off in Task Manager / Settings.
pub fn approved_disabled(data: &[u8]) -> bool {
    data.first().is_some_and(|b| b & 1 == 1)
}

fn values(root: HKEY, key: PCWSTR) -> Vec<(String, Vec<u8>, u32)> {
    let mut out = Vec::new();
    unsafe {
        let mut h = HKEY::default();
        if RegOpenKeyExW(root, key, Some(0), KEY_READ, &mut h).is_err() {
            return out;
        }
        for i in 0.. {
            let mut name = [0u16; 512];
            let mut nlen = name.len() as u32;
            let mut ty = 0u32;
            let mut data = vec![0u8; 4096];
            let mut dlen = data.len() as u32;
            let r = RegEnumValueW(h, i, Some(PWSTR(name.as_mut_ptr())), &mut nlen, None, Some(&mut ty), Some(data.as_mut_ptr()), Some(&mut dlen));
            if r == ERROR_NO_MORE_ITEMS {
                break;
            }
            if r != ERROR_SUCCESS {
                continue;
            }
            data.truncate(dlen as usize);
            out.push((String::from_utf16_lossy(&name[..nlen as usize]), data, ty));
        }
        let _ = RegCloseKey(h);
    }
    out
}

fn reg_string(data: &[u8]) -> String {
    let w: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
    String::from_utf16_lossy(&w)
}

fn expand(s: &str) -> String {
    let src = HSTRING::from(s);
    let mut buf = vec![0u16; 4096];
    let n = unsafe { windows::Win32::System::Environment::ExpandEnvironmentStringsW(&src, Some(&mut buf)) } as usize;
    if n == 0 || n > buf.len() { s.to_string() } else { String::from_utf16_lossy(&buf[..n - 1]) }
}

/// What Explorer starts at sign-in: the Run keys and the Startup folders, minus what you turned off.
pub fn startup_entries() -> Vec<String> {
    let approved = |root: HKEY, sub: PCWSTR| -> Vec<(String, bool)> {
        values(root, sub).into_iter().map(|(n, d, _)| (n.to_lowercase(), approved_disabled(&d))).collect()
    };
    let base = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run");
    let base32 = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run32");
    let folder = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\StartupFolder");
    let off: Vec<String> = [approved(HKEY_CURRENT_USER, base), approved(HKEY_LOCAL_MACHINE, base), approved(HKEY_LOCAL_MACHINE, base32), approved(HKEY_CURRENT_USER, folder), approved(HKEY_LOCAL_MACHINE, folder)]
        .concat()
        .into_iter()
        .filter(|(_, d)| *d)
        .map(|(n, _)| n)
        .collect();
    let mut cmds = Vec::new();
    for (root, key) in [
        (HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run")),
        (HKEY_LOCAL_MACHINE, w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run")),
        (HKEY_LOCAL_MACHINE, w!("Software\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Run")),
    ] {
        for (name, data, ty) in values(root, key) {
            if (ty == REG_SZ.0 || ty == REG_EXPAND_SZ.0) && !off.contains(&name.to_lowercase()) {
                cmds.push(expand(&reg_string(&data)));
            }
        }
    }
    for id in [&FOLDERID_Startup, &FOLDERID_CommonStartup] {
        unsafe {
            if let Ok(p) = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) {
                let dir = p.to_string().unwrap_or_default();
                windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const c_void));
                if let Ok(rd) = std::fs::read_dir(&dir) {
                    for e in rd.flatten() {
                        let f = e.file_name().to_string_lossy().to_string();
                        if e.path().is_file() && !f.eq_ignore_ascii_case("desktop.ini") && !off.contains(&f.to_lowercase()) {
                            cmds.push(format!("\u{1}{}", e.path().display())); // \u{1} = open via the shell
                        }
                    }
                }
            }
        }
    }
    cmds
}

/// Start them once per sign-in (a restarted shell must not launch everything twice).
pub fn run_startup() {
    use windows::Win32::System::Threading::CreateMutexW;
    unsafe {
        let _ = CreateMutexW(None, true, w!("shellbits.startup.ran"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
    }
    std::thread::spawn(|| {
        for c in startup_entries() {
            if let Some(path) = c.strip_prefix('\u{1}') {
                unsafe {
                    ShellExecuteW(None, w!("open"), &HSTRING::from(path), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
                }
            } else {
                let mut cmd: Vec<u16> = c.encode_utf16().chain([0]).collect();
                let si = STARTUPINFOW { cb: size_of::<STARTUPINFOW>() as u32, ..Default::default() };
                let mut pi = PROCESS_INFORMATION::default();
                unsafe {
                    if CreateProcessW(None, Some(PWSTR(cmd.as_mut_ptr())), None, None, false, PROCESS_CREATION_FLAGS(0), None, None, &si, &mut pi).is_ok() {
                        let _ = CloseHandle(pi.hThread);
                        let _ = CloseHandle(pi.hProcess);
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn show_desktop_spares_the_shell() {
        assert!(super::minimisable("Chrome_WidgetWin_1"));
        assert!(super::minimisable("shellbits_settings"));
        assert!(!super::minimisable("shellbits_desktop"));
        assert!(!super::minimisable("shellbits_wall"));
        assert!(!super::minimisable("shellbits_taskbar"));
    }
    #[test]
    fn startup_approved_flags() {
        assert!(!super::approved_disabled(&[2, 0, 0]));
        assert!(!super::approved_disabled(&[6]));
        assert!(super::approved_disabled(&[3, 0]));
        assert!(super::approved_disabled(&[7]));
        assert!(!super::approved_disabled(&[]));
    }
}

// ---------------------------------------------------------------- shell hotkeys (Explorer's job otherwise)

use windows::Win32::UI::WindowsAndMessaging::*;

static HIDDEN: std::sync::Mutex<Vec<isize>> = std::sync::Mutex::new(Vec::new());

unsafe extern "system" fn collect(h: HWND, lp: LPARAM) -> windows::core::BOOL {
    let v = &mut *(lp.0 as *mut Vec<isize>);
    let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
    let mut cls = [0u16; 64];
    let n = GetClassNameW(h, &mut cls) as usize;
    // ask for the title with a time limit: a window that has stopped answering must not hold up the shell
    let mut len = 0usize;
    let titled = SendMessageTimeoutW(h, WM_GETTEXTLENGTH, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG, 200, Some(&mut len)).0 != 0 && len > 0;
    if IsWindowVisible(h).as_bool() && !IsIconic(h).as_bool() && titled && ex & WS_EX_TOOLWINDOW.0 == 0 && minimisable(&String::from_utf16_lossy(&cls[..n])) {
        v.push(h.0 as isize);
    }
    true.into()
}

/// Win+D has the desktop raised over the app windows (the desktop and wallpaper windows then stop pinning themselves to the back).
pub static DESKTOP_UP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Show desktop leaves the shell's own surfaces alone (desktop, wallpaper, taskbar, panels): only
/// app windows go down, ours included (Settings, File Explorer, Claude).
pub fn minimisable(class: &str) -> bool {
    !class.starts_with("shellbits_") || matches!(class, "shellbits_settings" | "shellbits_files" | "shellbits_claude")
}

/// Win+D / the taskbar corner: minimize everything, or bring it all back.
pub fn show_desktop(toggle: bool) {
    let trace = |m: String| {
        if std::env::var_os("SHELLBITS_TOAST_TEST").is_some() {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(std::env::temp_dir().join("shellbits-keys.txt")) {
                let _ = writeln!(f, "{m}");
            }
        }
    };
    unsafe {
        trace(format!("show_desktop called toggle={toggle}"));
        // As the shell, Win+D works like Explorer's: the desktop comes up over every window and goes
        // back under them on the next press. Nothing is minimised, so windows that refuse to minimise
        // are covered too, and everything returns exactly as it was.
        if toggle && SHELL.load(std::sync::atomic::Ordering::Relaxed) {
            if let (Ok(desk), Ok(wall)) = (FindWindowW(w!("shellbits_desktop"), None), FindWindowW(w!("shellbits_wall"), None)) {
                static BEFORE: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
                let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
                let fg = GetForegroundWindow();
                // an app brought forward since the last press means the desktop is no longer "shown"
                // the desktop cannot take the keyboard focus, so 'shown' means: raised, and still above the window that was in front
                let showing = DESKTOP_UP.load(std::sync::atomic::Ordering::Relaxed) && (fg.is_invalid() || fg == desk || fg == wall || fg == HWND(BEFORE.load(std::sync::atomic::Ordering::Relaxed) as *mut c_void));
                if showing {
                    DESKTOP_UP.store(false, std::sync::atomic::Ordering::Relaxed);
                    let _ = SetWindowPos(desk, Some(HWND_BOTTOM), 0, 0, 0, 0, flags);
                    let _ = SetWindowPos(wall, Some(HWND_BOTTOM), 0, 0, 0, 0, flags);
                    let back = HWND(BEFORE.swap(0, std::sync::atomic::Ordering::Relaxed) as *mut c_void);
                    if IsWindow(Some(back)).as_bool() {
                        crate::flyout::force_foreground(back);
                    }
                    trace("show_desktop: desktop sent back under the windows".into());
                } else {
                    BEFORE.store(fg.0 as isize, std::sync::atomic::Ordering::Relaxed);
                    DESKTOP_UP.store(true, std::sync::atomic::Ordering::Relaxed);
                    let _ = SetWindowPos(wall, Some(HWND_TOP), 0, 0, 0, 0, flags);
                    let _ = SetWindowPos(desk, Some(HWND_TOP), 0, 0, 0, 0, flags);
                    crate::flyout::force_foreground(desk);
                    trace("show_desktop: desktop raised over the windows".into());
                }
                return;
            }
        }
        let mut hidden = HIDDEN.lock().unwrap();
        trace(format!("show_desktop remembered={}", hidden.len()));
        if toggle && !hidden.is_empty() {
            for h in hidden.drain(..).rev() {
                let _ = ShowWindowAsync(HWND(h as *mut c_void), SW_RESTORE);
            }
            trace("show_desktop restored".into());
            return;
        }
        let mut v: Vec<isize> = Vec::new();
        let _ = EnumWindows(Some(collect), LPARAM(&mut v as *mut _ as isize));
        trace(format!("show_desktop found {} windows", v.len()));
        for h in &v {
            let _ = ShowWindowAsync(HWND(*h as *mut c_void), SW_MINIMIZE);
        }
        *hidden = v;
        // like Explorer: with every window down, the desktop itself takes the focus (never "no window at all")
        if SHELL.load(std::sync::atomic::Ordering::Relaxed) {
            if let Ok(d) = FindWindowW(w!("shellbits_desktop"), None) {
                // (a plain SetForegroundWindow is refused here; without this the focus can land on some
                // helper app's invisible window, as seen with an OEM service window)
                crate::flyout::force_foreground(d);
            }
        }
        trace("show_desktop minimised".into());
        if std::env::var_os("SHELLBITS_TOAST_TEST").is_some() {
            // test runs only: for 12 s, note twice a second whether input still reaches us and who has the focus
            std::thread::spawn(move || {
                for _ in 0..24 {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    let fg = GetForegroundWindow();
                    let mut cls = [0u16; 64];
                    let n = GetClassNameW(fg, &mut cls) as usize;
                    let bar = FindWindowW(w!("shellbits_taskbar"), None).unwrap_or_default();
                    let mut r = 0usize;
                    let ui_ok = SendMessageTimeoutW(bar, WM_NULL, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG, 300, Some(&mut r)).0 != 0;
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    trace(format!("hb fg={} ui_thread_answers={ui_ok} taskbar_visible={} keys_seen={} mouse_seen={} cursor={},{}", String::from_utf16_lossy(&cls[..n]), IsWindowVisible(bar).as_bool(), crate::launcher::KEY_EVENTS.load(std::sync::atomic::Ordering::Relaxed), crate::launcher::MOUSE_EVENTS.load(std::sync::atomic::Ordering::Relaxed), pt.x, pt.y));
                }
            });
        }
    }
}

/// Win+<key> while we are the shell. Returns true if we handled it.
pub fn hotkey(vk: u32, shift: bool) -> bool {
    let open = |s: PCWSTR| unsafe {
        ShellExecuteW(None, w!("open"), s, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    };
    unsafe {
        match (vk as u8, shift) {
            (b'E', false) => open(w!("explorer.exe")),
            (b'I', false) => open(w!("ms-settings:")),
            (b'S', true) => crate::snip::start(), // Windows' own overlay does not appear without Explorer
            (b'D', false) => show_desktop(true),
            (b'M', false) => show_desktop(false),
            (b'L', false) => {
                let _ = windows::Win32::System::Shutdown::LockWorkStation();
            }
            (0x25..=0x28, false) => {
                let fg = GetForegroundWindow();
                if fg.is_invalid() {
                    return true;
                }
                crate::snap::arrow_key(fg, vk); // halves, quarters, maximise, minimise - as Windows does
            }
            _ => return false,
        }
    }
    true
}


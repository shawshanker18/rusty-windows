#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! shellbits: Windows-style volume / media / lock-key flyouts + a Start/search launcher, in one tiny process.
//! Keys: the keyboard's volume + media keys, Caps/Num/Scroll Lock, Win (tap alone) for the launcher.
//! Extra (Ctrl+Alt+...): Space play/pause, Left/Right prev/next, Up/Down volume, M mute,
//! T pin the active window on top, Q quit.

mod alttab;
mod calc;
mod claude;
mod desktop;
mod files;
mod flyout;
mod launcher;
mod lucide;
mod notepad;
mod notify;
mod peek;
mod snap;
mod clip;
mod emoji;
mod lang;
mod perf;
mod qsx;
mod quick;
mod session;
mod settings;
mod taskbar;
mod toast;
mod tray;
mod wallpaper;
mod winx;

/// Measuring aid: `SHELLBITS_SKIP=tray,desktop,preview,toast,wall` leaves those parts of shell mode
/// unstarted, so shell-mem.ps1 can see what each one costs. Never set in normal use.
fn skip(part: &str) -> bool {
    std::env::var("SHELLBITS_SKIP").is_ok_and(|v| v.split(',').any(|p| p.trim() == part))
}

/// Video wallpaper, if wallpaper.mp4 sits next to the exe. Called again when Explorer restarts.
unsafe fn start_wallpaper() {
    if skip("wall") {
        return;
    }
    if let (Some(video), Ok(m)) = (wallpaper::video_path(), GetModuleHandleW(None)) {
        let attached = if session::SHELL.load(Relaxed) { wallpaper::attach_shell(m.into()) } else { wallpaper::attach(m.into()) };
        if let Some(h) = attached {
            wallpaper::spawn(h, video);
        }
    }
}

static TASKBAR_CREATED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

use std::ffi::c_void;
use std::sync::atomic::Ordering::Relaxed;

use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::Controls::{BufferedPaintInit, MARGINS};
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

// hotkey id -> Ctrl+Alt+key
const KEYS: [(i32, VIRTUAL_KEY); 7] = [
    (1, VK_SPACE), // play/pause
    (2, VK_RIGHT), // next
    (3, VK_LEFT),  // previous
    (4, VK_UP),    // volume up
    (5, VK_DOWN),  // volume down
    (6, VIRTUAL_KEY(b'M' as u16)), // mute
    (7, VIRTUAL_KEY(b'T' as u16)), // pin foreground window on top
];
const QUIT_ID: i32 = 99;
const RESTORE_ID: i32 = 98;
const CLAUDE_ID: i32 = 97;
pub(crate) const SETTINGS_ID: i32 = 96;
pub(crate) const CALC_ID: i32 = 93;
pub(crate) const NOTEPAD_ID: i32 = 92;
pub(crate) const FILES_ID: i32 = 95;

/// How a process ended: crashes (access violation, the abort a Rust panic ends in, ...) have exit codes
/// that start with 0xC; a clean exit, a kill from Task Manager or Stop-Process do not.
fn is_crash(exit_code: u32) -> bool {
    exit_code & 0xF000_0000 == 0xC000_0000
}

/// Restart after a crash unless it already crashed 3 times in the last minute (a crash at start-up must not loop).
fn may_restart(earlier_crashes: &[u64], now: u64) -> bool {
    earlier_crashes.iter().filter(|t| now.saturating_sub(**t) < 60).count() < 3
}

/// `shellbits --watchdog <pid> [the main copy's own arguments]`: wait for the main copy to end and, if it
/// crashed, start it again the same way. Every crash is noted in %TEMP%\shellbits-crashes.txt.
unsafe fn watchdog(args: &[String]) -> windows::core::Result<()> {
    use windows::Win32::System::Threading::*;
    let pid: u32 = args.get(2).and_then(|p| p.parse().ok()).unwrap_or(0);
    let h = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid)?;
    WaitForSingleObject(h, u32::MAX);
    let mut code = 0u32;
    GetExitCodeProcess(h, &mut code)?;
    if !is_crash(code) {
        return Ok(());
    }
    let log = std::env::temp_dir().join("shellbits-crashes.txt");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let old = std::fs::read_to_string(&log).unwrap_or_default();
    let earlier: Vec<u64> = old.lines().filter_map(|l| l.split_whitespace().next()?.parse().ok()).collect();
    let again = may_restart(&earlier, now);
    let _ = std::fs::write(&log, format!("{old}{now} exit code {code:#x} restarted={again}\n"));
    if again {
        if let Ok(exe) = std::env::current_exe() {
            // "--restarted" tells the new copy not to launch the sign-in startup apps a second time
            let extra = (!args.iter().any(|a| a == "--restarted")).then_some("--restarted");
            let _ = std::process::Command::new(exe).args(&args[3..]).args(extra).spawn();
        }
    }
    Ok(())
}

fn main() -> windows::core::Result<()> {
    unsafe {
        // test mode: render the Start menu to an image, touch nothing on screen
        let args: Vec<String> = std::env::args().collect();
        if args.get(1).map(String::as_str) == Some("--watchdog") {
            return watchdog(&args);
        }
        if args.get(1).map(String::as_str) == Some("--snapshot-qs") {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            return quick::snapshot(args.get(2).map(String::as_str).unwrap_or("qs.bmp"), args.get(3).map(String::as_str).unwrap_or(""));
        }
        if args.get(1).map(String::as_str) == Some("--snapshot-calc") {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            return calc::snapshot(args.get(2).map(String::as_str).unwrap_or("calc.bmp"), args.get(3).map(String::as_str).unwrap_or(""));
        }
        if args.get(1).map(String::as_str) == Some("--snapshot-alttab") {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            return alttab::snapshot(args.get(2).map(String::as_str).unwrap_or("alttab.bmp"));
        }
        if args.get(1).map(String::as_str) == Some("--snapshot-winx") {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            return winx::snapshot_menu(args.get(2).map(String::as_str).unwrap_or("winx.bmp"));
        }
        if args.get(1).map(String::as_str) == Some("--snapshot-run") {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            return winx::snapshot_run(args.get(2).map(String::as_str).unwrap_or("run.bmp"));
        }
        // the shell switch and the way back (per-user setting, takes effect at next sign-in)
        match args.get(1).map(String::as_str) {
            Some("--install-shell") => return session::install(),
            Some("--restore-explorer") => return session::restore(),
            Some("--jumplist-selftest") => {
                // what Windows keeps as recent items for an app id, e.g. Microsoft.Windows.Explorer
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                let id = args.get(2).cloned().unwrap_or_default();
                let v = taskbar::jump_list(&id, 10);
                let _ = std::fs::write(args.get(3).map(String::as_str).unwrap_or("jumplist-selftest.txt"), format!("{id}: {} items
{}", v.len(), v.iter().map(|(n, p)| format!("{n} -> {p}")).collect::<Vec<_>>().join("
")));
                return Ok(());
            }
            Some("--nc-selftest") => {
                notify::selftest(args.get(2).map(String::as_str).unwrap_or("nc-selftest.txt"));
                return Ok(());
            }
            Some("--toast-selftest") => {
                toast::selftest(args.get(2).map(String::as_str).unwrap_or("toast-selftest.txt"));
                return Ok(());
            }
            Some("--lang-selftest") => {
                let _ = std::fs::write(args.get(2).map(String::as_str).unwrap_or("lang-selftest.txt"), lang::report());
                return Ok(());
            }
            Some("--emoji-selftest") => {
                let _ = std::fs::write(args.get(2).map(String::as_str).unwrap_or("emoji-selftest.txt"), emoji::report());
                return Ok(());
            }
            Some("--clip-selftest") => {
                // how many text items Windows' clipboard history holds, and their lengths (never the text itself)
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED); // Windows' clipboard only answers single-threaded callers
                let report = match clip::history() {
                    None => format!("clipboard history is off (Windows says: {:?})", windows::ApplicationModel::DataTransfer::Clipboard::IsHistoryEnabled()),
                    Some(v) => format!("{} text items, lengths: {:?}", v.len(), v.iter().map(|i| i.1.chars().count()).collect::<Vec<_>>()),
                };
                let _ = std::fs::write(args.get(2).map(String::as_str).unwrap_or("clip-selftest.txt"), report);
                return Ok(());
            }
            Some("--zones-selftest") => {
                let _ = CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
                let _ = std::fs::write(args.get(2).map(String::as_str).unwrap_or("zones-selftest.txt"), launcher::zone_report());
                return Ok(());
            }
            Some("--bt-selftest") => {
                // how long Windows takes to list paired Bluetooth devices, and what it returns
                let _ = CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
                let t = std::time::Instant::now();
                let v = qsx::bt_devices();
                let _ = std::fs::write(args.get(2).map(String::as_str).unwrap_or("bt-selftest.txt"), format!("{} ms, {} devices: {}", t.elapsed().as_millis(), v.len(), v.iter().map(|d| format!("{} ({})", d.name, d.sub)).collect::<Vec<_>>().join(", ")));
                return Ok(());
            }
            Some("--mixer-selftest") => {
                // list the mixer, nudge one app's volume by 1%, read it back, restore it
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                let before = qsx::mixer();
                let mut log: Vec<String> = before.iter().map(|i| format!("{} = {}%", i.name, i.signal)).collect();
                if let Some(first) = before.first() {
                    let target = if first.signal >= 50 { first.signal - 1 } else { first.signal + 1 };
                    qsx::set_app_volume(&first.name, target as f32 / 100.0);
                    let after = qsx::mixer().into_iter().find(|i| i.name == first.name).map(|i| i.signal);
                    qsx::set_app_volume(&first.name, first.signal as f32 / 100.0);
                    let restored = qsx::mixer().into_iter().find(|i| i.name == first.name).map(|i| i.signal);
                    log.push(format!("set {} to {target}% -> read back {after:?}; restored -> {restored:?}", first.name));
                    log.push(if after == Some(target) && restored == Some(first.signal) { "PASS".into() } else { "FAIL".into() });
                }
                let _ = std::fs::write("mixer-selftest.txt", log.join("
"));
                return Ok(());
            }
            Some("--list-startup") => {
                let _ = std::fs::write("startup-apps.txt", session::startup_entries().join("
"));
                return Ok(());
            }
            _ => {}
        }
        if args.iter().any(|a| a == "--shell") {
            session::SHELL.store(true, Relaxed);
        }
        if args.get(1).map(String::as_str) == Some("--snapshot-settings") {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            return settings::snapshot(args.get(2).map(String::as_str).unwrap_or("settings.bmp"), args.get(3).map(String::as_str).unwrap_or("System"));
        }
        if args.get(1).map(String::as_str) == Some("--snapshot-claude") {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            return claude::snapshot(args.get(2).map(String::as_str).unwrap_or("claude.bmp"));
        }
        if args.get(1).map(String::as_str) == Some("--snapshot-desktop") {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            return desktop::snapshot(args.get(2).map(String::as_str).unwrap_or("desktop.bmp"));
        }
        if args.get(1).map(String::as_str) == Some("--tray-selftest") {
            return tray::selftest();
        }
        if args.get(1).map(String::as_str) == Some("--snapshot-tray") {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            return tray::snapshot(args.get(2).map(String::as_str).unwrap_or("tray.bmp"));
        }
        if args.get(1).map(String::as_str) == Some("--snapshot-nc") {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            return notify::snapshot(args.get(2).map(String::as_str).unwrap_or("nc.bmp"), args.iter().any(|a| a == "collapsed")).inspect_err(|e| eprintln!("snapshot: {e:?}"));
        }
        if args.get(1).map(String::as_str) == Some("--snapshot-taskbar") {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            return taskbar::snapshot(args.get(2).map(String::as_str).unwrap_or("taskbar.bmp"));
        }
        if args.get(1).map(String::as_str) == Some("--snapshot") {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let mode = args.get(3).cloned().unwrap_or_else(|| "start".into());
            let q = args.get(4).cloned().unwrap_or_default();
            let scroll = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(0.0);
            return launcher::snapshot(args.get(2).map(String::as_str).unwrap_or("start.bmp"), &mode, &q, scroll);
        }
        // one copy only: a second launch just exits (or asks the running one to open Claude Code)
        let single = windows::Win32::System::Threading::CreateMutexW(None, true, w!("shellbits.single"));
        if GetLastError() == ERROR_ALREADY_EXISTS && session::SHELL.load(Relaxed) {
            // the shell always wins: ask the normal copy (started by the logon task) to quit, take over
            if let Ok(f) = FindWindowW(w!("shellbits_flyout"), None) {
                let _ = PostMessageW(Some(f), WM_HOTKEY, WPARAM(QUIT_ID as usize), LPARAM(0));
            }
            if let Ok(m) = &single {
                let _ = windows::Win32::System::Threading::WaitForSingleObject(*m, 5000);
            }
        } else if GetLastError() == ERROR_ALREADY_EXISTS {
            if let Some(i) = args.iter().position(|a| a == "--files") {
                // `--files <folder>` opens that folder; without one, the last folder (or your user folder)
                match args.get(i + 1) {
                    Some(path) => files::request(path),
                    None => {
                        if let Ok(f) = FindWindowW(w!("shellbits_flyout"), None) {
                            let _ = PostMessageW(Some(f), WM_HOTKEY, WPARAM(FILES_ID as usize), LPARAM(0));
                        }
                    }
                }
            }
            if let Some(i) = args.iter().position(|a| a == "--notepad") {
                // `--notepad <file>` opens that file
                if let Some(path) = args.get(i + 1) {
                    notepad::request(path);
                }
                if let Ok(f) = FindWindowW(w!("shellbits_flyout"), None) {
                    let _ = PostMessageW(Some(f), WM_HOTKEY, WPARAM(NOTEPAD_ID as usize), LPARAM(0));
                }
            }
            if args.iter().any(|a| a == "--calc") {
                if let Ok(f) = FindWindowW(w!("shellbits_flyout"), None) {
                    let _ = PostMessageW(Some(f), WM_HOTKEY, WPARAM(CALC_ID as usize), LPARAM(0));
                }
            }
            if args.iter().any(|a| a == "--settings") {
                if let Ok(f) = FindWindowW(w!("shellbits_flyout"), None) {
                    let _ = PostMessageW(Some(f), WM_HOTKEY, WPARAM(SETTINGS_ID as usize), LPARAM(0));
                }
            }
            if args.iter().any(|a| a == "--claude") {
                if let Ok(f) = FindWindowW(w!("shellbits_flyout"), None) {
                    let _ = PostMessageW(Some(f), WM_HOTKEY, WPARAM(CLAUDE_ID as usize), LPARAM(0));
                }
            }
            return Ok(());
        }
        // a second, idle copy of this program waits for this one: if this one crashes, it starts it again
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe).arg("--watchdog").arg(std::process::id().to_string()).args(&args[1..]).spawn();
        }
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        BufferedPaintInit()?;
        let inst = GetModuleHandleW(None)?.into();

        let mut wc = WNDCLASSW {
            lpfnWndProc: Some(flyout_proc),
            hInstance: inst,
            lpszClassName: w!("shellbits_flyout"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        wc.lpfnWndProc = Some(launcher::proc);
        wc.lpszClassName = w!("shellbits_launcher");
        RegisterClassW(&wc);
        wc.lpfnWndProc = Some(launcher::pill_proc);
        wc.lpszClassName = w!("shellbits_pill");
        RegisterClassW(&wc);
        wc.lpfnWndProc = Some(quick::qs_proc);
        wc.lpszClassName = w!("shellbits_qs");
        RegisterClassW(&wc);
        wc.lpfnWndProc = Some(quick::media_proc);
        wc.lpszClassName = w!("shellbits_media");
        RegisterClassW(&wc);
        wc.lpfnWndProc = Some(notify::nc_proc);
        wc.lpszClassName = w!("shellbits_nc");
        RegisterClassW(&wc);
        wc.lpfnWndProc = Some(notify::cal_proc);
        wc.lpszClassName = w!("shellbits_cal");
        RegisterClassW(&wc);
        wc.lpfnWndProc = Some(taskbar::proc);
        wc.lpszClassName = w!("shellbits_taskbar");
        wc.style = CS_DBLCLKS; // tray icons get double-clicks, like Explorer's
        RegisterClassW(&wc);
        wc.style = WNDCLASS_STYLES(0);
        wc.lpfnWndProc = Some(wallpaper::wall_proc);
        wc.lpszClassName = w!("shellbits_wall");
        RegisterClassW(&wc);
        wc.lpfnWndProc = Some(alttab::proc);
        wc.lpszClassName = w!("shellbits_alttab");
        RegisterClassW(&wc);
        wc.lpfnWndProc = Some(toast::proc);
        wc.lpszClassName = w!("shellbits_toast");
        RegisterClassW(&wc);
        wc.lpfnWndProc = Some(tray::flow_proc);
        wc.lpszClassName = w!("shellbits_trayflow");
        RegisterClassW(&wc);
        wc.lpfnWndProc = Some(winx::menu_proc);
        wc.lpszClassName = w!("shellbits_winx");
        RegisterClassW(&wc);
        wc.lpfnWndProc = Some(winx::run_proc);
        wc.lpszClassName = w!("shellbits_run");
        RegisterClassW(&wc);
        start_wallpaper();

        // --- flyout (also the main window: owns hotkeys + timers) ---
        let fly = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            w!("shellbits_flyout"), w!("shellbits"), WS_POPUP,
            0, 0, 192, 48, None, None, Some(inst), None,
        )?;
        flyout::FLYOUT.store(fly.0 as isize, Relaxed);
        fluent_window(fly)?;

        // --- launcher (takes focus so you can type) ---
        let launch = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("shellbits_launcher"), w!("Search"), WS_POPUP,
            0, 0, 640, 720, None, None, Some(inst), None,
        )?;
        launcher::LAUNCHER.store(launch.0 as isize, Relaxed);
        // our copy of the taskbar search box, shown on top of it while you type
        let pill = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            w!("shellbits_pill"), w!(""), WS_POPUP,
            0, 0, 1, 1, None, None, Some(inst), None,
        )?;
        launcher::PILL.store(pill.0 as isize, Relaxed);
        // Quick Settings + the media card above it
        let qs = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST, w!("shellbits_qs"), w!("Quick settings"), WS_POPUP, 0, 0, 356, 386, None, None, Some(inst), None)?;
        quick::QS.store(qs.0 as isize, Relaxed);
        fluent_window(qs)?;
        let media = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE, w!("shellbits_media"), w!("Media"), WS_POPUP, 0, 0, 356, 176, None, None, Some(inst), None)?;
        quick::MEDIA.store(media.0 as isize, Relaxed);
        fluent_window(media)?;
        // notification center + calendar
        let nc = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST, w!("shellbits_nc"), w!("Notification Center"), WS_POPUP, 0, 0, 336, 400, None, None, Some(inst), None)?;
        notify::NC.store(nc.0 as isize, Relaxed);
        // the notification list is NOT fetched at start-up: it costs ~17 MB for good (measured with
        // mem-probe.ps1), so it is loaded the first time the panel is opened
        fluent_window(nc)?;
        let cal = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE, w!("shellbits_cal"), w!("Calendar"), WS_POPUP, 0, 0, 336, 455, None, None, Some(inst), None)?;
        notify::CAL.store(cal.0 as isize, Relaxed);
        fluent_window(cal)?;
        fluent_window(launch)?;
        // Alt+Tab switcher (never takes focus: the keyboard hook drives it)
        let at = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST, w!("shellbits_alttab"), w!("Task Switching"), WS_POPUP, 0, 0, 1, 1, None, None, Some(inst), None)?;
        alttab::ALTTAB.store(at.0 as isize, Relaxed);
        fluent_window(at)?;
        // tray overflow (^) flyout; the tray host itself only exists when we are the shell
        let flow = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST, w!("shellbits_trayflow"), w!("Show hidden icons"), WS_POPUP, 0, 0, 1, 1, None, None, Some(inst), None)?;
        tray::FLOW.store(flow.0 as isize, Relaxed);
        fluent_window(flow)?;
        if args.iter().any(|a| a == "--shell") {
            if !skip("tray") {
                tray::create_host(inst, w!("Shell_TrayWnd"))?;
            }
            if !skip("desktop") {
                desktop::create(inst)?;
            }
            if !skip("preview") {
                taskbar::show_preview(inst)?;
            }
            taskbar::reserve_space();
            if !args.iter().any(|a| a == "--restarted") {
                session::run_startup();
            }
            // notification banners (Explorer shows them otherwise)
            let t = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE, w!("shellbits_toast"), w!("New notification"), WS_POPUP, 0, 0, 364, 108, None, None, Some(inst), None)?;
            toast::TOAST.store(t.0 as isize, Relaxed);
            fluent_window(t)?;
            if !skip("toast") {
                toast::watch();
            }
            if !skip("tray") {
                tray::announce();
            }
        }
        snap::create(inst)?;
        clip::create(inst)?;
        emoji::create(inst)?;
        lang::create(inst)?;
        // Win+X menu (+ its shut-down submenu) and the Run box
        let wx = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST, w!("shellbits_winx"), w!("Quick Link"), WS_POPUP, 0, 0, 1, 1, None, None, Some(inst), None)?;
        winx::MENU.store(wx.0 as isize, Relaxed);
        fluent_window(wx)?;
        let wsub = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE, w!("shellbits_winx"), w!("Shut down or sign out"), WS_POPUP, 0, 0, 1, 1, None, None, Some(inst), None)?;
        winx::SUB.store(wsub.0 as isize, Relaxed);
        fluent_window(wsub)?;
        let run = CreateWindowExW(WINDOW_EX_STYLE(0), w!("shellbits_run"), w!("Run"), WS_POPUP | WS_CLIPCHILDREN, 0, 0, 400, 200, None, None, Some(inst), None)?;
        winx::RUN.store(run.0 as isize, Relaxed);
        let _ = PostMessageW(Some(launch), launcher::WM_PRELOAD, WPARAM(0), LPARAM(0));
        if args.iter().any(|a| a == "--taskbar") && !session::SHELL.load(Relaxed) {
            taskbar::show_preview(inst)?; // preview: sits on top of Explorer's taskbar
        }
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Relaxed);
        // Taskbar button positions move as icons change. UI Automation calls into Explorer and can
        // stall, so they run on their own thread, never the one that draws our panels.
        std::thread::spawn(|| {
            let _ = CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
            loop {
                launcher::update_search_rect();
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        });

        for (id, vk) in KEYS {
            let _ = RegisterHotKey(Some(fly), id, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, vk.0 as u32);
        }
        // Claim the keyboard's real media/volume keys: we handle them, so Windows' own flyout never shows.
        // Volume keys allow auto-repeat (hold to ramp).
        for (id, vk) in [(11, VK_MEDIA_PLAY_PAUSE), (12, VK_MEDIA_NEXT_TRACK), (13, VK_MEDIA_PREV_TRACK),
                         (14, VK_VOLUME_UP), (15, VK_VOLUME_DOWN), (16, VK_VOLUME_MUTE)] {
            let _ = RegisterHotKey(Some(fly), id, HOT_KEY_MODIFIERS(0), vk.0 as u32);
        }
        let _ = RegisterHotKey(Some(fly), QUIT_ID, MOD_CONTROL | MOD_ALT, b'Q' as u32);
        // emergency way back to Explorer while shellbits is the shell
        let _ = RegisterHotKey(Some(fly), RESTORE_ID, MOD_CONTROL | MOD_ALT | MOD_SHIFT, b'E' as u32);
        // Claude Code in a native Rust window
        if RegisterHotKey(Some(fly), CLAUDE_ID, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, b'C' as u32).is_err() {
            let _ = std::fs::write(std::env::temp_dir().join("shellbits-hotkeys.txt"), "Ctrl+Alt+C is taken by another app");
        }
        // PowerToys Always On Top shortcut
        let _ = RegisterHotKey(Some(fly), 17, MOD_WIN | MOD_CONTROL | MOD_NOREPEAT, b'T' as u32);

        // Lock keys (observed) + lone Win tap (launcher). The hook lives on its own thread so a
        // busy UI thread can never delay your typing (Windows also drops hooks that answer late).
        std::thread::spawn(|| {
            let inst = GetModuleHandleW(None).ok().map(|m| m.into());
            let _hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(flyout::kb_hook), inst, 0);
            let _mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(launcher::mouse_hook), inst, 0);
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).into() {}
        });

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Ok(())
    }
}

/// Dark, rounded, acrylic-backed window like Windows 11 flyouts.
unsafe fn fluent_window(h: HWND) -> windows::core::Result<()> {
    let set = |attr: DWMWINDOWATTRIBUTE, v: u32| {
        let _ = DwmSetWindowAttribute(h, attr, &v as *const _ as *const c_void, 4);
    };
    set(DWMWA_USE_IMMERSIVE_DARK_MODE, 1);
    set(DWMWA_WINDOW_CORNER_PREFERENCE, 2); // round (8px)
    set(DWMWA_SYSTEMBACKDROP_TYPE, 3); // acrylic
    DwmExtendFrameIntoClientArea(h, &MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 })
}

// ---------------- hotkeys -> flyout ----------------
unsafe extern "system" fn flyout_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg != 0 && msg == TASKBAR_CREATED.load(Relaxed) {
        start_wallpaper(); // Explorer restarted: the old desktop layer is gone
        return LRESULT(0);
    }
    match msg {
        WM_HOTKEY => {
            let id = wp.0 as i32;
            let id = if (11..=17).contains(&id) { id - 10 } else { id }; // physical keys / Win+Ctrl+T -> same actions
            match id {
                94 => {
                    // show desktop / bring windows back (what Win+D does when we are the shell); also lets tests run it
                    std::thread::spawn(|| session::show_desktop(true));
                    return LRESULT(0);
                }
                FILES_ID => {
                    if let Ok(m) = GetModuleHandleW(None) {
                        files::show(m.into(), files::take_request());
                    }
                    return LRESULT(0);
                }
                NOTEPAD_ID => {
                    if let Ok(m) = GetModuleHandleW(None) {
                        notepad::show(m.into(), notepad::take_request());
                    }
                    return LRESULT(0);
                }
                CALC_ID => {
                    if let Ok(m) = GetModuleHandleW(None) {
                        calc::show(m.into());
                    }
                    return LRESULT(0);
                }
                SETTINGS_ID => {
                    if let Ok(m) = GetModuleHandleW(None) {
                        settings::show(m.into());
                    }
                    return LRESULT(0);
                }
                CLAUDE_ID => {
                    if let Ok(m) = GetModuleHandleW(None) {
                        claude::show(m.into());
                    }
                    return LRESULT(0);
                }
                RESTORE_ID => {
                    if session::SHELL.load(Relaxed) {
                        session::back_to_explorer_now();
                    }
                    return LRESULT(0);
                }
                91 => {
                    // crash test: only when the test has left its flag file (crash-test.ps1)
                    let flag = std::env::temp_dir().join("shellbits-crash-test");
                    if std::fs::remove_file(flag).is_ok() {
                        panic!("crash test");
                    }
                    return LRESULT(0);
                }
                QUIT_ID => {
                    PostQuitMessage(0);
                    return LRESULT(0);
                }
                1..=3 => flyout::media(id), // repaints again when the worker posts WM_TRACK
                4..=6 => flyout::volume(id),
                7 => flyout::pin_foreground(),
                _ => return LRESULT(0),
            }
            flyout::show(hwnd);
            LRESULT(0)
        }
        flyout::WM_TRACK => {
            let _ = InvalidateRect(Some(hwnd), None, false); // new track info / album art arrived
            LRESULT(0)
        }
        flyout::WM_LOCK => {
            flyout::lock_key(wp.0 as u32);
            flyout::show(hwnd);
            LRESULT(0)
        }
        WM_TIMER => {
            match wp.0 {
                flyout::T_HIDE => flyout::hide_tick(hwnd),
                flyout::T_ANIM => flyout::animate(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            flyout::click(hwnd, lp);
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            flyout::paint(hwnd);
            LRESULT(0)
        }
        WM_SETTINGCHANGE | WM_DPICHANGED => {
            flyout::reset_gfx();
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod watchdog_tests {
    #[test]
    fn crashes_restart_but_not_in_a_loop() {
        assert!(super::is_crash(0xC0000409)); // what a Rust panic ends in here
        assert!(super::is_crash(0xC0000005)); // access violation
        assert!(!super::is_crash(0)); // clean exit
        assert!(!super::is_crash(1)); // taskkill
        assert!(!super::is_crash(0xFFFFFFFF)); // Stop-Process
        assert!(super::may_restart(&[], 1000));
        assert!(super::may_restart(&[100, 200, 990, 995], 1000)); // two recent ones
        assert!(!super::may_restart(&[950, 990, 995], 1000)); // three within the minute: stay down
    }
}

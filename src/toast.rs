//! Notification banners (toasts) while Rust UI is the shell: a 364x108 card, 16px from the right
//! edge above the taskbar, shown for 5 seconds when a new notification arrives. Click opens the app.
//! Silent while Do Not Disturb is on, like Windows.

use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};
use std::sync::Mutex;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::flyout::{rect, utf16};
use crate::quick::rgb;

pub static TOAST: AtomicIsize = AtomicIsize::new(0);
const WM_TOAST: u32 = WM_APP + 110;
const T_HIDE: usize = 9;
const W: f32 = 364.0;
const H: f32 = 108.0;

/// (app, title, body, app id) of the banner on screen.
static CUR: Mutex<Option<(String, String, String, String)>> = Mutex::new(None);

/// `--toast-selftest <file>`: for 20 s, time how long Windows takes to list notifications and when
/// its store changes on disk (post a notification meanwhile to see how late we would notice it).
pub fn selftest(path: &str) {
    use windows::Win32::Storage::FileSystem::*;
    use windows::Win32::System::Threading::WaitForSingleObject;
    let mut log = Vec::new();
    let clock = || {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        format!("{}.{:03}", t.as_secs() % 100_000, t.subsec_millis())
    };
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let n = crate::notify::ids().len();
            log.push(format!("{} listing {n} ids took {} ms", clock(), t.elapsed().as_millis()));
        }
        let mut next = crate::notify::ids().into_iter().max().unwrap_or(0) + 1;
        let t = std::time::Instant::now();
        let hit = crate::notify::get(next - 1).is_some();
        log.push(format!("{} asking for the newest one (id {}) took {} ms, found={hit}", clock(), next - 1, t.elapsed().as_millis()));
        let dir = format!(r"{}\Microsoft\Windows\Notifications", std::env::var("LOCALAPPDATA").unwrap_or_default());
        let Ok(h) = FindFirstChangeNotificationW(&HSTRING::from(dir), false, FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE) else { return };
        let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < end {
            // every 250 ms: did the list change, and did the store on disk change?
            let changed = WaitForSingleObject(h, 250) == windows::Win32::Foundation::WAIT_OBJECT_0;
            if changed {
                let _ = FindNextChangeNotification(h);
                log.push(format!("{} store changed on disk", clock()));
            }
            let t = std::time::Instant::now();
            // ids count up: a new notification is the next id (allow a few skipped numbers)
            if let Some(id) = (next..next + 4).find(|i| crate::notify::get(*i).is_some()) {
                log.push(format!("{} new notification id {id} seen (probe took {} ms)", clock(), t.elapsed().as_millis()));
                next = id + 1;
            } else if t.elapsed().as_millis() > 100 {
                log.push(format!("{} slow probe: {} ms", clock(), t.elapsed().as_millis()));
            }
        }
    }
    let _ = std::fs::write(path, log.join("\n"));
}

/// Watch for new notifications.
pub fn watch() {
    std::thread::spawn(|| unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        let test = std::env::var_os("SHELLBITS_TOAST_TEST").is_some();
        let t0 = std::time::Instant::now();
        let diag = |m: &str| {
            if test {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(std::env::temp_dir().join("shellbits-toast.txt")) {
                    let _ = writeln!(f, "{:>6}ms {m}", t0.elapsed().as_millis());
                }
            }
        };
        let show = move |n: crate::notify::Note| {
            if test || !crate::notify::dnd_get() {
                *CUR.lock().unwrap() = Some((n.app, n.title, n.body, n.aumid));
                let _ = PostMessageW(Some(HWND(TOAST.load(Relaxed) as *mut c_void)), WM_TOAST, WPARAM(0), LPARAM(0));
            }
        };
        // Windows tells us the moment a notification arrives; asking for the whole list takes seconds
        if crate::notify::on_new(show) {
            diag("subscribed");
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3600)); // keep the subscription's thread alive
            }
        }
        diag("subscription refused: watching the store");
        // Listing every notification takes ~5.5 s on this PC (measured with --toast-selftest), asking for
        // one by id ~30 ms, and ids count up - so list once, then just ask for the next few ids whenever
        // Windows writes to its notification store.
        let newest = || crate::notify::ids().into_iter().max().unwrap_or(0);
        let mut next = newest() + 1;
        let mut synced = std::time::Instant::now();
        use windows::Win32::Storage::FileSystem::*;
        use windows::Win32::System::Threading::WaitForSingleObject;
        let dir = format!(r"{}\Microsoft\Windows\Notifications", std::env::var("LOCALAPPDATA").unwrap_or_default());
        let change = FindFirstChangeNotificationW(&HSTRING::from(dir), false, FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE).ok();
        diag(&format!("watching store: {}, next id {next}", change.is_some()));
        loop {
            match change {
                Some(h) => {
                    WaitForSingleObject(h, u32::MAX);
                    let _ = FindNextChangeNotification(h);
                }
                None => std::thread::sleep(std::time::Duration::from_secs(2)),
            }
            // ponytail: looks 8 ids ahead; a burst that skips more is caught by the re-list every 10 minutes
            let mut new = None;
            while let Some((id, n)) = (next..next + 8).find_map(|i| crate::notify::get(i).map(|n| (i, n))) {
                next = id + 1;
                new = Some(n);
            }
            if new.is_none() && synced.elapsed().as_secs() > 600 {
                next = next.max(newest() + 1);
                synced = std::time::Instant::now();
            }
            diag(&format!("checked, new={:?}", new.as_ref().map(|n| n.title.clone())));
            if let Some(n) = new {
                show(n);
            }
        }
    });
}

unsafe fn draw(rt: &ID2D1DCRenderTarget) -> windows::core::Result<()> {
    rt.Clear(Some(&crate::flyout::base(rgb(0x2C2C2C))));
    let Some((app, title, body, _)) = CUR.lock().unwrap().clone() else { return Ok(()) };
    let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
    let fmt = |size: f32, weight| dw.CreateTextFormat(w!("Segoe UI Variable Text"), None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"));
    let white = rt.CreateSolidColorBrush(&rgb(0xFFFFFF), None)?;
    let dim = rt.CreateSolidColorBrush(&rgb(0xC5C5C5), None)?;
    let text = |s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush| rt.DrawText(&utf16(s), f, &r, b, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
    text(&app, &fmt(12.0, DWRITE_FONT_WEIGHT_NORMAL)?, rect(16.0, 12.0, W - 16.0, 30.0), &dim);
    text(&title, &fmt(14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?, rect(16.0, 36.0, W - 16.0, 56.0), &white);
    text(&body, &fmt(14.0, DWRITE_FONT_WEIGHT_NORMAL)?, rect(16.0, 57.0, W - 16.0, H - 12.0), &white);
    Ok(())
}

pub unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TOAST => {
            let mut wa = RECT::default();
            let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut wa as *mut _ as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
            let s = GetDpiForWindow(hwnd) as f32 / 96.0;
            let (w_, h) = ((W * s) as i32, (H * s) as i32);
            let m = (16.0 * s) as i32;
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), wa.right - w_ - m, wa.bottom - h - m, w_, h, SWP_SHOWWINDOW | SWP_NOACTIVATE);
            let _ = UpdateWindow(hwnd);
            crate::flyout::slide(&[hwnd], true); // rises in like Windows' banners
            SetTimer(Some(hwnd), T_HIDE, 5000, None);
        }
        WM_TIMER => {
            let _ = KillTimer(Some(hwnd), T_HIDE);
            crate::flyout::slide(&[hwnd], false);
        }
        WM_LBUTTONUP => {
            let _ = ShowWindow(hwnd, SW_HIDE);
            if let Some((.., aumid)) = CUR.lock().unwrap().clone() {
                if !aumid.is_empty() {
                    windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), &HSTRING::from(format!("shell:AppsFolder\\{aumid}")), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
                }
            }
        }
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let params = BP_PAINTPARAMS { cbSize: size_of::<BP_PAINTPARAMS>() as u32, dwFlags: BPPF_ERASE, ..Default::default() };
            let mut mem = HDC::default();
            let pb = BeginBufferedPaint(hdc, &rc, BPBF_TOPDOWNDIB, Some(&params), &mut mem);
            if !mem.is_invalid() {
                let made = (|| -> windows::core::Result<()> {
                    let f: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
                    let props = D2D1_RENDER_TARGET_PROPERTIES {
                        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                        ..Default::default()
                    };
                    let rt = f.CreateDCRenderTarget(&props)?;
                    rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                    rt.BindDC(mem, &rc)?;
                    let dpi = GetDpiForWindow(hwnd) as f32;
                    rt.SetDpi(dpi, dpi);
                    rt.BeginDraw();
                    draw(&rt)?;
                    rt.EndDraw(None, None)
                })();
                let _ = made;
                let _ = EndBufferedPaint(pb, true);
            }
            let _ = EndPaint(hwnd, &ps);
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

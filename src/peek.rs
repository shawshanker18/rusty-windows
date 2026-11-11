//! Taskbar hover preview: rest the mouse on an app's taskbar icon and, after 400 ms, a small card
//! with the window's title and a live thumbnail (DWM) appears above it, like Windows 11's.

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};

use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

pub static PEEK: AtomicIsize = AtomicIsize::new(0);
const T_SHOW: usize = 1;
const MAX_W: f32 = 220.0;
const MAX_H: f32 = 124.0;
const PAD: f32 = 8.0;
const TITLE: f32 = 28.0;

thread_local! {
    static TARGET: Cell<(isize, i32, i32)> = const { Cell::new((0, 0, 0)) }; // window, icon centre x, taskbar top (screen px)
    static THUMB: Cell<isize> = const { Cell::new(0) };
}

/// Thumbnail size that fits MAX_W x MAX_H keeping the window's shape.
pub fn fit(w: f32, h: f32) -> (f32, f32) {
    if w <= 0.0 || h <= 0.0 {
        return (MAX_W, MAX_H);
    }
    let s = (MAX_W / w).min(MAX_H / h);
    ((w * s).round(), (h * s).round())
}

/// Called by the taskbar when the hovered app changes (None = hide).
pub unsafe fn hover(target: Option<(HWND, i32, i32)>) {
    let p = HWND(PEEK.load(Relaxed) as *mut c_void);
    if std::env::var_os("SHELLBITS_TOAST_TEST").is_some() {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(std::env::temp_dir().join("shellbits-keys.txt")) {
            let _ = writeln!(f, "peek hover target={:?} window={:?}", target.map(|t| (t.1, t.2)), p.0 as isize);
        }
    }
    if p.is_invalid() {
        return;
    }
    hide(p);
    if let Some((h, cx, top)) = target {
        TARGET.with(|t| t.set((h.0 as isize, cx, top)));
        SetTimer(Some(p), T_SHOW, 400, None);
    }
}

unsafe fn hide(p: HWND) {
    let _ = KillTimer(Some(p), T_SHOW);
    let t = THUMB.with(|t| t.replace(0));
    if t != 0 {
        let _ = DwmUnregisterThumbnail(t);
    }
    let _ = ShowWindow(p, SW_HIDE);
}

unsafe fn show(p: HWND) {
    let (src, cx, top) = TARGET.with(|t| t.get());
    let src = HWND(src as *mut c_void);
    if !IsWindow(Some(src)).as_bool() {
        return;
    }
    let s = GetDpiForWindow(p) as f32 / 96.0;
    let mut r = RECT::default();
    // a minimised window has no size of its own: use where it would restore to
    let mut wp = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
    if IsIconic(src).as_bool() && GetWindowPlacement(src, &mut wp).is_ok() {
        r = wp.rcNormalPosition;
    } else {
        let _ = GetWindowRect(src, &mut r);
    }
    let (tw, th) = fit((r.right - r.left) as f32, (r.bottom - r.top) as f32);
    let (w_, h) = (((tw + PAD * 2.0) * s) as i32, ((th + PAD + TITLE) * s) as i32);
    let _ = SetWindowPos(p, Some(HWND_TOPMOST), cx - w_ / 2, top - h - (8.0 * s) as i32, w_, h, SWP_SHOWWINDOW | SWP_NOACTIVATE);
    if let Ok(t) = DwmRegisterThumbnail(p, src) {
        THUMB.with(|c| c.set(t));
        let props = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY,
            rcDestination: RECT { left: (PAD * s) as i32, top: (TITLE * s) as i32, right: ((PAD + tw) * s) as i32, bottom: ((TITLE + th) * s) as i32 },
            opacity: 255,
            fVisible: true.into(),
            ..Default::default()
        };
        let _ = DwmUpdateThumbnailProperties(t, &props);
    }
    let _ = InvalidateRect(Some(p), None, true);
}

pub unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            let _ = KillTimer(Some(hwnd), T_SHOW);
            show(hwnd);
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let bg = CreateSolidBrush(COLORREF(0x2C2C2C));
            FillRect(dc, &rc, bg);
            let _ = DeleteObject(bg.into());
            let s = GetDpiForWindow(hwnd) as f32 / 96.0;
            let src = HWND(TARGET.with(|t| t.get()).0 as *mut c_void);
            let mut buf = [0u16; 200];
            let n = GetWindowTextW(src, &mut buf) as usize;
            let font = CreateFontW(-(12.0 * s) as i32, 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI Variable Text"));
            let old = SelectObject(dc, font.into());
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(0xFFFFFF));
            let mut tr = RECT { left: (PAD * s) as i32, top: 0, right: rc.right - (PAD * s) as i32, bottom: (TITLE * s) as i32 };
            DrawTextW(dc, &mut buf[..n], &mut tr, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
            SelectObject(dc, old);
            let _ = DeleteObject(font.into());
            let _ = EndPaint(hwnd, &ps);
        }
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

pub unsafe fn create(inst: HINSTANCE) -> windows::core::Result<()> {
    let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: inst, lpszClassName: w!("shellbits_peek"), ..Default::default() };
    RegisterClassW(&wc);
    let h = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE, w!("shellbits_peek"), w!(""), WS_POPUP, 0, 0, 10, 10, None, None, Some(inst), None)?;
    let (dark, round) = (1u32, 2u32);
    let _ = DwmSetWindowAttribute(h, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const c_void, 4);
    let _ = DwmSetWindowAttribute(h, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as *const c_void, 4);
    PEEK.store(h.0 as isize, Relaxed);
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn thumbnails_keep_shape() {
        assert_eq!(super::fit(1920.0, 1080.0), (220.0, 124.0));
        assert_eq!(super::fit(500.0, 1000.0), (62.0, 124.0));
        assert_eq!(super::fit(0.0, 0.0), (220.0, 124.0));
    }
}

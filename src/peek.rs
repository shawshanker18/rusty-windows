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
            let s = GetDpiForWindow(hwnd) as f32 / 96.0;
            let src = HWND(TARGET.with(|t| t.get()).0 as *mut c_void);
            let mut buf = [0u16; 200];
            let n = GetWindowTextW(src, &mut buf) as usize;
            // Direct2D into a buffer that keeps alpha: Windows' blurred panel material shows through
            use windows::Win32::UI::Controls::{BeginBufferedPaint, EndBufferedPaint, BPBF_TOPDOWNDIB, BPPF_ERASE, BP_PAINTPARAMS};
            let params = BP_PAINTPARAMS { cbSize: size_of::<BP_PAINTPARAMS>() as u32, dwFlags: BPPF_ERASE, ..Default::default() };
            let mut mem = HDC::default();
            let pb = BeginBufferedPaint(dc, &rc, BPBF_TOPDOWNDIB, Some(&params), &mut mem);
            if !mem.is_invalid() {
                let _ = draw(mem, &rc, s, &String::from_utf16_lossy(&buf[..n]));
                let _ = EndBufferedPaint(pb, true);
            }
            let _ = EndPaint(hwnd, &ps);
        }
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

/// The preview's frame: see-through panel, window title on the top row (the thumbnail is DWM's).
unsafe fn draw(dc: HDC, rc: &RECT, s: f32, title: &str) -> windows::core::Result<()> {
    use windows::Win32::Graphics::Direct2D::Common::*;
    use windows::Win32::Graphics::Direct2D::*;
    use windows::Win32::Graphics::DirectWrite::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use crate::flyout::{rect, utf16};
    use crate::quick::rgb;
    let f: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
    let props = D2D1_RENDER_TARGET_PROPERTIES { pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }, ..Default::default() };
    let rt = f.CreateDCRenderTarget(&props)?;
    rt.BindDC(dc, rc)?;
    rt.SetDpi(96.0 * s, 96.0 * s);
    let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
    let fmt = dw.CreateTextFormat(w!("Segoe UI Variable Text"), None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 12.0, w!(""))?;
    fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
    fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
    let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
    fmt.SetTrimming(&trim, None)?;
    let w = rc.right as f32 / s;
    rt.BeginDraw();
    rt.Clear(Some(&crate::flyout::base(rgb(0x2C2C2C))));
    rt.DrawText(&utf16(title), &fmt, &rect(PAD, 0.0, w - PAD, TITLE), &rt.CreateSolidColorBrush(&rgb(0xFFFFFF), None)?, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
    rt.EndDraw(None, None)
}

pub unsafe fn create(inst: HINSTANCE) -> windows::core::Result<()> {
    let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: inst, lpszClassName: w!("shellbits_peek"), ..Default::default() };
    RegisterClassW(&wc);
    let h = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE, w!("shellbits_peek"), w!(""), WS_POPUP, 0, 0, 10, 10, None, None, Some(inst), None)?;
    let (dark, round) = (1u32, 2u32);
    let _ = DwmSetWindowAttribute(h, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const c_void, 4);
    let _ = DwmSetWindowAttribute(h, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as *const c_void, 4);
    let _ = crate::fluent_window(h); // Windows' blurred panel material
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

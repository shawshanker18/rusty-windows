//! Snap layouts: Win+Z shows the layout picker by the active window's top-right corner; click a
//! zone (or press the layout's number, then the zone's number) and the window moves into it.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering::Relaxed};

use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub static SNAP: AtomicIsize = AtomicIsize::new(0);
static OPEN: AtomicBool = AtomicBool::new(false);
const WM_SNAP_OPEN: u32 = WM_APP + 60; // lParam: the window to arrange
const WM_SNAP_KEY: u32 = WM_APP + 61; // wParam: digit 1..9, or 0 for Esc
const WM_SNAP_ARROW: u32 = WM_APP + 71; // wParam: the arrow's vk, lParam: the window (Win+arrow)

type Zone = (f32, f32, f32, f32); // x, y, width, height as fractions of the work area

/// Windows 11's six layouts for a wide screen.
pub const LAYOUTS: [&[Zone]; 6] = [
    &[(0.0, 0.0, 0.5, 1.0), (0.5, 0.0, 0.5, 1.0)],
    &[(0.0, 0.0, 2.0 / 3.0, 1.0), (2.0 / 3.0, 0.0, 1.0 / 3.0, 1.0)],
    &[(0.0, 0.0, 1.0 / 3.0, 1.0), (1.0 / 3.0, 0.0, 1.0 / 3.0, 1.0), (2.0 / 3.0, 0.0, 1.0 / 3.0, 1.0)],
    &[(0.0, 0.0, 0.5, 1.0), (0.5, 0.0, 0.5, 0.5), (0.5, 0.5, 0.5, 0.5)],
    &[(0.0, 0.0, 0.5, 0.5), (0.5, 0.0, 0.5, 0.5), (0.0, 0.5, 0.5, 0.5), (0.5, 0.5, 0.5, 0.5)],
    &[(0.0, 0.0, 0.25, 1.0), (0.25, 0.0, 0.5, 1.0), (0.75, 0.0, 0.25, 1.0)],
];

// the picker: three tiles per row
const PAD: f32 = 14.0; // measured on Windows' own picker: 98x64 tiles, 12 apart, 14 of padding
const TILE_W: f32 = 98.0;
const TILE_H: f32 = 64.0;
const GAP: f32 = 12.0;
const W: f32 = PAD * 2.0 + TILE_W * 3.0 + GAP * 2.0;
const H: f32 = PAD * 2.0 + TILE_H * 2.0 + GAP;

thread_local! {
    static TARGET: Cell<isize> = const { Cell::new(0) };
    static HOVER: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
    static PICKED: Cell<Option<usize>> = const { Cell::new(None) }; // layout chosen by its number, waiting for the zone's
}

/// Where a zone lands inside a work area, in pixels (edges shared with its neighbours).
pub fn zone_px(work: (i32, i32, i32, i32), z: Zone) -> (i32, i32, i32, i32) {
    let (l, t, r, b) = work;
    let (w_, h) = ((r - l) as f32, (b - t) as f32);
    let x0 = l + (z.0 * w_).round() as i32;
    let y0 = t + (z.1 * h).round() as i32;
    let x1 = l + ((z.0 + z.2) * w_).round() as i32;
    let y1 = t + ((z.1 + z.3) * h).round() as i32;
    (x0, y0, x1, y1)
}

/// A zone's box inside the picker, in picker units.
fn tile_zone(layout: usize, z: Zone) -> (f32, f32, f32, f32) {
    let (tx, ty) = (PAD + (layout % 3) as f32 * (TILE_W + GAP), PAD + (layout / 3) as f32 * (TILE_H + GAP));
    // zones share a 3px gap inside the tile, none at its outer edge
    let g = |v: f32| if v > 0.01 && v < 0.99 { 1.5 } else { 0.0 };
    (tx + z.0 * TILE_W + g(z.0), ty + z.1 * TILE_H + g(z.1), tx + (z.0 + z.2) * TILE_W - g(z.0 + z.2), ty + (z.1 + z.3) * TILE_H - g(z.1 + z.3))
}

/// Which (layout, zone) is under a point of the picker.
pub fn hit(x: f32, y: f32) -> Option<(usize, usize)> {
    LAYOUTS.iter().enumerate().find_map(|(l, zones)| {
        zones.iter().position(|z| {
            let r = tile_zone(l, *z);
            x >= r.0 && x < r.2 && y >= r.1 && y < r.3
        })
        .map(|z| (l, z))
    })
}

/// Move `h` into a zone of its monitor's work area, so that its visible frame fills the zone.
unsafe fn place(h: HWND, z: Zone) {
    let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    if !GetMonitorInfoW(MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() {
        return;
    }
    if IsZoomed(h).as_bool() || IsIconic(h).as_bool() {
        let _ = ShowWindow(h, SW_RESTORE);
    }
    // windows carry an invisible resize border: measure it so the visible edges meet the zone
    let (mut outer, mut seen) = (RECT::default(), RECT::default());
    let _ = GetWindowRect(h, &mut outer);
    if DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS, &mut seen as *mut _ as *mut c_void, size_of::<RECT>() as u32).is_err() {
        seen = outer;
    }
    let (ml, mt, mr, mb) = (seen.left - outer.left, seen.top - outer.top, outer.right - seen.right, outer.bottom - seen.bottom);
    let w_ = mi.rcWork;
    let (x0, y0, x1, y1) = zone_px((w_.left, w_.top, w_.right, w_.bottom), z);
    let _ = SetWindowPos(h, None, x0 - ml, y0 - mt, x1 - x0 + ml + mr, y1 - y0 + mt + mb, SWP_NOZORDER | SWP_NOACTIVATE);
}

unsafe fn hide(p: HWND) {
    OPEN.store(false, Relaxed);
    let _ = KillTimer(Some(p), 1);
    HOVER.with(|c| c.set(None));
    PICKED.with(|c| c.set(None));
    let _ = ShowWindow(p, SW_HIDE);
}

unsafe fn choose(p: HWND, layout: usize, zone: usize) {
    let target = HWND(TARGET.with(|t| t.get()) as *mut c_void);
    hide(p);
    if let Some(z) = LAYOUTS.get(layout).and_then(|l| l.get(zone)) {
        if IsWindow(Some(target)).as_bool() {
            place(target, *z);
            assist_start(layout, zone, target);
        }
    }
}

pub unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    match msg {
        WM_SNAP_OPEN => {
            let target = HWND(lp.0 as *mut c_void);
            // only ordinary, resizable windows can be arranged
            let style = GetWindowLongW(target, GWL_STYLE) as u32;
            if !IsWindow(Some(target)).as_bool() || style & WS_THICKFRAME.0 == 0 {
                return LRESULT(0);
            }
            TARGET.with(|t| t.set(target.0 as isize));
            HOVER.with(|c| c.set(None));
            PICKED.with(|c| c.set(None));
            let mut r = RECT::default();
            if DwmGetWindowAttribute(target, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut _ as *mut c_void, size_of::<RECT>() as u32).is_err() {
                let _ = GetWindowRect(target, &mut r);
            }
            // under the window's maximize button, kept on its screen
            let (w_, h) = ((W * s) as i32, (H * s) as i32);
            let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(MonitorFromWindow(target, MONITOR_DEFAULTTONEAREST), &mut mi);
            // centred under the maximize button, 32px below the window's top, like Windows' own
            let x = (r.right - (69.0 * s) as i32 - w_ / 2).clamp(mi.rcWork.left, (mi.rcWork.right - w_).max(mi.rcWork.left));
            let y = (r.top + (32.0 * s) as i32).clamp(mi.rcWork.top, (mi.rcWork.bottom - h).max(mi.rcWork.top));
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w_, h, SWP_SHOWWINDOW | SWP_NOACTIVATE);
            OPEN.store(true, Relaxed);
            SetTimer(Some(hwnd), 1, 200, None); // watch for the window going away or losing focus
            let _ = InvalidateRect(Some(hwnd), None, true);
        }
        WM_SNAP_ARROW => {
            let h = HWND(lp.0 as *mut c_void);
            let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !IsWindow(Some(h)).as_bool() || !GetMonitorInfoW(MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() {
                return LRESULT(0);
            }
            let mut f = RECT::default();
            if DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS, &mut f as *mut _ as *mut c_void, size_of::<RECT>() as u32).is_err() {
                let _ = GetWindowRect(h, &mut f);
            }
            let w_ = mi.rcWork;
            let sizable = GetWindowLongW(h, GWL_STYLE) as u32 & WS_THICKFRAME.0 != 0;
            match arrow((f.left, f.top, f.right, f.bottom), (w_.left, w_.top, w_.right, w_.bottom), IsZoomed(h).as_bool(), wp.0 as u32) {
                Arrow::Max => _ = ShowWindowAsync(h, SW_MAXIMIZE),
                Arrow::Min => _ = ShowWindowAsync(h, SW_MINIMIZE),
                Arrow::Restore => _ = ShowWindowAsync(h, SW_RESTORE),
                Arrow::Zone(_) if !sizable => {}
                // a half: the other half then offers the remaining windows
                Arrow::Zone(z) if z.3 == 1.0 => {
                    TARGET.with(|t| t.set(lp.0));
                    choose(hwnd, 0, (z.0 > 0.0) as usize);
                }
                Arrow::Zone(z) => place(h, z),
            }
        }
        WM_TIMER => {
            // the picker belongs to one window: when that window closes or another app comes forward, it goes too
            let target = HWND(TARGET.with(|t| t.get()) as *mut c_void);
            if !IsWindow(Some(target)).as_bool() || GetForegroundWindow() != target {
                hide(hwnd);
            }
        }
        WM_SNAP_KEY => {
            let n = wp.0;
            match (n, PICKED.with(|c| c.get())) {
                (0, _) => hide(hwnd),
                (n, None) if n <= LAYOUTS.len() => {
                    PICKED.with(|c| c.set(Some(n - 1)));
                    let _ = InvalidateRect(Some(hwnd), None, true);
                }
                (n, Some(l)) if n <= LAYOUTS[l].len() => choose(hwnd, l, n - 1),
                _ => {}
            }
        }
        crate::flyout::WM_DISMISS => hide(hwnd),
        WM_MOUSEMOVE => {
            let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            let h = hit((lp.0 & 0xFFFF) as i16 as f32 / s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s);
            if HOVER.with(|c| c.replace(h)) != h {
                let _ = InvalidateRect(Some(hwnd), None, true);
            }
        }
        WM_MOUSELEAVE => {
            HOVER.with(|c| c.set(None));
            let _ = InvalidateRect(Some(hwnd), None, true);
        }
        WM_LBUTTONUP => {
            if let Some((l, z)) = hit((lp.0 & 0xFFFF) as i16 as f32 / s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s) {
                choose(hwnd, l, z);
            }
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            // Direct2D into a buffer that keeps alpha: Windows' blurred panel material shows through
            use windows::Win32::UI::Controls::{BeginBufferedPaint, EndBufferedPaint, BPBF_TOPDOWNDIB, BPPF_ERASE, BP_PAINTPARAMS};
            let params = BP_PAINTPARAMS { cbSize: size_of::<BP_PAINTPARAMS>() as u32, dwFlags: BPPF_ERASE, ..Default::default() };
            let mut mem = HDC::default();
            let pb = BeginBufferedPaint(dc, &rc, BPBF_TOPDOWNDIB, Some(&params), &mut mem);
            if !mem.is_invalid() {
                let _ = draw_picker(mem, &rc, s, HOVER.with(|c| c.get()), PICKED.with(|c| c.get()));
                let _ = EndBufferedPaint(pb, true);
            }
            let _ = EndPaint(hwnd, &ps);
        }
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

#[derive(Debug, PartialEq)]
pub enum Arrow {
    Zone(Zone),
    Max,
    Min,
    Restore,
}

/// What Win+arrow does to a window whose visible frame is `frame` on a screen with work area `work`:
/// Left/Right give that half (a quarter stays a quarter, on that side); Up goes half -> top quarter,
/// bottom quarter -> half, otherwise maximise; Down goes maximised -> restored, top quarter -> half,
/// half -> bottom quarter, otherwise minimise.
pub fn arrow(frame: (i32, i32, i32, i32), work: (i32, i32, i32, i32), zoomed: bool, vk: u32) -> Arrow {
    let near = |z: Zone| {
        let r = zone_px(work, z);
        [(r.0, frame.0), (r.1, frame.1), (r.2, frame.2), (r.3, frame.3)].iter().all(|(a, b)| (a - b).abs() <= 2)
    };
    let half = |side: f32| (side, 0.0, 0.5, 1.0);
    let quarter = |side: f32, row: f32| (side, row, 0.5, 0.5);
    // where it is now: (side, None = the whole half / Some(row) = a quarter)
    let at = [0.0, 0.5].iter().find_map(|&s| {
        if near(half(s)) { Some((s, None)) } else { [0.0, 0.5].iter().find(|&&r| near(quarter(s, r))).map(|&r| (s, Some(r))) }
    });
    match (vk, at) {
        (0x25 | 0x27, at) => {
            let side = if vk == 0x25 { 0.0 } else { 0.5 };
            Arrow::Zone(match at { Some((_, Some(row))) => quarter(side, row), _ => half(side) })
        }
        (0x26, Some((s, None))) => Arrow::Zone(quarter(s, 0.0)),
        (0x26, Some((s, Some(r)))) if r > 0.0 => Arrow::Zone(half(s)),
        (0x26, _) => Arrow::Max,
        (_, _) if zoomed => Arrow::Restore,
        (_, Some((s, Some(r)))) if r == 0.0 => Arrow::Zone(half(s)),
        (_, Some((s, None))) => Arrow::Zone(quarter(s, 0.5)),
        _ => Arrow::Min,
    }
}

/// Win+arrow on window `h` (shell mode). Any thread.
pub unsafe fn arrow_key(h: HWND, vk: u32) {
    let _ = PostMessageW(Some(HWND(SNAP.load(Relaxed) as *mut c_void)), WM_SNAP_ARROW, WPARAM(vk as usize), LPARAM(h.0 as isize));
}

/// Win+Z: open the picker for the window in front, or close it if it is open. Called from the keyboard hook.
pub unsafe fn toggle() {
    let p = HWND(SNAP.load(Relaxed) as *mut c_void);
    if p.is_invalid() {
        return;
    }
    if OPEN.load(Relaxed) {
        let _ = PostMessageW(Some(p), WM_SNAP_KEY, WPARAM(0), LPARAM(0));
    } else {
        let _ = PostMessageW(Some(p), WM_SNAP_OPEN, WPARAM(0), LPARAM(GetForegroundWindow().0 as isize));
    }
}

/// While the picker is open, number keys choose and Esc closes. Returns true to swallow the key.
pub unsafe fn on_key(k: &KBDLLHOOKSTRUCT, up: bool) -> bool {
    if k.dwExtraInfo == crate::launcher::MARK {
        return false;
    }
    if ASSIST_OPEN.load(Relaxed) {
        // Snap Assist: arrows/Tab move, Enter picks, Esc leaves the zone empty
        if !matches!(k.vkCode, 0x1B | 0x0D | 0x09 | 0x25..=0x28) {
            return false;
        }
        if !up {
            let _ = PostMessageW(Some(HWND(ASSIST.load(Relaxed) as *mut c_void)), WM_ASSIST_KEY, WPARAM(k.vkCode as usize), LPARAM(0));
        }
        return true;
    }
    if !OPEN.load(Relaxed) {
        return false;
    }
    let n = match k.vkCode {
        0x1B => 0,
        v @ 0x31..=0x39 => (v - 0x30) as usize,
        v @ 0x61..=0x69 => (v - 0x60) as usize, // number pad
        _ => return false,
    };
    if !up {
        let _ = PostMessageW(Some(HWND(SNAP.load(Relaxed) as *mut c_void)), WM_SNAP_KEY, WPARAM(n), LPARAM(0));
    }
    true
}

/// The picker: Windows' layouts as tiles of zones; the zone under the mouse in the accent colour,
/// the layout under the mouse (or chosen by number) a step lighter, each layout's number on a plate.
unsafe fn draw_picker(dc: HDC, rc: &RECT, s: f32, hover: Option<(usize, usize)>, picked: Option<usize>) -> windows::core::Result<()> {
    use windows::Win32::Graphics::Direct2D::Common::*;
    use windows::Win32::Graphics::Direct2D::*;
    use windows::Win32::Graphics::DirectWrite::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use crate::flyout::{rect, rrect, utf16};
    use crate::quick::rgb;
    let f: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
    let props = D2D1_RENDER_TARGET_PROPERTIES { pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }, ..Default::default() };
    let rt = f.CreateDCRenderTarget(&props)?;
    rt.BindDC(dc, rc)?;
    rt.SetDpi(96.0 * s, 96.0 * s);
    let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
    let num = dw.CreateTextFormat(w!("Segoe UI Variable Text"), None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 12.0, w!(""))?;
    num.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
    num.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
    let b = |c: u32| rt.CreateSolidColorBrush(&rgb(c), None);
    rt.BeginDraw();
    rt.Clear(Some(&crate::flyout::base(rgb(0x202020))));
    for (l, zones) in LAYOUTS.iter().enumerate() {
        for (zi, z) in zones.iter().enumerate() {
            let colour = if hover == Some((l, zi)) { 0x4CC2FF } else if picked == Some(l) || hover.is_some_and(|h| h.0 == l) { 0x606060 } else { 0x454545 };
            let r = tile_zone(l, *z);
            let zr = rect(r.0, r.1, r.2, r.3);
            rt.FillRoundedRectangle(&rrect(zr, 4.0), &b(colour)?);
            rt.DrawRoundedRectangle(&rrect(rect(r.0 + 0.5, r.1 + 0.5, r.2 - 0.5, r.3 - 0.5), 4.0), &b(0x5F5F5F)?, 1.0, None);
        }
    }
    for l in 0..LAYOUTS.len() {
        let (cx, cy) = (PAD + (l % 3) as f32 * (TILE_W + GAP) + TILE_W / 2.0, PAD + (l / 3) as f32 * (TILE_H + GAP) + TILE_H / 2.0);
        let plate = rect(cx - 7.0, cy - 12.0, cx + 7.0, cy + 12.0);
        rt.FillRoundedRectangle(&rrect(plate, 2.0), &b(0x6B6B6B)?);
        rt.DrawText(&utf16(&(l + 1).to_string()), &num, &plate, &b(0xFFFFFF)?, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
    }
    rt.EndDraw(None, None)
}

/// Snap Assist's cards: the one under the mouse lighter, the keyboard's choice outlined in the accent
/// colour, each with its window's title (icons are added afterwards).
unsafe fn draw_assist(dc: HDC, rc: &RECT, s: f32, cards: &[((f32, f32, f32, f32), String, isize)], sel: usize, hover: Option<usize>) -> windows::core::Result<()> {
    use windows::Win32::Graphics::Direct2D::Common::*;
    use windows::Win32::Graphics::Direct2D::*;
    use windows::Win32::Graphics::DirectWrite::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use crate::flyout::{rect, rrect, utf16};
    use crate::quick::rgb;
    let f: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
    let props = D2D1_RENDER_TARGET_PROPERTIES { pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }, ..Default::default() };
    let rt = f.CreateDCRenderTarget(&props)?;
    rt.BindDC(dc, rc)?;
    rt.SetDpi(96.0 * s, 96.0 * s);
    let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
    let title = dw.CreateTextFormat(w!("Segoe UI Variable Text"), None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 12.0, w!(""))?;
    title.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
    title.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
    let b = |c: u32| rt.CreateSolidColorBrush(&rgb(c), None);
    rt.BeginDraw();
    rt.Clear(Some(&crate::flyout::base(rgb(0x202020))));
    for (i, ((x, y, w, h), name, icon)) in cards.iter().enumerate() {
        let card = rect(x - 6.0, y - 4.0, x + w + 6.0, y + h + 6.0);
        if i == sel || hover == Some(i) {
            rt.FillRoundedRectangle(&rrect(card, 5.0), &b(0x383838)?);
        }
        if i == sel {
            rt.DrawRoundedRectangle(&rrect(rect(card.left + 1.0, card.top + 1.0, card.right - 1.0, card.bottom - 1.0), 5.0), &b(0x4CC2FF)?, 2.0, None);
        }
        let left = if *icon != 0 { x + 22.0 } else { *x };
        rt.DrawText(&utf16(name), &title, &rect(left, *y, x + w, y + A_HEAD * w / A_BOX.0), &b(0xFFFFFF)?, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
    }
    rt.EndDraw(None, None)
}

pub unsafe fn create(inst: HINSTANCE) -> windows::core::Result<()> {
    let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: inst, lpszClassName: w!("shellbits_snap"), hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
    RegisterClassW(&wc);
    let h = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE, w!("shellbits_snap"), w!("Snap layouts"), WS_POPUP, 0, 0, 10, 10, None, None, Some(inst), None)?;
    let (dark, round) = (1u32, 2u32);
    let _ = DwmSetWindowAttribute(h, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const c_void, 4);
    let _ = DwmSetWindowAttribute(h, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as *const c_void, 4);
    let _ = crate::fluent_window(h); // Windows' blurred panel material
    SNAP.store(h.0 as isize, Relaxed);
    let wc = WNDCLASSW { lpfnWndProc: Some(assist_proc), hInstance: inst, lpszClassName: w!("shellbits_assist"), hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
    RegisterClassW(&wc);
    let a = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE, w!("shellbits_assist"), w!("Snap Assist"), WS_POPUP, 0, 0, 10, 10, None, None, Some(inst), None)?;
    let _ = DwmSetWindowAttribute(a, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const c_void, 4);
    let _ = crate::fluent_window(a); // Windows' blurred panel material
    ASSIST.store(a.0 as isize, Relaxed);
    Ok(())
}

// ======================= Snap Assist =======================
// After a window is put in a zone, the next empty zone shows the other open windows; picking one
// puts it there, and so on until the layout is full or you click away / press Esc.

pub static ASSIST: AtomicIsize = AtomicIsize::new(0);
static ASSIST_OPEN: AtomicBool = AtomicBool::new(false);
const WM_ASSIST_KEY: u32 = WM_APP + 69; // wParam: vk
const WM_ASSIST_PICK: u32 = WM_APP + 70; // wParam: the window to put in the zone on offer
const A_HEAD: f32 = 28.0; // title row above each picture
const A_GAP: f32 = 16.0;
const A_BOX: (f32, f32) = (300.0, 190.0); // the most room one window's picture gets

#[derive(Default)]
struct Assist {
    layout: usize,
    filled: Vec<bool>,
    zone: usize,
    placed: Vec<isize>,
    wins: Vec<(isize, Vec<u16>, f32, isize, isize)>, // window, title, width/height, DWM thumbnail, its small icon (0 = none)
    cols: usize,
    cells: Vec<(f32, f32, f32, f32)>, // x, y, width, height of each card (title row + picture)
    sel: usize,
    hover: Option<usize>,
}
thread_local! {
    static AS: RefCell<Assist> = RefCell::new(Assist::default());
}

/// Cards for `n` windows inside a zone of w x h: as many columns as keep the cards biggest,
/// every row centred. Returns the column count and each card's box.
pub fn assist_cells(n: usize, w: f32, h: f32) -> (usize, Vec<(f32, f32, f32, f32)>) {
    let (mut best, mut cols) = (0.0f32, 1);
    for c in 1..=n {
        let rows = n.div_ceil(c) as f32;
        let k = ((w - A_GAP * (c as f32 + 1.0)) / (c as f32 * A_BOX.0)).min((h - A_GAP * (rows + 1.0)) / (rows * (A_BOX.1 + A_HEAD))).min(1.0);
        if k >= best {
            (best, cols) = (k, c);
        }
    }
    let k = best.max(0.05);
    let (cw, ch) = (A_BOX.0 * k, (A_BOX.1 + A_HEAD) * k);
    let rows = n.div_ceil(cols);
    let y0 = (h - rows as f32 * ch - (rows as f32 - 1.0) * A_GAP) / 2.0;
    let cells = (0..n)
        .map(|i| {
            let (r, c) = (i / cols, i % cols);
            let in_row = if r + 1 == rows { n - r * cols } else { cols } as f32;
            let x0 = (w - in_row * cw - (in_row - 1.0) * A_GAP) / 2.0;
            (x0 + c as f32 * (cw + A_GAP), y0 + r as f32 * (ch + A_GAP), cw, ch)
        })
        .collect();
    (cols, cells)
}

/// A window's own small icon (owned by that window - never destroyed here). 0 if it has none to give.
unsafe fn small_icon(h: HWND) -> isize {
    // asked with a short timeout: a hung app must not hang the panel
    let ask = |which: u32| {
        let mut r = 0usize;
        let _ = SendMessageTimeoutW(h, WM_GETICON, WPARAM(which as usize), LPARAM(0), SMTO_ABORTIFHUNG, 50, Some(&mut r));
        r as isize
    };
    [ask(ICON_SMALL2), ask(ICON_SMALL), GetClassLongPtrW(h, GCLP_HICONSM) as isize, ask(ICON_BIG), GetClassLongPtrW(h, GCLP_HICON) as isize].into_iter().find(|i| *i != 0).unwrap_or(0)
}

fn assist_hit(cells: &[(f32, f32, f32, f32)], x: f32, y: f32) -> Option<usize> {
    cells.iter().position(|c| x >= c.0 && x < c.0 + c.2 && y >= c.1 && y < c.1 + c.3)
}

unsafe fn assist_hide(p: HWND, a: &mut Assist) {
    ASSIST_OPEN.store(false, Relaxed);
    let _ = KillTimer(Some(p), 1);
    for w in a.wins.drain(..) {
        if w.3 != 0 {
            let _ = DwmUnregisterThumbnail(w.3);
        }
    }
    a.cells.clear();
    a.hover = None;
    let _ = ShowWindow(p, SW_HIDE);
}

/// Offer the remaining windows in the next empty zone, or finish.
unsafe fn assist_next(p: HWND, a: &mut Assist) {
    assist_hide(p, a);
    let rest: Vec<HWND> = crate::alttab::windows().into_iter().filter(|h| !a.placed.contains(&(h.0 as isize))).collect();
    let (Some(zone), false) = (a.filled.iter().position(|f| !f), rest.is_empty()) else { return };
    a.zone = zone;
    let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    let first = HWND(a.placed[0] as *mut c_void);
    if !GetMonitorInfoW(MonitorFromWindow(first, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() {
        return;
    }
    let w_ = mi.rcWork;
    let (x0, y0, x1, y1) = zone_px((w_.left, w_.top, w_.right, w_.bottom), LAYOUTS[a.layout][zone]);
    let _ = SetWindowPos(p, Some(HWND_TOPMOST), x0, y0, x1 - x0, y1 - y0, SWP_NOACTIVATE);
    let s = GetDpiForWindow(p) as f32 / 96.0;
    (a.cols, a.cells) = assist_cells(rest.len(), (x1 - x0) as f32 / s, (y1 - y0) as f32 / s);
    a.sel = 0;
    for (h, c) in rest.iter().zip(&a.cells) {
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(*h, &mut buf).max(0) as usize;
        let aspect = crate::alttab::frame(*h).0.clamp(0.25, 4.0);
        let thumb = DwmRegisterThumbnail(p, *h).unwrap_or(0);
        // the picture keeps the window's shape inside the room under the title row
        let (bw, bh) = (c.2, c.3 - A_HEAD * c.2 / A_BOX.0);
        let (tw, th) = if bh * aspect > bw { (bw, bw / aspect) } else { (bh * aspect, bh) };
        let (tx, ty) = (c.0 + (bw - tw) / 2.0, c.1 + (c.3 - bh) + (bh - th) / 2.0);
        let px = |v: f32| (v * s).round() as i32;
        let props = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE,
            rcDestination: RECT { left: px(tx), top: px(ty), right: px(tx + tw), bottom: px(ty + th) },
            fVisible: TRUE,
            ..Default::default()
        };
        let _ = DwmUpdateThumbnailProperties(thumb, &props);
        a.wins.push((h.0 as isize, buf[..n].to_vec(), aspect, thumb, small_icon(*h)));
    }
    let _ = ShowWindow(p, SW_SHOWNOACTIVATE);
    ASSIST_OPEN.store(true, Relaxed);
    SetTimer(Some(p), 1, 250, None);
    let _ = InvalidateRect(Some(p), None, true);
}

/// Put window `i` of the offer into the zone and move on.
unsafe fn assist_pick(p: HWND, a: &mut Assist, i: usize) {
    let Some(w) = a.wins.get(i) else { return };
    let h = HWND(w.0 as *mut c_void);
    if IsWindow(Some(h)).as_bool() {
        place(h, LAYOUTS[a.layout][a.zone]);
        crate::alttab::activate(h);
        a.placed.push(h.0 as isize);
        a.filled[a.zone] = true;
    }
    assist_next(p, a);
}

unsafe fn assist_start(layout: usize, zone: usize, target: HWND) {
    let p = HWND(ASSIST.load(Relaxed) as *mut c_void);
    if p.is_invalid() {
        return;
    }
    AS.with(|a| {
        let a = &mut *a.borrow_mut();
        a.layout = layout;
        a.filled = vec![false; LAYOUTS[layout].len()];
        a.filled[zone] = true;
        a.placed = vec![target.0 as isize];
        assist_next(p, a);
    });
}

unsafe extern "system" fn assist_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    let at = |lp: LPARAM| ((lp.0 & 0xFFFF) as i16 as f32 / s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s);
    AS.with(|a| {
        let Ok(mut a) = a.try_borrow_mut() else { return DefWindowProcW(hwnd, msg, wp, lp) }; // re-entered while placing a window
        let a = &mut *a;
        match msg {
            WM_TIMER => {
                // another app came forward (not one of the windows being arranged): the offer ends
                if !a.placed.contains(&(GetForegroundWindow().0 as isize)) {
                    assist_hide(hwnd, a);
                }
            }
            WM_ASSIST_KEY => {
                let n = a.wins.len();
                if n == 0 {
                    return LRESULT(0);
                }
                match VIRTUAL_KEY(wp.0 as u16) {
                    VK_ESCAPE => assist_hide(hwnd, a),
                    VK_RETURN => assist_pick(hwnd, a, a.sel),
                    VK_RIGHT | VK_TAB => a.sel = (a.sel + 1) % n,
                    VK_LEFT => a.sel = (a.sel + n - 1) % n,
                    VK_DOWN => a.sel = (a.sel + a.cols).min(n - 1),
                    VK_UP => a.sel = a.sel.saturating_sub(a.cols),
                    _ => {}
                }
                let _ = InvalidateRect(Some(hwnd), None, true);
            }
            WM_ASSIST_PICK => {
                if let Some(i) = a.wins.iter().position(|w| w.0 == wp.0 as isize) {
                    assist_pick(hwnd, a, i);
                }
            }
            crate::flyout::WM_DISMISS => assist_hide(hwnd, a),
            WM_MOUSEMOVE => {
                let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                let _ = TrackMouseEvent(&mut tme);
                let (x, y) = at(lp);
                let h = assist_hit(&a.cells, x, y);
                if std::mem::replace(&mut a.hover, h) != h {
                    let _ = InvalidateRect(Some(hwnd), None, true);
                }
            }
            WM_MOUSELEAVE => {
                a.hover = None;
                let _ = InvalidateRect(Some(hwnd), None, true);
            }
            WM_LBUTTONUP => {
                let (x, y) = at(lp);
                match assist_hit(&a.cells, x, y) {
                    Some(i) => assist_pick(hwnd, a, i),
                    None => assist_hide(hwnd, a), // a click on the empty part leaves the zone empty
                }
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut ps);
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                // Direct2D into a buffer that keeps alpha (Windows' blurred material shows through);
                // the app icons go on top with GDI, which keeps their own transparency
                use windows::Win32::UI::Controls::{BeginBufferedPaint, EndBufferedPaint, BPBF_TOPDOWNDIB, BPPF_ERASE, BP_PAINTPARAMS};
                let params = BP_PAINTPARAMS { cbSize: size_of::<BP_PAINTPARAMS>() as u32, dwFlags: BPPF_ERASE, ..Default::default() };
                let mut mem = HDC::default();
                let pb = BeginBufferedPaint(dc, &rc, BPBF_TOPDOWNDIB, Some(&params), &mut mem);
                if !mem.is_invalid() {
                    let cards: Vec<((f32, f32, f32, f32), String, isize)> = a.cells.iter().zip(&a.wins).map(|(c, w)| (*c, String::from_utf16_lossy(&w.1).trim_end_matches('\0').to_string(), w.4 as isize)).collect();
                    let _ = draw_assist(mem, &rc, s, &cards, a.sel, a.hover);
                    for ((x, y, cw, _), _, icon) in &cards {
                        if *icon != 0 {
                            let (top, bot) = ((y * s) as i32, ((y + A_HEAD * cw / A_BOX.0) * s) as i32);
                            let side = (16.0 * s) as i32;
                            let _ = DrawIconEx(mem, (x * s) as i32, (top + bot - side) / 2, HICON(*icon as *mut c_void), side, side, 0, None, DI_NORMAL);
                        }
                    }
                    let _ = EndBufferedPaint(pb, true);
                }
                let _ = EndPaint(hwnd, &ps);
            }
            WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
            _ => return DefWindowProcW(hwnd, msg, wp, lp),
        }
        LRESULT(0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn zones_tile_the_work_area_exactly() {
        let work = (0, 0, 1920, 1032);
        for layout in LAYOUTS {
            // every layout covers the whole area with no overlap: the zone areas add up to it
            let area: i64 = layout.iter().map(|z| { let r = zone_px(work, *z); ((r.2 - r.0) as i64) * ((r.3 - r.1) as i64) }).sum();
            assert_eq!(area, 1920 * 1032);
        }
        assert_eq!(zone_px(work, LAYOUTS[0][1]), (960, 0, 1920, 1032));
        assert_eq!(zone_px((100, 50, 1100, 850), LAYOUTS[4][3]), (600, 450, 1100, 850));
    }
    #[test]
    fn assist_cards_fit_the_zone() {
        // two windows in a half-screen zone: side by side at full size, centred
        let (cols, c) = assist_cells(2, 960.0, 1032.0);
        assert_eq!((cols, c.len()), (2, 2));
        assert_eq!((c[0].2, c[0].3), (A_BOX.0, A_BOX.1 + A_HEAD));
        assert!((c[0].0 - (960.0 - c[1].0 - c[1].2)).abs() < 0.01);
        // many windows in a small zone: every card stays inside it and none overlap
        let (cols, c) = assist_cells(9, 480.0, 516.0);
        assert!(c.iter().all(|r| r.0 >= 0.0 && r.1 >= 0.0 && r.0 + r.2 <= 480.0 && r.1 + r.3 <= 516.0));
        assert!(c[1].0 >= c[0].0 + c[0].2 && c[cols].1 >= c[0].1 + c[0].3);
        assert_eq!(assist_hit(&c, c[4].0 + 1.0, c[4].1 + 1.0), Some(4));
        assert_eq!(assist_hit(&c, 0.5, 0.5), None);
    }
    #[test]
    fn win_arrows() {
        let work = (0, 0, 1920, 1032);
        let free = (300, 200, 900, 700);
        let (l, r) = ((0.0, 0.0, 0.5, 1.0), (0.5, 0.0, 0.5, 1.0));
        assert_eq!(arrow(free, work, false, 0x27), Arrow::Zone(r));
        assert_eq!(arrow(free, work, false, 0x26), Arrow::Max);
        assert_eq!(arrow(free, work, false, 0x28), Arrow::Min);
        assert_eq!(arrow((0, 0, 1920, 1032), work, true, 0x28), Arrow::Restore);
        let right = zone_px(work, r);
        assert_eq!(arrow(right, work, false, 0x26), Arrow::Zone((0.5, 0.0, 0.5, 0.5)));
        assert_eq!(arrow(right, work, false, 0x28), Arrow::Zone((0.5, 0.5, 0.5, 0.5)));
        assert_eq!(arrow(right, work, false, 0x25), Arrow::Zone(l));
        let top_right = zone_px(work, (0.5, 0.0, 0.5, 0.5));
        assert_eq!(arrow(top_right, work, false, 0x28), Arrow::Zone(r));
        assert_eq!(arrow(top_right, work, false, 0x26), Arrow::Max);
        assert_eq!(arrow(top_right, work, false, 0x25), Arrow::Zone((0.0, 0.0, 0.5, 0.5)));
        let bottom_left = zone_px(work, (0.0, 0.5, 0.5, 0.5));
        assert_eq!(arrow(bottom_left, work, false, 0x26), Arrow::Zone(l));
        assert_eq!(arrow(bottom_left, work, false, 0x28), Arrow::Min);
    }
    #[test]
    fn picker_hit_testing() {
        assert_eq!(hit(PAD + 10.0, PAD + 10.0), Some((0, 0)));
        assert_eq!(hit(PAD + TILE_W - 10.0, PAD + 10.0), Some((0, 1)));
        assert_eq!(hit(PAD + 2.0 * (TILE_W + GAP) + TILE_W / 2.0, PAD + TILE_H + GAP + 10.0), Some((5, 1)));
        assert_eq!(hit(2.0, 2.0), None);
    }
}

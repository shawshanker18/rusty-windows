//! Windows 11 (24H2) Alt+Tab, dark theme. A centred acrylic panel with one card per window
//! (16px app icon + 12px title over a live DWM thumbnail fitted in 260x190), wrapping into rows.
//! Selected card: 3px white rounded outline. Hovered card: subtle fill + close (X) button.
//! The keyboard hook (its own thread) only flips an atomic and posts messages; all UI work runs here.
//! Quick Alt+Tab (released before the panel's 150ms delay) switches without ever showing it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, Ordering::Relaxed};

use windows::core::{w, BOOL};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::flyout::{glyph, rect, rrect};
use crate::quick::rgb;

pub static ALTTAB: AtomicIsize = AtomicIsize::new(0);
const WM_AT_START: u32 = WM_APP + 50; // wParam: 1 = Shift held (start from the last window)
const WM_AT_KEY: u32 = WM_APP + 51; // wParam: vk, lParam: Shift held
const WM_AT_COMMIT: u32 = WM_APP + 52;
const WM_AT_CANCEL: u32 = WM_APP + 53;
const T_SHOW: usize = 1;
const SHOW_DELAY_MS: u32 = 150; // quicker Alt+Tab = switch without flashing the UI, like Windows

// layout, DIPs
const TW: f32 = 338.0; // thumbnail box: wide enough for a 16:9 window at full height, so cards share one height like Windows
const TH: f32 = 190.0;
const HEAD: f32 = 32.0; // icon + title row
const IN: f32 = 8.0; // card inner padding
const MIN_W: f32 = 168.0;
const CARD_H: f32 = HEAD + TH + IN;
const GAP: f32 = 4.0;
const PAD: f32 = 12.0;
const MARGIN: f32 = 48.0; // panel never closer than this to the work-area edge
const CLOSE: f32 = 24.0;
const T_BOX: (f32, f32) = (778.0, 304.0); // the most room one window's picture gets in Task View on a 1920-wide screen
const STRIP: f32 = 190.0; // room kept at the bottom of Task View for the desktops strip

const C_BG: D2D1_COLOR_F = D2D1_COLOR_F { a: 0.88, ..rgb(0x202020) };
const C_HOVER: D2D1_COLOR_F = D2D1_COLOR_F { a: 0.06, ..rgb(0xFFFFFF) };
const C_PLACE: D2D1_COLOR_F = rgb(0x2B2B2B);
const C_CLOSE_HOT: D2D1_COLOR_F = rgb(0xC42B1C);

// 0 idle, 1 switching (Alt held), 2 done but Alt still held (its release must not open a menu bar)
static STATE: AtomicU8 = AtomicU8::new(0);
static TAB_UP: AtomicBool = AtomicBool::new(false);

struct Item {
    hwnd: HWND,
    title: Vec<u16>,
    icon: String,       // shell parsing name, key into the icon cache
    aspect: f32,
    src: Option<RECT>,  // visible frame inside the window rect (drops the invisible resize borders)
    thumb: isize,
}

struct Gfx {
    rt: ID2D1DCRenderTarget,
    title: IDWriteTextFormat,
    title14: IDWriteTextFormat, // Task View card titles
    icon10: IDWriteTextFormat,
    brush: ID2D1SolidColorBrush,
}

#[derive(Default)]
struct State {
    items: Vec<Item>,
    cards: Vec<D2D_RECT_F>,
    sel: usize,
    hover: Option<(usize, bool)>, // card, over its close button
    shown: bool,
    snapshot: bool,
    task: bool,            // Task View: full screen, bigger cards, the desktops strip
    area: (f32, f32),      // window size in DIPs (Task View)
    gfx: Option<Gfx>,
    icons: HashMap<String, ID2D1Bitmap>, // tied to gfx.rt; cleared with it
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
}

// ======================= window list =======================

/// Alt+Tab rules (the parts that need no window handle, so they can be tested).
fn eligible(visible: bool, cloaked: bool, ex: u32, has_title: bool, class: &str, represents_owner_chain: bool) -> bool {
    let app = ex & WS_EX_APPWINDOW.0 != 0;
    visible
        && !cloaked
        && (represents_owner_chain || app)
        && (app || ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) == 0)
        && (has_title || app)
        && !class.starts_with("shellbits_")
        && class != "Progman"
}

/// Owner rules: the root owner stands for its owned popups; if it's hidden (apps with an invisible
/// owner window) the first visible window down its last-active-popup chain stands in for it.
unsafe fn represents(h: HWND) -> bool {
    let mut w = GetAncestor(h, GA_ROOTOWNER);
    while !IsWindowVisible(w).as_bool() {
        let t = GetLastActivePopup(w);
        if t == w {
            break;
        }
        w = t;
    }
    w == h
}

unsafe extern "system" fn enum_cb(h: HWND, lp: LPARAM) -> BOOL {
    let v = &mut *(lp.0 as *mut Vec<HWND>);
    let mut cloaked = 0u32;
    let _ = DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut c_void, 4);
    let mut class = [0u16; 64];
    let n = GetClassNameW(h, &mut class).max(0) as usize;
    let ok = eligible(
        IsWindowVisible(h).as_bool(),
        cloaked != 0,
        GetWindowLongPtrW(h, GWL_EXSTYLE) as u32,
        GetWindowTextLengthW(h) > 0,
        &String::from_utf16_lossy(&class[..n]),
        represents(h),
    );
    if ok {
        v.push(h);
    }
    TRUE
}

/// The windows Alt+Tab would offer, front to back.
pub(crate) unsafe fn windows() -> Vec<HWND> {
    let mut v: Vec<HWND> = Vec::new();
    let _ = EnumWindows(Some(enum_cb), LPARAM(&mut v as *mut _ as isize));
    v
}

/// Visible size of a window (restored size if minimized) and its frame inside the window rect.
pub(crate) unsafe fn frame(h: HWND) -> (f32, Option<RECT>) {
    if IsIconic(h).as_bool() {
        let mut wp = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        let _ = GetWindowPlacement(h, &mut wp);
        let mut r = wp.rcNormalPosition;
        // minimised from maximised (WPF_RESTORETOMAXIMIZED): it comes back filling the work area, and
        // its picture has that shape - the small "restored" rectangle gave a squeezed card
        if wp.flags.0 & 2 != 0 {
            let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            if GetMonitorInfoW(MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() {
                r = mi.rcWork;
            }
        }
        return (((r.right - r.left) as f32 / (r.bottom - r.top).max(1) as f32), None);
    }
    let (mut wr, mut fb) = (RECT::default(), RECT::default());
    let _ = GetWindowRect(h, &mut wr);
    if DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS, &mut fb as *mut _ as *mut c_void, size_of::<RECT>() as u32).is_err() {
        fb = wr;
    }
    let src = RECT { left: fb.left - wr.left, top: fb.top - wr.top, right: fb.right - wr.left, bottom: fb.bottom - wr.top };
    ((fb.right - fb.left) as f32 / (fb.bottom - fb.top).max(1) as f32, Some(src))
}

/// Top-level windows in Z order (most recent first), with titles and icons.
unsafe fn build(st: &mut State, icon_px: u32) {
    let mut wins: Vec<HWND> = Vec::new();
    let _ = EnumWindows(Some(enum_cb), LPARAM(&mut wins as *mut _ as isize));
    st.items.clear();
    for h in wins {
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(h, &mut buf).max(0) as usize;
        let (aspect, src) = frame(h);
        let (_, icon) = crate::taskbar::app_key(h);
        st.items.push(Item { hwnd: h, title: buf[..n].to_vec(), icon, aspect, src, thumb: 0 });
    }
    // icons: decoded once per app, kept as GPU bitmaps (4 KB each)
    let Some(g) = st.gfx.as_ref() else { return };
    let Ok(wic) = CoCreateInstance::<_, IWICImagingFactory>(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) else { return };
    let bp = D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
    };
    for it in &st.items {
        if it.icon.is_empty() || st.icons.contains_key(&it.icon) {
            continue;
        }
        if let Ok(px) = crate::launcher::icon_pixels(&wic, &it.icon, icon_px) {
            if let Ok(b) = g.rt.CreateBitmap(D2D_SIZE_U { width: icon_px, height: icon_px }, Some(px.as_ptr() as *const c_void), icon_px * 4, &bp) {
                st.icons.insert(it.icon.clone(), b);
            }
        }
    }
}

// ======================= layout =======================

fn thumb_size(aspect: f32) -> (f32, f32) {
    let a = aspect.clamp(0.25, 4.0);
    if TH * a > TW { (TW, (TW / a).round()) } else { ((TH * a).round(), TH) }
}

fn card_w(aspect: f32) -> f32 {
    (thumb_size(aspect).0 + 2.0 * IN).max(MIN_W)
}

/// Cards left to right, wrapping into centred rows no wider than `max_w`. Returns the panel size.
// ponytail: no vertical cap, a few dozen windows on a small screen overflow; scale cards down if that matters
fn layout(ws: &[f32], max_w: f32, out: &mut Vec<D2D_RECT_F>) -> (f32, f32) {
    out.clear();
    let avail = max_w - 2.0 * PAD;
    let row_end = |s: usize| {
        let (mut e, mut w) = (s, 0.0);
        while e < ws.len() {
            let add = ws[e] + if e > s { GAP } else { 0.0 };
            if e > s && w + add > avail {
                break;
            }
            w += add;
            e += 1;
        }
        (e, w)
    };
    let (mut panel, mut s, mut rows) = (0.0f32, 0, 0);
    while s < ws.len() {
        let (e, w) = row_end(s);
        panel = panel.max(w);
        s = e;
        rows += 1;
    }
    let (mut s, mut y) = (0, PAD);
    while s < ws.len() {
        let (e, w) = row_end(s);
        let mut x = PAD + ((panel - w) / 2.0).round();
        for &cw in &ws[s..e] {
            out.push(rect(x, y, x + cw, y + CARD_H));
            x += cw + GAP;
        }
        y += CARD_H + GAP;
        s = e;
    }
    (panel + 2.0 * PAD, rows as f32 * (CARD_H + GAP) - GAP + 2.0 * PAD)
}

fn thumb_rect(c: &D2D_RECT_F, aspect: f32) -> D2D_RECT_F {
    let (w, h) = thumb_size(aspect);
    let (cx, cy) = (((c.left + c.right) / 2.0).round(), c.top + HEAD + TH / 2.0);
    rect(cx - (w / 2.0).round(), cy - (h / 2.0).round(), cx - (w / 2.0).round() + w, cy - (h / 2.0).round() + h)
}

fn close_rect(c: &D2D_RECT_F) -> D2D_RECT_F {
    rect(c.right - IN - CLOSE, c.top + (HEAD - CLOSE) / 2.0, c.right - IN, c.top + (HEAD + CLOSE) / 2.0)
}

/// Tab / Shift+Tab / Left / Right: wrap around.
fn step(sel: usize, n: usize, d: isize) -> usize {
    if n == 0 { 0 } else { (sel as isize + d).rem_euclid(n as isize) as usize }
}

/// Up / Down: nearest card (by centre) in the row above / below; stays put at the edge.
fn step_row(cards: &[D2D_RECT_F], sel: usize, down: bool) -> usize {
    let Some(c) = cards.get(sel) else { return sel };
    let row = cards.iter().map(|r| r.top).filter(|&t| if down { t > c.top } else { t < c.top }).fold(None, |a: Option<f32>, t| Some(match a {
        None => t,
        Some(a) => if down { a.min(t) } else { a.max(t) },
    }));
    let Some(row) = row else { return sel };
    let cx = (c.left + c.right) / 2.0;
    (0..cards.len())
        .filter(|&i| cards[i].top == row)
        .min_by(|&a, &b| {
            let d = |i: usize| ((cards[i].left + cards[i].right) / 2.0 - cx).abs();
            d(a).total_cmp(&d(b))
        })
        .unwrap_or(sel)
}

const T_HEAD: f32 = 36.0; // Task View card: title row, then the window at its scaled size
const T_GAP: f32 = 20.0;
const T_TOP: f32 = 76.0; // first row's top, as in Windows' own Task View

/// Task View cards for windows of the given real sizes. Each window is shrunk to fit a box
/// (778 x 304 on a 1920-wide screen, as measured on Windows' own), rows wrap at 60% of the screen
/// and are centred; with many windows the box shrinks until the rows fit above the desktops strip.
/// Returns the box scale used (1 = full size) and the cards.
pub fn task_layout(sizes: &[(f32, f32)], area_w: f32, area_h: f32) -> (f32, Vec<D2D_RECT_F>) {
    let (max_row, space_h) = (area_w * 0.6, area_h - STRIP - T_TOP - 12.0);
    let card = |k: f32, (w, h): (f32, f32)| {
        let z = (T_BOX.0 * k * area_w / 1920.0 / w.max(1.0)).min(T_BOX.1 * k * area_w / 1920.0 / h.max(1.0)).min(1.0);
        ((w * z).round(), T_HEAD + (h * z).round())
    };
    let rows_at = |k: f32| {
        let mut rows: Vec<(usize, usize, f32, f32)> = Vec::new(); // start, end, width, height
        let mut s0 = 0;
        while s0 < sizes.len() {
            let (mut e, mut w, mut h) = (s0, 0.0f32, 0.0f32);
            while e < sizes.len() {
                let (cw, ch) = card(k, sizes[e]);
                if e > s0 && w + T_GAP + cw > max_row {
                    break;
                }
                w += cw + if e > s0 { T_GAP } else { 0.0 };
                h = h.max(ch);
                e += 1;
            }
            rows.push((s0, e, w, h));
            s0 = e;
        }
        rows
    };
    let total = |rows: &[(usize, usize, f32, f32)]| rows.iter().map(|r| r.3).sum::<f32>() + T_GAP * rows.len().saturating_sub(1) as f32;
    let mut k = 1.0f32;
    while k > 0.1 && total(&rows_at(k)) > space_h {
        k *= 0.94;
    }
    let mut out = Vec::with_capacity(sizes.len());
    let mut y = T_TOP;
    for (s0, e, w, h) in rows_at(k) {
        let mut x = ((area_w - w) / 2.0).round();
        for sz in &sizes[s0..e] {
            let (cw, ch) = card(k, *sz);
            out.push(rect(x, y, x + cw, y + ch));
            x += cw + T_GAP;
        }
        y += h + T_GAP;
    }
    (k, out)
}

fn task_close(c: &D2D_RECT_F) -> D2D_RECT_F {
    rect(c.right - 32.0, c.top + 6.0, c.right - 8.0, c.top + 30.0)
}

fn hit(st: &State, x: f32, y: f32) -> Option<(usize, bool)> {
    if st.task {
        let i = st.cards.iter().position(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom)?;
        let c = task_close(&st.cards[i]);
        return Some((i, x >= c.left && x < c.right && y >= c.top && y < c.bottom));
    }
    let i = st.cards.iter().position(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom)?;
    let c = close_rect(&st.cards[i]);
    Some((i, x >= c.left && x < c.right && y >= c.top && y < c.bottom))
}

// ======================= drawing =======================

unsafe fn make_gfx() -> windows::core::Result<Gfx> {
    let f: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
    let props = D2D1_RENDER_TARGET_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        ..Default::default()
    };
    let rt = f.CreateDCRenderTarget(&props)?;
    rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
    let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
    let fmt = |name, size, align| -> windows::core::Result<IDWriteTextFormat> {
        let t = dw.CreateTextFormat(name, None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))?;
        t.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        t.SetTextAlignment(align)?;
        t.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
        t.SetTrimming(&trim, &dw.CreateEllipsisTrimmingSign(&t)?)?;
        Ok(t)
    };
    Ok(Gfx {
        title: fmt(w!("Segoe UI Variable Text"), 12.0, DWRITE_TEXT_ALIGNMENT_LEADING)?,
        title14: fmt(w!("Segoe UI Variable Text"), 14.0, DWRITE_TEXT_ALIGNMENT_LEADING)?,
        icon10: fmt(w!("Segoe Fluent Icons"), 10.0, DWRITE_TEXT_ALIGNMENT_CENTER)?,
        brush: rt.CreateSolidColorBrush(&rgb(0xFFFFFF), None)?,
        rt,
    })
}

unsafe fn draw(st: &State, g: &Gfx) {
    let (rt, b) = (&g.rt, &g.brush);
    let white = rgb(0xFFFFFF);
    if st.task {
        // ponytail: a flat grey like Windows shows over a dark wallpaper; it really is the wallpaper, blurred
        rt.Clear(Some(&rgb(0x373737)));
        let (aw, ah) = st.area;
        let strip = rect(10.0, ah - STRIP + 8.0, aw - 10.0, ah - 8.0);
        b.SetColor(&rgb(0x141414));
        rt.FillRoundedRectangle(&rrect(strip, 8.0), b);
        // ponytail: one desktop, shown as it is; more desktops need Windows' private virtual-desktop interfaces
        let tile = rect((aw / 2.0 - 246.0).round(), strip.top + 8.0, (aw / 2.0 - 246.0).round() + 244.0, strip.bottom - 8.0);
        b.SetColor(&rgb(0x2B2B2B));
        rt.FillRoundedRectangle(&rrect(tile, 6.0), b);
        b.SetColor(&white);
        rt.DrawText(&"Desktop 1".encode_utf16().collect::<Vec<u16>>(), &g.title, &rect(tile.left + 10.0, tile.top + 4.0, tile.right - 8.0, tile.top + 26.0), b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        b.SetColor(&rgb(0x4CC2FF));
        rt.FillRoundedRectangle(&rrect(rect(tile.left + 106.0, tile.bottom - 4.0, tile.right - 106.0, tile.bottom - 1.0), 1.5), b);
        for (i, (it, c)) in st.items.iter().zip(&st.cards).enumerate() {
            b.SetColor(&rgb(0x202020));
            rt.FillRoundedRectangle(&rrect(*c, 8.0), b);
            if let Some(bmp) = st.icons.get(&it.icon) {
                crate::flyout::draw_icon(rt, bmp, c.left + 12.0, c.top + 10.0, 16.0);
            }
            let hov = st.hover.map(|h| h.0) == Some(i);
            b.SetColor(&white);
            rt.DrawText(&it.title, &g.title14, &rect(c.left + 38.0, c.top, if hov { c.right - 36.0 } else { c.right - 10.0 }, c.top + T_HEAD), b, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
            if hov {
                let x = task_close(c);
                if st.hover == Some((i, true)) {
                    b.SetColor(&C_CLOSE_HOT);
                    rt.FillRoundedRectangle(&rrect(x, 4.0), b);
                    b.SetColor(&white);
                }
                rt.DrawText(&glyph(0xE8BB), &g.icon10, &x, b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
            }
            if i == st.sel {
                b.SetColor(&rgb(0x4CC2FF));
                rt.DrawRoundedRectangle(&rrect(rect(c.left - 4.0, c.top - 4.0, c.right + 4.0, c.bottom + 4.0), 10.0), b, 3.0, None);
            }
        }
        return;
    } else {
        rt.Clear(Some(&C_BG));
    }
    for (i, (it, c)) in st.items.iter().zip(&st.cards).enumerate() {
        let hov = st.hover.map(|h| h.0) == Some(i);
        if hov {
            b.SetColor(&C_HOVER);
            rt.FillRoundedRectangle(&rrect(*c, 8.0), b);
        }
        let icon = st.icons.get(&it.icon);
        if let Some(bmp) = icon {
            crate::flyout::draw_icon(rt, bmp, c.left + IN, c.top + 8.0, 16.0);
        }
        let tr = if hov { c.right - IN - CLOSE - 4.0 } else { c.right - IN };
        b.SetColor(&white);
        rt.DrawText(&it.title, &g.title, &rect(c.left + IN + 24.0, c.top, tr, c.top + HEAD), b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        if hov {
            let x = close_rect(c);
            if st.hover == Some((i, true)) {
                b.SetColor(&C_CLOSE_HOT);
                rt.FillRoundedRectangle(&rrect(x, 4.0), b);
                b.SetColor(&white);
            }
            rt.DrawText(&glyph(0xE8BB), &g.icon10, &x, b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        }
        if st.snapshot {
            // DWM thumbnails only exist on screen: stand-in is the app icon on a dark card
            let t = thumb_rect(c, it.aspect);
            b.SetColor(&C_PLACE);
            rt.FillRoundedRectangle(&rrect(t, 4.0), b);
            if let Some(bmp) = icon {
                let (cx, cy) = ((t.left + t.right) / 2.0, (t.top + t.bottom) / 2.0);
                rt.DrawBitmap(bmp, Some(&rect(cx - 24.0, cy - 24.0, cx + 24.0, cy + 24.0)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
            }
        }
        if i == st.sel {
            b.SetColor(&white);
            rt.DrawRoundedRectangle(&rrect(rect(c.left + 1.5, c.top + 1.5, c.right - 1.5, c.bottom - 1.5), 7.0), b, 3.0, None);
        }
    }
}

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let params = BP_PAINTPARAMS { cbSize: size_of::<BP_PAINTPARAMS>() as u32, dwFlags: BPPF_ERASE, ..Default::default() };
    let mut mem = HDC::default();
    let pb = BeginBufferedPaint(hdc, &rc, BPBF_TOPDOWNDIB, Some(&params), &mut mem);
    if !mem.is_invalid() {
        let dpi = GetDpiForWindow(hwnd) as f32;
        ST.with(|s| {
            let mut st = s.borrow_mut();
            let ok = (|| -> windows::core::Result<()> {
                let g = st.gfx.as_ref().ok_or(windows::core::Error::from(E_FAIL))?;
                g.rt.BindDC(mem, &rc)?;
                g.rt.SetDpi(dpi, dpi);
                g.rt.BeginDraw();
                draw(&st, g);
                g.rt.EndDraw(None, None)
            })();
            if ok.is_err() {
                st.gfx = None; // device lost: rebuilt (with its icons) on the next show
                st.icons.clear();
            }
        });
        let _ = EndBufferedPaint(pb, true);
    }
    let _ = EndPaint(hwnd, &ps);
}

// ======================= show / hide / switch =======================

/// Size + centre the panel on the foreground window's monitor, then line the thumbnails up with the cards.
unsafe fn place(hwnd: HWND, st: &mut State) {
    let mon = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY);
    let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    let _ = GetMonitorInfoW(mon, &mut mi);
    let (mut dx, mut dy) = (96, 96);
    let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    let s = dx as f32 / 96.0;
    let wa = mi.rcWork;
    let ws: Vec<f32> = st.items.iter().map(|i| card_w(i.aspect)).collect();
    let (aw, ah) = ((wa.right - wa.left) as f32 / s, (wa.bottom - wa.top) as f32 / s);
    if st.task {
        // the whole work area; every window at the same share of its real size
        let sizes: Vec<(f32, f32)> = st
            .items
            .iter()
            .map(|it| match it.src {
                Some(r) => ((r.right - r.left) as f32 / s, (r.bottom - r.top) as f32 / s),
                None => {
                    // minimised: the size it will come back to
                    let mut wp = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
                    let _ = GetWindowPlacement(it.hwnd, &mut wp);
                    let r = wp.rcNormalPosition;
                    (((r.right - r.left) as f32 / s).max(120.0), ((r.bottom - r.top) as f32 / s).max(90.0))
                }
            })
            .collect();
        st.cards = task_layout(&sizes, aw, ah).1;
        st.area = (aw, ah);
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), wa.left, wa.top, wa.right - wa.left, wa.bottom - wa.top, SWP_SHOWWINDOW | SWP_NOACTIVATE);
    } else {
        let (w, h) = layout(&ws, aw - 2.0 * MARGIN, &mut st.cards);
        let (w, h) = ((w * s).round() as i32, (h * s).round() as i32);
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), (wa.left + wa.right - w) / 2, (wa.top + wa.bottom - h) / 2, w, h, SWP_SHOWWINDOW | SWP_NOACTIVATE);
    }
    let task = st.task;
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    for (it, c) in st.items.iter_mut().zip(&st.cards) {
        if it.thumb == 0 {
            it.thumb = DwmRegisterThumbnail(hwnd, it.hwnd).unwrap_or(0);
        }
        let t = if task { rect(c.left, c.top + T_HEAD, c.right, c.bottom) } else { thumb_rect(c, it.aspect) };
        let px = |v: f32| (v * s).round() as i32;
        let mut p = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY,
            rcDestination: RECT { left: px(t.left), top: px(t.top), right: px(t.right), bottom: px(t.bottom) },
            opacity: 255,
            fVisible: TRUE,
            fSourceClientAreaOnly: FALSE,
            ..Default::default()
        };
        if let Some(src) = it.src {
            p.dwFlags |= DWM_TNP_RECTSOURCE;
            p.rcSource = src;
        }
        let _ = DwmUpdateThumbnailProperties(it.thumb, &p);
    }
    let _ = InvalidateRect(Some(hwnd), None, false);
}

unsafe fn hide(hwnd: HWND, st: &mut State) {
    let _ = KillTimer(Some(hwnd), T_SHOW);
    let _ = ShowWindow(hwnd, SW_HIDE);
    for it in st.items.drain(..) {
        if it.thumb != 0 {
            let _ = DwmUnregisterThumbnail(it.thumb);
        }
    }
    st.cards.clear();
    st.hover = None;
    st.shown = false;
}

pub(crate) unsafe fn activate(h: HWND) {
    if IsIconic(h).as_bool() {
        let _ = ShowWindowAsync(h, SW_RESTORE);
    }
    // a window with an open dialog: Windows focuses the dialog
    let p = GetLastActivePopup(h);
    let t = if p != h && IsWindowVisible(p).as_bool() && IsWindowEnabled(p).as_bool() { p } else { h };
    crate::flyout::force_foreground(t);
}

unsafe fn commit(hwnd: HWND, st: &mut State) {
    let target = st.items.get(st.sel).map(|i| i.hwnd);
    hide(hwnd, st);
    if let Some(h) = target {
        activate(h);
    }
}

/// Close (X) / Delete: ask the window to close and drop its card.
unsafe fn close(hwnd: HWND, st: &mut State, i: usize) {
    if i >= st.items.len() {
        return;
    }
    let it = st.items.remove(i);
    let _ = PostMessageW(Some(it.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
    if it.thumb != 0 {
        let _ = DwmUnregisterThumbnail(it.thumb);
    }
    st.hover = None;
    if st.items.is_empty() {
        let _ = STATE.compare_exchange(1, 2, Relaxed, Relaxed);
        hide(hwnd, st);
        return;
    }
    st.sel = st.sel.min(st.items.len() - 1);
    if st.shown {
        place(hwnd, st);
    }
}

pub unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let dips = |lp: LPARAM| {
        let s = GetDpiForWindow(hwnd) as f32 / 96.0;
        ((lp.0 & 0xFFFF) as i16 as f32 / s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s)
    };
    let redraw = || {
        let _ = InvalidateRect(Some(hwnd), None, false);
    };
    // SetWindowPos / ShowWindow re-enter this proc synchronously: only our own messages touch state
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            paint(hwnd);
            return LRESULT(0);
        }
        WM_DPICHANGED => return LRESULT(0), // place() already sized us for the target monitor
        WM_AT_START | WM_AT_KEY | WM_AT_COMMIT | WM_AT_CANCEL | WM_TIMER | WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONUP | crate::flyout::WM_DISMISS => {}
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    ST.with(|s| {
        let st = &mut *s.borrow_mut();
        match msg {
            WM_AT_START => {
                hide(hwnd, st);
                if st.gfx.is_none() {
                    st.gfx = make_gfx().ok();
                }
                build(st, 32);
                let n = st.items.len();
                st.task = wp.0 == 2;
                if wp.0 == 2 {
                    // Task View (Win+Tab): stays open until you pick, and starts on the window you're in
                    st.sel = 0;
                    if n > 0 {
                        st.shown = true;
                        place(hwnd, st);
                    } else {
                        STATE.store(0, Relaxed);
                    }
                } else {
                    st.sel = if n < 2 { 0 } else if wp.0 == 1 { n - 1 } else { 1 };
                    SetTimer(Some(hwnd), T_SHOW, SHOW_DELAY_MS, None);
                }
            }
            WM_TIMER if wp.0 == T_SHOW => {
                let _ = KillTimer(Some(hwnd), T_SHOW);
                if STATE.load(Relaxed) == 1 && !st.items.is_empty() {
                    st.shown = true;
                    place(hwnd, st);
                }
            }
            WM_AT_KEY => {
                let n = st.items.len();
                let vk = VIRTUAL_KEY(wp.0 as u16);
                st.sel = match vk {
                    VK_TAB => step(st.sel, n, if lp.0 != 0 { -1 } else { 1 }),
                    VK_LEFT => step(st.sel, n, -1),
                    VK_RIGHT => step(st.sel, n, 1),
                    VK_UP | VK_DOWN => step_row(&st.cards, st.sel, vk == VK_DOWN),
                    _ => st.sel,
                };
                if vk == VK_DELETE {
                    close(hwnd, st, st.sel);
                }
                redraw();
            }
            WM_AT_COMMIT => commit(hwnd, st),
            WM_AT_CANCEL => hide(hwnd, st),
            crate::flyout::WM_DISMISS => {
                // a click somewhere else closes Task View (Alt+Tab ends with Alt instead)
                if STATE.compare_exchange(3, 0, Relaxed, Relaxed).is_ok() {
                    hide(hwnd, st);
                }
            }
            WM_MOUSEMOVE => {
                let (x, y) = dips(lp);
                let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                let _ = TrackMouseEvent(&mut tme);
                let h = hit(st, x, y);
                if std::mem::replace(&mut st.hover, h) != h {
                    redraw();
                }
            }
            WM_MOUSELEAVE => {
                st.hover = None;
                redraw();
            }
            WM_LBUTTONUP => {
                let (x, y) = dips(lp);
                match hit(st, x, y) {
                    Some((i, true)) => close(hwnd, st, i),
                    Some((i, false)) => {
                        let _ = STATE.compare_exchange(1, 2, Relaxed, Relaxed); // Alt's release now just ends the session
                        let _ = STATE.compare_exchange(3, 0, Relaxed, Relaxed); // Task View is done
                        st.sel = i;
                        commit(hwnd, st);
                    }
                    None => {}
                }
            }
            _ => {}
        }
    });
    LRESULT(0)
}

// ======================= keyboard (hook thread) =======================

/// Called from the low-level keyboard hook for every key. Returns true to swallow it.
/// Only atomics + PostMessage here: the hook must answer fast.
pub unsafe fn on_key(k: &KBDLLHOOKSTRUCT, up: bool) -> bool {
    if k.dwExtraInfo == crate::launcher::MARK {
        return false;
    }
    let vk = VIRTUAL_KEY(k.vkCode as u16);
    let alt = k.flags.0 & LLKHF_ALTDOWN.0 != 0;
    let shift = GetAsyncKeyState(VK_SHIFT.0 as i32) < 0;
    let post = |m: u32, wp: usize, lp: isize| {
        let _ = PostMessageW(Some(HWND(ALTTAB.load(Relaxed) as *mut c_void)), m, WPARAM(wp), LPARAM(lp));
    };
    let state = STATE.load(Relaxed);
    if state == 3 {
        // Task View: no key is held, so the keys below drive it directly
        return match vk {
            VK_ESCAPE | VK_RETURN => {
                if !up {
                    STATE.store(0, Relaxed);
                    post(if vk == VK_ESCAPE { WM_AT_CANCEL } else { WM_AT_COMMIT }, 0, 0);
                }
                true
            }
            // Win+Tab again closes it: that one belongs to the Win-key handler
            VK_TAB if GetAsyncKeyState(VK_LWIN.0 as i32) < 0 || GetAsyncKeyState(VK_RWIN.0 as i32) < 0 => false,
            VK_TAB | VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN | VK_DELETE => {
                if !up {
                    post(WM_AT_KEY, vk.0 as usize, shift as isize);
                }
                true
            }
            _ => false,
        };
    }
    if vk == VK_TAB {
        if up {
            return TAB_UP.swap(false, Relaxed);
        }
        if !alt || GetAsyncKeyState(VK_CONTROL.0 as i32) < 0 {
            return false; // plain Tab, or Ctrl+Alt+Tab (Windows' sticky switcher) stays Windows'
        }
        TAB_UP.store(true, Relaxed);
        if state == 1 {
            post(WM_AT_KEY, vk.0 as usize, shift as isize);
        } else {
            STATE.store(1, Relaxed);
            post(WM_AT_START, shift as usize, 0);
        }
        return true;
    }
    if state == 0 {
        return false;
    }
    if matches!(vk, VK_MENU | VK_LMENU | VK_RMENU) {
        if !up {
            return false;
        }
        // Alt released: the app saw Alt down with no key in between (Tab was ours), so a bare Alt-up
        // would open its menu bar. Same trick as the Win key: swallow it, inject dummy key + Alt-up.
        STATE.store(0, Relaxed);
        crate::launcher::inject(&[(0xE8, false), (0xE8, true), (vk.0, true)]);
        if state == 1 {
            post(WM_AT_COMMIT, 0, 0);
        }
        return true;
    }
    if state != 1 {
        return false;
    }
    if !alt {
        // we missed Alt's release (hook timed out?): don't keep eating keys
        STATE.store(0, Relaxed);
        post(WM_AT_CANCEL, 0, 0);
        return false;
    }
    match vk {
        VK_ESCAPE | VK_RETURN => {
            if !up {
                STATE.store(2, Relaxed);
                post(if vk == VK_ESCAPE { WM_AT_CANCEL } else { WM_AT_COMMIT }, 0, 0);
            }
            true
        }
        VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN | VK_DELETE => {
            if !up {
                post(WM_AT_KEY, vk.0 as usize, 0);
            }
            true
        }
        _ => false,
    }
}

/// Win+Tab: open Task View, or close it if it is open. Called from the keyboard hook.
pub unsafe fn task_view() {
    let w = Some(HWND(ALTTAB.load(Relaxed) as *mut c_void));
    if STATE.compare_exchange(3, 0, Relaxed, Relaxed).is_ok() {
        let _ = PostMessageW(w, WM_AT_CANCEL, WPARAM(0), LPARAM(0));
    } else if STATE.compare_exchange(0, 3, Relaxed, Relaxed).is_ok() {
        let _ = PostMessageW(w, WM_AT_START, WPARAM(2), LPARAM(0));
    }
}

// ======================= snapshot =======================

/// `shellbits --snapshot-alttab out.bmp`: the switcher for the real window list, drawn off-screen.
pub unsafe fn snapshot(path: &str) -> windows::core::Result<()> {
    ST.with(|s| -> windows::core::Result<()> {
        let st = &mut *s.borrow_mut();
        st.snapshot = true;
        st.gfx = Some(make_gfx()?);
        build(st, 64);
        st.sel = st.items.len().min(2).saturating_sub(1);
        st.hover = (!st.items.is_empty()).then_some((st.sel, false)); // show the close button too
        let mut wa = RECT::default();
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut wa as *mut _ as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        let ws: Vec<f32> = st.items.iter().map(|i| card_w(i.aspect)).collect();
        let (w, h) = layout(&ws, (wa.right - wa.left) as f32 - 2.0 * MARGIN, &mut st.cards);
        let (w, h) = (w as i32, h as i32);
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, ..Default::default() },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let dib = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let dc = CreateCompatibleDC(None);
        let old = SelectObject(dc, dib.into());
        let g = st.gfx.as_ref().unwrap();
        g.rt.BindDC(dc, &RECT { left: 0, top: 0, right: w, bottom: h })?;
        g.rt.SetDpi(96.0, 96.0);
        g.rt.BeginDraw();
        draw(st, g);
        g.rt.EndDraw(None, None)?;
        let _ = GdiFlush();
        let px = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize);
        let mut out = Vec::with_capacity(54 + px.len());
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&((54 + px.len()) as u32).to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&w.to_le_bytes());
        out.extend_from_slice(&(-h).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&[0; 24]);
        for p in px.chunks(4) {
            out.extend_from_slice(&[p[0], p[1], p[2], 255]);
        }
        let _ = std::fs::write(path, out);
        SelectObject(dc, old);
        let _ = DeleteDC(dc);
        let _ = DeleteObject(dib.into());
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_view_layout() {
        // the windows of the reference capture: a tall narrow one, a wide short one, a big one
        let sizes = [(540.0, 872.0), (1536.0, 439.0), (1338.0, 721.0)];
        let (k, cards) = task_layout(&sizes, 1920.0, 1032.0);
        assert_eq!(k, 1.0); // they fit at full size
        // measured on Windows' own: 192 x 340 and 778 x 260 cards, 20 apart, first row at y = 76, centred
        assert_eq!((cards[0].right - cards[0].left, cards[0].bottom - cards[0].top), (188.0, 340.0));
        assert_eq!((cards[1].right - cards[1].left, cards[1].bottom - cards[1].top), (778.0, 258.0));
        assert_eq!(cards[1].left - cards[0].right, 20.0);
        assert_eq!(cards[0].top, 76.0);
        assert_eq!((cards[0].left + cards[1].right) / 2.0, 960.0);
        // the third does not fit beside them and wraps, centred
        assert!(cards[2].top > cards[0].bottom);
        assert_eq!((cards[2].left + cards[2].right) / 2.0, 960.0);
        // a crowd shrinks until it fits above the desktops strip
        let many = vec![(1200.0, 800.0); 12];
        let (k2, c2) = task_layout(&many, 1920.0, 1032.0);
        assert!(k2 < 1.0 && c2.iter().all(|c| c.bottom <= 1032.0 - STRIP));
    }
    #[test]
    fn filter() {
        let normal = WS_EX_OVERLAPPEDWINDOW.0;
        assert!(eligible(true, false, normal, true, "Notepad", true));
        assert!(!eligible(false, false, normal, true, "Notepad", true)); // hidden
        assert!(!eligible(true, true, normal, true, "Notepad", true)); // cloaked (other desktop, suspended UWP)
        assert!(!eligible(true, false, WS_EX_TOOLWINDOW.0, true, "Palette", true));
        assert!(eligible(true, false, WS_EX_TOOLWINDOW.0 | WS_EX_APPWINDOW.0, true, "Odd", false)); // APPWINDOW wins
        assert!(!eligible(true, false, normal, false, "Untitled", true)); // no title
        assert!(!eligible(true, false, normal, true, "Dialog", false)); // its owner stands for it
        assert!(!eligible(true, false, WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0, true, "shellbits_alttab", true));
        assert!(!eligible(true, false, normal, true, "shellbits_launcher", true));
        assert!(!eligible(true, false, normal, true, "Progman", true));
    }

    #[test]
    fn cycling() {
        assert_eq!(step(1, 4, 1), 2);
        assert_eq!(step(3, 4, 1), 0);
        assert_eq!(step(0, 4, -1), 3);
        assert_eq!(step(0, 0, 1), 0);
        // 5 cards of 300 in a 1000-wide panel: rows of 3 + 2 (centred)
        let mut cards = Vec::new();
        let (w, _) = layout(&[300.0; 5], 1000.0, &mut cards);
        assert_eq!(cards.len(), 5);
        assert!(cards[2].top == cards[0].top && cards[3].top > cards[0].top);
        assert!(w <= 1000.0);
        assert_eq!(step_row(&cards, 1, true), 3); // middle of row 1 -> nearest in row 2 (tie: first)
        assert_eq!(step_row(&cards, 4, false), 1); // equidistant from 1 and 2: first wins
        cards.clear();
        layout(&[300.0; 4], 1000.0, &mut cards); // 3 + 1, the lone card centred under card 1
        assert_eq!(step_row(&cards, 3, false), 1);
        assert_eq!(step_row(&cards, 0, true), 3);
        assert_eq!(step_row(&cards, 0, false), 0); // top edge: stay
    }
}

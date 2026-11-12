//! Clipboard history: Win+V lists what Windows has kept from your recent copies (text and pictures);
//! click one, or pick it with the arrows and Enter, and it is pasted into the app you were in.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering::Relaxed};
use std::sync::Mutex;

use windows::core::w;
use windows::ApplicationModel::DataTransfer::{Clipboard, StandardDataFormats};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub static CLIP: AtomicIsize = AtomicIsize::new(0);
static OPEN: AtomicBool = AtomicBool::new(false);
const WM_CLIP_OPEN: u32 = WM_APP + 62; // lParam: the window to paste into
const WM_CLIP_KEY: u32 = WM_APP + 63; // wParam: virtual key (Esc, Up, Down, Enter)
const WM_CLIP_DATA: u32 = WM_APP + 64; // the list has arrived

const W: f32 = 360.0;
const HEAD: f32 = 44.0;
const ROW: f32 = 64.0;
const GAP: f32 = 4.0;
const PAD: f32 = 12.0;
const MAX_ROWS: usize = 6;

#[derive(Default)]
struct State {
    target: isize,
    items: Vec<(String, String)>, // history item id, its text
    off: bool,                    // Windows' clipboard history is switched off
    loaded: bool,
    sel: usize,           // index into items
    top: usize,           // first item shown (the list scrolls)
    hover: Option<usize>, // index into items
    hover_x: bool,        // the mouse is on that row's delete button
    hover_clear: bool,    // the mouse is on "Clear all"
}
static ST: Mutex<State> = Mutex::new(State { target: 0, items: Vec::new(), off: false, loaded: false, sel: 0, top: 0, hover: None, hover_x: false, hover_clear: false });

/// First row to show so that `sel` is on screen, given the current first row.
pub fn scroll_to(sel: usize, top: usize, shown: usize, total: usize) -> usize {
    let max_top = total.saturating_sub(shown);
    if sel < top { sel } else if sel >= top + shown { sel + 1 - shown } else { top }.min(max_top)
}

/// Remove one item from Windows' history (or all of them), then post the fresh list.
fn remove(id: Option<String>, hwnd: isize) {
    std::thread::spawn(move || unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        match id {
            None => {
                let _ = Clipboard::ClearHistory();
            }
            Some(id) => {
                let item = Clipboard::GetHistoryItemsAsync().and_then(|a| a.get()).and_then(|r| r.Items()).ok().and_then(|items| items.into_iter().find(|i| i.Id().map(|x| x.to_string() == id).unwrap_or(false)));
                if let Some(item) = item {
                    let _ = Clipboard::DeleteItemFromHistory(&item);
                }
            }
        }
        let list = history();
        let mut st = ST.lock().unwrap();
        st.off = list.is_none();
        st.items = list.unwrap_or_default();
        st.sel = st.sel.min(st.items.len().saturating_sub(1));
        st.top = scroll_to(st.sel, st.top, MAX_ROWS, st.items.len());
        st.hover = None;
        drop(st);
        let _ = PostMessageW(Some(HWND(hwnd as *mut c_void)), WM_CLIP_DATA, WPARAM(0), LPARAM(0));
    });
}

/// Thumbnails of the picture items, by history item id: (width, height, premultiplied BGRA).
static THUMBS: Mutex<Vec<(String, u32, u32, Vec<u8>)>> = Mutex::new(Vec::new());
pub fn thumb_sizes() -> Vec<(u32, u32)> {
    THUMBS.lock().unwrap().iter().map(|t| (t.1, t.2)).collect()
}
const THUMB_W: u32 = 300;
const THUMB_H: u32 = 96;

/// The size a w x h picture is drawn at to fit max_w x max_h, keeping its shape (never enlarged).
pub fn fit(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (0, 0);
    }
    let k = (max_w as f64 / w as f64).min(max_h as f64 / h as f64).min(1.0);
    (((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1))
}

/// Decode a clipboard picture (a WinRT stream) into a small premultiplied thumbnail.
fn thumbnail(view: &windows::ApplicationModel::DataTransfer::DataPackageView) -> Option<(u32, u32, Vec<u8>)> {
    use windows::Win32::Graphics::Imaging::*;
    use windows::Win32::System::Com::*;
    unsafe {
        let r = view.GetBitmapAsync().ok()?.get().ok()?;
        let ras = r.OpenReadAsync().ok()?.get().ok()?;
        let stream: IStream = windows::Win32::System::WinRT::CreateStreamOverRandomAccessStream(&ras).ok()?;
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let dec = wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = dec.GetFrame(0).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        let (tw, th) = fit(w, h, THUMB_W, THUMB_H);
        let sc = wic.CreateBitmapScaler().ok()?;
        sc.Initialize(&frame, tw, th, WICBitmapInterpolationModeFant).ok()?;
        let conv = wic.CreateFormatConverter().ok()?;
        conv.Initialize(&sc, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        let mut px = vec![0u8; (tw * th * 4) as usize];
        conv.CopyPixels(std::ptr::null(), tw * 4, &mut px).ok()?;
        Some((tw, th, px))
    }
}

/// The text and picture items in Windows' clipboard history, newest first: (id, text - empty for a
/// picture, whose thumbnail goes to THUMBS). None = history is off.
pub fn history() -> Option<Vec<(String, String)>> {
    if !Clipboard::IsHistoryEnabled().unwrap_or(false) {
        return None;
    }
    let mut out = Vec::new();
    let text_format = StandardDataFormats::Text().unwrap_or_default();
    let bitmap_format = StandardDataFormats::Bitmap().unwrap_or_default();
    let mut thumbs = Vec::new();
    if let Ok(items) = Clipboard::GetHistoryItemsAsync().and_then(|a| a.get()).and_then(|r| r.Items()) {
        for it in items {
            let (Ok(id), Ok(view)) = (it.Id(), it.Content()) else { continue };
            if view.Contains(&text_format).unwrap_or(false) {
                if let Ok(t) = view.GetTextAsync().and_then(|a| a.get()) {
                    out.push((id.to_string(), t.to_string()));
                }
            } else if view.Contains(&bitmap_format).unwrap_or(false) {
                // a copied picture: shown as its thumbnail
                if let Some((w, h, px)) = thumbnail(&view) {
                    thumbs.push((id.to_string(), w, h, px));
                    out.push((id.to_string(), String::new()));
                }
            }
        }
    }
    *THUMBS.lock().unwrap() = thumbs;
    Some(out)
}

/// What a row shows: the text on one line, cut to `max` characters.
pub fn preview(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max { flat.chars().take(max.saturating_sub(1)).collect::<String>() + "\u{2026}" } else { flat }
}

/// Make history item `id` the current clipboard content, give the focus back to `target`, press Ctrl+V.
fn paste(id: String, target: isize) {
    std::thread::spawn(move || unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        let item = Clipboard::GetHistoryItemsAsync().and_then(|a| a.get()).and_then(|r| r.Items()).ok().and_then(|items| items.into_iter().find(|i| i.Id().map(|x| x.to_string() == id).unwrap_or(false)));
        let Some(item) = item else { return };
        if Clipboard::SetHistoryItemAsContent(&item).is_err() {
            return;
        }
        let t = HWND(target as *mut c_void);
        if IsWindow(Some(t)).as_bool() {
            crate::flyout::force_foreground(t);
            std::thread::sleep(std::time::Duration::from_millis(120));
            crate::launcher::inject(&[(VK_CONTROL.0, false), (0x56, false), (0x56, true), (VK_CONTROL.0, true)]);
        }
    });
}

fn rows(st: &State) -> usize {
    if st.items.is_empty() { 1 } else { st.items.len().min(MAX_ROWS) }
}

/// A picture's card is taller than a text card, like Windows' own.
const PIC_ROW: f32 = 112.0;
fn row_h(st: &State, i: usize) -> f32 {
    if st.items.get(i).is_some_and(|it| it.1.is_empty()) { PIC_ROW } else { ROW }
}
/// Top of the k-th card shown (k counted from the first one on screen).
fn row_top(st: &State, k: usize) -> f32 {
    HEAD + (st.top..st.top + k).map(|i| row_h(st, i) + GAP).sum::<f32>()
}

fn height(st: &State) -> f32 {
    if st.items.is_empty() {
        return HEAD + ROW + PAD;
    }
    row_top(st, rows(st)) + PAD - GAP
}

/// The item under a point of the panel (index into items), if any.
fn row_at(st: &State, y: f32) -> Option<usize> {
    if y < HEAD || st.items.is_empty() {
        return None;
    }
    (0..rows(st)).filter(|k| st.top + k < st.items.len()).find(|k| {
        let t = row_top(st, *k);
        y >= t && y < t + row_h(st, st.top + k)
    }).map(|k| st.top + k)
}

/// "Clear all" in the header, and the delete button at the right of a row (panel units).
fn clear_rect() -> (f32, f32, f32, f32) {
    (W - PAD - 70.0, 8.0, W - PAD, HEAD - 8.0)
}
fn on_delete(x: f32) -> bool {
    x >= W - PAD - 34.0 && x < W - PAD - 6.0
}

/// Show a `w` x `h` (DIP) panel for `target`: just under its text caret when the app says where that
/// is, else bottom centre of its screen. Shared with the emoji panel.
pub(crate) unsafe fn show_by_caret(hwnd: HWND, target: HWND, w: f32, h: f32) {
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    let _ = GetMonitorInfoW(MonitorFromWindow(target, MONITOR_DEFAULTTOPRIMARY), &mut mi);
    let (w_, h) = ((w * s) as i32, (h * s) as i32);
    let mut gui = GUITHREADINFO { cbSize: size_of::<GUITHREADINFO>() as u32, ..Default::default() };
    let mut pt = POINT { x: (mi.rcWork.left + mi.rcWork.right - w_) / 2, y: mi.rcWork.bottom - h - (12.0 * s) as i32 };
    if GetGUIThreadInfo(GetWindowThreadProcessId(target, None), &mut gui).is_ok() && !gui.hwndCaret.is_invalid() {
        let mut c = POINT { x: gui.rcCaret.left, y: gui.rcCaret.bottom };
        let _ = ClientToScreen(gui.hwndCaret, &mut c);
        pt = POINT { x: c.x, y: c.y + (8.0 * s) as i32 };
    }
    let x = pt.x.clamp(mi.rcWork.left, (mi.rcWork.right - w_).max(mi.rcWork.left));
    let y = pt.y.clamp(mi.rcWork.top, (mi.rcWork.bottom - h).max(mi.rcWork.top));
    let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w_, h, SWP_SHOWWINDOW | SWP_NOACTIVATE);
    let _ = InvalidateRect(Some(hwnd), None, true);
}

unsafe fn place(hwnd: HWND, st: &State) {
    show_by_caret(hwnd, HWND(st.target as *mut c_void), W, height(st));
}

unsafe fn hide(hwnd: HWND) {
    OPEN.store(false, Relaxed);
    let _ = ShowWindow(hwnd, SW_HIDE);
}

pub unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    match msg {
        WM_CLIP_OPEN => {
            {
                let mut st = ST.lock().unwrap();
                *st = State { target: lp.0, ..Default::default() };
                place(hwnd, &st);
            }
            OPEN.store(true, Relaxed);
            SetTimer(Some(hwnd), 1, 250, None);
            // the list comes from Windows asynchronously: fetch it off this thread
            let h = hwnd.0 as isize;
            std::thread::spawn(move || {
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
                let list = history();
                let mut st = ST.lock().unwrap();
                st.off = list.is_none();
                st.items = list.unwrap_or_default();
                st.loaded = true;
                drop(st);
                let _ = PostMessageW(Some(HWND(h as *mut c_void)), WM_CLIP_DATA, WPARAM(0), LPARAM(0));
            });
        }
        WM_CLIP_DATA => {
            if OPEN.load(Relaxed) {
                place(hwnd, &ST.lock().unwrap());
            }
        }
        WM_TIMER => {
            // belongs to the app it was opened for: gone when that app loses the focus
            let t = HWND(ST.lock().unwrap().target as *mut c_void);
            if !IsWindow(Some(t)).as_bool() || GetForegroundWindow() != t {
                let _ = KillTimer(Some(hwnd), 1);
                hide(hwnd);
            }
        }
        WM_CLIP_KEY => {
            let mut st = ST.lock().unwrap();
            let n = st.items.len();
            match VIRTUAL_KEY(wp.0 as u16) {
                VK_DELETE if n > 0 => {
                    let id = st.items[st.sel].0.clone();
                    drop(st);
                    remove(Some(id), hwnd.0 as isize);
                }
                VK_ESCAPE => {
                    drop(st);
                    hide(hwnd);
                }
                VK_DOWN if n > 0 => {
                    st.sel = (st.sel + 1) % n;
                    st.top = scroll_to(st.sel, st.top, MAX_ROWS, n);
                    let _ = InvalidateRect(Some(hwnd), None, true);
                }
                VK_UP if n > 0 => {
                    st.sel = (st.sel + n - 1) % n;
                    st.top = scroll_to(st.sel, st.top, MAX_ROWS, n);
                    let _ = InvalidateRect(Some(hwnd), None, true);
                }
                VK_RETURN => {
                    let pick = st.items.get(st.sel).map(|i| (i.0.clone(), st.target));
                    drop(st);
                    hide(hwnd);
                    if let Some((id, target)) = pick {
                        paste(id, target);
                    }
                }
                _ => {}
            }
        }
        crate::flyout::WM_DISMISS => hide(hwnd),
        WM_MOUSEMOVE => {
            let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            let mut st = ST.lock().unwrap();
            let (x, y) = ((lp.0 & 0xFFFF) as i16 as f32 / s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s);
            let h = row_at(&st, y);
            let c = clear_rect();
            let now = (h, h.is_some() && on_delete(x), !st.items.is_empty() && x >= c.0 && x < c.2 && y >= c.1 && y < c.3);
            if (st.hover, st.hover_x, st.hover_clear) != now {
                (st.hover, st.hover_x, st.hover_clear) = now;
                let _ = InvalidateRect(Some(hwnd), None, true);
            }
        }
        WM_MOUSEWHEEL => {
            let mut st = ST.lock().unwrap();
            let up_ = ((wp.0 >> 16) & 0xFFFF) as i16 > 0;
            let max_top = st.items.len().saturating_sub(MAX_ROWS);
            st.top = if up_ { st.top.saturating_sub(1) } else { (st.top + 1).min(max_top) };
            st.hover = None;
            let _ = InvalidateRect(Some(hwnd), None, true);
        }
        WM_MOUSELEAVE => {
            ST.lock().unwrap().hover = None;
            let _ = InvalidateRect(Some(hwnd), None, true);
        }
        WM_LBUTTONUP => {
            let (x, y) = ((lp.0 & 0xFFFF) as i16 as f32 / s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s);
            let st = ST.lock().unwrap();
            let c = clear_rect();
            if !st.items.is_empty() && x >= c.0 && x < c.2 && y >= c.1 && y < c.3 {
                drop(st);
                remove(None, hwnd.0 as isize);
                return LRESULT(0);
            }
            if let Some(i) = row_at(&st, y).filter(|_| on_delete(x)) {
                let id = st.items[i].0.clone();
                drop(st);
                remove(Some(id), hwnd.0 as isize);
                return LRESULT(0);
            }
            if st.off && y > HEAD {
                drop(st);
                hide(hwnd);
                windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), w!("ms-settings:clipboard"), None, None, SW_SHOWNORMAL);
            } else if let Some(i) = row_at(&st, y) {
                let pick = (st.items[i].0.clone(), st.target);
                drop(st);
                hide(hwnd);
                paste(pick.0, pick.1);
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
                let st = ST.lock().unwrap();
                let _ = draw(mem, &rc, s, &st);
                drop(st);
                let _ = EndBufferedPaint(pb, true);
            }
            let _ = EndPaint(hwnd, &ps);
        }
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

/// Win+V: open the panel for the window in front, or close it. Called from the keyboard hook.
pub unsafe fn toggle() {
    let p = HWND(CLIP.load(Relaxed) as *mut c_void);
    if p.is_invalid() {
        return;
    }
    if OPEN.load(Relaxed) {
        let _ = PostMessageW(Some(p), WM_CLIP_KEY, WPARAM(VK_ESCAPE.0 as usize), LPARAM(0));
    } else {
        let _ = PostMessageW(Some(p), WM_CLIP_OPEN, WPARAM(0), LPARAM(GetForegroundWindow().0 as isize));
    }
}

/// While the panel is open: Esc, Up, Down and Enter drive it. Returns true to swallow the key.
pub unsafe fn on_key(k: &KBDLLHOOKSTRUCT, up: bool) -> bool {
    if !OPEN.load(Relaxed) || k.dwExtraInfo == crate::launcher::MARK {
        return false;
    }
    if !matches!(VIRTUAL_KEY(k.vkCode as u16), VK_ESCAPE | VK_UP | VK_DOWN | VK_RETURN | VK_DELETE) {
        return false;
    }
    if !up {
        let _ = PostMessageW(Some(HWND(CLIP.load(Relaxed) as *mut c_void)), WM_CLIP_KEY, WPARAM(k.vkCode as usize), LPARAM(0));
    }
    true
}

pub unsafe fn create(inst: HINSTANCE) -> windows::core::Result<()> {
    let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: inst, lpszClassName: w!("shellbits_clip"), hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
    RegisterClassW(&wc);
    let h = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE, w!("shellbits_clip"), w!("Clipboard"), WS_POPUP, 0, 0, 10, 10, None, None, Some(inst), None)?;
    let (dark, round) = (1u32, 2u32);
    let _ = DwmSetWindowAttribute(h, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const c_void, 4);
    let _ = DwmSetWindowAttribute(h, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as *const c_void, 4);
    let _ = crate::fluent_window(h); // Windows' blurred panel material
    CLIP.store(h.0 as isize, Relaxed);
    Ok(())
}

/// The panel, drawn with Direct2D: title, Clear all, then one card per history item.
unsafe fn draw(dc: HDC, rc: &RECT, s: f32, st: &State) -> windows::core::Result<()> {
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
    let font = |px: f32, weight: DWRITE_FONT_WEIGHT| dw.CreateTextFormat(w!("Segoe UI Variable Text"), None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, px, w!(""));
    let (title, body) = (font(14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?, font(13.0, DWRITE_FONT_WEIGHT_NORMAL)?);
    title.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
    let w = rc.right as f32 / s;
    let b = |c: u32| rt.CreateSolidColorBrush(&rgb(c), None);
    let txt = |t: &str, fmt: &IDWriteTextFormat, r: windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F, c: u32| -> windows::core::Result<()> {
        rt.DrawText(&utf16(t), fmt, &r, &b(c)?, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        Ok(())
    };
    rt.BeginDraw();
    rt.Clear(Some(&crate::flyout::base(rgb(0x2C2C2C))));
    txt("Clipboard", &title, rect(PAD + 4.0, 0.0, w, HEAD), 0xFFFFFF)?;
    if st.items.is_empty() {
        let msg = if !st.loaded { "Loading\u{2026}" } else if st.off { "Clipboard history is off. Click here to turn it on in Settings." } else { "Nothing here yet. Copy some text and it will show up." };
        txt(msg, &body, rect(PAD + 4.0, HEAD, w - PAD, HEAD + ROW), 0xC5C5C5)?;
    } else {
        let c = clear_rect();
        let right = font(13.0, DWRITE_FONT_WEIGHT_NORMAL)?;
        right.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
        right.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        txt("Clear all", &right, rect(c.0, c.1, c.2, c.3), if st.hover_clear { 0xFFFFFF } else { 0xC5C5C5 })?;
    }
    for (i, (_, text)) in st.items.iter().enumerate().skip(st.top).take(MAX_ROWS) {
        let y = row_top(st, i - st.top);
        let rh = row_h(st, i);
        let card = rect(PAD, y, w - PAD, y + rh);
        rt.FillRoundedRectangle(&rrect(card, 4.0), &b(if st.hover == Some(i) { 0x454545 } else { 0x383838 })?);
        if i == st.sel {
            rt.FillRoundedRectangle(&rrect(rect(PAD, y + 18.0, PAD + 3.0, y + rh - 18.0), 1.5), &b(0x4CC2FF)?);
        }
        let id = &st.items[i].0;
        let thumb = THUMBS.lock().unwrap().iter().find(|t| &t.0 == id).map(|t| (t.1, t.2, t.3.clone()));
        if let Some((tw, th, px)) = thumb {
            let props = D2D1_BITMAP_PROPERTIES { pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }, dpiX: 96.0, dpiY: 96.0 };
            if let Ok(bmp) = rt.CreateBitmap(D2D_SIZE_U { width: tw, height: th }, Some(px.as_ptr() as *const c_void), tw * 4, &props) {
                let top = y + (rh - th as f32) / 2.0;
                rt.DrawBitmap(&bmp, Some(&rect(PAD + 12.0, top, PAD + 12.0 + tw as f32, top + th as f32)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
            }
        } else {
            txt(&preview(text, 110), &body, rect(PAD + 12.0, y + 10.0, w - PAD - 40.0, y + ROW - 10.0), 0xFFFFFF)?;
        }
        if st.hover == Some(i) {
            let x = font(12.0, DWRITE_FONT_WEIGHT_NORMAL)?;
            x.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            x.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            txt("\u{2715}", &x, rect(w - PAD - 34.0, y, w - PAD - 6.0, y + rh), if st.hover_x { 0xFFFFFF } else { 0x9A9A9A })?;
        }
    }
    rt.EndDraw(None, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn picture_fit() {
        assert_eq!(fit(1920, 1080, 300, 48), (85, 48));
        let st = State { items: vec![("1".into(), "text".into()), ("2".into(), String::new()), ("3".into(), "t".into())], ..Default::default() };
        assert_eq!(row_at(&st, HEAD + ROW + GAP + 100.0), Some(1)); // inside the tall picture card
        assert_eq!(row_at(&st, HEAD + ROW + GAP + PIC_ROW + GAP + 5.0), Some(2));
        assert_eq!(fit(3000, 100, 300, 48), (300, 10));
        assert_eq!(fit(20, 10, 300, 48), (20, 10)); // small pictures are not enlarged
        assert_eq!(fit(0, 10, 300, 48), (0, 0));
    }

    #[test]
    fn previews_and_rows() {
        assert_eq!(preview("a\n  b\tc", 20), "a b c");
        assert_eq!(preview("abcdefghij", 5), "abcd\u{2026}");
        let st = State { items: vec![("1".into(), "x".into()), ("2".into(), "y".into())], ..Default::default() };
        assert_eq!(row_at(&st, HEAD + 10.0), Some(0));
        assert_eq!(row_at(&st, HEAD + ROW + GAP + 10.0), Some(1));
        assert_eq!(row_at(&st, HEAD + ROW + 2.0), None); // the gap between rows
        assert_eq!(row_at(&st, HEAD + 2.0 * (ROW + GAP) + 10.0), None);
        assert_eq!(row_at(&st, 10.0), None);
        // scrolling keeps the selection on screen
        assert_eq!(scroll_to(0, 0, 6, 18), 0);
        assert_eq!(scroll_to(6, 0, 6, 18), 1);
        assert_eq!(scroll_to(17, 0, 6, 18), 12);
        assert_eq!(scroll_to(2, 5, 6, 18), 2);
        assert_eq!(scroll_to(0, 3, 6, 4), 0);
    }
}

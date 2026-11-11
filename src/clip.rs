//! Clipboard history: Win+V lists what Windows has kept from your recent copies (text items);
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

/// The text items in Windows' clipboard history, newest first: (id, text). None = history is off.
pub fn history() -> Option<Vec<(String, String)>> {
    if !Clipboard::IsHistoryEnabled().unwrap_or(false) {
        return None;
    }
    let mut out = Vec::new();
    let text_format = StandardDataFormats::Text().unwrap_or_default();
    if let Ok(items) = Clipboard::GetHistoryItemsAsync().and_then(|a| a.get()).and_then(|r| r.Items()) {
        for it in items {
            let (Ok(id), Ok(view)) = (it.Id(), it.Content()) else { continue };
            if view.Contains(&text_format).unwrap_or(false) {
                if let Ok(t) = view.GetTextAsync().and_then(|a| a.get()) {
                    out.push((id.to_string(), t.to_string()));
                }
            }
        }
    }
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

fn height(st: &State) -> f32 {
    HEAD + rows(st) as f32 * (ROW + GAP) + PAD - GAP
}

/// The item under a point of the panel (index into items), if any.
fn row_at(st: &State, y: f32) -> Option<usize> {
    if y < HEAD || st.items.is_empty() {
        return None;
    }
    let i = ((y - HEAD) / (ROW + GAP)) as usize;
    (i < st.items.len().min(MAX_ROWS) && (y - HEAD) - i as f32 * (ROW + GAP) < ROW && st.top + i < st.items.len()).then_some(st.top + i)
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
            let bg = CreateSolidBrush(COLORREF(0x2C2C2C));
            FillRect(dc, &rc, bg);
            let _ = DeleteObject(bg.into());
            SetBkMode(dc, TRANSPARENT);
            let font = |px: f32, weight: i32| CreateFontW(-(px * s) as i32, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI Variable Text"));
            let (title, body) = (font(14.0, 600), font(13.0, 400));
            let old = SelectObject(dc, title.into());
            SetTextColor(dc, COLORREF(0xFFFFFF));
            let mut tr = RECT { left: (PAD * s) as i32 + 4, top: 0, right: rc.right, bottom: (HEAD * s) as i32 };
            let mut head: Vec<u16> = "Clipboard".encode_utf16().collect();
            DrawTextW(dc, &mut head, &mut tr, DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
            SelectObject(dc, body.into());
            let st = ST.lock().unwrap();
            let pen = SelectObject(dc, GetStockObject(NULL_PEN));
            if st.items.is_empty() {
                let msg = if !st.loaded { "Loading\u{2026}" } else if st.off { "Clipboard history is off. Click here to turn it on in Settings." } else { "Nothing here yet. Copy some text and it will show up." };
                SetTextColor(dc, COLORREF(0xC5C5C5));
                let mut r = RECT { left: (PAD * s) as i32 + 4, top: (HEAD * s) as i32, right: rc.right - (PAD * s) as i32, bottom: ((HEAD + ROW) * s) as i32 };
                let mut t: Vec<u16> = msg.encode_utf16().collect();
                DrawTextW(dc, &mut t, &mut r, DT_WORDBREAK | DT_NOPREFIX);
            }
            if !st.items.is_empty() {
                SetTextColor(dc, COLORREF(if st.hover_clear { 0xFFFFFF } else { 0xC5C5C5 }));
                let c = clear_rect();
                let mut r = RECT { left: (c.0 * s) as i32, top: (c.1 * s) as i32, right: (c.2 * s) as i32, bottom: (c.3 * s) as i32 };
                let mut t: Vec<u16> = "Clear all".encode_utf16().collect();
                DrawTextW(dc, &mut t, &mut r, DT_SINGLELINE | DT_VCENTER | DT_RIGHT | DT_NOPREFIX);
            }
            for (i, (_, text)) in st.items.iter().enumerate().skip(st.top).take(MAX_ROWS) {
                let y = HEAD + (i - st.top) as f32 * (ROW + GAP);
                // colours are 0x00BBGGRR
                let brush = CreateSolidBrush(COLORREF(if st.hover == Some(i) { 0x454545 } else { 0x383838 }));
                let ob = SelectObject(dc, brush.into());
                let _ = RoundRect(dc, (PAD * s) as i32, (y * s) as i32, rc.right - (PAD * s) as i32, ((y + ROW) * s) as i32, (8.0 * s) as i32, (8.0 * s) as i32);
                SelectObject(dc, ob);
                let _ = DeleteObject(brush.into());
                if i == st.sel {
                    let accent = CreateSolidBrush(COLORREF(0xFFC24C));
                    let bar = RECT { left: (PAD * s) as i32, top: ((y + 18.0) * s) as i32, right: ((PAD + 3.0) * s) as i32, bottom: ((y + ROW - 18.0) * s) as i32 };
                    FillRect(dc, &bar, accent);
                    let _ = DeleteObject(accent.into());
                }
                SetTextColor(dc, COLORREF(0xFFFFFF));
                let mut r = RECT { left: ((PAD + 12.0) * s) as i32, top: ((y + 10.0) * s) as i32, right: rc.right - ((PAD + 40.0) * s) as i32, bottom: ((y + ROW - 10.0) * s) as i32 };
                let mut t: Vec<u16> = preview(text, 110).encode_utf16().collect();
                DrawTextW(dc, &mut t, &mut r, DT_WORDBREAK | DT_END_ELLIPSIS | DT_NOPREFIX | DT_EDITCONTROL);
                if st.hover == Some(i) {
                    // delete this item (the cross lights up under the mouse)
                    SetTextColor(dc, COLORREF(if st.hover_x { 0xFFFFFF } else { 0x9A9A9A }));
                    let mut xr = RECT { left: rc.right - ((PAD + 34.0) * s) as i32, top: (y * s) as i32, right: rc.right - ((PAD + 6.0) * s) as i32, bottom: ((y + ROW) * s) as i32 };
                    let mut cross: Vec<u16> = "\u{2715}".encode_utf16().collect();
                    DrawTextW(dc, &mut cross, &mut xr, DT_SINGLELINE | DT_VCENTER | DT_CENTER | DT_NOPREFIX);
                }
            }
            SelectObject(dc, pen);
            SelectObject(dc, old);
            let _ = DeleteObject(title.into());
            let _ = DeleteObject(body.into());
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
    CLIP.store(h.0 as isize, Relaxed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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

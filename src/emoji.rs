//! Emoji panel: Win+. (or Win+;) shows the emoji Windows' own emoji font contains, in colour.
//! Click one, or move with the arrows and press Enter, and it is typed into the app you were in.
//! The panel never takes the focus, so you can add several in a row; Esc closes it.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering::Relaxed};

use windows::core::{w, Interface};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::flyout::{rect, rrect, utf16};
use crate::quick::rgb;

pub static EMOJI: AtomicIsize = AtomicIsize::new(0);
static OPEN: AtomicBool = AtomicBool::new(false);
const WM_EMOJI_OPEN: u32 = WM_APP + 65; // lParam: the window to type into
const WM_EMOJI_KEY: u32 = WM_APP + 66; // wParam: virtual key

const COLS: usize = 8;
const ROWS: usize = 6;
const CELL: f32 = 40.0;
const PAD: f32 = 12.0;
const TITLE: f32 = 36.0; // "Emoji"
const HEAD: f32 = TITLE + CELL; // then the row of category buttons
const W: f32 = PAD * 2.0 + COLS as f32 * CELL;
const H: f32 = HEAD + ROWS as f32 * CELL + PAD;

#[derive(Default)]
struct State {
    target: isize,
    all: Vec<u32>,    // every emoji the font offers, in block order
    shown: Vec<u32>,  // the grid: a row of recently used (0 = empty cell), then `all`
    hover_cat: Option<usize>,
    top: usize,    // first row shown
    sel: usize,
    hover: Option<usize>,
    gfx: Option<(ID2D1DCRenderTarget, IDWriteTextFormat, IDWriteTextFormat)>,
}
thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
}

/// Blocks of emoji in the order the panel shows them: faces first, then people and gestures,
/// animals/food/places, transport, and the older symbols.
const BLOCKS: [(u32, u32); 6] = [(0x1F600, 0x1F64F), (0x1F900, 0x1F9FF), (0x1F300, 0x1F5FF), (0x1F680, 0x1F6FF), (0x1FA70, 0x1FAFF), (0x2600, 0x27BF)];

/// Pick the emoji out of the code-point ranges a font covers, block by block.
/// Skin-tone modifiers are left out: they change the emoji before them rather than stand alone.
pub fn pick(ranges: &[(u32, u32)]) -> Vec<u32> {
    let mut out = Vec::new();
    for (b0, b1) in BLOCKS {
        for &(r0, r1) in ranges {
            for cp in r0.max(b0)..=r1.min(b1) {
                if !(0x1F3FB..=0x1F3FF).contains(&cp) {
                    out.push(cp);
                }
            }
        }
    }
    out
}

/// The grid's contents: one row of recently used emoji (blank cells are 0) when there are any, then all of them.
pub fn grid(recent: &[u32], all: &[u32]) -> Vec<u32> {
    let mut v = Vec::with_capacity(all.len() + COLS);
    if !recent.is_empty() {
        v.extend(recent.iter().take(COLS - 1));
        v.resize(COLS, 0);
    }
    v.extend_from_slice(all);
    v
}

/// Recently used list after typing `cp`: it moves to the front, no repeats, one short of a full row.
pub fn used(recent: &[u32], cp: u32) -> Vec<u32> {
    let mut v = vec![cp];
    v.extend(recent.iter().copied().filter(|c| *c != cp));
    v.truncate(COLS - 1); // always one blank cell left, which is how the row is recognised
    v
}

fn recent_file() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join("shellbits").join("emoji-recent.txt")
}
fn load_recent() -> Vec<u32> {
    std::fs::read_to_string(recent_file()).unwrap_or_default().split_whitespace().filter_map(|t| u32::from_str_radix(t, 16).ok()).take(COLS).collect()
}
fn save_recent(v: &[u32]) {
    let f = recent_file();
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(f, v.iter().map(|c| format!("{c:X}")).collect::<Vec<_>>().join(" "));
}

/// Grid row where each block of emoji starts (None when the font has nothing from that block).
fn block_rows(shown: &[u32]) -> [Option<usize>; 6] {
    // a first row with blank cells is the recently-used row, not part of any block
    let skip = if shown.iter().take(COLS).any(|c| *c == 0) { COLS } else { 0 };
    BLOCKS.map(|(b0, b1)| shown.iter().skip(skip).position(|c| (b0..=b1).contains(c)).map(|i| (i + skip) / COLS))
}

/// What gets typed for a code point, as UTF-16: older symbols need the "show as emoji" mark after them.
pub fn typed(cp: u32) -> Vec<u16> {
    let mut s: String = char::from_u32(cp).map(String::from).unwrap_or_default();
    if cp < 0x1F000 {
        s.push('\u{FE0F}');
    }
    s.encode_utf16().collect()
}

/// Every code point range Windows' emoji font has a picture for.
unsafe fn font_ranges() -> Vec<(u32, u32)> {
    (|| -> windows::core::Result<Vec<(u32, u32)>> {
        let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let mut fonts = None;
        dw.GetSystemFontCollection(&mut fonts, false)?;
        let fonts = fonts.ok_or(windows::core::Error::from(E_FAIL))?;
        let (mut index, mut found) = (0u32, FALSE);
        fonts.FindFamilyName(w!("Segoe UI Emoji"), &mut index, &mut found)?;
        if !found.as_bool() {
            return Ok(Vec::new());
        }
        let face: IDWriteFontFace1 = fonts.GetFontFamily(index)?.GetFont(0)?.CreateFontFace()?.cast()?;
        let mut n = 0u32;
        let _ = face.GetUnicodeRanges(None, &mut n); // first call only reports how many
        let mut v = vec![DWRITE_UNICODE_RANGE::default(); n as usize];
        face.GetUnicodeRanges(Some(&mut v), &mut n)?;
        Ok(v.iter().map(|r| (r.first, r.last)).collect())
    })()
    .unwrap_or_default()
}

/// Type the emoji into whatever has the keyboard focus (the panel never takes it).
unsafe fn send(cp: u32) {
    let mut inputs = Vec::new();
    for unit in typed(cp) {
        for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
            inputs.push(INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wScan: unit, dwFlags: flags, dwExtraInfo: crate::launcher::MARK, ..Default::default() } } });
        }
    }
    SendInput(&inputs, size_of::<INPUT>() as i32);
}

fn cell_at(st: &State, x: f32, y: f32) -> Option<usize> {
    if x < PAD || y < HEAD || x >= PAD + COLS as f32 * CELL || y >= HEAD + ROWS as f32 * CELL {
        return None;
    }
    let i = (st.top + ((y - HEAD) / CELL) as usize) * COLS + ((x - PAD) / CELL) as usize;
    (i < st.shown.len() && st.shown[i] != 0).then_some(i)
}

/// The category button under a point, if any.
fn cat_at(x: f32, y: f32) -> Option<usize> {
    (y >= TITLE && y < HEAD && x >= PAD && x < PAD + BLOCKS.len() as f32 * CELL).then(|| ((x - PAD) / CELL) as usize)
}

fn keep_visible(st: &mut State) {
    let row = st.sel / COLS;
    if row < st.top {
        st.top = row;
    } else if row >= st.top + ROWS {
        st.top = row + 1 - ROWS;
    }
}

unsafe fn hide(hwnd: HWND) {
    OPEN.store(false, Relaxed);
    let _ = KillTimer(Some(hwnd), 1);
    let _ = ShowWindow(hwnd, SW_HIDE);
}

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    // drawn into a buffer that keeps its alpha channel, so the blurred backdrop shows through
    use windows::Win32::UI::Controls::{BeginBufferedPaint, EndBufferedPaint, BPBF_TOPDOWNDIB, BPPF_ERASE, BP_PAINTPARAMS};
    let params = BP_PAINTPARAMS { cbSize: size_of::<BP_PAINTPARAMS>() as u32, dwFlags: BPPF_ERASE, ..Default::default() };
    let mut mem = HDC::default();
    let pb = BeginBufferedPaint(hdc, &rc, BPBF_TOPDOWNDIB, Some(&params), &mut mem);
    if mem.is_invalid() {
        let _ = EndBufferedPaint(pb, true);
    let _ = EndPaint(hwnd, &ps);
        return;
    }
    let dpi = GetDpiForWindow(hwnd) as f32;
    ST.with(|s| {
        let mut st = s.borrow_mut();
        if st.gfx.is_none() {
            st.gfx = (|| -> windows::core::Result<_> {
                let f: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
                let props = D2D1_RENDER_TARGET_PROPERTIES { pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }, ..Default::default() };
                let rt = f.CreateDCRenderTarget(&props)?;
                let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
                let glyphs = dw.CreateTextFormat(w!("Segoe UI Emoji"), None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 24.0, w!("en-us"))?;
                glyphs.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                glyphs.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                let title = dw.CreateTextFormat(w!("Segoe UI Variable Text"), None, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 14.0, w!("en-us"))?;
                title.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                Ok((rt, glyphs, title))
            })()
            .ok();
        }
        let Some((rt, glyphs, title)) = st.gfx.clone() else { return };
        let Ok(small_glyphs) = (|| -> windows::core::Result<IDWriteTextFormat> {
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let f = dw.CreateTextFormat(w!("Segoe UI Emoji"), None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 18.0, w!("en-us"))?;
            f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            Ok(f)
        })() else { return };
        let ok = (|| -> windows::core::Result<()> {
            rt.BindDC(mem, &rc)?;
            rt.SetDpi(dpi, dpi);
            rt.BeginDraw();
            rt.Clear(Some(&crate::flyout::base(rgb(0x2C2C2C))));
            let b = rt.CreateSolidColorBrush(&rgb(0xFFFFFF), None)?;
            rt.DrawText(&utf16("Emoji"), &title, &rect(PAD + 4.0, 0.0, W, TITLE), &b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
            // one button per block, showing its first emoji; the block you are looking at is underlined
            let starts = block_rows(&st.shown);
            let current = starts.iter().enumerate().filter(|(_, r)| r.is_some_and(|r| r <= st.top)).map(|(k, _)| k).last();
            for (k, start) in starts.iter().enumerate() {
                let Some(row) = start else { continue };
                let c = rect(PAD + k as f32 * CELL, TITLE, PAD + (k + 1) as f32 * CELL, HEAD);
                if st.hover_cat == Some(k) {
                    b.SetColor(&rgb(0x454545));
                    rt.FillRoundedRectangle(&rrect(rect(c.left + 2.0, c.top + 2.0, c.right - 2.0, c.bottom - 4.0), 6.0), &b);
                }
                b.SetColor(&rgb(0xFFFFFF));
                rt.DrawText(&typed(st.shown[row * COLS]), &small_glyphs, &rect(c.left, c.top, c.right, c.bottom - 4.0), &b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, DWRITE_MEASURING_MODE_NATURAL);
                if current == Some(k) {
                    b.SetColor(&rgb(0x4CC2FF));
                    rt.FillRoundedRectangle(&rrect(rect(c.left + 12.0, c.bottom - 4.0, c.right - 12.0, c.bottom - 1.0), 1.5), &b);
                    b.SetColor(&rgb(0xFFFFFF));
                }
            }
            for i in st.top * COLS..((st.top + ROWS) * COLS).min(st.shown.len()) {
                if st.shown[i] == 0 {
                    continue;
                }
                let (col, row) = ((i % COLS) as f32, (i / COLS - st.top) as f32);
                let c = rect(PAD + col * CELL, HEAD + row * CELL, PAD + (col + 1.0) * CELL, HEAD + (row + 1.0) * CELL);
                if st.hover == Some(i) || st.sel == i {
                    b.SetColor(&rgb(if st.hover == Some(i) { 0x454545 } else { 0x3A3A3A }));
                    rt.FillRoundedRectangle(&rrect(rect(c.left + 2.0, c.top + 2.0, c.right - 2.0, c.bottom - 2.0), 6.0), &b);
                    b.SetColor(&rgb(0xFFFFFF));
                }
                // ENABLE_COLOR_FONT: draw the font's colour layers, not its black-and-white outline
                rt.DrawText(&typed(st.shown[i]), &glyphs, &c, &b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, DWRITE_MEASURING_MODE_NATURAL);
            }
            rt.EndDraw(None, None)
        })();
        if ok.is_err() {
            st.gfx = None;
        }
    });
    let _ = EndPaint(hwnd, &ps);
}

pub unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    let pos = |lp: LPARAM| ((lp.0 & 0xFFFF) as i16 as f32 / s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s);
    let redraw = || {
        let _ = InvalidateRect(Some(hwnd), None, false);
    };
    match msg {
        WM_EMOJI_OPEN => {
            ST.with(|st| {
                let mut st = st.borrow_mut();
                if st.all.is_empty() {
                    st.all = pick(&font_ranges());
                }
                st.target = lp.0;
                st.hover = None;
                st.hover_cat = None;
                st.shown = grid(&load_recent(), &st.all);
                st.top = 0;
                st.sel = st.shown.iter().position(|c| *c != 0).unwrap_or(0);
            });
            crate::clip::show_by_caret(hwnd, HWND(lp.0 as *mut c_void), W, H);
            OPEN.store(true, Relaxed);
            SetTimer(Some(hwnd), 1, 250, None);
        }
        WM_TIMER => {
            // belongs to the app it was opened for: gone when that app loses the focus
            let t = HWND(ST.with(|st| st.borrow().target) as *mut c_void);
            if !IsWindow(Some(t)).as_bool() || GetForegroundWindow() != t {
                hide(hwnd);
            }
        }
        WM_EMOJI_KEY => {
            let pick = ST.with(|st| {
                let mut st = st.borrow_mut();
                let n = st.shown.len();
                if n == 0 {
                    return None;
                }
                // move, skipping the empty cells that pad the recently-used row
                let shown = st.shown.clone();
                let step = |sel: usize, d: isize| {
                    let mut i = (sel as isize + d).clamp(0, n as isize - 1) as usize;
                    while shown[i] == 0 && i + 1 < n && i > 0 {
                        i = if d > 0 { i + 1 } else { i - 1 };
                    }
                    if shown[i] == 0 { sel } else { i }
                };
                match VIRTUAL_KEY(wp.0 as u16) {
                    VK_LEFT => st.sel = step(st.sel, -1),
                    VK_RIGHT => st.sel = step(st.sel, 1),
                    VK_UP => st.sel = step(st.sel, -(COLS as isize)),
                    VK_DOWN => st.sel = step(st.sel, COLS as isize),
                    VK_RETURN => return Some(st.shown[st.sel]),
                    _ => {}
                }
                keep_visible(&mut st);
                None
            });
            if wp.0 as u16 == VK_ESCAPE.0 {
                hide(hwnd);
            } else if let Some(cp) = pick {
                send(cp);
                save_recent(&used(&load_recent(), cp));
            }
            redraw();
        }
        crate::flyout::WM_DISMISS => hide(hwnd),
        WM_MOUSEMOVE => {
            let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            let (x, y) = pos(lp);
            if ST.with(|st| {
                let mut st = st.borrow_mut();
                let now = (cell_at(&st, x, y), cat_at(x, y));
                std::mem::replace(&mut st.hover, now.0) != now.0 || std::mem::replace(&mut st.hover_cat, now.1) != now.1
            }) {
                redraw();
            }
        }
        WM_MOUSELEAVE => {
            ST.with(|st| {
                let mut st = st.borrow_mut();
                st.hover = None;
                st.hover_cat = None;
            });
            redraw();
        }
        WM_MOUSEWHEEL => {
            ST.with(|st| {
                let mut st = st.borrow_mut();
                let max_top = (st.shown.len().div_ceil(COLS)).saturating_sub(ROWS);
                st.top = if ((wp.0 >> 16) & 0xFFFF) as i16 > 0 { st.top.saturating_sub(2) } else { (st.top + 2).min(max_top) };
                st.hover = None;
            });
            redraw();
        }
        WM_LBUTTONUP => {
            let (x, y) = pos(lp);
            let pick = ST.with(|st| {
                let mut st = st.borrow_mut();
                if let Some(row) = cat_at(x, y).and_then(|k| block_rows(&st.shown)[k]) {
                    // jump to that block
                    st.top = row.min((st.shown.len().div_ceil(COLS)).saturating_sub(ROWS));
                    st.sel = row * COLS;
                    return None;
                }
                let i = cell_at(&st, x, y)?;
                st.sel = i;
                Some(st.shown[i])
            });
            if let Some(cp) = pick {
                send(cp);
                save_recent(&used(&load_recent(), cp));
            }
            redraw();
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => paint(hwnd),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

/// Win+. / Win+; : open the panel for the window in front, or close it. Called from the keyboard hook.
pub unsafe fn toggle() {
    let p = HWND(EMOJI.load(Relaxed) as *mut c_void);
    if p.is_invalid() {
        return;
    }
    if OPEN.load(Relaxed) {
        let _ = PostMessageW(Some(p), WM_EMOJI_KEY, WPARAM(VK_ESCAPE.0 as usize), LPARAM(0));
    } else {
        let _ = PostMessageW(Some(p), WM_EMOJI_OPEN, WPARAM(0), LPARAM(GetForegroundWindow().0 as isize));
    }
}

/// While the panel is open: arrows move, Enter types the emoji, Esc closes. Returns true to swallow the key.
pub unsafe fn on_key(k: &KBDLLHOOKSTRUCT, up: bool) -> bool {
    if !OPEN.load(Relaxed) || k.dwExtraInfo == crate::launcher::MARK {
        return false;
    }
    if !matches!(VIRTUAL_KEY(k.vkCode as u16), VK_ESCAPE | VK_UP | VK_DOWN | VK_LEFT | VK_RIGHT | VK_RETURN) {
        return false;
    }
    if !up {
        let _ = PostMessageW(Some(HWND(EMOJI.load(Relaxed) as *mut c_void)), WM_EMOJI_KEY, WPARAM(k.vkCode as usize), LPARAM(0));
    }
    true
}

/// `--emoji-selftest`: how many emoji the system font offers the panel.
pub unsafe fn report() -> String {
    let r = font_ranges();
    let all = pick(&r);
    format!("font ranges: {}, emoji offered: {}, first: U+{:X}", r.len(), all.len(), all.first().copied().unwrap_or(0))
}

pub unsafe fn create(inst: HINSTANCE) -> windows::core::Result<()> {
    let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: inst, lpszClassName: w!("shellbits_emoji"), hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
    RegisterClassW(&wc);
    let h = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE, w!("shellbits_emoji"), w!("Emoji"), WS_POPUP, 0, 0, 10, 10, None, None, Some(inst), None)?;
    let (dark, round) = (1u32, 2u32);
    let _ = DwmSetWindowAttribute(h, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const c_void, 4);
    let _ = DwmSetWindowAttribute(h, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as *const c_void, 4);
    let _ = crate::fluent_window(h); // Windows' blurred panel material, like the other flyouts
    EMOJI.store(h.0 as isize, Relaxed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn picking_and_typing() {
        // a font covering digits, some faces, the skin tones and an old symbol
        let got = pick(&[(0x30, 0x39), (0x1F600, 0x1F602), (0x1F3FA, 0x1F3FC), (0x2764, 0x2764)]);
        assert_eq!(got, vec![0x1F600, 0x1F601, 0x1F602, 0x1F3FA, 0x2764]); // faces first; digits and skin tones left out
        assert_eq!(typed(0x1F600), vec![0xD83D, 0xDE00]); // one emoji = two UTF-16 units
        assert_eq!(typed(0x2764), vec![0x2764, 0xFE0F]); // old symbol + "show as emoji"
        // recently used: newest first, no repeats, one row at most; the grid pads that row with blanks
        assert_eq!(used(&[1, 2, 3], 2), vec![2, 1, 3]);
        assert_eq!(used(&[1, 2, 3, 4, 5, 6, 7, 8], 9).len(), COLS - 1);
        assert_eq!(grid(&[], &[5, 6]), vec![5, 6]);
        assert_eq!(grid(&[9], &[5, 6]), vec![9, 0, 0, 0, 0, 0, 0, 0, 5, 6]);
        assert_eq!(block_rows(&grid(&[0x1F601], &[0x1F600, 0x1F601]))[0], Some(1)); // faces start on the row after the recents
        let st = State { shown: (1..101).collect(), top: 2, ..Default::default() };
        assert_eq!(cell_at(&st, PAD + 1.0, HEAD + 1.0), Some(16));
        assert_eq!(cat_at(PAD + CELL + 1.0, TITLE + 1.0), Some(1));
        assert_eq!(cat_at(PAD + 1.0, HEAD + 1.0), None);
        assert_eq!(cell_at(&st, PAD + CELL * 7.0 + 1.0, HEAD + CELL + 1.0), Some(31));
        assert_eq!(cell_at(&st, 2.0, HEAD + 1.0), None);
    }
}

//! Notepad, laid out like Windows 11's: tab strip in the title bar, File / Edit / View row, the text,
//! and a status bar (line and column, character count, zoom, line ending, encoding).
//! The text itself is a standard Windows edit control, so typing, selection, undo and the clipboard
//! behave exactly as everywhere else.
//! ponytail: one document per window (no extra tabs, no session restore) and the plain edit control
//! (slow beyond a few MB); move to RichEdit and real tabs when that matters.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};

use windows::core::{w, HSTRING, PCWSTR};
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
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::flyout::{glyph, rect, rrect, utf16};
use crate::quick::rgb;

pub static WIN: AtomicIsize = AtomicIsize::new(0);

// ======================= text files =======================

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum Eol {
    #[default]
    Crlf,
    Lf,
    Cr,
}
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum Enc {
    #[default]
    Utf8,
    Utf8Bom,
    Utf16Le,
}
impl Eol {
    fn label(self) -> &'static str {
        ["Windows (CRLF)", "Unix (LF)", "Macintosh (CR)"][self as usize]
    }
    fn text(self) -> &'static str {
        ["\r\n", "\n", "\r"][self as usize]
    }
}
impl Enc {
    fn label(self) -> &'static str {
        ["UTF-8", "UTF-8 with BOM", "UTF-16 LE"][self as usize]
    }
}

/// A file's bytes as text, and how it was encoded.
pub fn decode(b: &[u8]) -> (String, Enc) {
    match b {
        [0xEF, 0xBB, 0xBF, rest @ ..] => (String::from_utf8_lossy(rest).into_owned(), Enc::Utf8Bom),
        [0xFF, 0xFE, rest @ ..] => (String::from_utf16_lossy(&rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()), Enc::Utf16Le),
        _ => (String::from_utf8_lossy(b).into_owned(), Enc::Utf8),
    }
}

pub fn encode(s: &str, enc: Enc) -> Vec<u8> {
    match enc {
        Enc::Utf8 => s.as_bytes().to_vec(),
        Enc::Utf8Bom => [&[0xEF, 0xBB, 0xBF], s.as_bytes()].concat(),
        Enc::Utf16Le => [0xFF, 0xFE].into_iter().chain(s.encode_utf16().flat_map(u16::to_le_bytes)).collect(),
    }
}

/// The line ending a text uses (its first one; Windows' if it has none).
pub fn eol_of(s: &str) -> Eol {
    match s.find(['\r', '\n']).map(|i| &s[i..]) {
        Some(r) if r.starts_with("\r\n") => Eol::Crlf,
        Some(r) if r.starts_with('\n') => Eol::Lf,
        Some(_) => Eol::Cr,
        None => Eol::Crlf,
    }
}

/// Any mix of line endings -> `eol` everywhere. (The edit control wants CRLF; files get theirs back on save.)
pub fn with_eol(s: &str, eol: Eol) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n").replace('\n', eol.text())
}

/// 1-based line and column of the caret, and the character count, for text as the edit control holds it
/// (UTF-16, CRLF). A line break counts as one character.
pub fn position(text: &[u16], caret: usize) -> (usize, usize, usize) {
    let before = &text[..caret.min(text.len())];
    let line = before.iter().filter(|c| **c == 10).count() + 1;
    let col = before.len() - before.iter().rposition(|c| *c == 10).map_or(0, |i| i + 1) + 1;
    (line, col, text.iter().filter(|c| **c != 13).count())
}

/// Where `needle` next occurs in `text`, ignoring letter case: forward from `from`, or backward
/// ending before `from`; the search wraps around the end of the text. Both are UTF-16.
/// ponytail: case folding is per UTF-16 unit (fine for everything but a few special letters).
pub fn find(text: &[u16], needle: &[u16], from: usize, forward: bool) -> Option<usize> {
    let (t, n) = (folded(text), folded(needle));
    if n.is_empty() || n.len() > t.len() {
        return None;
    }
    let last = t.len() - n.len();
    let at = |i: &usize| t[*i..*i + n.len()] == n[..];
    if forward {
        (from.min(last + 1)..=last).chain(0..from.min(last + 1)).find(at)
    } else {
        (0..from.min(last + 1)).rev().chain((from.min(last + 1)..=last).rev()).find(at)
    }
}

/// Lower-cased copy, unit by unit (so positions stay the same).
fn folded(t: &[u16]) -> Vec<u16> {
    t.iter().map(|c| char::from_u32(*c as u32).and_then(|c| c.to_lowercase().next()).filter(|l| (*l as u32) < 0x10000).map_or(*c, |l| l as u16)).collect()
}

/// Every occurrence of `needle` (letter case ignored, no overlaps) replaced by `with`; also how many.
pub fn replace_all(text: &[u16], needle: &[u16], with: &[u16]) -> (Vec<u16>, usize) {
    let (t, n) = (folded(text), folded(needle));
    let (mut out, mut i, mut count) = (Vec::with_capacity(text.len()), 0, 0);
    while i < text.len() {
        if !n.is_empty() && t[i..].starts_with(&n) {
            out.extend_from_slice(with);
            i += n.len();
            count += 1;
        } else {
            out.push(text[i]);
            i += 1;
        }
    }
    (out, count)
}

/// Where line `n` (1-based) starts in the text, if it has that many lines.
pub fn line_start(text: &[u16], n: usize) -> Option<usize> {
    match n {
        0 => None,
        1 => Some(0),
        _ => text.iter().enumerate().filter(|(_, c)| **c == 10).nth(n - 2).map(|(i, _)| i + 1),
    }
}

/// What is remembered between runs: zoom percent, word wrap, status bar ("zoom=120 wrap=0 status=1").
pub fn parse_prefs(s: &str) -> (i32, bool, bool) {
    let get = |key: &str, default: i32| s.split_whitespace().find_map(|t| t.strip_prefix(key)).and_then(|v| v.parse().ok()).unwrap_or(default);
    (get("zoom=", 100).clamp(10, 500), get("wrap=", 1) != 0, get("status=", 1) != 0)
}
fn prefs_file() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join("shellbits").join("notepad.txt")
}
fn save_prefs() {
    let Some((zoom, wrap, status)) = ST.with(|st| st.borrow().as_ref().map(|s| (s.zoom, s.wrap, s.status))) else { return };
    let f = prefs_file();
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(f, format!("zoom={zoom} wrap={} status={}", wrap as u8, status as u8));
}

fn thousands(n: usize) -> String {
    crate::calc::grouped(&n.to_string())
}

// ======================= window =======================

const TABS: f32 = 42.0; // title bar with the tab strip
const MENU: f32 = 33.0; // File / Edit / View
const STATUS: f32 = 33.0;
const TAB_W: f32 = 200.0;
const MENUS: [(&str, f32, f32); 3] = [("File", 4.0, 48.0), ("Edit", 52.0, 100.0), ("View", 104.0, 156.0)];
const BASE_FONT: i32 = 15; // Consolas 11pt at 100%

// command ids (menus, shortcuts)
const NEW: usize = 1;
const OPEN: usize = 2;
const SAVE: usize = 3;
const SAVE_AS: usize = 4;
const EXIT: usize = 5;
const UNDO: usize = 10;
const CUT: usize = 11;
const COPY: usize = 12;
const PASTE: usize = 13;
const DELETE: usize = 14;
const SELECT_ALL: usize = 15;
const ZOOM_IN: usize = 20;
const ZOOM_OUT: usize = 21;
const ZOOM_RESET: usize = 22;
const STATUS_BAR: usize = 23;
const WRAP: usize = 24;
const FIND: usize = 30;
const FIND_NEXT: usize = 31;
const FIND_PREV: usize = 32;
const FIND_CLOSE: usize = 33;
const REPLACE: usize = 34;
const REPLACE_ONE: usize = 35;
const REPLACE_ALL: usize = 36;
const GOTO: usize = 37;
const ROW2: f32 = 38.0; // the replace row under the find row
const FIND_H: f32 = 44.0; // the find bar, between the menu row and the text
const FIND_W: f32 = 260.0; // its text field

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Menu(usize),
    Min,
    Max,
    Close,
    TabClose,
    FindNext,
    FindPrev,
    FindClose,
    ReplaceOne,
    ReplaceAll,
}

struct State {
    edit: HWND,
    find: HWND,       // the find bar's text field (hidden until Ctrl+F)
    repl: HWND,       // the "replace with" field (Ctrl+H)
    mode: u8,         // what the bar is for: 0 find, 1 find and replace, 2 go to line
    note: String,     // shown in the bar: "3 replaced", "No such line"
    finding: bool,    // the find bar is open
    find_miss: bool,  // the last search found nothing
    font: HFONT,
    bg: HBRUSH,
    path: Option<String>,
    eol: Eol,
    enc: Enc,
    dirty: bool,
    zoom: i32, // percent
    status: bool,
    wrap: bool, // long lines wrap at the window edge (else a horizontal scroll bar)
    hover: Option<Hit>,
    pos: (usize, usize, usize), // line, column, characters
    gfx: Option<(ID2D1DCRenderTarget, IDWriteFactory, Option<ID2D1Bitmap>)>,
}

thread_local! {
    static ST: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn name(path: &Option<String>) -> String {
    path.as_ref().and_then(|p| std::path::Path::new(p).file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Untitled".into())
}

unsafe fn edit_text(edit: HWND) -> Vec<u16> {
    let n = GetWindowTextLengthW(edit) as usize;
    let mut buf = vec![0u16; n + 1];
    let got = GetWindowTextW(edit, &mut buf) as usize;
    buf.truncate(got);
    buf
}

unsafe fn retitle(hwnd: HWND) {
    let (title, _) = ST.with(|s| s.borrow().as_ref().map(|st| (format!("{}{} - Notepad", if st.dirty { "*" } else { "" }, name(&st.path)), ())).unwrap_or_default());
    let _ = SetWindowTextW(hwnd, &HSTRING::from(title));
    let _ = InvalidateRect(Some(hwnd), None, false);
}

/// Put the edit control between the menu row and the status bar, with its font at the current zoom.
unsafe fn arrange(hwnd: HWND) {
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let Some((edit, find, repl, finding, mode, status)) = ST.with(|st| st.borrow().as_ref().map(|s| (s.edit, s.find, s.repl, s.finding, s.mode, s.status))) else { return };
    let bar = if !finding { 0.0 } else if mode == 1 { FIND_H + ROW2 } else { FIND_H };
    let top = ((TABS + MENU + bar) * s) as i32;
    let bottom = rc.bottom - if status { (STATUS * s) as i32 } else { 0 };
    let _ = SetWindowPos(edit, None, 1, top, rc.right - 2, (bottom - top).max(0), SWP_NOZORDER);
    let f = find_field(rc.right as f32 / s);
    let place = |h: HWND, dy: f32, show: bool| {
        let _ = SetWindowPos(h, None, ((f.left + 8.0) * s) as i32, ((f.top + dy + 6.0) * s) as i32, ((f.right - f.left - 16.0) * s) as i32, ((f.bottom - f.top - 12.0) * s) as i32, SWP_NOZORDER | if show { SWP_SHOWWINDOW } else { SWP_HIDEWINDOW });
    };
    place(find, 0.0, finding);
    place(repl, ROW2, finding && mode == 1);
}

/// The find bar's field and its three buttons (next, previous, close), right-aligned.
fn find_field(w: f32) -> D2D_RECT_F {
    rect(w - FIND_W - 124.0, TABS + MENU + 6.0, w - 124.0, TABS + MENU + FIND_H - 6.0)
}
fn find_button(w: f32, k: usize) -> D2D_RECT_F {
    let x = w - 116.0 + k as f32 * 36.0;
    rect(x, TABS + MENU + 6.0, x + 32.0, TABS + MENU + FIND_H - 6.0)
}

fn replace_button(w: f32, k: usize) -> D2D_RECT_F {
    let (x0, x1) = [(w - 116.0, w - 58.0), (w - 54.0, w - 12.0)][k];
    rect(x0, TABS + MENU + ROW2 + 6.0, x1, TABS + MENU + ROW2 + FIND_H - 6.0)
}

/// Search from the selection for the find bar's text and select the match.
unsafe fn search(hwnd: HWND, forward: bool) {
    // nothing of ours is borrowed while the edit control is driven: it can call back into proc
    let Some((edit, field)) = ST.with(|st| st.borrow().as_ref().map(|s| (s.edit, s.find))) else { return };
    let (mut a, mut b) = (0u32, 0u32);
    SendMessageW(edit, EM_GETSEL, Some(WPARAM(&mut a as *mut _ as usize)), Some(LPARAM(&mut b as *mut _ as isize)));
    let needle = edit_text(field);
    let goto = ST.with(|st| st.borrow().as_ref().is_some_and(|s| s.mode == 2));
    let hit = if goto {
        // "go to line": the field holds a line number
        String::from_utf16_lossy(&needle).trim().parse().ok().and_then(|n| line_start(&edit_text(edit), n)).map(|i| (i, i))
    } else {
        // forward: after the current match; backward: before it
        find(&edit_text(edit), &needle, if forward { b as usize } else { a as usize }, forward).map(|i| (i, i + needle.len()))
    };
    if let Some((from, to)) = hit {
        SendMessageW(edit, EM_SETSEL, Some(WPARAM(from)), Some(LPARAM(to as isize)));
        SendMessageW(edit, EM_SCROLLCARET, None, None);
    }
    ST.with(|st| {
        if let Some(st) = st.borrow_mut().as_mut() {
            st.find_miss = hit.is_none(); // the bar then says so
            st.note = if hit.is_some() { String::new() } else if goto { "No such line".into() } else { "No results".into() };
        }
    });
    if goto && hit.is_some() {
        command(hwnd, FIND_CLOSE); // the caret is on the line: back to the text
        return;
    }
    refresh_position();
    let _ = InvalidateRect(Some(hwnd), None, false);
}

/// Replace the selected match (or everything, with `all`) with the replace field's text.
unsafe fn replace(hwnd: HWND, all: bool) {
    let Some((edit, field, repl)) = ST.with(|st| st.borrow().as_ref().map(|s| (s.edit, s.find, s.repl))) else { return };
    let (text, needle, mut with) = (edit_text(edit), edit_text(field), edit_text(repl));
    if needle.is_empty() {
        return;
    }
    with.push(0);
    let put = |from: usize, to: isize, new: &[u16]| {
        // through the control's own "replace selection", so Ctrl+Z undoes it
        SendMessageW(edit, EM_SETSEL, Some(WPARAM(from)), Some(LPARAM(to)));
        SendMessageW(edit, EM_REPLACESEL, Some(WPARAM(1)), Some(LPARAM(new.as_ptr() as isize)));
    };
    if all {
        let (mut new, count) = replace_all(&text, &needle, &with[..with.len() - 1]);
        if count > 0 {
            new.push(0);
            put(0, -1, &new);
        }
        ST.with(|st| {
            if let Some(st) = st.borrow_mut().as_mut() {
                (st.find_miss, st.note) = (count == 0, if count == 0 { "No results".into() } else { format!("{} replaced", thousands(count)) });
            }
        });
        refresh_position();
        let _ = InvalidateRect(Some(hwnd), None, false);
        return;
    }
    let (mut a, mut b) = (0u32, 0u32);
    SendMessageW(edit, EM_GETSEL, Some(WPARAM(&mut a as *mut _ as usize)), Some(LPARAM(&mut b as *mut _ as isize)));
    let (a, b) = (a as usize, (b as usize).min(text.len()));
    if b > a && folded(&text[a..b]) == folded(&needle) {
        put(a, b as isize, &with);
    }
    search(hwnd, true); // on to the next match
}

unsafe fn set_font(hwnd: HWND) {
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    let Some((edit, zoom)) = ST.with(|st| st.borrow().as_ref().map(|s| (s.edit, s.zoom))) else { return };
    let px = (BASE_FONT as f32 * s * zoom as f32 / 100.0).round() as i32;
    let f = CreateFontW(-px, 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Consolas"));
    SendMessageW(edit, WM_SETFONT, Some(WPARAM(f.0 as usize)), Some(LPARAM(1)));
    SendMessageW(edit, EM_SETMARGINS, Some(WPARAM((EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize)), Some(LPARAM((((16.0 * s) as isize) << 16) | (16.0 * s) as isize)));
    let old = ST.with(|st| st.borrow_mut().as_mut().map(|s| std::mem::replace(&mut s.font, f)));
    if let Some(old) = old {
        let _ = DeleteObject(old.into());
    }
}

/// Line / column / character count from the edit control. Returns true if they changed.
/// ponytail: copies the whole text each time; fine for ordinary files.
unsafe fn refresh_position() -> bool {
    let Some(edit) = ST.with(|st| st.try_borrow().ok().and_then(|s| s.as_ref().map(|s| s.edit))) else { return false };
    let mut caret = 0u32;
    SendMessageW(edit, EM_GETSEL, None, Some(LPARAM(&mut caret as *mut _ as isize)));
    let p = position(&edit_text(edit), caret as usize);
    ST.with(|st| st.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(|s| std::mem::replace(&mut s.pos, p) != p)).unwrap_or(false))
}

/// Replace the text and what we know about its file. The control is filled with nothing borrowed
/// (it can call back into proc), and the document is marked unchanged afterwards.
unsafe fn set_document(text: &str, path: Option<String>, eol: Eol, enc: Enc) {
    let Some(edit) = ST.with(|st| st.borrow().as_ref().map(|s| s.edit)) else { return };
    let _ = SetWindowTextW(edit, &HSTRING::from(text));
    ST.with(|st| {
        if let Some(st) = st.borrow_mut().as_mut() {
            (st.path, st.eol, st.enc, st.dirty) = (path, eol, enc, false);
        }
    });
}

/// Load `path` into the window. An unreadable file leaves everything as it was.
unsafe fn load(hwnd: HWND, path: &str) {
    let Ok(bytes) = std::fs::read(path) else {
        MessageBoxW(Some(hwnd), &HSTRING::from(format!("Cannot open {path}")), w!("Notepad"), MB_OK | MB_ICONWARNING);
        return;
    };
    let (text, enc) = decode(&bytes);
    set_document(&with_eol(&text, Eol::Crlf), Some(path.to_string()), eol_of(&text), enc);
    refresh_position();
    retitle(hwnd);
}

/// Write the text to `path` in the document's own encoding and line endings. False if that failed.
unsafe fn write(hwnd: HWND, path: &str) -> bool {
    let ok = ST.with(|st| {
        let mut st = st.borrow_mut();
        let Some(st) = st.as_mut() else { return false };
        let text = with_eol(&String::from_utf16_lossy(&edit_text(st.edit)), st.eol);
        let ok = std::fs::write(path, encode(&text, st.enc)).is_ok();
        if ok {
            (st.path, st.dirty) = (Some(path.to_string()), false);
        }
        ok
    });
    if !ok {
        MessageBoxW(Some(hwnd), &HSTRING::from(format!("Cannot save {path}")), w!("Notepad"), MB_OK | MB_ICONWARNING);
    }
    retitle(hwnd);
    ok
}

/// Windows' own Open / Save dialog. None = cancelled.
unsafe fn pick(hwnd: HWND, save: bool, suggested: &str) -> Option<String> {
    let d: IFileDialog = if save { CoCreateInstance::<_, IFileSaveDialog>(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?.into() } else { CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?.into() };
    let types = [windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC { pszName: w!("Text documents (*.txt)"), pszSpec: w!("*.txt") }, windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC { pszName: w!("All files"), pszSpec: w!("*.*") }];
    let _ = d.SetFileTypes(&types);
    if save {
        let _ = d.SetFileName(&HSTRING::from(suggested));
        let _ = d.SetDefaultExtension(w!("txt"));
    }
    d.Show(Some(hwnd)).ok()?;
    let p = d.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
    let s = p.to_string().ok();
    CoTaskMemFree(Some(p.0 as *const c_void));
    s
}

unsafe fn save(hwnd: HWND, ask_name: bool) -> bool {
    let (path, title) = ST.with(|st| st.borrow().as_ref().map(|s| (s.path.clone(), name(&s.path))).unwrap_or_default());
    match path.filter(|_| !ask_name).or_else(|| pick(hwnd, true, &title)) {
        Some(p) => write(hwnd, &p),
        None => false,
    }
}

/// Before the text is thrown away: offer to save changes. False = the user cancelled.
unsafe fn may_discard(hwnd: HWND) -> bool {
    let (dirty, title) = ST.with(|st| st.borrow().as_ref().map(|s| (s.dirty, name(&s.path))).unwrap_or_default());
    if !dirty {
        return true;
    }
    match MessageBoxW(Some(hwnd), &HSTRING::from(format!("Do you want to save changes to {title}?")), w!("Notepad"), MB_YESNOCANCEL | MB_ICONQUESTION) {
        IDYES => save(hwnd, false),
        IDNO => true,
        _ => false,
    }
}

unsafe fn command(hwnd: HWND, id: usize) {
    let edit = ST.with(|st| st.borrow().as_ref().map(|s| s.edit)).unwrap_or_default();
    let to_edit = |m: u32, w: usize, l: isize| _ = SendMessageW(edit, m, Some(WPARAM(w)), Some(LPARAM(l)));
    match id {
        NEW => {
            if may_discard(hwnd) {
                set_document("", None, Eol::Crlf, Enc::Utf8);
                refresh_position();
                retitle(hwnd);
            }
        }
        OPEN => {
            if may_discard(hwnd) {
                if let Some(p) = pick(hwnd, false, "") {
                    load(hwnd, &p);
                }
            }
        }
        SAVE => _ = save(hwnd, false),
        SAVE_AS => _ = save(hwnd, true),
        EXIT => _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)),
        UNDO => to_edit(EM_UNDO, 0, 0),
        CUT => to_edit(WM_CUT, 0, 0),
        COPY => to_edit(WM_COPY, 0, 0),
        PASTE => to_edit(WM_PASTE, 0, 0),
        DELETE => to_edit(WM_CLEAR, 0, 0),
        SELECT_ALL => to_edit(EM_SETSEL, 0, -1),
        ZOOM_IN | ZOOM_OUT | ZOOM_RESET => {
            ST.with(|st| {
                if let Some(st) = st.borrow_mut().as_mut() {
                    st.zoom = match id {
                        ZOOM_IN => (st.zoom + 10).min(500),
                        ZOOM_OUT => (st.zoom - 10).max(10),
                        _ => 100,
                    };
                }
            });
            set_font(hwnd);
            save_prefs();
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        FIND | REPLACE | GOTO | FIND_CLOSE => {
            let mode = [FIND, REPLACE, GOTO].iter().position(|m| *m == id).map(|m| m as u8);
            let Some((edit, field, clear)) = ST.with(|st| {
                let mut st = st.borrow_mut();
                let st = st.as_mut()?;
                // the line-number field and the search field are one control: empty it when its meaning changes
                let clear = mode.is_some_and(|m| (m == 2) != (st.mode == 2)) || (mode.is_none() && st.mode == 2);
                (st.finding, st.find_miss, st.note) = (mode.is_some(), false, String::new());
                st.mode = mode.unwrap_or(st.mode);
                Some((st.edit, st.find, clear))
            }) else { return };
            if clear {
                let _ = SetWindowTextW(field, w!(""));
            }
            arrange(hwnd);
            let target = if mode.is_some() { field } else { edit };
            let _ = SetFocus(Some(target));
            if mode.is_some() {
                SendMessageW(field, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
            }
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        REPLACE_ONE => replace(hwnd, false),
        REPLACE_ALL => replace(hwnd, true),
        FIND_NEXT | FIND_PREV => {
            if ST.with(|st| st.borrow().as_ref().is_some_and(|s| GetWindowTextLengthW(s.find) == 0)) {
                return command(hwnd, FIND); // nothing to look for yet: open the bar
            }
            search(hwnd, id == FIND_NEXT);
        }
        WRAP => {
            let now = ST.with(|st| st.borrow().as_ref().is_some_and(|s| s.wrap));
            set_wrap(hwnd, !now);
            save_prefs();
        }
        STATUS_BAR => {
            ST.with(|st| {
                if let Some(st) = st.borrow_mut().as_mut() {
                    st.status = !st.status;
                }
            });
            arrange(hwnd);
            save_prefs();
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        _ => {}
    }
}

/// The drop-down under a menu-row word. Runs the chosen command.
unsafe fn open_menu(hwnd: HWND, which: usize) {
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    let (status, wrap) = ST.with(|st| st.borrow().as_ref().map(|s| (s.status, s.wrap))).unwrap_or((true, true));
    let items: &[(&str, usize)] = match which {
        0 => &[("New\tCtrl+N", NEW), ("Open\tCtrl+O", OPEN), ("Save\tCtrl+S", SAVE), ("Save as\tCtrl+Shift+S", SAVE_AS), ("", 0), ("Exit", EXIT)],
        1 => &[("Undo\tCtrl+Z", UNDO), ("", 0), ("Cut\tCtrl+X", CUT), ("Copy\tCtrl+C", COPY), ("Paste\tCtrl+V", PASTE), ("Delete\tDel", DELETE), ("", 0), ("Find\tCtrl+F", FIND), ("Find next\tF3", FIND_NEXT), ("Find previous\tShift+F3", FIND_PREV), ("Replace\tCtrl+H", REPLACE), ("Go to\tCtrl+G", GOTO), ("", 0), ("Select all\tCtrl+A", SELECT_ALL)],
        _ => &[("Zoom in\tCtrl+Plus", ZOOM_IN), ("Zoom out\tCtrl+Minus", ZOOM_OUT), ("Restore default zoom\tCtrl+0", ZOOM_RESET), ("", 0), ("Word wrap", WRAP), ("Status bar", STATUS_BAR)],
    };
    let Ok(m) = CreatePopupMenu() else { return };
    for (label, id) in items {
        if label.is_empty() {
            let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
        } else {
            let wide = utf16(&format!("{label}\0"));
            let _ = AppendMenuW(m, if (*id == STATUS_BAR && status) || (*id == WRAP && wrap) { MF_STRING | MF_CHECKED } else { MF_STRING }, *id, PCWSTR(wide.as_ptr()));
        }
    }
    let mut pt = POINT { x: (MENUS[which].1 * s) as i32, y: ((TABS + MENU - 2.0) * s) as i32 };
    let _ = ClientToScreen(hwnd, &mut pt);
    let id = TrackPopupMenu(m, TPM_RETURNCMD | TPM_NONOTIFY, pt.x, pt.y, None, hwnd, None).0;
    let _ = DestroyMenu(m);
    if id > 0 {
        command(hwnd, id as usize);
    }
}

fn hit(x: f32, y: f32, w: f32, finding: bool) -> Option<Hit> {
    if finding {
        let inside = |r: D2D_RECT_F| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
        if let Some(k) = (0..3).find(|k| inside(find_button(w, *k))) {
            return Some([Hit::FindNext, Hit::FindPrev, Hit::FindClose][k]);
        }
        // the replace row (it is only on screen in replace mode; otherwise the text covers this spot)
        if let Some(k) = (0..2).find(|k| inside(replace_button(w, *k))) {
            return Some([Hit::ReplaceOne, Hit::ReplaceAll][k]);
        }
    }
    if y < 32.0 && x >= w - 138.0 {
        return Some([Hit::Min, Hit::Max, Hit::Close][(((x - (w - 138.0)) / 46.0) as usize).min(2)]);
    }
    if (14.0..38.0).contains(&y) && (48.0 + TAB_W - 32.0..48.0 + TAB_W - 6.0).contains(&x) {
        return Some(Hit::TabClose);
    }
    if (TABS..TABS + MENU).contains(&y) {
        return MENUS.iter().position(|m| x >= m.1 && x < m.2).map(Hit::Menu);
    }
    None
}

unsafe fn draw(st: &State, w: f32, h: f32, maximized: bool) -> windows::core::Result<()> {
    let Some((rt, dw, icon)) = &st.gfx else { return Ok(()) };
    rt.Clear(Some(&rgb(0x0A0A0A)));
    let b = |v: u32| rt.CreateSolidColorBrush(&rgb(v), None);
    let font = |name: PCWSTR, size: f32, align| -> windows::core::Result<IDWriteTextFormat> {
        let t = dw.CreateTextFormat(name, None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))?;
        t.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        t.SetTextAlignment(align)?;
        t.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
        t.SetTrimming(&trim, None)?;
        Ok(t)
    };
    let (ui, icons) = (w!("Segoe UI Variable Text"), w!("Segoe Fluent Icons"));
    let (lead, mid) = (DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_CENTER);
    let text = |s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, br: &ID2D1SolidColorBrush| rt.DrawText(&utf16(s), f, &r, br, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
    let ico = |cp: u32, f: &IDWriteTextFormat, r: D2D_RECT_F, br: &ID2D1SolidColorBrush| rt.DrawText(&glyph(cp), f, &r, br, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
    let (white, dim) = (b(0xFFFFFF)?, b(0xC7C7C7)?);
    let (small, body, icon10, icon8) = (font(ui, 12.0, lead)?, font(ui, 14.0, lead)?, font(icons, 10.0, mid)?, font(icons, 8.0, mid)?);

    // title bar: app icon, the document's tab, new-tab plus, caption buttons
    if let Some(i) = icon {
        rt.DrawBitmap(i, Some(&rect(16.0, 16.0, 32.0, 32.0)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
    }
    let tab = rect(48.0, 10.0, 48.0 + TAB_W, TABS + 4.0);
    rt.FillRoundedRectangle(&rrect(tab, 6.0), &b(0x1F1F1F)?);
    text(&name(&st.path), &small, rect(tab.left + 12.0, 10.0, tab.right - 36.0, TABS), &white);
    if st.hover == Some(Hit::TabClose) {
        rt.FillRoundedRectangle(&rrect(rect(tab.right - 32.0, 14.0, tab.right - 6.0, 38.0), 4.0), &b(0x2D2D2D)?);
    }
    if st.dirty && st.hover != Some(Hit::TabClose) {
        rt.FillEllipse(&D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: tab.right - 19.0, Y: 26.0 }, radiusX: 3.0, radiusY: 3.0 }, &dim); // unsaved changes
    } else {
        ico(0xE8BB, &icon8, rect(tab.right - 32.0, 14.0, tab.right - 6.0, 38.0), &white);
    }
    ico(0xE710, &icon10, rect(tab.right + 6.0, 10.0, tab.right + 38.0, TABS), &dim);
    for (k, (cp, which)) in [(0xE921u32, Hit::Min), (if maximized { 0xE923 } else { 0xE922 }, Hit::Max), (0xE8BB, Hit::Close)].into_iter().enumerate() {
        let r = rect(w - 138.0 + k as f32 * 46.0, 0.0, w - 92.0 + k as f32 * 46.0, 32.0);
        if st.hover == Some(which) {
            rt.FillRectangle(&r, &b(if which == Hit::Close { 0xC42B1C } else { 0x2D2D2D })?);
        }
        ico(cp, &icon10, rect(r.left - 2.0, r.top, r.right, r.bottom), &white);
    }

    // menu row, with the settings gear at its right end
    rt.FillRectangle(&rect(0.0, TABS, w, TABS + MENU), &b(0x1F1F1F)?);
    for (i, (label, x0, x1)) in MENUS.iter().enumerate() {
        if st.hover == Some(Hit::Menu(i)) {
            rt.FillRoundedRectangle(&rrect(rect(*x0, TABS + 2.0, *x1, TABS + MENU - 3.0), 4.0), &b(0x2D2D2D)?);
        }
        text(label, &body, rect(x0 + 11.0, TABS, *x1, TABS + MENU - 2.0), &white);
    }
    ico(0xE713, &font(icons, 16.0, mid)?, rect(w - 34.0, TABS, w, TABS + MENU - 1.0), &white);
    rt.FillRectangle(&rect(0.0, TABS + MENU - 1.0, w, TABS + MENU), &b(0x272727)?);
    rt.FillRectangle(&rect(0.0, TABS + MENU, w, h), &b(0x272727)?); // behind the edit control's edges

    // find bar: field, then next / previous / close; in replace mode a second row; in go-to mode a line number
    if st.finding {
        let bar = if st.mode == 1 { FIND_H + ROW2 } else { FIND_H };
        rt.FillRectangle(&rect(0.0, TABS + MENU, w, TABS + MENU + bar), &b(0x1F1F1F)?);
        let f = find_field(w);
        let field = |dy: f32, miss: bool| -> windows::core::Result<()> {
            rt.FillRoundedRectangle(&rrect(rect(f.left, f.top + dy, f.right, f.bottom + dy), 4.0), &b(0x272727)?);
            rt.FillRectangle(&rect(f.left + 2.0, f.bottom + dy - 1.0, f.right - 2.0, f.bottom + dy), &b(if miss { 0xFF99A4 } else { 0x9A9A9A })?);
            Ok(())
        };
        field(0.0, st.find_miss)?;
        let label = |s: &str, dy: f32, colour: u32| -> windows::core::Result<()> {
            let fmt = font(ui, 12.0, DWRITE_TEXT_ALIGNMENT_TRAILING)?;
            text(s, &fmt, rect(f.left - 160.0, f.top + dy, f.left - 10.0, f.bottom + dy), &b(colour)?);
            Ok(())
        };
        match (st.note.is_empty(), st.mode) {
            (false, _) => label(&st.note, 0.0, if st.find_miss { 0xFF99A4 } else { 0xC7C7C7 })?,
            (true, 2) => label("Go to line", 0.0, 0xC7C7C7)?,
            (true, 1) => label("Find", 0.0, 0xC7C7C7)?,
            _ => {}
        }
        for (k, (cp, which)) in [(0xE74Bu32, Hit::FindNext), (0xE74A, Hit::FindPrev), (0xE8BB, Hit::FindClose)].into_iter().enumerate() {
            let r = find_button(w, k);
            if st.hover == Some(which) {
                rt.FillRoundedRectangle(&rrect(r, 4.0), &b(0x2D2D2D)?);
            }
            ico(cp, &icon10, r, &white);
        }
        if st.mode == 1 {
            field(ROW2, false)?;
            label("Replace with", ROW2, 0xC7C7C7)?;
            let centred = font(ui, 12.0, mid)?;
            for (k, (name, which)) in [("Replace", Hit::ReplaceOne), ("All", Hit::ReplaceAll)].into_iter().enumerate() {
                let r = replace_button(w, k);
                rt.FillRoundedRectangle(&rrect(r, 4.0), &b(if st.hover == Some(which) { 0x3A3A3A } else { 0x2D2D2D })?);
                text(name, &centred, r, &white);
            }
        }
    }

    // status bar
    if st.status {
        let y = h - STATUS;
        rt.FillRectangle(&rect(0.0, y + 1.0, w, h), &b(0x1F1F1F)?);
        let cell = |s: &str, x: f32| text(s, &small, rect(x, y, x + 220.0, h - 1.0), &white);
        cell(&format!("Ln {}, Col {}", thousands(st.pos.0), thousands(st.pos.1)), 24.0);
        cell(&format!("{} character{}", thousands(st.pos.2), if st.pos.2 == 1 { "" } else { "s" }), 115.0);
        cell(&format!("{}%", st.zoom), w - 368.0);
        cell(st.eol.label(), w - 287.0);
        cell(st.enc.label(), w - 133.0);
        for x in [103.0, w - 380.0, w - 299.0, w - 145.0] {
            rt.FillRectangle(&rect(x, y + 9.0, x + 1.0, h - 8.0), &b(0x3A3A3A)?);
        }
    }
    Ok(())
}

/// The text control. Word wrap is fixed when an edit control is made, so switching it means making a new one.
unsafe fn make_edit(parent: HWND, wrap: bool) -> Option<HWND> {
    let across = if wrap { WINDOW_STYLE(0) } else { WS_HSCROLL | WINDOW_STYLE(ES_AUTOHSCROLL as u32) };
    let style = WS_CHILD | WS_VISIBLE | WS_VSCROLL | across | WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL | ES_NOHIDESEL | ES_WANTRETURN) as u32);
    let edit = CreateWindowExW(WINDOW_EX_STYLE(0), w!("EDIT"), w!(""), style, 0, 0, 10, 10, Some(parent), None, None, None).ok()?;
    SendMessageW(edit, EM_SETLIMITTEXT, Some(WPARAM(0)), None); // no 30,000-character cap
    let _ = SetWindowTheme(edit, w!("DarkMode_Explorer"), PCWSTR::null()); // dark scroll bars
    let _ = SetWindowSubclass(edit, Some(edit_proc), 1, 0);
    Some(edit)
}

/// Switch word wrap: a new text control takes over the text and the caret; unsaved state is unchanged.
/// (The undo history does not carry over.)
unsafe fn set_wrap(hwnd: HWND, wrap: bool) {
    let Some(old) = ST.with(|st| st.borrow().as_ref().map(|s| s.edit)) else { return };
    let Some(new) = make_edit(hwnd, wrap) else { return };
    let (mut a, mut b) = (0u32, 0u32);
    SendMessageW(old, EM_GETSEL, Some(WPARAM(&mut a as *mut _ as usize)), Some(LPARAM(&mut b as *mut _ as isize)));
    let mut text = edit_text(old);
    text.push(0);
    let _ = SetWindowTextW(new, PCWSTR(text.as_ptr()));
    ST.with(|st| {
        if let Some(st) = st.borrow_mut().as_mut() {
            (st.edit, st.wrap) = (new, wrap);
        }
    });
    let _ = DestroyWindow(old);
    set_font(hwnd);
    arrange(hwnd);
    SendMessageW(new, EM_SETSEL, Some(WPARAM(a as usize)), Some(LPARAM(b as isize)));
    SendMessageW(new, EM_SCROLLCARET, None, None);
    let _ = SetFocus(Some(new));
    let _ = InvalidateRect(Some(hwnd), None, false);
}

/// Ctrl shortcuts typed into the text go to the window as commands.
unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, id: usize, _data: usize) -> LRESULT {
    let post = |cmd: usize| _ = PostMessageW(GetParent(hwnd).ok(), WM_COMMAND, WPARAM(cmd), LPARAM(0));
    let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
    let in_find = id >= 2; // 2 = the find / line-number field, 3 = the replace field
    if msg == WM_KEYDOWN {
        match VIRTUAL_KEY(wp.0 as u16) {
            VK_F3 => return { post(if shift { FIND_PREV } else { FIND_NEXT }); LRESULT(0) },
            VK_RETURN if id == 3 => return { post(REPLACE_ONE); LRESULT(0) },
            VK_RETURN if in_find => return { post(if shift { FIND_PREV } else { FIND_NEXT }); LRESULT(0) },
            VK_ESCAPE if in_find => return { post(FIND_CLOSE); LRESULT(0) },
            _ => {}
        }
    }
    if msg == WM_CHAR && in_find && matches!(wp.0, 13 | 27) {
        return LRESULT(0); // Enter and Esc in the find field: handled above, no beep
    }
    if msg == WM_KEYDOWN && GetKeyState(VK_CONTROL.0 as i32) < 0 && GetKeyState(VK_MENU.0 as i32) >= 0 {
        let id = match (wp.0 as u8, shift) {
            (b'F', false) => FIND,
            (b'H', false) => REPLACE,
            (b'G', false) => GOTO,
            (b'N', false) => NEW,
            (b'O', false) => OPEN,
            (b'S', false) => SAVE,
            (b'S', true) => SAVE_AS,
            (0xBB | 0x6B, _) => ZOOM_IN,
            (0xBD | 0x6D, _) => ZOOM_OUT,
            (b'0', false) => ZOOM_RESET,
            _ => 0,
        };
        if id != 0 {
            let _ = PostMessageW(GetParent(hwnd).ok(), WM_COMMAND, WPARAM(id), LPARAM(0));
            return LRESULT(0);
        }
    }
    if msg == WM_CHAR && GetKeyState(VK_CONTROL.0 as i32) < 0 && matches!(wp.0, 14 | 15 | 19) {
        return LRESULT(0); // the control characters of Ctrl+N / O / S: no beep, nothing typed
    }
    if msg == WM_CHAR && GetKeyState(VK_CONTROL.0 as i32) < 0 && matches!(wp.0, 6 | 7 | 8) {
        return LRESULT(0); // Ctrl+F, Ctrl+G, Ctrl+H (whose character would otherwise act as Backspace)
    }
    DefSubclassProc(hwnd, msg, wp, lp)
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    let at = |lp: LPARAM| {
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let finding = ST.with(|st| st.borrow().as_ref().is_some_and(|s| s.finding));
        hit((lp.0 & 0xFFFF) as i16 as f32 / s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s, rc.right as f32 / s, finding)
    };
    match msg {
        WM_NCCALCSIZE if wp.0 == 1 => {
            if IsZoomed(hwnd).as_bool() {
                let p = &mut *(lp.0 as *mut NCCALCSIZE_PARAMS);
                let f = GetSystemMetrics(SM_CXSIZEFRAME) + GetSystemMetrics(SM_CXPADDEDBORDER);
                p.rgrc[0].left += f;
                p.rgrc[0].top += f;
                p.rgrc[0].right -= f;
                p.rgrc[0].bottom -= f;
            }
        }
        WM_NCHITTEST => {
            let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
            let _ = ScreenToClient(hwnd, &mut pt);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let e = (6.0 * s) as i32;
            let (l, t, r, bt) = (pt.x < e, pt.y < e, pt.x >= rc.right - e, pt.y >= rc.bottom - e);
            let on_button = hit(pt.x as f32 / s, pt.y as f32 / s, rc.right as f32 / s, false).is_some();
            let code = match (IsZoomed(hwnd).as_bool(), l, t, r, bt) {
                (false, true, true, _, _) => HTTOPLEFT,
                (false, _, true, true, _) => HTTOPRIGHT,
                (false, true, _, _, true) => HTBOTTOMLEFT,
                (false, _, _, true, true) => HTBOTTOMRIGHT,
                (false, true, ..) => HTLEFT,
                (false, _, true, ..) => HTTOP,
                (false, _, _, true, _) => HTRIGHT,
                (false, .., true) => HTBOTTOM,
                _ if pt.y < (TABS * s) as i32 && !on_button => HTCAPTION,
                _ => HTCLIENT,
            };
            return LRESULT(code as isize);
        }
        WM_GETMINMAXINFO => {
            let m = &mut *(lp.0 as *mut MINMAXINFO);
            m.ptMinTrackSize = POINT { x: (420.0 * s) as i32, y: (240.0 * s) as i32 };
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_SIZE => {
            arrange(hwnd);
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        WM_SETFOCUS => {
            let edit = ST.with(|st| st.borrow().as_ref().map(|s| s.edit)).unwrap_or_default();
            let _ = SetFocus(Some(edit));
        }
        WM_TIMER => {
            // the edit control does not announce caret moves: look a few times a second
            if refresh_position() {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
        WM_COMMAND => {
            if lp.0 != 0 {
                // from the edit control
                // a control's notice can arrive while we are in the middle of something: then it is skipped
                let Some(main) = ST.with(|st| st.try_borrow().ok().and_then(|s| s.as_ref().map(|s| s.edit.0 as isize == lp.0))) else { return LRESULT(0) };
                let changed = (wp.0 >> 16) as u32 == EN_CHANGE;
                if !main {
                    // typing in the find field: forget the last "No results"
                    if changed && ST.with(|st| st.try_borrow_mut().is_ok_and(|mut s| s.as_mut().is_some_and(|s| std::mem::replace(&mut s.find_miss, false) | !std::mem::take(&mut s.note).is_empty()))) {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                } else if changed {
                    let first = ST.with(|st| st.try_borrow_mut().is_ok_and(|mut s| s.as_mut().is_some_and(|s| !std::mem::replace(&mut s.dirty, true))));
                    refresh_position();
                    if first {
                        retitle(hwnd);
                    }
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            } else {
                command(hwnd, wp.0 & 0xFFFF);
            }
        }
        WM_CTLCOLOREDIT => {
            let dc = HDC(wp.0 as *mut c_void);
            SetTextColor(dc, COLORREF(0xFFFFFF));
            SetBkColor(dc, COLORREF(0x272727));
            return LRESULT(ST.with(|st| st.try_borrow().ok().and_then(|s| s.as_ref().map(|s| s.bg.0 as isize))).unwrap_or(0));
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            ST.with(|st| {
                let Ok(mut st) = st.try_borrow_mut() else { return };
                let Some(st) = st.as_mut() else { return };
                if st.gfx.is_none() {
                    st.gfx = make_gfx().ok();
                }
                let ok = st.gfx.as_ref().is_some_and(|(rt, ..)| {
                    if rt.BindDC(dc, &rc).is_err() {
                        return true;
                    }
                    rt.SetDpi(96.0 * s, 96.0 * s);
                    rt.BeginDraw();
                    let _ = draw(st, rc.right as f32 / s, rc.bottom as f32 / s, IsZoomed(hwnd).as_bool());
                    rt.EndDraw(None, None).is_ok()
                });
                if !ok {
                    st.gfx = None; // device lost: rebuilt on the next paint
                }
            });
            let _ = EndPaint(hwnd, &ps);
        }
        WM_MOUSEMOVE => {
            let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            let h = at(lp);
            if ST.with(|st| st.borrow_mut().as_mut().is_some_and(|s| std::mem::replace(&mut s.hover, h) != h)) {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
        WM_MOUSELEAVE => {
            ST.with(|st| {
                if let Some(s) = st.borrow_mut().as_mut() {
                    s.hover = None;
                }
            });
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        WM_LBUTTONUP => match at(lp) {
            Some(Hit::Menu(i)) => open_menu(hwnd, i),
            Some(Hit::Min) => _ = ShowWindow(hwnd, SW_MINIMIZE),
            Some(Hit::Max) => _ = ShowWindow(hwnd, if IsZoomed(hwnd).as_bool() { SW_RESTORE } else { SW_MAXIMIZE }),
            Some(Hit::Close | Hit::TabClose) => _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)),
            Some(Hit::FindNext) => command(hwnd, FIND_NEXT),
            Some(Hit::FindPrev) => command(hwnd, FIND_PREV),
            Some(Hit::FindClose) => command(hwnd, FIND_CLOSE),
            Some(Hit::ReplaceOne) => command(hwnd, REPLACE_ONE),
            Some(Hit::ReplaceAll) => command(hwnd, REPLACE_ALL),
            None => {}
        },
        WM_CLOSE => {
            if may_discard(hwnd) {
                let _ = DestroyWindow(hwnd);
            }
        }
        WM_DESTROY => {
            WIN.store(0, Relaxed);
            if let Some(st) = ST.with(|st| st.borrow_mut().take()) {
                let _ = DeleteObject(st.font.into());
                let _ = DeleteObject(st.bg.into());
            }
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

unsafe fn make_gfx() -> windows::core::Result<(ID2D1DCRenderTarget, IDWriteFactory, Option<ID2D1Bitmap>)> {
    let f: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
    let props = D2D1_RENDER_TARGET_PROPERTIES { pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }, ..Default::default() };
    let rt = f.CreateDCRenderTarget(&props)?;
    rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
    // Windows' own Notepad icon (none if the app is not installed)
    let icon = (|| -> windows::core::Result<ID2D1Bitmap> {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let px = crate::launcher::icon_pixels(&wic, r"shell:AppsFolder\Microsoft.WindowsNotepad_8wekyb3d8bbwe!App", 32)?;
        let props = D2D1_BITMAP_PROPERTIES { pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }, dpiX: 96.0, dpiY: 96.0 };
        rt.CreateBitmap(D2D_SIZE_U { width: 32, height: 32 }, Some(px.as_ptr() as *const c_void), 128, &props)
    })()
    .ok();
    Ok((rt, DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?, icon))
}

fn request_file() -> std::path::PathBuf {
    std::env::temp_dir().join("shellbits-notepad-open.txt")
}
/// From a second `shellbits --notepad <file>` process: leave the file name for the running one.
pub fn request(path: &str) {
    let _ = std::fs::write(request_file(), path);
}
pub fn take_request() -> Option<String> {
    let s = std::fs::read_to_string(request_file()).ok()?;
    let _ = std::fs::remove_file(request_file());
    let s = s.trim().trim_matches('"').to_string();
    (!s.is_empty()).then_some(s)
}

/// Open Notepad (or bring the open one forward), loading `path` if given. Built on demand, freed on close.
pub unsafe fn show(inst: HINSTANCE, path: Option<String>) {
    let mut h = HWND(WIN.load(Relaxed) as *mut c_void);
    if h.is_invalid() {
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: inst, lpszClassName: w!("shellbits_notepad"), hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
        RegisterClassW(&wc);
        let Ok(w_) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("shellbits_notepad"), w!("Untitled - Notepad"), WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN, CW_USEDEFAULT, CW_USEDEFAULT, 1140, 583, None, None, Some(inst), None) else { return };
        h = w_;
        // zoom, word wrap and the status bar come back as they were left
        let (zoom, wrap, status) = parse_prefs(&std::fs::read_to_string(prefs_file()).unwrap_or_default());
        let Some(edit) = make_edit(h, wrap) else {
            let _ = DestroyWindow(h);
            return;
        };
        let find = CreateWindowExW(WINDOW_EX_STYLE(0), w!("EDIT"), w!(""), WS_CHILD | WINDOW_STYLE(ES_AUTOHSCROLL as u32), 0, 0, 10, 10, Some(h), None, Some(inst), None).unwrap_or_default();
        SendMessageW(find, WM_SETFONT, Some(WPARAM(GetStockObject(DEFAULT_GUI_FONT).0 as usize)), Some(LPARAM(1)));
        let _ = SetWindowSubclass(find, Some(edit_proc), 2, 0);
        let repl = CreateWindowExW(WINDOW_EX_STYLE(0), w!("EDIT"), w!(""), WS_CHILD | WINDOW_STYLE(ES_AUTOHSCROLL as u32), 0, 0, 10, 10, Some(h), None, Some(inst), None).unwrap_or_default();
        SendMessageW(repl, WM_SETFONT, Some(WPARAM(GetStockObject(DEFAULT_GUI_FONT).0 as usize)), Some(LPARAM(1)));
        let _ = SetWindowSubclass(repl, Some(edit_proc), 3, 0);
        ST.with(|st| *st.borrow_mut() = Some(State { edit, find, repl, mode: 0, note: String::new(), finding: false, find_miss: false, font: HFONT::default(), bg: CreateSolidBrush(COLORREF(0x272727)), path: None, eol: Eol::Crlf, enc: Enc::Utf8, dirty: false, zoom, status, wrap, hover: None, pos: (1, 1, 0), gfx: None }));
        WIN.store(h.0 as isize, Relaxed);
        let dark = 1u32;
        let _ = DwmSetWindowAttribute(h, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const c_void, 4);
        let _ = DwmExtendFrameIntoClientArea(h, &MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 1, cyBottomHeight: 0 });
        let _ = SetWindowPos(h, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER);
        set_font(h);
        arrange(h);
        SetTimer(Some(h), 1, 200, None);
    }
    let _ = ShowWindow(h, if IsIconic(h).as_bool() { SW_RESTORE } else { SW_SHOW });
    crate::flyout::force_foreground(h);
    if let Some(p) = path {
        if may_discard(h) {
            load(h, &p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encodings_round_trip() {
        for (bytes, enc, text) in [(&b"plain \xc3\xa9"[..], Enc::Utf8, "plain é"), (&b"\xef\xbb\xbfbom"[..], Enc::Utf8Bom, "bom"), (&b"\xff\xfeh\0i\0"[..], Enc::Utf16Le, "hi")] {
            assert_eq!(decode(bytes), (text.to_string(), enc));
            assert_eq!(encode(text, enc), bytes);
        }
    }
    #[test]
    fn line_endings_survive_an_edit() {
        assert_eq!(eol_of("a\nb\r\nc"), Eol::Lf);
        assert_eq!(eol_of("a\r\nb"), Eol::Crlf);
        assert_eq!(eol_of("a\rb"), Eol::Cr);
        assert_eq!(eol_of("no breaks"), Eol::Crlf);
        // a Unix file goes into the edit control as CRLF and comes back out as LF
        let shown = with_eol("one\ntwo\n", Eol::Crlf);
        assert_eq!(shown, "one\r\ntwo\r\n");
        assert_eq!(with_eol(&shown, Eol::Lf), "one\ntwo\n");
        assert_eq!(with_eol("mixed\r\nends\rhere\n", Eol::Lf), "mixed\nends\nhere\n");
    }
    #[test]
    fn caret_position() {
        let t: Vec<u16> = "ab\r\ncde\r\n".encode_utf16().collect();
        assert_eq!(position(&t, 0), (1, 1, 7)); // 5 letters + 2 line breaks
        assert_eq!(position(&t, 2), (1, 3, 7));
        assert_eq!(position(&t, 4), (2, 1, 7));
        assert_eq!(position(&t, 7), (2, 4, 7));
        assert_eq!(position(&t, 99), (3, 1, 7)); // past the end = the end
        assert_eq!(thousands(14852), "14,852");
    }
    #[test]
    fn hits() {
        assert_eq!(hit(20.0, 58.0, 1140.0, false), Some(Hit::Menu(0)));
        assert_eq!(hit(120.0, 58.0, 1140.0, false), Some(Hit::Menu(2)));
        assert_eq!(hit(1120.0, 10.0, 1140.0, false), Some(Hit::Close));
        assert_eq!(hit(48.0 + TAB_W - 20.0, 26.0, 1140.0, false), Some(Hit::TabClose));
        assert_eq!(hit(500.0, 20.0, 1140.0, false), None); // empty title bar: drags the window
        // the find bar's buttons only exist while it is open
        assert_eq!(hit(1140.0 - 100.0, TABS + MENU + 20.0, 1140.0, true), Some(Hit::FindNext));
        assert_eq!(hit(1140.0 - 30.0, TABS + MENU + 20.0, 1140.0, true), Some(Hit::FindClose));
        assert_eq!(hit(1140.0 - 100.0, TABS + MENU + 20.0, 1140.0, false), None);
    }
    #[test]
    fn replacing_and_lines() {
        let u = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        assert_eq!(replace_all(&u("One two ONE two one"), &u("one"), &u("1")), (u("1 two 1 two 1"), 3));
        assert_eq!(replace_all(&u("aaaa"), &u("aa"), &u("b")), (u("bb"), 2)); // no overlaps
        assert_eq!(replace_all(&u("abc"), &u("x"), &u("y")), (u("abc"), 0));
        assert_eq!(replace_all(&u("abc"), &u(""), &u("y")), (u("abc"), 0));
        assert_eq!(replace_all(&u("a.b.c"), &u("."), &u("")), (u("abc"), 2)); // replace with nothing
        let t = u("ab\r\ncde\r\n\r\nlast");
        assert_eq!(line_start(&t, 1), Some(0));
        assert_eq!(line_start(&t, 2), Some(4));
        assert_eq!(line_start(&t, 3), Some(9));
        assert_eq!(line_start(&t, 4), Some(11));
        assert_eq!(line_start(&t, 5), None);
        assert_eq!(line_start(&t, 0), None);
    }
    #[test]
    fn preferences() {
        assert_eq!(parse_prefs(""), (100, true, true));
        assert_eq!(parse_prefs("zoom=120 wrap=0 status=1"), (120, false, true));
        assert_eq!(parse_prefs("zoom=9999 wrap=1 status=0"), (500, true, false));
        assert_eq!(parse_prefs("zoom=abc"), (100, true, true));
    }
    #[test]
    fn searching() {
        let u = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        let t = u("One two ONE two one");
        assert_eq!(find(&t, &u("one"), 0, true), Some(0));
        assert_eq!(find(&t, &u("one"), 3, true), Some(8)); // case is ignored
        assert_eq!(find(&t, &u("one"), 11, true), Some(16));
        assert_eq!(find(&t, &u("one"), 19, true), Some(0)); // wraps to the start
        assert_eq!(find(&t, &u("one"), 16, false), Some(8)); // backward: the match before
        assert_eq!(find(&t, &u("one"), 0, false), Some(16)); // wraps to the end
        assert_eq!(find(&t, &u("three"), 0, true), None);
        assert_eq!(find(&t, &u(""), 0, true), None);
        assert_eq!(find(&u("ab"), &u("abc"), 0, true), None);
        assert_eq!(find(&u("Éa é"), &u("é"), 1, true), Some(3));
    }
}

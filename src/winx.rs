//! Windows 11 Win+X power-user menu (+ its "Shut down or sign out" submenu) and the Run box (Win+R).
//! Menu: Win11 dark context-menu metrics (DIPs): 4px padding, 32px items, 9px separators, 14px text,
//! hover #3D3D3D rounded 4. Run: 400x200 at the bottom-left of the work area, real EDIT child for typing.

use std::cell::{Cell, RefCell};
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
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::flyout::{glyph, rect, rrect, utf16};
use crate::quick::rgb;

pub const WM_WINX_TOGGLE: u32 = WM_APP + 70; // wParam: 0 = anchored to Start, 1 = at the cursor
pub const WM_RUN_SHOW: u32 = WM_APP + 71;
const WM_WINX_REFOCUS: u32 = WM_APP + 72;
const WM_RUN_EXEC: u32 = WM_APP + 73; // wParam: 1 = as admin

pub static MENU: AtomicIsize = AtomicIsize::new(0);
pub static SUB: AtomicIsize = AtomicIsize::new(0);
pub static RUN: AtomicIsize = AtomicIsize::new(0);
static EDIT: AtomicIsize = AtomicIsize::new(0);
static LIST: AtomicIsize = AtomicIsize::new(0);
static EDIT_PROC: AtomicIsize = AtomicIsize::new(0);

fn hw(a: &AtomicIsize) -> HWND {
    HWND(a.load(Relaxed) as *mut c_void)
}

/// Posts a WM_WINX_TOGGLE / WM_RUN_SHOW to the menu window (safe from the hook threads).
pub fn post(msg: u32, wp: usize) {
    unsafe {
        let _ = PostMessageW(Some(hw(&MENU)), msg, WPARAM(wp), LPARAM(0));
    }
}

// ======================= menu model =======================

#[derive(Clone, Copy, PartialEq, Debug)]
enum Act {
    Sep,
    Open(&'static str, &'static str),
    Admin(&'static str),
    Search,
    Run,
    Power,
    Desktop,
    SignOut,
    Sleep,
    Shutdown,
    Restart,
}

type Entry = (&'static str, u8, Act); // label, access key, action

const SEP: Entry = ("", 0, Act::Sep);
const ITEMS: [Entry; 20] = [
    ("Installed apps", b'F', Act::Open("ms-settings:appsfeatures", "")),
    ("Mobility Center", b'B', Act::Open("mblctr.exe", "")),
    ("Power Options", b'O', Act::Open("control.exe", "powercfg.cpl")),
    ("Event Viewer", b'V', Act::Open("eventvwr.msc", "")),
    ("System", b'Y', Act::Open("ms-settings:about", "")),
    ("Device Manager", b'M', Act::Open("devmgmt.msc", "")),
    ("Network Connections", b'W', Act::Open("control.exe", "ncpa.cpl")),
    ("Disk Management", b'K', Act::Open("diskmgmt.msc", "")),
    ("Computer Management", b'G', Act::Open("compmgmt.msc", "")),
    ("Terminal", b'I', Act::Open("wt.exe", "")),
    ("Terminal (Admin)", b'A', Act::Admin("wt.exe")),
    SEP,
    ("Task Manager", b'T', Act::Open("taskmgr.exe", "")),
    ("Settings", b'N', Act::Open("ms-settings:", "")),
    ("File Explorer", b'E', Act::Open("explorer.exe", "")),
    ("Search", b'S', Act::Search),
    ("Run", b'R', Act::Run),
    SEP,
    ("Shut down or sign out", b'U', Act::Power),
    ("Desktop", b'D', Act::Desktop),
];
const POWER: [Entry; 4] = [
    ("Sign out", b'I', Act::SignOut),
    ("Sleep", b'S', Act::Sleep),
    ("Shut down", b'U', Act::Shutdown),
    ("Restart", b'R', Act::Restart),
];
const POWER_I: usize = 18;

/// Next selectable row in `dir` (+1 / -1), skipping separators, wrapping like Windows menus.
fn step(list: &[Entry], cur: Option<usize>, dir: i32) -> Option<usize> {
    let n = list.len() as i32;
    let mut i = cur.map_or(if dir > 0 { -1 } else { n }, |c| c as i32);
    for _ in 0..n {
        i = (i + dir).rem_euclid(n);
        if list[i as usize].2 != Act::Sep {
            return Some(i as usize);
        }
    }
    None
}

fn by_key(list: &[Entry], ch: u8) -> Option<usize> {
    list.iter().position(|e| e.1 == ch.to_ascii_uppercase() && e.2 != Act::Sep)
}

/// Selection state: `sub_sel` is Some only when the keyboard is inside the submenu.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
struct Nav {
    sel: Option<usize>,
    sub: bool,
    sub_sel: Option<usize>,
}

#[derive(PartialEq, Debug)]
enum Out {
    Nothing,
    Close,
    Do(Act),
}

fn enter(n: &mut Nav, i: usize) -> Out {
    n.sel = Some(i);
    if ITEMS[i].2 == Act::Power {
        n.sub = true;
        n.sub_sel = step(&POWER, None, 1);
        Out::Nothing
    } else {
        Out::Do(ITEMS[i].2)
    }
}

/// Keyboard: Up/Down/Left/Right/Enter/Space/Esc + access letters.
fn key(n: &mut Nav, vk: u16) -> Out {
    let in_sub = n.sub && n.sub_sel.is_some();
    match vk {
        0x26 | 0x28 => {
            let d = if vk == 0x28 { 1 } else { -1 };
            if in_sub {
                n.sub_sel = step(&POWER, n.sub_sel, d);
            } else {
                n.sel = step(&ITEMS, n.sel, d);
                n.sub = false;
            }
            Out::Nothing
        }
        0x27 if !in_sub && n.sel == Some(POWER_I) => enter(n, POWER_I),
        0x25 | 0x1B if n.sub => {
            n.sub = false;
            n.sub_sel = None;
            Out::Nothing
        }
        0x1B => Out::Close,
        0x0D | 0x20 if in_sub => Out::Do(POWER[n.sub_sel.unwrap()].2),
        0x0D | 0x20 => n.sel.map_or(Out::Nothing, |i| enter(n, i)),
        0x30..=0x5A if in_sub => by_key(&POWER, vk as u8).map_or(Out::Nothing, |i| Out::Do(POWER[i].2)),
        0x30..=0x5A => by_key(&ITEMS, vk as u8).map_or(Out::Nothing, |i| enter(n, i)),
        _ => Out::Nothing,
    }
}

// ======================= menu layout =======================

const PAD: f32 = 4.0;
const ITEM_H: f32 = 32.0;
const SEP_H: f32 = 9.0;
const MW: f32 = 232.0;
const SW: f32 = 160.0;
const GAP: f32 = 12.0; // above the taskbar

/// (index, top, bottom) of every row.
fn rows(list: &[Entry]) -> Vec<(usize, f32, f32)> {
    let mut y = PAD;
    list.iter()
        .enumerate()
        .map(|(i, e)| {
            let h = if e.2 == Act::Sep { SEP_H } else { ITEM_H };
            y += h;
            (i, y - h, y)
        })
        .collect()
}

fn height(list: &[Entry]) -> f32 {
    rows(list).last().map_or(0.0, |r| r.2) + PAD
}

fn hit(list: &[Entry], y: f32) -> Option<usize> {
    rows(list).into_iter().find(|&(i, t, b)| y >= t && y < b && list[i].2 != Act::Sep).map(|r| r.0)
}

// ======================= drawing =======================

struct Gfx {
    rt: ID2D1DCRenderTarget,
    body: IDWriteTextFormat,
    body_c: IDWriteTextFormat,
    small: IDWriteTextFormat,
    desc: IDWriteTextFormat,
    icon10: IDWriteTextFormat,
    icon12: IDWriteTextFormat,
    icon16: IDWriteTextFormat,
    icon32: IDWriteTextFormat,
    accent: D2D1_COLOR_F,
}

thread_local! {
    static GFX: RefCell<Option<Gfx>> = const { RefCell::new(None) };
    static NAV: Cell<Nav> = const { Cell::new(Nav { sel: None, sub: false, sub_sel: None }) };
    static SHOWN_AT: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
    static RUN_HOVER: Cell<Option<RHit>> = const { Cell::new(None) };
    static MRU: RefCell<(Vec<String>, Option<usize>)> = const { RefCell::new((Vec::new(), None)) };
    static GDI: Cell<(isize, isize, isize)> = const { Cell::new((0, 0, 0)) }; // edit font, edit brush, list brush
}

unsafe fn make_gfx() -> windows::core::Result<Gfx> {
    let f: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
    let props = D2D1_RENDER_TARGET_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        ..Default::default()
    };
    let rt = f.CreateDCRenderTarget(&props)?;
    rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
    let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
    let fmt = |name, size, align, wrap: bool| -> windows::core::Result<IDWriteTextFormat> {
        let t = dw.CreateTextFormat(name, None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))?;
        t.SetTextAlignment(align)?;
        if wrap {
            t.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
        } else {
            t.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            t.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
            t.SetTrimming(&trim, &dw.CreateEllipsisTrimmingSign(&t)?)?;
        }
        Ok(t)
    };
    let (text, icons) = (w!("Segoe UI Variable Text"), w!("Segoe Fluent Icons"));
    let (l, c) = (DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_CENTER);
    let a = windows::UI::ViewManagement::UISettings::new()?
        .GetColorValue(windows::UI::ViewManagement::UIColorType::AccentLight2)
        .unwrap_or(windows::UI::Color { A: 255, R: 0x4C, G: 0xC2, B: 0xFF });
    Ok(Gfx {
        body: fmt(text, 14.0, l, false)?,
        body_c: fmt(text, 14.0, c, false)?,
        small: fmt(text, 12.0, l, false)?,
        desc: fmt(text, 12.0, l, true)?,
        icon10: fmt(icons, 10.0, c, false)?,
        icon12: fmt(icons, 12.0, c, false)?,
        icon16: fmt(icons, 16.0, c, false)?,
        icon32: fmt(icons, 32.0, c, false)?,
        accent: D2D1_COLOR_F { r: a.R as f32 / 255.0, g: a.G as f32 / 255.0, b: a.B as f32 / 255.0, a: 1.0 },
        rt,
    })
}

unsafe fn draw_menu(g: &Gfx, list: &[Entry], hi: Option<usize>, w: f32) -> windows::core::Result<()> {
    let rt = &g.rt;
    let bg = crate::flyout::BG;
    rt.Clear(Some(&crate::flyout::base(D2D1_COLOR_F { r: bg.0, g: bg.1, b: bg.2, a: bg.3 })));
    let white = rt.CreateSolidColorBrush(&rgb(0xFFFFFF), None)?;
    let line = rt.CreateSolidColorBrush(&rgb(0x3D3D3D), None)?;
    for (i, t, b) in rows(list) {
        let (label, _, act) = list[i];
        if act == Act::Sep {
            let y = t + (SEP_H - 1.0) / 2.0;
            rt.FillRectangle(&rect(0.0, y, w, y + 1.0), &line);
            continue;
        }
        if hi == Some(i) {
            rt.FillRoundedRectangle(&rrect(rect(PAD, t + 2.0, w - PAD, b - 2.0), 4.0), &line);
        }
        rt.DrawText(&utf16(label), &g.body, &rect(PAD + 12.0, t, w - PAD - 28.0, b), &white, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        if act == Act::Power {
            rt.DrawText(&glyph(0xE76C), &g.icon12, &rect(w - PAD - 28.0, t, w - PAD - 8.0, b), &white, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        }
    }
    Ok(())
}

/// Render into a DC through the shared Gfx (rebuilt if the device was lost).
unsafe fn with_gfx(dc: HDC, rc: &RECT, dpi: f32, f: impl FnOnce(&Gfx) -> windows::core::Result<()>) {
    GFX.with(|cell| {
        let mut g = cell.borrow_mut();
        if g.is_none() {
            *g = make_gfx().ok();
        }
        let ok = g.as_ref().map(|g| -> windows::core::Result<()> {
            g.rt.BindDC(dc, rc)?;
            g.rt.SetDpi(dpi, dpi);
            g.rt.BeginDraw();
            f(g)?;
            g.rt.EndDraw(None, None)
        });
        if !matches!(ok, Some(Ok(()))) {
            *g = None;
        }
    });
}

unsafe fn paint(hwnd: HWND, f: impl FnOnce(&Gfx, f32, f32) -> windows::core::Result<()>) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let params = BP_PAINTPARAMS { cbSize: size_of::<BP_PAINTPARAMS>() as u32, dwFlags: BPPF_ERASE, ..Default::default() };
    let mut mem = HDC::default();
    let pb = BeginBufferedPaint(hdc, &rc, BPBF_TOPDOWNDIB, Some(&params), &mut mem);
    if !mem.is_invalid() {
        let dpi = GetDpiForWindow(hwnd) as f32;
        let (w, h) = (rc.right as f32 * 96.0 / dpi, rc.bottom as f32 * 96.0 / dpi);
        with_gfx(mem, &rc, dpi, |g| f(g, w, h));
        let _ = EndBufferedPaint(pb, true);
    }
    let _ = EndPaint(hwnd, &ps);
}

// ======================= menu window =======================

unsafe fn work_area() -> RECT {
    let mut wa = RECT::default();
    let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut wa as *mut _ as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    wa
}

unsafe fn scale(h: HWND) -> impl Fn(f32) -> i32 {
    let s = GetDpiForWindow(h) as f32 / 96.0;
    move |v: f32| (v * s).round() as i32
}

pub unsafe fn toggle(at_cursor: bool) {
    let m = hw(&MENU);
    if IsWindowVisible(m).as_bool() {
        // a repeat within 300ms of opening is the same key press arriving twice, not "close"
        if SHOWN_AT.with(|t| t.get()).is_some_and(|t| t.elapsed().as_millis() < 300) {
            return;
        }
        hide_menu();
        return;
    }
    NAV.with(|n| n.set(Nav::default()));
    let px = scale(m);
    let wa = work_area();
    let (w, h) = (px(MW), px(height(&ITEMS)));
    let (x, y) = if at_cursor {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        (pt.x, pt.y - h)
    } else {
        // Windows anchors Win+X to the taskbar Start button
        let s = crate::launcher::ZONES[0].each_ref().map(|a| a.load(Relaxed));
        (if s[2] > s[0] { s[0] } else { wa.left + px(GAP) }, wa.bottom - h - px(GAP))
    };
    let x = x.clamp(wa.left, (wa.right - w).max(wa.left));
    let y = y.clamp(wa.top, (wa.bottom - h).max(wa.top));
    let _ = SetWindowPos(m, Some(HWND_TOPMOST), x, y, w, h, SWP_SHOWWINDOW);
    SHOWN_AT.with(|t| t.set(Some(std::time::Instant::now())));
    crate::flyout::force_foreground(m);
}

unsafe fn hide_menu() {
    let _ = ShowWindow(hw(&SUB), SW_HIDE);
    let _ = ShowWindow(hw(&MENU), SW_HIDE);
    let _ = windows::Win32::System::ProcessStatus::K32EmptyWorkingSet(windows::Win32::System::Threading::GetCurrentProcess());
}

/// Show / hide the submenu to match NAV, repaint both.
unsafe fn sync() {
    let (m, s) = (hw(&MENU), hw(&SUB));
    let open = NAV.with(|n| n.get().sub);
    if open && !IsWindowVisible(s).as_bool() {
        let px = scale(m);
        let wa = work_area();
        let mut r = RECT::default();
        let _ = GetWindowRect(m, &mut r);
        let (w, h) = (px(SW), px(height(&POWER)));
        let top = rows(&ITEMS)[POWER_I].1;
        let mut x = r.right;
        if x + w > wa.right {
            x = r.left - w;
        }
        let y = (r.top + px(top - PAD)).min(wa.bottom - h);
        let _ = SetWindowPos(s, Some(HWND_TOPMOST), x, y, w, h, SWP_SHOWWINDOW | SWP_NOACTIVATE);
    } else if !open {
        let _ = ShowWindow(s, SW_HIDE);
    }
    let _ = InvalidateRect(Some(m), None, false);
    let _ = InvalidateRect(Some(s), None, false);
}

fn open(file: &str, args: &str, verb: PCWSTR) {
    unsafe {
        if args.is_empty() && !verb.to_string().is_ok_and(|v| v == "runas") && crate::launcher::open_rust(file) {
            return;
        }
        ShellExecuteW(None, verb, &HSTRING::from(file), &HSTRING::from(args), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

unsafe fn act(a: Act) {
    hide_menu();
    let shutdown = |args: &str| open("shutdown.exe", args, w!("open"));
    match a {
        Act::Open(f, args) => open(f, args, w!("open")),
        Act::Admin(f) => open(f, "", w!("runas")),
        Act::Search => {
            let _ = PostMessageW(Some(hw(&crate::launcher::LAUNCHER)), crate::launcher::WM_TOGGLE, WPARAM(1), LPARAM(0));
        }
        Act::Run => show_run(),
        Act::Desktop => {
            if let Ok(sh) = CoCreateInstance::<_, IShellDispatch4>(&Shell, None, CLSCTX_INPROC_SERVER) {
                let _ = sh.ToggleDesktop();
            }
        }
        Act::SignOut => {
            let _ = windows::Win32::System::Shutdown::ExitWindowsEx(windows::Win32::System::Shutdown::EWX_LOGOFF, windows::Win32::System::Shutdown::SHUTDOWN_REASON(0));
        }
        Act::Sleep => {
            let _ = windows::Win32::System::Power::SetSuspendState(false, false, false);
        }
        Act::Shutdown => shutdown("/s /t 0"),
        Act::Restart => shutdown("/r /t 0"),
        Act::Sep | Act::Power => {}
    }
}

pub unsafe extern "system" fn menu_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let is_sub = hwnd == hw(&SUB);
    let list: &[Entry] = if is_sub { &POWER } else { &ITEMS };
    let y = || ((lp.0 >> 16) & 0xFFFF) as i16 as f32 * 96.0 / GetDpiForWindow(hwnd) as f32;
    let nav = || NAV.with(|n| n.get());
    let set = |v: Nav| {
        if NAV.with(|n| n.replace(v)) != v {
            sync();
        }
    };
    match msg {
        WM_WINX_TOGGLE => toggle(wp.0 == 1),
        WM_RUN_SHOW => show_run(),
        crate::flyout::WM_DISMISS => hide_menu(),
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            // the Win key's release can steal focus right after we open: take it back once
            if SHOWN_AT.with(|t| t.get()).is_some_and(|t| t.elapsed().as_millis() < 900) {
                let _ = PostMessageW(Some(hwnd), WM_WINX_REFOCUS, WPARAM(0), LPARAM(0));
            } else {
                hide_menu();
            }
        }
        WM_WINX_REFOCUS => crate::flyout::force_foreground(hwnd),
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let mut n = nav();
            match key(&mut n, wp.0 as u16) {
                Out::Close => hide_menu(),
                Out::Do(a) => act(a),
                Out::Nothing => set(n),
            }
        }
        WM_MOUSEMOVE => {
            let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            let mut n = nav();
            match hit(list, y()) {
                Some(i) if is_sub => n.sub_sel = Some(i),
                Some(i) => {
                    n.sel = Some(i);
                    n.sub = i == POWER_I; // hovering "Shut down or sign out" opens the submenu
                    if !n.sub {
                        n.sub_sel = None;
                    }
                }
                None => {}
            }
            set(n);
        }
        WM_MOUSELEAVE => {
            let mut n = nav();
            if is_sub {
                n.sub_sel = None;
            } else if !n.sub {
                n.sel = None;
            }
            set(n);
        }
        WM_LBUTTONUP | WM_RBUTTONUP => {
            if let Some(i) = hit(list, y()) {
                if is_sub {
                    act(POWER[i].2);
                } else if let Out::Do(a) = enter(&mut nav(), i) {
                    act(a);
                }
            }
        }
        WM_MOUSEACTIVATE if is_sub => return LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let n = nav();
            let hi = if is_sub { n.sub_sel } else { n.sel };
            paint(hwnd, |g, w, _| draw_menu(g, list, hi, w));
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

// ======================= Run: command parsing =======================

fn is_url(s: &str) -> bool {
    if s.len() > 4 && s[..4].eq_ignore_ascii_case("www.") {
        return true;
    }
    // scheme: (http:, ms-settings:, shell:, mailto: ...), never a drive letter
    match s.find(':') {
        Some(i) if i >= 2 => s.as_bytes()[0].is_ascii_alphabetic() && s[..i].bytes().all(|c| c.is_ascii_alphanumeric() || b"+-.".contains(&c)),
        _ => false,
    }
}

fn expand(s: &str) -> String {
    use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
    unsafe {
        let src = HSTRING::from(s);
        let mut buf = vec![0u16; ExpandEnvironmentStringsW(&src, None) as usize + 1];
        let n = ExpandEnvironmentStringsW(&src, Some(&mut buf)) as usize;
        if n == 0 {
            return s.to_string();
        }
        String::from_utf16_lossy(&buf[..n - 1])
    }
}

/// Run box text -> (file, parameters) for ShellExecuteEx, like Windows:
/// %vars% expanded, URLs / shell: links whole, "quoted path" args, unquoted paths with spaces.
fn parse(cmd: &str, exists: impl Fn(&str) -> bool) -> (String, String) {
    let s = expand(cmd.trim());
    if is_url(&s) {
        let s = if s[..4].eq_ignore_ascii_case("www.") { format!("http://{s}") } else { s };
        return (s, String::new());
    }
    if let Some(rest) = s.strip_prefix('"') {
        return match rest.split_once('"') {
            Some((p, a)) => (p.into(), a.trim().into()),
            None => (rest.into(), String::new()),
        };
    }
    if exists(&s) {
        return (s, String::new());
    }
    // longest leading part that is a real path: C:\Program Files\App\app.exe -flag
    for (i, _) in s.match_indices(' ').collect::<Vec<_>>().into_iter().rev() {
        if exists(&s[..i]) {
            return (s[..i].into(), s[i + 1..].trim().into());
        }
    }
    match s.split_once(' ') {
        Some((p, a)) => (p.into(), a.trim().into()),
        None => (s, String::new()),
    }
}

fn mru_push(list: &mut Vec<String>, cmd: &str) {
    list.retain(|c| !c.eq_ignore_ascii_case(cmd));
    list.insert(0, cmd.to_string());
    list.truncate(26);
}

/// Combo-box style: Down = older, Up = newer, stops at the ends.
fn cycle(len: usize, cur: Option<usize>, down: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match (cur, down) {
        (None, _) => 0,
        (Some(i), true) => (i + 1).min(len - 1),
        (Some(i), false) => i.saturating_sub(1),
    })
}

fn mru_path() -> Option<std::path::PathBuf> {
    std::env::current_exe().ok().map(|p| p.with_file_name("runmru.txt"))
}

fn mru_load() -> Vec<String> {
    let text = mru_path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
}

// ======================= Run: window =======================

const RW: f32 = 400.0;
const RH: f32 = 200.0;
const TITLE_H: f32 = 32.0;
const BOX: (f32, f32, f32, f32) = (60.0, 100.0, RW - 14.0, 132.0);
const BTN_T: f32 = 152.0;
const BTN_W: f32 = 96.0;
const BTNS: [(f32, &str); 3] = [(RW - 14.0 - 3.0 * BTN_W - 16.0, "OK"), (RW - 14.0 - 2.0 * BTN_W - 8.0, "Cancel"), (RW - 14.0 - BTN_W, "Browse...")];
const LIST_ROW: f32 = 28.0;

#[derive(Clone, Copy, PartialEq, Debug)]
enum RHit {
    Ok,
    Cancel,
    Browse,
    Close,
    Chevron,
}

fn run_hit(x: f32, y: f32) -> Option<RHit> {
    if y < TITLE_H && x >= RW - 46.0 {
        return Some(RHit::Close);
    }
    if (BOX.1..BOX.3).contains(&y) && (BOX.2 - 32.0..BOX.2).contains(&x) {
        return Some(RHit::Chevron);
    }
    if (BTN_T..BTN_T + 32.0).contains(&y) {
        for (i, (bx, _)) in BTNS.iter().enumerate() {
            if (*bx..bx + BTN_W).contains(&x) {
                return Some([RHit::Ok, RHit::Cancel, RHit::Browse][i]);
            }
        }
    }
    None
}

unsafe fn draw_run(g: &Gfx, hover: Option<RHit>, typed: Option<&str>) -> windows::core::Result<()> {
    let rt = &g.rt;
    rt.Clear(Some(&rgb(0x202020)));
    let b = |c: D2D1_COLOR_F| rt.CreateSolidColorBrush(&c, None);
    let white = b(rgb(0xFFFFFF))?;
    let text = |s: &[u16], f: &IDWriteTextFormat, r: D2D_RECT_F, br: &ID2D1SolidColorBrush| {
        rt.DrawText(s, f, &r, br, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL)
    };
    // caption: icon, title, close
    text(&glyph(0xE8A7), &g.icon16, rect(12.0, 0.0, 28.0, TITLE_H), &white);
    text(&utf16("Run"), &g.small, rect(40.0, 0.0, 200.0, TITLE_H), &white);
    if hover == Some(RHit::Close) {
        rt.FillRectangle(&rect(RW - 46.0, 0.0, RW, TITLE_H), &b(rgb(0xC42B1C))?);
    }
    text(&glyph(0xE8BB), &g.icon10, rect(RW - 46.0, 0.0, RW, TITLE_H), &white);
    // body
    text(&glyph(0xE8A7), &g.icon32, rect(14.0, 44.0, 46.0, 80.0), &b(g.accent)?);
    text(
        &utf16("Type the name of a program, folder, document, or Internet resource, and Windows will open it for you."),
        &g.desc, rect(60.0, 44.0, RW - 14.0, 92.0), &white,
    );
    text(&utf16("Open:"), &g.small, rect(14.0, BOX.1, BOX.0, BOX.3), &white);
    // text box (focused look: darker fill, accent underline); the EDIT child sits inside it
    let bx = rect(BOX.0, BOX.1, BOX.2, BOX.3);
    rt.FillRoundedRectangle(&rrect(bx, 4.0), &b(rgb(0x1F1F1F))?);
    rt.DrawRoundedRectangle(&rrect(rect(BOX.0 + 0.5, BOX.1 + 0.5, BOX.2 - 0.5, BOX.3 - 0.5), 4.0), &b(D2D1_COLOR_F { a: 0.08, ..rgb(0xFFFFFF) })?, 1.0, None);
    rt.FillRectangle(&rect(BOX.0 + 1.0, BOX.3 - 2.0, BOX.2 - 1.0, BOX.3), &b(g.accent)?);
    if hover == Some(RHit::Chevron) {
        rt.FillRoundedRectangle(&rrect(rect(BOX.2 - 30.0, BOX.1 + 4.0, BOX.2 - 4.0, BOX.3 - 4.0), 4.0), &b(D2D1_COLOR_F { a: 0.06, ..rgb(0xFFFFFF) })?);
    }
    text(&glyph(0xE70D), &g.icon12, rect(BOX.2 - 30.0, BOX.1, BOX.2 - 4.0, BOX.3), &b(D2D1_COLOR_F { a: crate::flyout::SECONDARY, ..rgb(0xFFFFFF) })?);
    if let Some(t) = typed {
        text(&utf16(t), &g.body, rect(BOX.0 + 11.0, BOX.1, BOX.2 - 34.0, BOX.3 - 2.0), &white);
    }
    // buttons: accent OK, standard Cancel / Browse...
    for (i, (x, label)) in BTNS.iter().enumerate() {
        let r = rect(*x, BTN_T, x + BTN_W, BTN_T + 32.0);
        let hov = hover == Some([RHit::Ok, RHit::Cancel, RHit::Browse][i]);
        if i == 0 {
            let fill = if hov { D2D1_COLOR_F { a: 0.9, ..g.accent } } else { g.accent };
            rt.FillRoundedRectangle(&rrect(r, 4.0), &b(fill)?);
            text(&utf16(label), &g.body_c, r, &b(rgb(0x000000))?);
        } else {
            rt.FillRoundedRectangle(&rrect(r, 4.0), &b(rgb(if hov { 0x323232 } else { 0x2D2D2D }))?);
            rt.DrawRoundedRectangle(&rrect(rect(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5), 4.0), &b(D2D1_COLOR_F { a: 0.07, ..rgb(0xFFFFFF) })?, 1.0, None);
            text(&utf16(label), &g.body_c, r, &white);
        }
    }
    Ok(())
}

unsafe fn edit_text() -> String {
    let e = hw(&EDIT);
    let mut buf = vec![0u16; GetWindowTextLengthW(e) as usize + 1];
    let n = GetWindowTextW(e, &mut buf) as usize;
    String::from_utf16_lossy(&buf[..n])
}

unsafe fn set_edit(s: &str) {
    let e = hw(&EDIT);
    let _ = SetWindowTextW(e, &HSTRING::from(s));
    SendMessageW(e, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
}

pub unsafe fn show_run() {
    let h = hw(&RUN);
    if !IsWindowVisible(h).as_bool() {
        let px = scale(h);
        let wa = work_area();
        let (w, hh) = (px(RW), px(RH));
        let _ = SetWindowPos(h, Some(HWND_TOP), wa.left + px(12.0), wa.bottom - hh - px(12.0), w, hh, SWP_SHOWWINDOW);
        // edit font + position follow the DPI of wherever we are now
        let (old, eb, lb) = GDI.with(|g| g.get());
        let font = CreateFontW(-px(14.0), 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI Variable Text"));
        for c in [hw(&EDIT), hw(&LIST)] {
            SendMessageW(c, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(0)));
        }
        if old != 0 {
            let _ = DeleteObject(HGDIOBJ(old as *mut c_void));
        }
        GDI.with(|g| g.set((font.0 as isize, eb, lb)));
        let eh = px(20.0);
        let mid = px((BOX.1 + BOX.3) / 2.0 - 1.0);
        let _ = SetWindowPos(hw(&EDIT), None, px(BOX.0 + 11.0), mid - eh / 2, px(BOX.2 - BOX.0 - 45.0), eh, SWP_NOZORDER);
        SendMessageW(hw(&LIST), LB_SETITEMHEIGHT, Some(WPARAM(0)), Some(LPARAM(px(LIST_ROW) as isize)));
        // last command pre-filled and selected, like Windows
        let list = mru_load();
        set_edit(list.first().map_or("", String::as_str));
        MRU.with(|m| *m.borrow_mut() = (list, Some(0)));
    }
    crate::flyout::force_foreground(h);
    let _ = SetFocus(Some(hw(&EDIT)));
}

unsafe fn hide_run() {
    let _ = ShowWindow(hw(&LIST), SW_HIDE);
    let _ = ShowWindow(hw(&RUN), SW_HIDE);
    let _ = windows::Win32::System::ProcessStatus::K32EmptyWorkingSet(windows::Win32::System::Threading::GetCurrentProcess());
}

unsafe fn toggle_list() {
    let l = hw(&LIST);
    if IsWindowVisible(l).as_bool() {
        let _ = ShowWindow(l, SW_HIDE);
        return;
    }
    let (items, cur) = MRU.with(|m| m.borrow().clone());
    if items.is_empty() {
        return;
    }
    SendMessageW(l, LB_RESETCONTENT, None, None);
    for it in &items {
        let s = HSTRING::from(it.as_str());
        SendMessageW(l, LB_ADDSTRING, Some(WPARAM(0)), Some(LPARAM(s.as_ptr() as isize)));
    }
    SendMessageW(l, LB_SETCURSEL, Some(WPARAM(cur.unwrap_or(usize::MAX))), None);
    let h = hw(&RUN);
    let px = scale(h);
    let mut r = RECT::default();
    let _ = GetWindowRect(h, &mut r);
    let lh = px(LIST_ROW) * items.len().min(10) as i32 + 2;
    let (x, w) = (r.left + px(BOX.0), px(BOX.2 - BOX.0));
    let mut y = r.top + px(BOX.3 + 2.0);
    if y + lh > work_area().bottom {
        y = r.top + px(BOX.1 - 2.0) - lh; // no room below (Run sits on the taskbar): drop up
    }
    let _ = SetWindowPos(l, Some(HWND_TOPMOST), x, y, w, lh, SWP_SHOWWINDOW | SWP_NOACTIVATE);
}

unsafe fn exec(admin: bool) {
    let h = hw(&RUN);
    let cmd = edit_text();
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return;
    }
    let _ = ShowWindow(hw(&LIST), SW_HIDE);
    let (file, args) = parse(cmd, |p| std::path::Path::new(p).exists());
    if !admin && args.is_empty() && crate::launcher::open_rust(&file) {
        hide_run();
        return;
    }
    let (file, args) = (HSTRING::from(file), HSTRING::from(args));
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC,
        hwnd: h,
        lpVerb: if admin { w!("runas") } else { PCWSTR::null() },
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: if args.is_empty() { PCWSTR::null() } else { PCWSTR(args.as_ptr()) },
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // failures show Windows' own "Windows cannot find ..." box; the Run box stays open like Windows
    if ShellExecuteExW(&mut sei).is_ok() {
        let mut list = mru_load();
        mru_push(&mut list, cmd);
        if let Some(p) = mru_path() {
            let _ = std::fs::write(p, list.join("\r\n"));
        }
        hide_run();
    } else {
        set_edit(cmd);
        let _ = SetFocus(Some(hw(&EDIT)));
    }
}

unsafe fn browse() {
    let _ = ShowWindow(hw(&LIST), SW_HIDE);
    let r = (|| -> windows::core::Result<String> {
        let d: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
        d.SetFileTypes(&[
            COMDLG_FILTERSPEC { pszName: w!("Programs"), pszSpec: w!("*.exe;*.pif;*.com;*.bat;*.cmd") },
            COMDLG_FILTERSPEC { pszName: w!("All Files"), pszSpec: w!("*.*") },
        ])?;
        d.SetTitle(w!("Browse"))?;
        d.Show(Some(hw(&RUN)))?;
        let p = d.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const c_void));
        Ok(s)
    })();
    if let Ok(p) = r {
        set_edit(&if p.contains(' ') { format!("\"{p}\"") } else { p });
    }
    let _ = SetFocus(Some(hw(&EDIT)));
}

/// Keys inside the Open: box (Enter, Ctrl+Shift+Enter, Esc, Up/Down history, Alt+Down/F4 list, Ctrl+A).
unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let down = |vk: VIRTUAL_KEY| GetKeyState(vk.0 as i32) < 0;
    let vk = VIRTUAL_KEY(wp.0 as u16);
    match msg {
        WM_KEYDOWN if vk == VK_RETURN => {
            let admin = down(VK_CONTROL) && down(VK_SHIFT);
            let _ = PostMessageW(Some(hw(&RUN)), WM_RUN_EXEC, WPARAM(admin as usize), LPARAM(0));
            return LRESULT(0);
        }
        WM_KEYDOWN if vk == VK_ESCAPE => {
            if IsWindowVisible(hw(&LIST)).as_bool() {
                let _ = ShowWindow(hw(&LIST), SW_HIDE);
            } else {
                hide_run();
            }
            return LRESULT(0);
        }
        WM_KEYDOWN if vk == VK_UP || vk == VK_DOWN => {
            let next = MRU.with(|m| {
                let mut m = m.borrow_mut();
                m.1 = cycle(m.0.len(), m.1, vk == VK_DOWN);
                m.1.map(|i| m.0[i].clone())
            });
            if let Some(s) = next {
                set_edit(&s);
                let i = MRU.with(|m| m.borrow().1.unwrap_or(0));
                SendMessageW(hw(&LIST), LB_SETCURSEL, Some(WPARAM(i)), None);
            }
            return LRESULT(0);
        }
        WM_KEYDOWN if vk == VK_F4 => {
            toggle_list();
            return LRESULT(0);
        }
        WM_KEYDOWN if vk.0 == b'A' as u16 && down(VK_CONTROL) => {
            SendMessageW(hwnd, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
            return LRESULT(0);
        }
        WM_SYSKEYDOWN if vk == VK_DOWN || vk == VK_UP => {
            toggle_list();
            return LRESULT(0);
        }
        WM_CHAR if matches!(wp.0, 0x0D | 0x0A | 0x1B | 0x01) => return LRESULT(0), // no beeps
        _ => {}
    }
    CallWindowProcW(std::mem::transmute::<isize, WNDPROC>(EDIT_PROC.load(Relaxed)), hwnd, msg, wp, lp)
}

pub unsafe extern "system" fn run_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let dips = |lp: LPARAM| {
        let s = GetDpiForWindow(hwnd) as f32 / 96.0;
        ((lp.0 & 0xFFFF) as i16 as f32 / s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s)
    };
    match msg {
        WM_CREATE => {
            // opaque, rounded, dark border: GDI text in the EDIT child needs a solid background
            for (attr, v) in [(DWMWA_USE_IMMERSIVE_DARK_MODE, 1u32), (DWMWA_WINDOW_CORNER_PREFERENCE, 2)] {
                let _ = DwmSetWindowAttribute(hwnd, attr, &v as *const _ as *const c_void, 4);
            }
            let inst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).ok().map(|m| m.into());
            let style = WS_CHILD | WS_VISIBLE | WINDOW_STYLE(ES_AUTOHSCROLL as u32);
            if let Ok(e) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("EDIT"), None, style, 0, 0, 10, 10, Some(hwnd), None, inst, None) {
                EDIT.store(e.0 as isize, Relaxed);
                EDIT_PROC.store(SetWindowLongPtrW(e, GWLP_WNDPROC, edit_proc as *const () as isize), Relaxed);
            }
            let ls = WS_POPUP | WS_VSCROLL | WINDOW_STYLE((LBS_NOTIFY | LBS_OWNERDRAWFIXED | LBS_HASSTRINGS | LBS_NOINTEGRALHEIGHT) as u32);
            if let Ok(l) = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE, w!("LISTBOX"), None, ls, 0, 0, 10, 10, Some(hwnd), None, inst, None) {
                LIST.store(l.0 as isize, Relaxed);
                let _ = SetWindowTheme(l, w!("DarkMode_Explorer"), PCWSTR::null());
                let v = 2u32;
                let _ = DwmSetWindowAttribute(l, DWMWA_WINDOW_CORNER_PREFERENCE, &v as *const _ as *const c_void, 4);
            }
            GDI.with(|g| g.set((0, CreateSolidBrush(COLORREF(0x1F1F1F)).0 as isize, CreateSolidBrush(COLORREF(0x2C2C2C)).0 as isize)));
        }
        WM_RUN_EXEC => exec(wp.0 == 1),
        WM_ACTIVATE => {
            if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE {
                if lp.0 != LIST.load(Relaxed) {
                    let _ = ShowWindow(hw(&LIST), SW_HIDE);
                }
            } else {
                let _ = SetFocus(Some(hw(&EDIT)));
            }
        }
        WM_CTLCOLOREDIT => {
            let dc = HDC(wp.0 as *mut c_void);
            SetTextColor(dc, COLORREF(0xFFFFFF));
            SetBkColor(dc, COLORREF(0x1F1F1F));
            return LRESULT(GDI.with(|g| g.get().1));
        }
        WM_CTLCOLORLISTBOX => return LRESULT(GDI.with(|g| g.get().2)),
        WM_DRAWITEM => {
            let d = &*(lp.0 as *const DRAWITEMSTRUCT);
            if d.CtlType == ODT_LISTBOX && d.itemID != u32::MAX {
                let br = CreateSolidBrush(COLORREF(if d.itemState.0 & ODS_SELECTED.0 != 0 { 0x3D3D3D } else { 0x2C2C2C }));
                FillRect(d.hDC, &d.rcItem, br);
                let _ = DeleteObject(br.into());
                let n = SendMessageW(d.hwndItem, LB_GETTEXTLEN, Some(WPARAM(d.itemID as usize)), None).0.max(0) as usize;
                let mut buf = vec![0u16; n + 1];
                SendMessageW(d.hwndItem, LB_GETTEXT, Some(WPARAM(d.itemID as usize)), Some(LPARAM(buf.as_mut_ptr() as isize)));
                SetBkMode(d.hDC, TRANSPARENT);
                SetTextColor(d.hDC, COLORREF(0xFFFFFF));
                let pad = scale(hwnd)(11.0);
                let mut r = RECT { left: d.rcItem.left + pad, right: d.rcItem.right - pad, ..d.rcItem };
                DrawTextW(d.hDC, &mut buf[..n], &mut r, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
            }
            return LRESULT(1);
        }
        WM_COMMAND if lp.0 == LIST.load(Relaxed) && (wp.0 >> 16) as u32 == LBN_SELCHANGE => {
            let i = SendMessageW(hw(&LIST), LB_GETCURSEL, None, None).0;
            if let Some(s) = MRU.with(|m| m.borrow().0.get(i as usize).cloned()) {
                MRU.with(|m| m.borrow_mut().1 = Some(i as usize));
                set_edit(&s);
            }
            let _ = ShowWindow(hw(&LIST), SW_HIDE);
            crate::flyout::force_foreground(hwnd);
            let _ = SetFocus(Some(hw(&EDIT)));
        }
        WM_NCHITTEST => {
            let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
            let _ = ScreenToClient(hwnd, &mut pt);
            let s = GetDpiForWindow(hwnd) as f32 / 96.0;
            let (x, y) = (pt.x as f32 / s, pt.y as f32 / s);
            return LRESULT((if y < TITLE_H && x < RW - 46.0 { HTCAPTION } else { HTCLIENT }) as isize);
        }
        WM_MOUSEMOVE => {
            let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            let (x, y) = dips(lp);
            let h = run_hit(x, y);
            if RUN_HOVER.with(|c| c.replace(h)) != h {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
        WM_MOUSELEAVE => {
            RUN_HOVER.with(|c| c.set(None));
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        WM_LBUTTONDOWN => {
            if run_hit(dips(lp).0, dips(lp).1) != Some(RHit::Chevron) {
                let _ = ShowWindow(hw(&LIST), SW_HIDE);
            }
        }
        WM_LBUTTONUP => {
            let (x, y) = dips(lp);
            match run_hit(x, y) {
                Some(RHit::Ok) => exec(false),
                Some(RHit::Cancel | RHit::Close) => hide_run(),
                Some(RHit::Browse) => browse(),
                Some(RHit::Chevron) => {
                    toggle_list();
                    let _ = SetFocus(Some(hw(&EDIT)));
                }
                None => {}
            }
        }
        WM_CLOSE => hide_run(),
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let hov = RUN_HOVER.with(|c| c.get());
            paint(hwnd, |g, _, _| draw_run(g, hov, None));
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

// ======================= snapshots (off-screen) =======================

unsafe fn snapshot(path: &str, w: i32, h: i32, draw: impl FnOnce(HDC)) -> windows::core::Result<()> {
    let bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, ..Default::default() },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    let dib = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0)?;
    let dc = CreateCompatibleDC(None);
    let old = SelectObject(dc, dib.into());
    draw(dc);
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
}

/// `shellbits --snapshot-winx out.bmp`: menu with "Shut down or sign out" open and "Shut down" hovered.
pub unsafe fn snapshot_menu(path: &str) -> windows::core::Result<()> {
    let (mh, sh) = (height(&ITEMS), height(&POWER));
    let sub_y = rows(&ITEMS)[POWER_I].1 - PAD;
    snapshot(path, (MW + SW) as i32, mh.max(sub_y + sh) as i32, |dc| {
        with_gfx(dc, &RECT { left: 0, top: 0, right: MW as i32, bottom: mh as i32 }, 96.0, |g| draw_menu(g, &ITEMS, Some(POWER_I), MW));
        let _ = SetViewportOrgEx(dc, MW as i32, sub_y as i32, None);
        with_gfx(dc, &RECT { left: 0, top: 0, right: SW as i32, bottom: sh as i32 }, 96.0, |g| draw_menu(g, &POWER, Some(2), SW));
    })
}

/// `shellbits --snapshot-run out.bmp`: the Run box with a command typed.
pub unsafe fn snapshot_run(path: &str) -> windows::core::Result<()> {
    snapshot(path, RW as i32, RH as i32, |dc| {
        with_gfx(dc, &RECT { left: 0, top: 0, right: RW as i32, bottom: RH as i32 }, 96.0, |g| draw_run(g, Some(RHit::Ok), Some("notepad %USERPROFILE%\\notes.txt")));
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_commands() {
        let none = |_: &str| false;
        assert_eq!(parse("notepad", none), ("notepad".into(), "".into()));
        assert_eq!(parse("  cmd /k dir  ", none), ("cmd".into(), "/k dir".into()));
        assert_eq!(parse("\"C:\\Program Files\\A\\a.exe\" -x y", none), ("C:\\Program Files\\A\\a.exe".into(), "-x y".into()));
        let exists = |p: &str| p == "C:\\Program Files\\A\\a.exe";
        assert_eq!(parse("C:\\Program Files\\A\\a.exe -x", exists), ("C:\\Program Files\\A\\a.exe".into(), "-x".into()));
        let root = std::env::var("SystemRoot").unwrap();
        assert_eq!(parse("%SystemRoot%\\notepad.exe a.txt", none), (format!("{root}\\notepad.exe"), "a.txt".into()));
        assert_eq!(parse("%NO_SUCH_VAR_X%", none).0, "%NO_SUCH_VAR_X%");
        assert_eq!(parse("https://example.com/a b", none), ("https://example.com/a b".into(), "".into()));
        assert_eq!(parse("www.example.com", none).0, "http://www.example.com");
        assert_eq!(parse("ms-settings:display", none).0, "ms-settings:display");
        assert!(!is_url("C:\\Windows") && !is_url("notepad c:\\x.txt") && is_url("shell:startup"));
    }

    #[test]
    fn mru_and_cycle() {
        let mut l = vec!["a".to_string(), "b".into()];
        mru_push(&mut l, "B");
        assert_eq!(l, ["B", "a"]);
        assert_eq!(cycle(2, Some(0), true), Some(1));
        assert_eq!(cycle(2, Some(1), true), Some(1));
        assert_eq!(cycle(2, Some(0), false), Some(0));
        assert_eq!(cycle(0, None, true), None);
    }

    #[test]
    fn menu_keys() {
        let mut n = Nav::default();
        key(&mut n, 0x28);
        assert_eq!(n.sel, Some(0));
        key(&mut n, 0x26);
        assert_eq!(n.sel, Some(19), "Up from the top wraps to Desktop");
        n.sel = Some(10);
        key(&mut n, 0x28);
        assert_eq!(n.sel, Some(12), "separator skipped");
        assert_eq!(key(&mut n, b'T' as u16), Out::Do(Act::Open("taskmgr.exe", "")));
        assert_eq!(key(&mut n, b'R' as u16), Out::Do(Act::Run));
        // U opens the submenu, keyboard moves into it
        let mut n = Nav::default();
        assert_eq!(key(&mut n, b'U' as u16), Out::Nothing);
        assert!(n.sub && n.sub_sel == Some(0));
        key(&mut n, 0x28);
        assert_eq!(key(&mut n, 0x0D), Out::Do(Act::Sleep));
        assert_eq!(key(&mut n, b'R' as u16), Out::Do(Act::Restart));
        key(&mut n, 0x25);
        assert!(!n.sub && n.sel == Some(POWER_I));
        key(&mut n, 0x27);
        assert!(n.sub);
        assert_eq!(key(&mut n, 0x1B), Out::Nothing);
        assert!(!n.sub);
        assert_eq!(key(&mut n, 0x1B), Out::Close);
    }
}

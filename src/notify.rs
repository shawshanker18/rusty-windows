//! Windows 11 notification center + calendar (Win+N / clock click), measured from the real one (DIPs):
//! both 336 wide, 12px from the right edge; calendar 455 tall (100 collapsed) 12px above the taskbar,
//! notifications panel 12px above it, growing up to the top of the screen. Panel #222222, cards #292929
//! with a #1C1C1C edge. Day cells 41x41 on a 47.17px pitch from (6,152); today = accent circle.
//! Real data: your notifications (UserNotificationListener), Do Not Disturb (the shell's quiet-hours
//! state), Focus sessions (FocusSessionManager).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTimeAsFileTime};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::flyout::{glyph, rect, rrect, utf16};
use crate::quick::rgb;

pub const WM_NC_TOGGLE: u32 = WM_APP + 30;
const WM_NC_DATA: u32 = WM_APP + 31;
pub static NC: AtomicIsize = AtomicIsize::new(0);
pub static CAL: AtomicIsize = AtomicIsize::new(0);
const WM_NC_REFOCUS: u32 = WM_APP + 32;

const W: f32 = 336.0;
const EDGE: f32 = 12.0; // from the screen's right edge, above the taskbar, and between the two
const CAL_FULL: f32 = 455.0;
const CAL_MIN: f32 = 100.0;

const C_PANEL: D2D1_COLOR_F = rgb(0x222222);
const C_CARD: D2D1_COLOR_F = rgb(0x292929);
const C_CARD_HOVER: D2D1_COLOR_F = rgb(0x2D2D2D);
const C_LINE: D2D1_COLOR_F = rgb(0x1C1C1C);
const C_BTN: D2D1_COLOR_F = rgb(0x363636);
const C_BTN_HOVER: D2D1_COLOR_F = rgb(0x3D3D3D);
const C_BTN_EDGE: D2D1_COLOR_F = rgb(0x3E3E3E);
const C_ACCENT: D2D1_COLOR_F = rgb(0x4CC2FF);
const C_DIM: D2D1_COLOR_F = rgb(0x8B8B8B);
const C_ARROW: D2D1_COLOR_F = rgb(0x9A9A9A);

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const FOCUS_STEPS: [u32; 13] = [5, 10, 15, 20, 25, 30, 45, 60, 90, 120, 150, 180, 240];

#[derive(Clone, Default)]
pub(crate) struct Note {
    pub(crate) id: u32,
    pub(crate) aumid: String,
    pub(crate) app: String,
    pub(crate) title: String,
    pub(crate) body: String,
    time: i64, // FILETIME ticks, UTC
}

static DATA: Mutex<Option<Vec<Note>>> = Mutex::new(None);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Dnd,
    ClearAll,
    Settings,
    Group(usize),   // group header (x clears the group)
    Card(usize),    // row index
    Dismiss(usize), // row index
    More(usize),    // group index
    Caret,
    Prev,
    Next,
    Minus,
    Plus,
    Focus,
}

enum Row {
    Group(usize),
    Card { note: usize, group: usize, more: usize, body_h: f32 },
}

struct Group {
    pub(crate) aumid: String,
    pub(crate) app: String,
    notes: Vec<usize>,
}

#[derive(Default)]
struct State {
    notes: Vec<Note>,
    groups: Vec<Group>,
    rows: Vec<(Row, f32, f32)>, // row, top (content coords), height
    content_h: f32,
    scroll: f32,
    view_h: f32,
    hover: Option<Hit>,
    dnd: bool,
    expanded: HashSet<String>,
    cal_open: bool,
    month_off: i32,
    focus_idx: usize,
    focus_active: bool,
    gfx: Option<Gfx>,
    icons: HashMap<String, Vec<u8>>,
}

struct Gfx {
    rt: ID2D1DCRenderTarget,
    dw: IDWriteFactory,
    icon16: IDWriteTextFormat,
    icon12: IDWriteTextFormat,
    icon8: IDWriteTextFormat,
    small: IDWriteTextFormat,
    small_c: IDWriteTextFormat,
    small_sb_c: IDWriteTextFormat,
    body: IDWriteTextFormat,
    body_c: IDWriteTextFormat,
    body_sb: IDWriteTextFormat,
    wrap: IDWriteTextFormat,
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State { cal_open: true, focus_idx: 5, ..Default::default() });
    static SHOWN_AT: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
}

// ======================= system =======================

#[link(name = "ntdll")]
extern "system" {
    fn NtQueryWnfStateData(name: *const u64, ty: *const c_void, scope: *const c_void, stamp: *mut u32, buf: *mut c_void, size: *mut u32) -> i32;
    fn NtUpdateWnfStateData(name: *const u64, buf: *const c_void, len: u32, ty: *const c_void, scope: *const c_void, matching: u32, check: u32) -> i32;
}
/// The shell's quiet-hours profile (0 off, 1 priority only = Do Not Disturb, 2 alarms only).
const WNF_QUIET_HOURS: u64 = 0x0D83_063E_A3BF_1C75;

pub(crate) fn dnd_get() -> bool {
    let (mut v, mut size, mut stamp) = (0u32, 4u32, 0u32);
    unsafe { NtQueryWnfStateData(&WNF_QUIET_HOURS, std::ptr::null(), std::ptr::null(), &mut stamp, &mut v as *mut _ as *mut c_void, &mut size) };
    v != 0
}

pub(crate) fn dnd_set(on: bool) {
    let v: u32 = on as u32;
    unsafe { NtUpdateWnfStateData(&WNF_QUIET_HOURS, &v as *const _ as *const c_void, 4, std::ptr::null(), std::ptr::null(), 0, 0) };
}

fn focus_active() -> bool {
    windows::UI::Shell::FocusSessionManager::GetDefault().and_then(|m| m.IsFocusActive()).unwrap_or(false)
}

fn focus_toggle(mins: u32) {
    std::thread::spawn(move || {
        let r = (|| -> windows::core::Result<()> {
            let m = windows::UI::Shell::FocusSessionManager::GetDefault()?;
            if m.IsFocusActive()? {
                m.DeactivateFocus()
            } else {
                let now = unsafe { GetSystemTimeAsFileTime() };
                let t = ((now.dwHighDateTime as i64) << 32 | now.dwLowDateTime as i64) + mins as i64 * 60 * 10_000_000;
                m.TryStartFocusSession2(windows::Foundation::DateTime { UniversalTime: t }).map(|_| ())
            }
        })();
        let _ = r;
        post_data();
    });
}

/// App, title and text of one notification (every text line of its first layout).
fn note_of(n: &windows::UI::Notifications::UserNotification) -> windows::core::Result<Note> {
    let info = n.AppInfo().ok();
    let aumid = info.as_ref().and_then(|i| i.AppUserModelId().ok()).map(|s| s.to_string()).unwrap_or_default();
    let app = info.as_ref().and_then(|i| i.DisplayInfo().ok()).and_then(|d| d.DisplayName().ok()).map(|s| s.to_string()).unwrap_or_default();
    let texts: Vec<String> = n
        .Notification()
        .and_then(|x| x.Visual())
        .and_then(|v| v.Bindings())
        .ok()
        .and_then(|b| b.into_iter().next())
        .and_then(|b| b.GetTextElements().ok())
        .map(|t| t.into_iter().filter_map(|e| e.Text().ok().map(|s| s.to_string())).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    Ok(Note {
        id: n.Id()?,
        aumid,
        app,
        title: texts.first().cloned().unwrap_or_default(),
        body: texts.get(1..).map(|t| t.join("
")).unwrap_or_default(),
        time: n.CreationTime().map(|t| t.UniversalTime).unwrap_or(0),
    })
}

/// Call `f` with each notification the moment it arrives. Returns false if Windows refuses the
/// subscription (then the caller falls back to checking on a timer).
pub(crate) fn on_new(f: impl Fn(Note) + Send + 'static) -> bool {
    use windows::Foundation::TypedEventHandler;
    use windows::UI::Notifications::{Management::UserNotificationListener, UserNotificationChangedKind};
    let Ok(l) = UserNotificationListener::Current() else { return false };
    let l2 = l.clone();
    let h = TypedEventHandler::new(move |_, a: windows::core::Ref<windows::UI::Notifications::UserNotificationChangedEventArgs>| {
        if let Some(a) = a.as_ref() {
            if a.ChangeKind()? == UserNotificationChangedKind::Added {
                if let Ok(n) = l2.GetNotification(a.UserNotificationId()?) {
                    if let Ok(note) = note_of(&n) {
                        f(note);
                    }
                }
            }
        }
        Ok(())
    });
    l.NotificationChanged(&h).is_ok()
}

/// One notification by its id (milliseconds, where listing them all takes seconds).
pub(crate) fn get(id: u32) -> Option<Note> {
    let n = windows::UI::Notifications::Management::UserNotificationListener::Current().ok()?.GetNotification(id).ok()?;
    note_of(&n).ok()
}

/// Just the ids of what's in the notification center.
pub(crate) fn ids() -> HashSet<u32> {
    use windows::UI::Notifications::{Management::UserNotificationListener, NotificationKinds};
    (|| -> windows::core::Result<HashSet<u32>> {
        Ok(UserNotificationListener::Current()?.GetNotificationsAsync(NotificationKinds::Toast)?.get()?.into_iter().filter_map(|n| n.Id().ok()).collect())
    })()
    .unwrap_or_default()
}

pub(crate) fn fetch() -> Vec<Note> {
    fetch_skip(None)
}

/// Like fetch(), but notifications whose id is in `skip` come back with only their id filled in
/// (looking up each one's app name and text is the slow part).
pub(crate) fn fetch_skip(skip: Option<&HashSet<u32>>) -> Vec<Note> {
    use windows::UI::Notifications::{Management::UserNotificationListener, NotificationKinds};
    (|| -> windows::core::Result<Vec<Note>> {
        let l = UserNotificationListener::Current()?;
        let mut v = Vec::new();
        for n in l.GetNotificationsAsync(NotificationKinds::Toast)?.get()? {
            if let (Some(skip), Ok(id)) = (skip, n.Id()) {
                if skip.contains(&id) {
                    v.push(Note { id, ..Default::default() });
                    continue;
                }
            }
            v.push(note_of(&n)?);
        }
        v.sort_by_key(|n| -n.time);
        Ok(v)
    })()
    .unwrap_or_default()
}

fn post_data() {
    unsafe {
        let _ = PostMessageW(Some(HWND(NC.load(Relaxed) as *mut c_void)), WM_NC_DATA, WPARAM(0), LPARAM(0));
    }
}

/// Add notifications newer than the newest one we hold, by asking for the next ids (they count up).
/// Takes ~0.25 s where a full listing takes ~5.5 s. Returns how many were added.
fn catch_up() -> usize {
    let Some(mut next) = DATA.lock().unwrap().as_ref().and_then(|v| v.iter().map(|n| n.id).max()).map(|m| m + 1) else { return 0 };
    let mut fresh = Vec::new();
    // ponytail: looks 8 ids ahead; anything further is picked up by the full listing that follows
    while let Some(n) = (next..next + 8).find_map(get) {
        next = n.id + 1;
        fresh.push(n);
    }
    let added = fresh.len();
    if let (true, Some(v)) = (added > 0, DATA.lock().unwrap().as_mut()) {
        v.extend(fresh);
        v.sort_by_key(|n| -n.time);
    }
    added
}

/// Refresh the list in the background: new arrivals first (fast), then the full listing (slow; also drops removed ones).
pub(crate) fn fetch_async() {
    static BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if BUSY.swap(true, Relaxed) {
        return;
    }
    std::thread::spawn(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if catch_up() > 0 {
            post_data();
        }
        *DATA.lock().unwrap() = Some(fetch());
        post_data();
        BUSY.store(false, Relaxed);
    });
}

/// `--nc-selftest <file>`: time the full listing, then forget the newest notification and time catching up.
pub fn selftest(path: &str) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let t = std::time::Instant::now();
    let mut all = fetch();
    let full = t.elapsed().as_millis();
    let total = all.len();
    let newest = all.iter().map(|n| n.id).max();
    all.retain(|n| Some(n.id) != newest);
    *DATA.lock().unwrap() = Some(all);
    let t = std::time::Instant::now();
    let added = catch_up();
    let back = DATA.lock().unwrap().as_ref().is_some_and(|v| v.iter().any(|n| Some(n.id) == newest));
    let first_is_newest = DATA.lock().unwrap().as_ref().and_then(|v| v.first().map(|n| Some(n.id) == newest)).unwrap_or(false);
    let _ = std::fs::write(path, format!("full listing: {total} notifications in {full} ms\ncatch-up: added {added} in {} ms, newest recovered={back}, listed first={first_is_newest}", t.elapsed().as_millis()));
}

fn remove(id: Option<u32>) {
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if let Ok(l) = windows::UI::Notifications::Management::UserNotificationListener::Current() {
            let _ = match id {
                Some(id) => l.RemoveNotification(id),
                None => l.ClearNotifications(),
            };
        }
    });
}

fn open(uri: &str) {
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(uri), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// "6:52 PM" for today, "11/9/2026" before that.
fn when(ft: i64) -> String {
    unsafe {
        let f = FILETIME { dwLowDateTime: ft as u32, dwHighDateTime: (ft >> 32) as u32 };
        let (mut utc, mut loc) = (SYSTEMTIME::default(), SYSTEMTIME::default());
        if FileTimeToSystemTime(&f, &mut utc).is_err() || SystemTimeToTzSpecificLocalTime(None, &utc, &mut loc).is_err() {
            return String::new();
        }
        let now = GetLocalTime();
        if (loc.wYear, loc.wMonth, loc.wDay) == (now.wYear, now.wMonth, now.wDay) {
            let h = loc.wHour % 12;
            format!("{}:{:02} {}", if h == 0 { 12 } else { h }, loc.wMinute, if loc.wHour < 12 { "AM" } else { "PM" })
        } else {
            format!("{}/{}/{}", loc.wMonth, loc.wDay, loc.wYear)
        }
    }
}

fn days_in(y: i32, m: i32) -> i32 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// 0 = Sunday (Sakamoto).
fn weekday(mut y: i32, m: i32, d: i32) -> i32 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    if m < 3 {
        y -= 1;
    }
    (y + y / 4 - y / 100 + y / 400 + T[(m - 1) as usize] + d) % 7
}

/// Month shown (year, month 1-12) for an offset from the current one.
fn shown_month(off: i32) -> (i32, i32) {
    let now = unsafe { GetLocalTime() };
    let i = now.wYear as i32 * 12 + now.wMonth as i32 - 1 + off;
    (i.div_euclid(12), i.rem_euclid(12) + 1)
}

// ======================= layout =======================

fn list_top(dnd: bool) -> f32 {
    if dnd { 163.0 } else { 53.0 }
}

const GROUP_H: f32 = 41.0;
const EMPTY_H: f32 = 64.0;

unsafe fn text_h(g: &Gfx, s: &str, width: f32, max_lines: f32) -> f32 {
    if s.is_empty() {
        return 0.0;
    }
    let h = g
        .dw
        .CreateTextLayout(&utf16(s), &g.wrap, width, 10_000.0)
        .and_then(|l| { let mut m = DWRITE_TEXT_METRICS::default(); l.GetMetrics(&mut m).map(|_| m.height) })
        .unwrap_or(19.0);
    h.min(19.0 * max_lines)
}

/// Group notifications by app (newest first) and lay out the rows.
unsafe fn layout(st: &mut State) {
    let mut groups: Vec<Group> = Vec::new();
    for (i, n) in st.notes.iter().enumerate() {
        match groups.iter_mut().find(|g| g.aumid == n.aumid) {
            Some(g) => g.notes.push(i),
            None => groups.push(Group { aumid: n.aumid.clone(), app: n.app.clone(), notes: vec![i] }),
        }
    }
    let mut rows = Vec::new();
    let mut y = 0.0;
    if let Some(g) = st.gfx.as_ref() {
        for (gi, grp) in groups.iter().enumerate() {
            rows.push((Row::Group(gi), y, GROUP_H));
            y += GROUP_H;
            let open = st.expanded.contains(&grp.aumid);
            let shown = if open { grp.notes.len() } else { 1 };
            for (k, &ni) in grp.notes.iter().take(shown).enumerate() {
                let n = &st.notes[ni];
                let more = if k == 0 && !open { grp.notes.len() - 1 } else { 0 };
                let body_h = text_h(g, &n.body, 301.0, 4.0);
                let h = 63.0 + body_h + 19.0 + if more > 0 { 40.0 } else { 0.0 } - if n.title.is_empty() { 19.0 } else { 0.0 };
                rows.push((Row::Card { note: ni, group: gi, more, body_h }, y, h));
                y += h;
            }
        }
    }
    st.groups = groups;
    st.rows = rows;
    st.content_h = if st.notes.is_empty() { EMPTY_H } else { y };
}

fn nc_height(st: &State) -> f32 {
    list_top(st.dnd) + st.content_h + EDGE
}

fn cal_height(st: &State) -> f32 {
    if st.cal_open { CAL_FULL } else { CAL_MIN }
}

fn nc_hit(st: &State, x: f32, y: f32) -> Option<Hit> {
    if (5.0..45.0).contains(&y) {
        if (217.0..257.0).contains(&x) {
            return Some(Hit::Dnd);
        }
        if (257.0..319.0).contains(&x) && (13.0..37.0).contains(&y) && !st.notes.is_empty() {
            return Some(Hit::ClearAll);
        }
    }
    if st.dnd && (128.0..156.0).contains(&y) && (9.0..158.0).contains(&x) {
        return Some(Hit::Settings);
    }
    let lt = list_top(st.dnd);
    if y < lt {
        return None;
    }
    let cy = y - lt + st.scroll;
    for (i, (row, top, h)) in st.rows.iter().enumerate() {
        if cy < *top || cy >= top + h {
            continue;
        }
        return match row {
            Row::Group(g) => Some(Hit::Group(*g)),
            Row::Card { group, more, .. } => {
                let ry = cy - top;
                if *more > 0 && (h - 40.0..h - 16.0).contains(&ry) && (17.0..127.0).contains(&x) {
                    Some(Hit::More(*group))
                } else if (287.0..335.0).contains(&x) && (2.0..34.0).contains(&ry) {
                    Some(Hit::Dismiss(i))
                } else {
                    Some(Hit::Card(i))
                }
            }
        };
    }
    None
}

fn cell_x(col: i32) -> f32 {
    6.0 + col as f32 * 47.17
}

fn cal_hit(st: &State, x: f32, y: f32) -> Option<Hit> {
    let h = cal_height(st);
    if (14.0..38.0).contains(&y) && (291.0..323.0).contains(&x) {
        return Some(Hit::Caret);
    }
    if st.cal_open && (61.0..97.0).contains(&y) {
        if (254.0..288.0).contains(&x) {
            return Some(Hit::Prev);
        }
        if (294.0..328.0).contains(&x) {
            return Some(Hit::Next);
        }
    }
    let fy = h - 50.0;
    if (fy + 13.0..fy + 37.0).contains(&y) {
        if (17.0..41.0).contains(&x) {
            return Some(Hit::Minus);
        }
        if (122.0..146.0).contains(&x) {
            return Some(Hit::Plus);
        }
        if (247.0..319.0).contains(&x) {
            return Some(Hit::Focus);
        }
    }
    None
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
    let fmt = |name, size, weight, align| -> windows::core::Result<IDWriteTextFormat> {
        let t = dw.CreateTextFormat(name, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))?;
        t.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        t.SetTextAlignment(align)?;
        t.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
        t.SetTrimming(&trim, &dw.CreateEllipsisTrimmingSign(&t)?)?;
        Ok(t)
    };
    let text = w!("Segoe UI Variable Text");
    let icons = w!("Segoe Fluent Icons");
    let (c, l) = (DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING);
    let (n, sb) = (DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD);
    let wrap = fmt(text, 14.0, n, l)?;
    wrap.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
    wrap.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
    wrap.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, 19.0, 15.0)?;
    Ok(Gfx {
        icon16: fmt(icons, 16.0, n, c)?,
        icon12: fmt(icons, 12.0, n, c)?,
        icon8: fmt(icons, 8.0, n, c)?,
        small: fmt(text, 12.0, n, l)?,
        small_c: fmt(text, 12.0, n, c)?,
        small_sb_c: fmt(text, 12.0, sb, c)?,
        body: fmt(text, 14.0, n, l)?,
        body_c: fmt(text, 14.0, n, c)?,
        body_sb: fmt(text, 14.0, sb, l)?,
        wrap,
        dw,
        rt,
    })
}

unsafe fn draw_nc(st: &State, g: &Gfx, h: f32) -> windows::core::Result<()> {
    let rt = &g.rt;
    rt.Clear(Some(&crate::flyout::base(C_PANEL)));
    let brush = |c: D2D1_COLOR_F| rt.CreateSolidColorBrush(&c, None);
    let white = brush(rgb(0xFFFFFF))?;
    let black = brush(rgb(0x000000))?;
    let text = |s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush| {
        rt.DrawText(&utf16(s), f, &r, b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL)
    };
    let icon = |cp: u32, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush| {
        rt.DrawText(&glyph(cp), f, &r, b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL)
    };
    // header
    text("Notifications", &g.body, rect(17.0, 15.0, 200.0, 34.0), &white);
    let dnd_r = rect(225.0, 13.0, 249.0, 37.0);
    if st.dnd {
        rt.FillRoundedRectangle(&rrect(dnd_r, 4.0), &brush(if st.hover == Some(Hit::Dnd) { rgb(0x48B1E8) } else { C_ACCENT })?);
        icon(0xF285, &g.icon16, dnd_r, &black);
    } else {
        if st.hover == Some(Hit::Dnd) {
            rt.FillRoundedRectangle(&rrect(dnd_r, 4.0), &brush(C_BTN)?);
        }
        icon(0xF2A3, &g.icon16, dnd_r, &white);
    }
    if !st.notes.is_empty() {
        let r = rect(257.0, 13.0, 319.0, 37.0);
        rt.FillRoundedRectangle(&rrect(r, 4.0), &brush(if st.hover == Some(Hit::ClearAll) { rgb(0x353535) } else { rgb(0x2E2E2E) })?);
        text("Clear all", &g.small_c, r, &white);
    }
    if st.dnd {
        icon(0xF285, &g.icon16, rect(17.0, 59.0, 35.0, 78.0), &white);
        text("Do not disturb is on", &g.body_sb, rect(39.0, 59.0, 320.0, 78.0), &white);
        rt.DrawText(&utf16("You\u{2019}ll only see banners for priority notifications and alarms."), &g.wrap, &rect(17.0, 78.0, 320.0, 118.0), &white, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        let link = brush(if st.hover == Some(Hit::Settings) { rgb(0x99EBFF) } else { C_ACCENT })?;
        text("Notification settings", &g.body, rect(17.0, 131.0, 200.0, 150.0), &link);
        rt.FillRectangle(&rect(17.0, 148.0, 142.0, 149.0), &link);
    }

    // list
    let lt = list_top(st.dnd);
    rt.PushAxisAlignedClip(&rect(0.0, lt, W, h), D2D1_ANTIALIAS_MODE_ALIASED);
    if st.notes.is_empty() {
        text("No new notifications", &g.body_c, rect(0.0, lt + 8.0, W, lt + 40.0), &brush(rgb(0xC5C5C5))?);
    }
    let bp = D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
    };
    for (i, (row, top, rh)) in st.rows.iter().enumerate() {
        let y = lt + top - st.scroll;
        if y > h || y + rh < lt {
            continue;
        }
        match row {
            Row::Group(gi) => {
                let grp = &st.groups[*gi];
                if let Some(px) = st.icons.get(&grp.aumid) {
                    if let Ok(b) = rt.CreateBitmap(D2D_SIZE_U { width: 16, height: 16 }, Some(px.as_ptr() as *const c_void), 64, &bp) {
                        crate::flyout::draw_icon(rt, &b, 17.0, y + 12.0, 16.0);
                    }
                }
                text(if grp.app.is_empty() { &grp.aumid } else { &grp.app }, &g.small, rect(40.0, y + 11.0, 280.0, y + 27.0), &white);
                if st.hover == Some(Hit::Group(*gi)) {
                    icon(0xE711, &g.icon12, rect(287.0, y, 335.0, y + 40.0), &white);
                }
            }
            Row::Card { note, more, body_h, .. } => {
                let n = &st.notes[*note];
                let hov = matches!(st.hover, Some(Hit::Card(j) | Hit::Dismiss(j)) if j == i);
                let card = rect(5.5, y + 3.5, 330.5, y + rh - 3.5);
                rt.FillRoundedRectangle(&rrect(card, 4.0), &brush(if hov { C_CARD_HOVER } else { C_CARD })?);
                rt.DrawRoundedRectangle(&rrect(card, 4.0), &brush(C_LINE)?, 1.0, None);
                text(&when(n.time), &g.small, rect(14.0, y + 10.0, 200.0, y + 26.0), &white);
                if hov {
                    if st.hover == Some(Hit::Dismiss(i)) {
                        rt.FillRoundedRectangle(&rrect(rect(295.0, y + 6.0, 327.0, y + 30.0), 4.0), &brush(C_BTN)?);
                    }
                    icon(0xE711, &g.icon12, rect(287.0, y + 2.0, 335.0, y + 34.0), &white);
                }
                let mut ty = y + 44.0;
                if !n.title.is_empty() {
                    text(&n.title, &g.body, rect(17.0, ty, 318.0, ty + 19.0), &white);
                    ty += 19.0;
                }
                if !n.body.is_empty() {
                    let l = g.dw.CreateTextLayout(&utf16(&n.body), &g.wrap, 301.0, *body_h)?;
                    let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
                    let _ = l.SetTrimming(&trim, &g.dw.CreateEllipsisTrimmingSign(&l)?);
                    rt.DrawTextLayout(windows_numerics::Vector2 { X: 17.0, Y: ty }, &l, &white, D2D1_DRAW_TEXT_OPTIONS_CLIP);
                }
                if *more > 0 {
                    let r = rect(17.0, y + rh - 40.0, 127.0, y + rh - 16.0);
                    rt.FillRoundedRectangle(&rrect(r, 4.0), &brush(if matches!(st.hover, Some(Hit::More(_))) && hov { rgb(0x3D3D3D) } else { rgb(0x373737) })?);
                    text(&format!("+{more} notification{}", if *more == 1 { "" } else { "s" }), &g.small_c, r, &white);
                }
            }
        }
    }
    rt.PopAxisAlignedClip();
    Ok(())
}

unsafe fn draw_cal(st: &State, g: &Gfx) -> windows::core::Result<()> {
    let rt = &g.rt;
    let h = cal_height(st);
    rt.Clear(Some(&crate::flyout::base(C_CARD)));
    let brush = |c: D2D1_COLOR_F| rt.CreateSolidColorBrush(&c, None);
    let white = brush(rgb(0xFFFFFF))?;
    let text = |s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush| {
        rt.DrawText(&utf16(s), f, &r, b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL)
    };
    let icon = |cp: u32, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush| {
        rt.DrawText(&glyph(cp), f, &r, b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL)
    };
    let button = |r: D2D_RECT_F, hov: bool| -> windows::core::Result<()> {
        rt.FillRoundedRectangle(&rrect(r, 4.0), &brush(if hov { C_BTN_HOVER } else { C_BTN })?);
        rt.DrawRoundedRectangle(&rrect(rect(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5), 4.0), &brush(C_BTN_EDGE)?, 1.0, None);
        Ok(())
    };
    let now = GetLocalTime();

    // date header + expand/collapse
    if st.cal_open {
        rt.FillRectangle(&rect(0.0, 0.0, W, 49.0), &brush(rgb(0x232323))?);
    }
    text(&format!("{}, {} {}", DAYS[now.wDayOfWeek as usize], MONTHS[now.wMonth as usize - 1], now.wDay), &g.body, rect(17.0, 16.0, 280.0, 35.0), &white);
    button(rect(295.0, 14.0, 319.0, 38.0), st.hover == Some(Hit::Caret))?;
    icon(if st.cal_open { 0xE70D } else { 0xE70E }, &g.icon12, rect(295.0, 14.0, 319.0, 38.0), &white);
    rt.FillRectangle(&rect(0.0, 49.0, W, 50.0), &brush(C_LINE)?);

    if st.cal_open {
        let (y, m) = shown_month(st.month_off);
        text(&format!("{} {}", MONTHS[m as usize - 1], y), &g.body_sb, rect(17.0, 61.0, 240.0, 97.0), &white);
        for (hit, x0, cp) in [(Hit::Prev, 254.0, 0xEDDB), (Hit::Next, 294.0, 0xEDDC)] {
            let r = rect(x0, 61.0, x0 + 34.0, 97.0);
            if st.hover == Some(hit) {
                rt.FillRoundedRectangle(&rrect(r, 4.0), &brush(C_BTN)?);
            }
            icon(cp, &g.icon8, r, &brush(C_ARROW)?);
        }
        for (i, d) in ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"].iter().enumerate() {
            let x = cell_x(i as i32);
            text(d, &g.small_sb_c, rect(x, 120.0, x + 41.0, 136.0), &white);
        }
        let first = weekday(y, m, 1);
        let dim = brush(C_DIM)?;
        let black = brush(rgb(0x000000))?;
        let (py, pm) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
        let prev_len = days_in(py, pm);
        let len = days_in(y, m);
        for cell in 0..42 {
            let (col, row) = (cell % 7, cell / 7);
            let d = cell - first + 1;
            let (label, inside) = if d < 1 { (prev_len + d, false) } else if d > len { (d - len, false) } else { (d, true) };
            let r = rect(cell_x(col), 152.0 + row as f32 * 42.0, cell_x(col) + 41.0, 193.0 + row as f32 * 42.0);
            let today = inside && st.month_off == 0 && d == now.wDay as i32;
            if today {
                let c = windows_numerics::Vector2 { X: (r.left + r.right) / 2.0, Y: (r.top + r.bottom) / 2.0 };
                rt.FillEllipse(&D2D1_ELLIPSE { point: c, radiusX: 20.0, radiusY: 20.0 }, &brush(C_ACCENT)?);
            }
            text(&label.to_string(), &g.body_c, r, if today { &black } else if inside { &white } else { &dim });
        }
    }

    // focus footer
    let fy = h - 50.0;
    rt.FillRectangle(&rect(0.0, fy - 1.0, W, fy), &brush(C_LINE)?);
    button(rect(17.0, fy + 13.0, 41.0, fy + 37.0), st.hover == Some(Hit::Minus))?;
    icon(0xE738, &g.icon12, rect(17.0, fy + 13.0, 41.0, fy + 37.0), &white);
    button(rect(122.0, fy + 13.0, 146.0, fy + 37.0), st.hover == Some(Hit::Plus))?;
    icon(0xE710, &g.icon12, rect(122.0, fy + 13.0, 146.0, fy + 37.0), &white);
    let mins = FOCUS_STEPS[st.focus_idx];
    let num = mins.to_string();
    text(&num, &g.body_sb, rect(57.0, fy + 16.0, 110.0, fy + 35.0), &white);
    text("mins", &g.body, rect(57.0 + 9.0 * num.len() as f32 + 4.0, fy + 16.0, 120.0, fy + 35.0), &white);
    let fr = rect(247.0, fy + 13.0, 319.0, fy + 37.0);
    button(fr, st.hover == Some(Hit::Focus))?;
    if st.focus_active {
        icon(0xE71A, &g.icon12, rect(253.0, fy + 13.0, 271.0, fy + 37.0), &white);
        text("Stop", &g.small, rect(275.0, fy + 14.0, 319.0, fy + 36.0), &white);
    } else {
        icon(0xF5B0, &g.icon12, rect(253.0, fy + 13.0, 271.0, fy + 37.0), &white);
        text("Focus", &g.small, rect(275.0, fy + 14.0, 319.0, fy + 36.0), &white);
    }
    Ok(())
}

unsafe fn paint(hwnd: HWND, cal: bool) {
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
            if st.gfx.is_none() {
                st.gfx = make_gfx().ok();
            }
            let ok = (|| -> windows::core::Result<()> {
                let g = st.gfx.as_ref().ok_or(windows::core::Error::from(E_FAIL))?;
                g.rt.BindDC(mem, &rc)?;
                g.rt.SetDpi(dpi, dpi);
                g.rt.BeginDraw();
                if cal { draw_cal(&st, g)?; } else { draw_nc(&st, g, rc.bottom as f32 * 96.0 / dpi)?; }
                g.rt.EndDraw(None, None)
            })();
            if ok.is_err() {
                st.gfx = None;
            }
        });
        let _ = EndBufferedPaint(pb, true);
    }
    let _ = EndPaint(hwnd, &ps);
}

// ======================= show / hide / input =======================

unsafe fn refresh_local(st: &mut State) {
    if let Some(n) = DATA.lock().unwrap().clone() {
        st.notes = n;
    }
    st.dnd = dnd_get();
    st.focus_active = focus_active();
    if st.gfx.is_none() {
        st.gfx = make_gfx().ok();
    }
    layout(st);
    let missing: Vec<String> = st.groups.iter().map(|g| g.aumid.clone()).filter(|a| !a.is_empty() && !st.icons.contains_key(a)).collect();
    if !missing.is_empty() {
        if let Ok(wic) = CoCreateInstance::<_, IWICImagingFactory>(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) {
            for a in missing {
                let px = crate::launcher::icon_pixels(&wic, &format!("shell:AppsFolder\\{a}"), 16).unwrap_or_default();
                st.icons.insert(a, px);
            }
        }
    }
    st.icons.retain(|_, v| v.len() == 16 * 16 * 4);
}

unsafe fn place() {
    let nc = HWND(NC.load(Relaxed) as *mut c_void);
    let cal = HWND(CAL.load(Relaxed) as *mut c_void);
    let mut wa = RECT::default();
    let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut wa as *mut _ as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    let s = GetDpiForWindow(nc) as f32 / 96.0;
    let px = |v: f32| (v * s).round() as i32;
    let (want, ch) = ST.with(|st| { let st = st.borrow(); (nc_height(&st), cal_height(&st)) });
    let x = wa.right - px(EDGE) - px(W);
    let cy = wa.bottom - px(EDGE) - px(ch);
    let bottom = cy - px(EDGE);
    let top = (bottom - px(want)).max(wa.top + px(8.0));
    ST.with(|st| {
        let mut st = st.borrow_mut();
        st.view_h = (bottom - top) as f32 / s;
        let max = (nc_height(&st) - st.view_h).max(0.0);
        st.scroll = st.scroll.clamp(0.0, max);
    });
    let _ = SetWindowPos(nc, Some(HWND_TOPMOST), x, top, px(W), bottom - top, SWP_SHOWWINDOW);
    let _ = SetWindowPos(cal, Some(HWND_TOPMOST), x, cy, px(W), px(ch), SWP_SHOWWINDOW | SWP_NOACTIVATE);
    let _ = InvalidateRect(Some(nc), None, false);
    let _ = InvalidateRect(Some(cal), None, false);
}

pub unsafe fn toggle(nc: HWND) {
    if IsWindowVisible(nc).as_bool() {
        hide();
        return;
    }
    let t0 = std::time::Instant::now();
    ST.with(|s| { let mut st = s.borrow_mut(); st.scroll = 0.0; st.month_off = 0; refresh_local(&mut st); });
    let t1 = t0.elapsed().as_millis();
    place();
    let t2 = t0.elapsed().as_millis();
    SHOWN_AT.with(|t| t.set(Some(std::time::Instant::now())));
    crate::flyout::force_foreground(nc);
    let _ = UpdateWindow(nc);
    crate::flyout::slide(&[nc, HWND(CAL.load(Relaxed) as *mut c_void)], true);
    fetch_async();
    // how long opening took on the UI thread (it also serves the taskbar): keep an eye on it
    let _ = std::fs::write(std::env::temp_dir().join("shellbits-nc-open.txt"), format!("state {t1} ms, placed {t2} ms, focused {} ms
", t0.elapsed().as_millis()));
}

unsafe fn hide() {
    crate::flyout::slide(&[HWND(NC.load(Relaxed) as *mut c_void), HWND(CAL.load(Relaxed) as *mut c_void)], false);
    let _ = ShowWindow(HWND(NC.load(Relaxed) as *mut c_void), SW_HIDE);
    let _ = ShowWindow(HWND(CAL.load(Relaxed) as *mut c_void), SW_HIDE);
    crate::flyout::give_back();
    let _ = windows::Win32::System::ProcessStatus::K32EmptyWorkingSet(windows::Win32::System::Threading::GetCurrentProcess());
}

fn dips(hwnd: HWND, lp: LPARAM) -> (f32, f32) {
    let s = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0;
    ((lp.0 & 0xFFFF) as i16 as f32 / s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s)
}

unsafe fn track(hwnd: HWND) {
    let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
    let _ = TrackMouseEvent(&mut tme);
}

pub unsafe extern "system" fn nc_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let redraw = || {
        let _ = InvalidateRect(Some(hwnd), None, false);
    };
    match msg {
        WM_NC_TOGGLE => toggle(hwnd),
        crate::flyout::WM_DISMISS => hide(),
        WM_NC_DATA => {
            ST.with(|s| refresh_local(&mut s.borrow_mut()));
            if IsWindowVisible(hwnd).as_bool() {
                place();
            }
        }
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            // the Win key's release can steal focus right after we open: take it back once
            let fresh = SHOWN_AT.with(|t| t.get()).is_some_and(|t| t.elapsed().as_millis() < 400);
            if fresh {
                let _ = PostMessageW(Some(hwnd), WM_NC_REFOCUS, WPARAM(0), LPARAM(0));
            } else if lp.0 as isize != CAL.load(Relaxed) {
                hide();
            }
        }
        WM_NC_REFOCUS => {
            crate::flyout::force_foreground(hwnd);
        }
        WM_KEYDOWN if wp.0 as u16 == VK_ESCAPE.0 => hide(),
        WM_MOUSEMOVE => {
            track(hwnd);
            let (x, y) = dips(hwnd, lp);
            if ST.with(|s| { let mut st = s.borrow_mut(); let h = nc_hit(&st, x, y); std::mem::replace(&mut st.hover, h) != h }) {
                redraw();
            }
        }
        WM_MOUSELEAVE => {
            ST.with(|s| s.borrow_mut().hover = None);
            redraw();
        }
        WM_MOUSEWHEEL => {
            let notches = ((wp.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
            ST.with(|s| {
                let mut st = s.borrow_mut();
                let max = (nc_height(&st) - st.view_h).max(0.0);
                st.scroll = (st.scroll - notches * 48.0).clamp(0.0, max);
            });
            redraw();
        }
        WM_LBUTTONUP => {
            let (x, y) = dips(hwnd, lp);
            let hit = ST.with(|s| { let st = s.borrow(); nc_hit(&st, x, y) });
            match hit {
                Some(Hit::Dnd) => {
                    let on = !dnd_get();
                    dnd_set(on);
                    ST.with(|s| { let mut st = s.borrow_mut(); st.dnd = on; });
                    place();
                }
                Some(Hit::ClearAll) => {
                    remove(None);
                    ST.with(|s| { let mut st = s.borrow_mut(); st.notes.clear(); layout(&mut st); });
                    *DATA.lock().unwrap() = Some(Vec::new());
                    place();
                }
                Some(Hit::Settings) => { hide(); open("ms-settings:notifications"); }
                Some(Hit::Group(gi)) if x >= 287.0 => {
                    let ids: Vec<u32> = ST.with(|s| { let st = s.borrow(); st.groups.get(gi).map(|g| g.notes.iter().map(|&n| st.notes[n].id).collect()).unwrap_or_default() });
                    drop_notes(&ids);
                }
                Some(Hit::Dismiss(i)) => {
                    let id = ST.with(|s| { let st = s.borrow(); match st.rows.get(i) { Some((Row::Card { note, .. }, _, _)) => Some(st.notes[*note].id), _ => None } });
                    if let Some(id) = id {
                        drop_notes(&[id]);
                    }
                }
                Some(Hit::More(gi)) => {
                    ST.with(|s| { let mut st = s.borrow_mut(); if let Some(a) = st.groups.get(gi).map(|g| g.aumid.clone()) { st.expanded.insert(a); } layout(&mut st); });
                    place();
                }
                Some(Hit::Card(i)) => {
                    // open the app that sent it (Windows would hand the toast's arguments to it)
                    let a = ST.with(|s| { let st = s.borrow(); match st.rows.get(i) { Some((Row::Card { note, .. }, _, _)) => st.notes[*note].aumid.clone(), _ => String::new() } });
                    if !a.is_empty() {
                        hide();
                        open(&format!("shell:AppsFolder\\{a}"));
                    }
                }
                _ => {}
            }
            redraw();
        }
        WM_MOUSEACTIVATE => return LRESULT(MA_ACTIVATE as isize),
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => paint(hwnd, false),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

unsafe fn drop_notes(ids: &[u32]) {
    for &id in ids {
        remove(Some(id));
    }
    ST.with(|s| {
        let mut st = s.borrow_mut();
        st.notes.retain(|n| !ids.contains(&n.id));
        *DATA.lock().unwrap() = Some(st.notes.clone());
        layout(&mut st);
    });
    place();
}

pub unsafe extern "system" fn cal_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let redraw = || {
        let _ = InvalidateRect(Some(hwnd), None, false);
    };
    match msg {
        WM_MOUSEMOVE => {
            track(hwnd);
            let (x, y) = dips(hwnd, lp);
            if ST.with(|s| { let mut st = s.borrow_mut(); let h = cal_hit(&st, x, y); std::mem::replace(&mut st.hover, h) != h }) {
                redraw();
            }
        }
        WM_MOUSELEAVE => {
            ST.with(|s| s.borrow_mut().hover = None);
            redraw();
        }
        WM_MOUSEWHEEL => {
            let notches = ((wp.0 >> 16) & 0xFFFF) as i16 as i32;
            ST.with(|s| { let mut st = s.borrow_mut(); if st.cal_open { st.month_off -= notches.signum(); } });
            redraw();
        }
        WM_LBUTTONUP => {
            let (x, y) = dips(hwnd, lp);
            let hit = ST.with(|s| { let st = s.borrow(); cal_hit(&st, x, y) });
            match hit {
                Some(Hit::Caret) => {
                    ST.with(|s| { let mut st = s.borrow_mut(); st.cal_open = !st.cal_open; st.month_off = 0; });
                    place();
                }
                Some(Hit::Prev) => ST.with(|s| s.borrow_mut().month_off -= 1),
                Some(Hit::Next) => ST.with(|s| s.borrow_mut().month_off += 1),
                Some(Hit::Minus) => ST.with(|s| { let mut st = s.borrow_mut(); st.focus_idx = st.focus_idx.saturating_sub(1); }),
                Some(Hit::Plus) => ST.with(|s| { let mut st = s.borrow_mut(); st.focus_idx = (st.focus_idx + 1).min(FOCUS_STEPS.len() - 1); }),
                Some(Hit::Focus) => {
                    let mins = ST.with(|s| { let mut st = s.borrow_mut(); st.focus_active = !st.focus_active; FOCUS_STEPS[st.focus_idx] });
                    focus_toggle(mins);
                }
                _ => {}
            }
            redraw();
        }
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => paint(hwnd, true),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

/// `shellbits --snapshot-nc out.bmp [collapsed]`: notifications panel + calendar with live data, stacked.
pub unsafe fn snapshot(path: &str, collapsed: bool) -> windows::core::Result<()> {
    let notes = std::thread::spawn(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        fetch()
    })
    .join()
    .unwrap_or_default();
    *DATA.lock().unwrap() = Some(notes);
    ST.with(|s| -> windows::core::Result<()> {
        let mut st = s.borrow_mut();
        st.cal_open = !collapsed;
        st.gfx = Some(make_gfx()?);
        refresh_local(&mut st);
        // same room as on a 1080p screen: up to 900px for the panel
        let ch = cal_height(&st);
        let nh = nc_height(&st).min(1032.0 - EDGE - ch - EDGE - 8.0);
        let (w, h) = (W as i32, (nh + EDGE + ch) as i32);
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, ..Default::default() },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let dib = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let dc = CreateCompatibleDC(None);
        let old = SelectObject(dc, dib.into());
        let g = st.gfx.as_ref().unwrap();
        g.rt.BindDC(dc, &RECT { left: 0, top: 0, right: w, bottom: nh as i32 })?;
        g.rt.SetDpi(96.0, 96.0);
        g.rt.BeginDraw();
        draw_nc(&st, g, nh)?;
        g.rt.EndDraw(None, None)?;
        let _ = SetViewportOrgEx(dc, 0, (nh + EDGE) as i32, None);
        g.rt.BindDC(dc, &RECT { left: 0, top: 0, right: w, bottom: ch as i32 })?;
        g.rt.BeginDraw();
        draw_cal(&st, g)?;
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
    #[test]
    fn calendar_math() {
        assert_eq!(super::weekday(2026, 11, 1), 0); // Nov 1 2026 is a Sunday (matches Windows' grid)
        assert_eq!(super::weekday(2026, 11, 10), 2); // Tuesday
        assert_eq!(super::days_in(2028, 2), 29);
        assert_eq!(super::days_in(2100, 2), 28);
    }
}

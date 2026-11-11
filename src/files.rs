//! File Explorer in Rust (Win+E / `shellbits --files [path]`): toolbar with back / forward / up /
//! refresh and an editable address box, a left pane with your folders and drives, and the details
//! list (name, date modified, type, size) with real shell icons, column sorting, keyboard and
//! double-click navigation, and Windows' own right-click menus for files and folders.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::os::windows::fs::MetadataExt;
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
static EDIT: AtomicIsize = AtomicIsize::new(0);
static EDIT_PROC: AtomicIsize = AtomicIsize::new(0);
static SEARCH: AtomicIsize = AtomicIsize::new(0);
const ROW: f32 = 28.0;
const THIS_PC: &str = "This PC";

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    pub modified: u64, // FILETIME
    pub kind: String,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Back,
    Forward,
    Up,
    Refresh,
    Place(usize),
    Column(usize),
    Row(usize),
    Crumb(usize),
    CrumbBox,
    Search,
    Cmd(u8), // 0 new folder, 1 cut, 2 copy, 3 paste, 4 rename, 5 delete, 6 sort
    Win(u8), // minimise, maximise, close
    Tab(usize),
    TabX(usize),
    NewTab,
}

#[derive(Default)]
struct Ui {
    path: String,
    all: Vec<Entry>,     // everything in the folder
    entries: Vec<Entry>, // what's listed (after the search filter)
    filter: String,
    editing: bool,           // address box is in typing mode
    searching: bool,         // search box has focus
    renaming: Option<usize>, // row being renamed in place
    view: u8,                // 0 details, 1 large icons, 2 list. ponytail: not remembered between runs
    big: HashMap<String, (u32, u32, Vec<u8>)>, // 48px icons and picture thumbnails for the icons view
    no_preview: bool,        // preview pane closed. ponytail: starts open, not saved; persist when someone asks
    thumb: Option<(String, u32, u32, Vec<u8>)>, // preview of the selected file: path, width, height, pixels
    tabs: Vec<(String, Vec<String>, Vec<String>)>, // every tab's folder, back and forward lists (the active one's live copy is in path/back/fwd)
    tab: usize,
    nav_scroll: f32,
    more: Vec<usize>,        // extra selected rows (Ctrl / Shift click); `sel` is the anchor
    places: Vec<(String, String, u32)>, // label, path, glyph
    sort: (usize, bool),               // column, ascending
    sel: Option<usize>,
    hover: Option<Hit>,
    scroll: f32,
    back: Vec<String>,
    fwd: Vec<String>,
    hits: Vec<(D2D_RECT_F, Hit)>,
    icons: HashMap<String, Vec<u8>>,
    gfx: Option<(ID2D1DCRenderTarget, IDWriteFactory)>,
}

thread_local! {
    static UI: RefCell<Ui> = RefCell::new(Ui { sort: (0, true), ..Default::default() });
}

// ======================= data =======================

fn known(id: *const windows::core::GUID) -> Option<String> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }
}

pub const HOME: &str = "Home";
static PINNED: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());
/// Home's recent files: name, full path, size, when it was last opened.
static RECENT: std::sync::Mutex<Vec<(String, String, u64, u64)>> = std::sync::Mutex::new(Vec::new());

/// The files you opened most recently, newest first: Windows keeps a shortcut per opened item in its
/// Recent folder; each one is resolved to its target and kept if that is a file that still exists.
unsafe fn recent_files(max: usize) -> Vec<(String, String, u64, u64)> {
    use windows::Win32::System::Com::{IPersistFile, STGM_READ};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    let dir = format!("{}\\Microsoft\\Windows\\Recent", std::env::var("APPDATA").unwrap_or_default());
    let mut links: Vec<(u64, std::path::PathBuf)> = std::fs::read_dir(dir).into_iter().flatten().flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")))
        .filter_map(|e| Some((e.metadata().ok()?.last_write_time(), e.path())))
        .collect();
    links.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out: Vec<(String, String, u64, u64)> = Vec::new();
    // ponytail: looks at the newest 80 shortcuts only; plenty for a list of 20
    for (when, lnk) in links.into_iter().take(80) {
        if out.len() >= max {
            break;
        }
        let Ok(link) = CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER) else { break };
        let Ok(file) = windows::core::Interface::cast::<IPersistFile>(&link) else { break };
        if file.Load(&HSTRING::from(lnk.as_os_str()), STGM_READ).is_err() {
            continue;
        }
        let mut buf = [0u16; 520];
        if link.GetPath(&mut buf, std::ptr::null_mut(), 0).is_err() {
            continue;
        }
        let target = String::from_utf16_lossy(&buf[..buf.iter().position(|c| *c == 0).unwrap_or(0)]);
        let path = std::path::Path::new(&target);
        let (Some(name), Ok(m)) = (path.file_name().map(|n| n.to_string_lossy().to_string()), std::fs::metadata(path)) else { continue };
        if m.is_file() && !out.iter().any(|r| r.0 == name) {
            out.push((name, target, m.len(), when));
        }
    }
    out
}

/// Where item `i` of `n` sits in the list area before scrolling, as (x, y, width, height) from the
/// list's top-left corner, and how tall the whole list is. `view`: 0 details, 1 large icons, 2 list.
// ponytail: the list view fills rows left to right and scrolls down; Windows' fills columns and scrolls sideways
pub fn cell(view: u8, i: usize, n: usize, width: f32) -> ((f32, f32, f32, f32), f32) {
    let (w, h) = match view {
        1 => (104.0, 112.0),
        2 => (260.0, 24.0),
        _ => (width, ROW),
    };
    let cols = if view == 0 { 1 } else { ((width / w).floor() as usize).max(1) };
    (((i % cols) as f32 * w, (i / cols) as f32 * h, w, h), n.div_ceil(cols) as f32 * h)
}

/// Width the items have to lay themselves out in, for a window `cw` wide.
fn list_width(ui: &Ui, cw: f32) -> f32 {
    if ui.view == 0 { COLS_END - 4.0 - COLS[0].1 } else { (if ui.no_preview { cw - 14.0 } else { cw - 290.0 }) - COLS[0].1 }
}

/// Your pinned folders: Windows' own Quick access list (name, path).
unsafe fn quick_access() -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    let mut pidl = std::ptr::null_mut();
    if SHParseDisplayName(w!("shell:::{679f85cb-0220-4080-b29b-5540cc05aab6}"), None, &mut pidl, 0, None).is_err() {
        return out;
    }
    if let Ok(folder) = SHBindToObject::<Option<&IShellFolder>, Option<&IBindCtx>, IShellFolder>(None, pidl, None) {
        let mut en = None;
        let _ = folder.EnumObjects(HWND::default(), SHCONTF_FOLDERS.0 as u32, &mut en);
        // Windows marks the ones you pinned (the rest are just frequent folders)
        let mut key = windows::Win32::Foundation::PROPERTYKEY::default();
        let pin_key = windows::Win32::UI::Shell::PropertiesSystem::PSGetPropertyKeyFromName(w!("System.Home.IsPinned"), &mut key).is_ok();
        let f2: Option<IShellFolder2> = windows::core::Interface::cast(&folder).ok();
        while let Some(e) = en.as_ref() {
            let mut item = [std::ptr::null_mut(); 1];
            let mut got = 0;
            if e.Next(&mut item, Some(&mut got)).is_err() || got == 0 {
                break;
            }
            let name = |flags: SHGDNF| -> String {
                let mut sr = windows::Win32::UI::Shell::Common::STRRET::default();
                if folder.GetDisplayNameOf(item[0], flags, &mut sr).is_ok() {
                    let mut pw = windows::core::PWSTR::null();
                    if StrRetToStrW(&mut sr, Some(item[0]), &mut pw).is_ok() {
                        let v = pw.to_string().unwrap_or_default();
                        CoTaskMemFree(Some(pw.0 as *const c_void));
                        return v;
                    }
                }
                String::new()
            };
            let (label, path) = (name(SHGDN_NORMAL), name(SHGDN_FORPARSING));
            let pinned = pin_key && f2.as_ref().and_then(|f| f.GetDetailsEx(item[0], &key).ok()).and_then(|v| bool::try_from(&v).ok()).unwrap_or(false);
            CoTaskMemFree(Some(item[0] as *const c_void));
            if !label.is_empty() && std::path::Path::new(&path).is_dir() {
                out.push((label, path, pinned));
            }
        }
    }
    CoTaskMemFree(Some(pidl as *const c_void));
    out
}

fn reg_str(key: &str, name: PCWSTR) -> Option<String> {
    use windows::Win32::System::Registry::*;
    let mut buf = [0u16; 520];
    let mut size = 1040u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, &HSTRING::from(key), name, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut c_void), Some(&mut size)).ok().ok()? };
    Some(String::from_utf16_lossy(&buf[..buf.iter().position(|c| *c == 0).unwrap_or(0)]))
}

static NS_ICONS: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

/// Extra top-level entries apps add to the left pane (phone link's "me", cloud drives...):
/// the ones registered for this user and marked as pinned to the tree. Returns (label, folder, icon source).
unsafe fn namespaces() -> Vec<(String, String, String)> {
    use windows::Win32::System::Registry::*;
    let mut out = Vec::new();
    let mut h = HKEY::default();
    if RegOpenKeyExW(HKEY_CURRENT_USER, w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Desktop\NameSpace"), None, KEY_READ, &mut h).is_err() {
        return out;
    }
    for i in 0.. {
        let mut name = [0u16; 64];
        let mut len = 64u32;
        if RegEnumKeyExW(h, i, Some(windows::core::PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None).is_err() {
            break;
        }
        let clsid = String::from_utf16_lossy(&name[..len as usize]);
        let key = format!(r"Software\Classes\CLSID\{clsid}");
        if crate::qsx::reg_dword(PCWSTR(HSTRING::from(key.as_str()).as_ptr()), w!("System.IsPinnedToNameSpaceTree")) != Some(1) {
            continue;
        }
        if let (Some(label), Some(target)) = (reg_str(&key, PCWSTR::null()), reg_str(&format!(r"{key}\Instance\InitPropertyBag"), w!("TargetFolderPath"))) {
            if std::path::Path::new(&target).is_dir() {
                out.push((label, target, format!("shell:::{clsid}")));
            }
        }
    }
    let _ = RegCloseKey(h);
    out
}

/// Left pane rows: (label, path, kind) with kind 0 = plain, 1 = pinned, 2 = drive, 3 = frequent folder, 4 = app-added entry, 9 = separator.
fn places() -> Vec<(String, String, u32)> {
    let pinned = unsafe { quick_access() };
    *PINNED.lock().unwrap() = pinned.iter().map(|(n, p, _)| (n.clone(), p.clone())).collect();
    *RECENT.lock().unwrap() = unsafe { recent_files(20) };
    let mut v = vec![(HOME.to_string(), HOME.to_string(), 0)];
    // ponytail: Gallery opens your Pictures folder as a list; Windows' photo-timeline view isn't built yet
    if let Some(pics) = known(&FOLDERID_Pictures) {
        v.push(("Gallery".into(), pics, 0));
    }
    v.push((String::new(), String::new(), 9));
    v.extend(pinned.into_iter().map(|(n, p, pin)| (n, p, if pin { 1 } else { 3 })));
    v.push((String::new(), String::new(), 9));
    let ns = unsafe { namespaces() };
    *NS_ICONS.lock().unwrap() = ns.iter().map(|(_, t, i)| (t.clone(), i.clone())).collect();
    v.extend(ns.into_iter().map(|(n, t, _)| (n, t, 4)));
    v.push((THIS_PC.into(), THIS_PC.into(), 0));
    for d in drives() {
        v.push((d.name.clone(), d.name[d.name.len() - 3..d.name.len() - 1].to_string() + "\\", 2));
    }
    v
}

/// Drives as entries named like Explorer: "Local Disk (C:)".
fn drives() -> Vec<Entry> {
    let mut out = Vec::new();
    unsafe {
        let mask = windows::Win32::Storage::FileSystem::GetLogicalDrives();
        for i in 0..26 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let root = format!("{}:\\", (b'A' + i as u8) as char);
            let mut label = [0u16; 64];
            let _ = windows::Win32::Storage::FileSystem::GetVolumeInformationW(&HSTRING::from(root.as_str()), Some(&mut label), None, None, None, None);
            let label = String::from_utf16_lossy(&label[..label.iter().position(|&c| c == 0).unwrap_or(0)]);
            let (mut free, mut total) = (0u64, 0u64);
            let _ = windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(&HSTRING::from(root.as_str()), None, Some(&mut total), Some(&mut free));
            out.push(Entry {
                name: format!("{} ({}:)", if label.is_empty() { "Local Disk" } else { &label }, (b'A' + i as u8) as char),
                dir: true,
                size: total,
                modified: 0,
                kind: format!("{:.0} GB free of {:.0} GB", free as f64 / 1073741824.0, total as f64 / 1073741824.0),
            });
        }
    }
    out
}

fn type_name(path: &str, dir: bool, cache: &mut HashMap<String, String>) -> String {
    if dir {
        return "File folder".into();
    }
    let ext = std::path::Path::new(path).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if let Some(t) = cache.get(&ext) {
        return t.clone();
    }
    let mut info = SHFILEINFOW::default();
    unsafe {
        SHGetFileInfoW(&HSTRING::from(format!("x.{ext}")), windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL, Some(&mut info), size_of::<SHFILEINFOW>() as u32, SHGFI_TYPENAME | SHGFI_USEFILEATTRIBUTES);
    }
    let t = String::from_utf16_lossy(&info.szTypeName[..info.szTypeName.iter().position(|&c| c == 0).unwrap_or(0)]);
    cache.insert(ext, t.clone());
    t
}

const ADVANCED: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced");
/// Windows' own folder-view switches (shared with the real Explorer): Hidden == 1 shows hidden items,
/// HideFileExt == 1 hides extensions.
fn show_hidden() -> bool {
    crate::qsx::reg_dword(ADVANCED, w!("Hidden")) == Some(1)
}
fn hide_ext() -> bool {
    crate::qsx::reg_dword(ADVANCED, w!("HideFileExt")) == Some(1)
}

pub fn list(path: &str) -> Vec<Entry> {
    if path == THIS_PC {
        return drives();
    }
    if path == HOME {
        // your pinned folders, then the files you opened most recently (like Windows' Home)
        let mut v: Vec<Entry> = PINNED.lock().unwrap().iter().map(|(n, _)| Entry { name: n.clone(), dir: true, size: 0, modified: 0, kind: "File folder".into() }).collect();
        let mut types = HashMap::new();
        v.extend(RECENT.lock().unwrap().iter().map(|(n, p, size, when)| Entry { name: n.clone(), dir: false, size: *size, modified: *when, kind: type_name(p, false, &mut types) }));
        return v;
    }
    let mut types = HashMap::new();
    let mut out = Vec::new();
    let skip = if show_hidden() { 0x4 } else { 0x6 }; // system files stay hidden either way
    for e in std::fs::read_dir(path).into_iter().flatten().flatten() {
        let Ok(m) = e.metadata() else { continue };
        if m.file_attributes() & skip != 0 {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        let full = e.path().display().to_string();
        out.push(Entry { kind: type_name(&full, m.is_dir(), &mut types), name, dir: m.is_dir(), size: m.len(), modified: m.last_write_time() });
    }
    out
}

/// Folders first (always), then by the chosen column.
pub fn sort(v: &mut [Entry], col: usize, asc: bool) {
    v.sort_by(|a, b| {
        let o = match col {
            1 => a.modified.cmp(&b.modified),
            2 => a.kind.to_lowercase().cmp(&b.kind.to_lowercase()),
            3 => a.size.cmp(&b.size),
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        };
        b.dir.cmp(&a.dir).then(if asc { o } else { o.reverse() })
    });
}

pub fn size_text(bytes: u64) -> String {
    if bytes == 0 {
        return "0 KB".into();
    }
    let kb = bytes.div_ceil(1024);
    let s = kb.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out + " KB"
}

fn date_text(ft: u64) -> String {
    if ft == 0 {
        return String::new();
    }
    unsafe {
        let f = FILETIME { dwLowDateTime: ft as u32, dwHighDateTime: (ft >> 32) as u32 };
        let (mut utc, mut loc) = (SYSTEMTIME::default(), SYSTEMTIME::default());
        if windows::Win32::System::Time::FileTimeToSystemTime(&f, &mut utc).is_err() || windows::Win32::System::Time::SystemTimeToTzSpecificLocalTime(None, &utc, &mut loc).is_err() {
            return String::new();
        }
        let h = loc.wHour % 12;
        format!("{}/{}/{} {}:{:02} {}", loc.wMonth, loc.wDay, loc.wYear, if h == 0 { 12 } else { h }, loc.wMinute, if loc.wHour < 12 { "AM" } else { "PM" })
    }
}

fn full(path: &str, e: &Entry) -> String {
    if path == HOME {
        if !e.dir {
            return RECENT.lock().unwrap().iter().find(|r| r.0 == e.name).map(|r| r.1.clone()).unwrap_or_default();
        }
        return PINNED.lock().unwrap().iter().find(|(n, _)| *n == e.name).map(|(_, p)| p.clone()).unwrap_or_default();
    }
    if path == THIS_PC {
        // "Local Disk (C:)" -> "C:\"
        let n = &e.name;
        return n[n.len() - 3..n.len() - 1].to_string() + "\\";
    }
    std::path::Path::new(path).join(&e.name).display().to_string()
}

pub fn parent(path: &str) -> String {
    if path == THIS_PC || path == HOME {
        return THIS_PC.into();
    }
    match std::path::Path::new(path).parent() {
        Some(p) if !p.as_os_str().is_empty() => p.display().to_string(),
        _ => THIS_PC.into(),
    }
}

// ======================= navigation =======================

fn load(ui: &mut Ui) {
    ui.all = list(&ui.path);
    refilter(ui);
    ui.scroll = 0.0;
    unsafe {
        let name = if ui.path == THIS_PC { THIS_PC.to_string() } else { std::path::Path::new(&ui.path).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| ui.path.clone()) };
        let _ = SetWindowTextW(HWND(WIN.load(Relaxed) as *mut c_void), &HSTRING::from(name));
    }
}

fn refilter(ui: &mut Ui) {
    let q = ui.filter.to_lowercase();
    ui.entries = ui.all.iter().filter(|e| q.is_empty() || e.name.to_lowercase().contains(&q)).cloned().collect();
    sort(&mut ui.entries, ui.sort.0, ui.sort.1);
    ui.sel = None;
    ui.more.clear();
}

/// Every selected row, anchor first.
fn selected(ui: &Ui) -> Vec<usize> {
    let mut v: Vec<usize> = ui.sel.into_iter().collect();
    v.extend(ui.more.iter().copied().filter(|i| Some(*i) != ui.sel));
    v
}

/// What a click does to the selection: plain = just that row, Ctrl = toggle it, Shift = range from the anchor.
pub fn click_select(sel: Option<usize>, more: &[usize], i: usize, ctrl: bool, shift: bool) -> (Option<usize>, Vec<usize>) {
    if shift {
        if let Some(a) = sel {
            return (Some(a), (a.min(i)..=a.max(i)).filter(|x| *x != a).collect());
        }
    }
    if ctrl {
        let mut m: Vec<usize> = more.to_vec();
        match (sel, m.iter().position(|x| *x == i)) {
            (Some(a), _) if a == i => return (m.pop(), m), // un-select the anchor: another row becomes it
            (_, Some(k)) => {
                m.remove(k);
            }
            (None, None) => return (Some(i), m),
            (Some(_), None) => m.push(i),
        }
        return (sel, m);
    }
    (Some(i), Vec::new())
}

/// A free "New folder" / "New folder (2)" ... name in `dir`.
pub fn new_folder_name(dir: &str) -> String {
    let base = std::path::Path::new(dir);
    (1..).map(|n| if n == 1 { "New folder".to_string() } else { format!("New folder ({n})") }).find(|n| !base.join(n).exists()).unwrap()
}

/// Run one of Windows' own file commands ("cut", "copy", "paste", "delete") on the given items of
/// `folder` (all at once, like a multi-selection), or on the folder itself when `items` is empty.
unsafe fn shell_verb(hwnd: HWND, folder: &str, items: &[String], verb: &'static [u8]) {
    let mut pidls = Vec::new();
    for t in if items.is_empty() { std::slice::from_ref(&folder.to_string()).to_vec() } else { items.to_vec() } {
        let mut pidl = std::ptr::null_mut();
        if SHParseDisplayName(&HSTRING::from(t.as_str()), None, &mut pidl, 0, None).is_ok() {
            pidls.push(pidl);
        }
    }
    let Some(first) = pidls.first().copied() else { return };
    let menu: Option<IContextMenu> = if items.is_empty() {
        SHBindToObject::<Option<&IShellFolder>, Option<&IBindCtx>, IShellFolder>(None, first, None).ok().and_then(|f| f.CreateViewObject(hwnd).ok())
    } else {
        let children: Vec<*const windows::Win32::UI::Shell::Common::ITEMIDLIST> = pidls.iter().map(|p| ILFindLastID(*p) as *const _).collect();
        SHBindToParent::<IShellFolder>(first, None).ok().and_then(|f| f.GetUIObjectOf(hwnd, &children, None).ok())
    };
    if let (Some(cm), Ok(m)) = (menu, CreatePopupMenu()) {
        if cm.QueryContextMenu(m, 0, 1, 0x7FFF, CMF_NORMAL).is_ok() {
            let ici = CMINVOKECOMMANDINFO { cbSize: size_of::<CMINVOKECOMMANDINFO>() as u32, hwnd, lpVerb: windows::core::PCSTR(verb.as_ptr()), nShow: SW_SHOWNORMAL.0, ..Default::default() };
            let _ = cm.InvokeCommand(&ici);
        }
        let _ = DestroyMenu(m);
    }
    for p in pidls {
        CoTaskMemFree(Some(p as *const c_void));
    }
}

fn go(ui: &mut Ui, path: String, remember: bool) {
    if path != THIS_PC && path != HOME && !std::path::Path::new(&path).is_dir() {
        return;
    }
    if remember && path != ui.path {
        let old = std::mem::take(&mut ui.path);
        ui.back.push(old);
        ui.fwd.clear();
    }
    ui.path = path;
    ui.filter.clear();
    unsafe {
        let _ = SetWindowTextW(HWND(SEARCH.load(Relaxed) as *mut c_void), w!(""));
    }
    load(ui);
}

/// Which tab is active after closing tab `k` of `n`; None when it was the last one (the window closes).
pub fn after_close(n: usize, active: usize, k: usize) -> Option<usize> {
    if n <= 1 {
        return None;
    }
    Some(if k < active || (k == active && active == n - 1) { active - 1 } else { active })
}

fn switch_tab(ui: &mut Ui, k: usize) {
    if k == ui.tab || k >= ui.tabs.len() {
        return;
    }
    let cur = ui.tab;
    ui.tabs[cur] = (ui.path.clone(), std::mem::take(&mut ui.back), std::mem::take(&mut ui.fwd));
    ui.tab = k;
    let (p, b, f) = ui.tabs[k].clone();
    (ui.back, ui.fwd) = (b, f);
    go(ui, p, false);
}

fn new_tab(ui: &mut Ui) {
    ui.tabs.push((HOME.to_string(), Vec::new(), Vec::new()));
    switch_tab(ui, ui.tabs.len() - 1);
}

/// Close tab `k`; true means it was the only one, so the window should close.
fn close_tab(ui: &mut Ui, k: usize) -> bool {
    let Some(a) = after_close(ui.tabs.len(), ui.tab, k) else { return true };
    let was_active = k == ui.tab;
    ui.tabs.remove(k);
    ui.tab = a;
    if was_active {
        let (p, b, f) = ui.tabs[a].clone();
        (ui.back, ui.fwd) = (b, f);
        go(ui, p, false);
    }
    false
}

fn tab_title(path: &str) -> String {
    if path == THIS_PC || path == HOME {
        return path.to_string();
    }
    std::path::Path::new(path).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string())
}

unsafe fn open(ui: &mut Ui, i: usize) {
    let Some(e) = ui.entries.get(i).cloned() else { return };
    let p = full(&ui.path, &e);
    if e.dir {
        go(ui, p, true);
    } else {
        // On its own thread: starting a program can show a prompt (UAC, SmartScreen, "open with") that
        // runs a message loop. Done here, with the window's state borrowed, that re-entered the window
        // procedure and the double borrow ended the whole process - opening an .exe crashed the shell.
        let dir = ui.path.clone();
        std::thread::spawn(move || {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
            ShellExecuteW(None, w!("open"), &HSTRING::from(p), PCWSTR::null(), &HSTRING::from(dir.as_str()), SW_SHOWNORMAL);
        });
    }
}

/// Windows' own context menu for a file or folder (or for the folder background when `item` is None).
unsafe fn context_menu(hwnd: HWND, folder: &str, item: Option<&str>, pt: POINT) {
    let target = item.unwrap_or(folder);
    let mut pidl = std::ptr::null_mut();
    if SHParseDisplayName(&HSTRING::from(target), None, &mut pidl, 0, None).is_err() {
        return;
    }
    let menu: Option<IContextMenu> = if item.is_some() {
        let mut child = std::ptr::null_mut();
        SHBindToParent::<IShellFolder>(pidl, Some(&mut child)).ok().and_then(|f| f.GetUIObjectOf(hwnd, &[child as *const _], None).ok())
    } else {
        SHBindToObject::<Option<&IShellFolder>, Option<&IBindCtx>, IShellFolder>(None, pidl, None).ok().and_then(|f| f.CreateViewObject(hwnd).ok())
    };
    if let (Some(cm), Ok(m)) = (menu, CreatePopupMenu()) {
        if cm.QueryContextMenu(m, 0, 1, 0x7FFF, CMF_NORMAL | CMF_EXPLORE).is_ok() {
            let cmd = TrackPopupMenuEx(m, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, pt.x, pt.y, hwnd, None).0;
            if cmd > 0 {
                let ici = CMINVOKECOMMANDINFOEX {
                    cbSize: size_of::<CMINVOKECOMMANDINFOEX>() as u32,
                    fMask: 0x4000 | CMIC_MASK_PTINVOKE, // CMIC_MASK_UNICODE
                    hwnd,
                    lpVerb: windows::core::PCSTR((cmd - 1) as usize as *const u8),
                    lpVerbW: PCWSTR((cmd - 1) as usize as *const u16),
                    nShow: SW_SHOWNORMAL.0,
                    ptInvoke: pt,
                    ..Default::default()
                };
                let _ = cm.InvokeCommand(&ici as *const _ as *const CMINVOKECOMMANDINFO);
            }
        }
        let _ = DestroyMenu(m);
    }
    CoTaskMemFree(Some(pidl as *const c_void));
}

// ======================= drawing (measured from Windows' File Explorer at 100%) =======================

const TAB: f32 = 40.0; // tab strip
const TOOL: f32 = 88.0; // bottom of the address row
const CMD: f32 = 136.0; // bottom of the command bar
const NAVW: f32 = 158.0;
const LIST_TOP: f32 = 165.0;
const STATUS: f32 = 24.0;
// column left edges and the right edge of the last one, like Explorer's defaults
const COLS: [(&str, f32); 4] = [("Name", 174.0), ("Date modified", 432.0), ("Type", 576.0), ("Size", 696.0)];
const COLS_END: f32 = 776.0;

/// "C:\Users\me" -> [("This PC", "This PC"), ("Windows (C:)", "C:\"), ("Users", "C:\Users"), ("me", "C:\Users\me")]
pub fn crumbs(path: &str, drive_label: impl Fn(&str) -> String) -> Vec<(String, String)> {
    if path == HOME {
        return vec![(HOME.to_string(), HOME.to_string())];
    }
    let mut out = vec![(THIS_PC.to_string(), THIS_PC.to_string())];
    if path == THIS_PC {
        return out;
    }
    let mut acc = String::new();
    for (i, part) in path.split('\\').filter(|p| !p.is_empty()).enumerate() {
        if i == 0 {
            acc = format!("{part}\\");
            out.push((drive_label(&acc), acc.clone()));
        } else {
            acc = std::path::Path::new(&acc).join(part).display().to_string();
            out.push((part.to_string(), acc.clone()));
        }
    }
    out
}

/// What Windows shows for a file in the preview pane: its thumbnail (pictures, videos, PDFs...) or
/// its big icon, fitted into `side` pixels. Returns width, height and premultiplied BGRA pixels.
unsafe fn thumb(path: &str, side: i32) -> Option<(u32, u32, Vec<u8>)> {
    image(path, side, SIIGBF_THUMBNAILONLY).or_else(|| image(path, side, SIIGBF_RESIZETOFIT)) // the real picture if it has one, else its icon
}

pub(crate) unsafe fn image(path: &str, side: i32, flags: SIIGBF) -> Option<(u32, u32, Vec<u8>)> {
    let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path), None).ok()?;
    let f: IShellItemImageFactory = windows::core::Interface::cast(&item).ok()?;
    let hbm = f.GetImage(SIZE { cx: side, cy: side }, flags).ok()?;
    let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
    let bmp = wic.CreateBitmapFromHBITMAP(hbm, HPALETTE::default(), WICBitmapUsePremultipliedAlpha);
    let _ = DeleteObject(hbm.into());
    let conv = wic.CreateFormatConverter().ok()?;
    conv.Initialize(&bmp.ok()?, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
    let (mut w_, mut h_) = (0, 0);
    conv.GetSize(&mut w_, &mut h_).ok()?;
    let mut px = vec![0u8; (w_ * h_ * 4) as usize];
    conv.CopyPixels(std::ptr::null(), w_ * 4, &mut px).ok()?;
    // pictures without transparency come back with an empty alpha channel: make those opaque. A
    // picture that is all zeros has nothing in it at all (fully transparent) and is left that way,
    // instead of being turned into a black square.
    if px.chunks(4).all(|p| p[3] == 0) && px.iter().any(|v| *v != 0) {
        px.chunks_mut(4).for_each(|p| p[3] = 255);
    }
    Some((w_, h_, px))
}

fn drive_label(root: &str) -> String {
    drives().into_iter().find(|d| d.name.ends_with(&format!("({})", &root[..2]))).map(|d| d.name).unwrap_or_else(|| root.to_string())
}

unsafe fn draw(ui: &mut Ui, cw: f32, ch: f32, maximized: bool) -> windows::core::Result<()> {
    let Some((rt, dw)) = ui.gfx.clone() else { return Ok(()) };
    let mut hits = Vec::new();
    rt.Clear(Some(&rgb(0x191919)));
    let b = |c: u32| rt.CreateSolidColorBrush(&rgb(c), None);
    let white = b(0xFFFFFF)?;
    let dim = b(0xC5C5C5)?;
    let off = b(0x6B6B6B)?;
    let fmt = |size: f32, family: PCWSTR| -> windows::core::Result<IDWriteTextFormat> {
        let t = dw.CreateTextFormat(family, None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))?;
        t.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        t.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
        t.SetTrimming(&trim, &dw.CreateEllipsisTrimmingSign(&t)?)?;
        Ok(t)
    };
    let body = fmt(12.0, w!("Segoe UI Variable Text"))?;
    let body14 = fmt(14.0, w!("Segoe UI Variable Text"))?;
    let icons = fmt(14.0, w!("Segoe Fluent Icons"))?;
    icons.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
    let icons12 = fmt(12.0, w!("Segoe Fluent Icons"))?;
    icons12.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
    let icons10 = fmt(10.0, w!("Segoe Fluent Icons"))?;
    icons10.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
    let ui9 = fmt(12.0, w!("Segoe UI"))?; // the list, tree and status bar are classic controls: Segoe UI 9pt, GDI spacing
    let right = fmt(12.0, w!("Segoe UI"))?;
    right.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
    let txt = |s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, br: &ID2D1SolidColorBrush| rt.DrawText(&utf16(s), f, &r, br, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
    let gtxt = |s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, br: &ID2D1SolidColorBrush| rt.DrawText(&utf16(s), f, &r, br, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_GDI_CLASSIC);
    let ico = |cp: u32, f: &IDWriteTextFormat, r: D2D_RECT_F, br: &ID2D1SolidColorBrush| rt.DrawText(&glyph(cp), f, &r, br, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
    let width = |s: &str, f: &IDWriteTextFormat| -> f32 {
        dw.CreateTextLayout(&utf16(s), f, 2000.0, 40.0).ok().map(|l| { let mut m = DWRITE_TEXT_METRICS::default(); let _ = l.GetMetrics(&mut m); m.widthIncludingTrailingWhitespace }).unwrap_or(40.0)
    };
    let hover = ui.hover;
    let bp = D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
    };
    let icon16 = |ui: &mut Ui, key: &str, path: &str, x: f32, y: f32| {
        if !ui.icons.contains_key(key) {
            let px = image(path, 16, SIIGBF_ICONONLY).filter(|i| i.0 == 16 && i.1 == 16).map(|i| i.2).unwrap_or_default(); // Windows' own 16px icon, untouched
            ui.icons.insert(key.to_string(), px);
        }
        if let Some(px) = ui.icons.get(key).filter(|p| p.len() == 16 * 16 * 4) {
            if let Ok(bm) = rt.CreateBitmap(D2D_SIZE_U { width: 16, height: 16 }, Some(px.as_ptr() as *const c_void), 64, &bp) {
                crate::flyout::draw_icon(&rt, &bm, x, y, 16.0);
            }
        }
    };

    // ---- tab strip, window buttons ----
    let title = tab_title(&ui.path);
    rt.FillRectangle(&rect(0.0, 0.0, cw, TAB), &b(0x0A0A0A)?);
    let n_tabs = ui.tabs.len().max(1);
    let tw = 240f32.min((cw - 8.0 - 138.0 - 44.0) / n_tabs as f32);
    for k in 0..n_tabs {
        let x0 = 8.0 + k as f32 * tw;
        let active = k == ui.tab;
        let path = if active { ui.path.clone() } else { ui.tabs[k].0.clone() };
        if active {
            rt.FillRoundedRectangle(&rrect(rect(x0, 8.0, x0 + tw, TAB + 8.0), 8.0), &b(0x1F1F1F)?);
        } else if matches!(hover, Some(Hit::Tab(h) | Hit::TabX(h)) if h == k) {
            rt.FillRoundedRectangle(&rrect(rect(x0, 8.0, x0 + tw, TAB - 2.0), 6.0), &b(0x151515)?);
        }
        icon16(ui, &format!("tab:{path}"), match path.as_str() { THIS_PC => "shell:MyComputerFolder", HOME => "shell:::{f874310e-b6b7-47dc-bc84-b9e6b38f5903}", c => c }, x0 + 12.0, 16.0);
        txt(&tab_title(&path), &body, rect(x0 + 34.0, 8.0, x0 + tw - 32.0, TAB), if active { &white } else { &dim });
        let close_tab = rect(x0 + tw - 32.0, 12.0, x0 + tw - 4.0, 36.0);
        if hover == Some(Hit::TabX(k)) {
            rt.FillRoundedRectangle(&rrect(close_tab, 4.0), &b(0x2D2D2D)?);
        }
        ico(0xE8BB, &icons10, close_tab, &white);
        hits.push((rect(x0, 8.0, x0 + tw - 32.0, TAB), Hit::Tab(k)));
        hits.push((close_tab, Hit::TabX(k)));
    }
    let plus = rect(8.0 + n_tabs as f32 * tw + 8.0, 10.0, 8.0 + n_tabs as f32 * tw + 36.0, 38.0);
    if hover == Some(Hit::NewTab) {
        rt.FillRoundedRectangle(&rrect(plus, 4.0), &b(0x2D2D2D)?);
    }
    ico(0xE710, &icons10, plus, &white);
    hits.push((plus, Hit::NewTab));
    for (k, cp) in [0xE921u32, if maximized { 0xE923 } else { 0xE922 }, 0xE8BB].iter().enumerate() {
        let r = rect(cw - 46.0 * (3 - k) as f32, 0.0, cw - 46.0 * (2 - k) as f32, 32.0);
        if hover == Some(Hit::Win(k as u8)) {
            rt.FillRectangle(&r, &b(if k == 2 { 0xC42B1C } else { 0x2D2D2D })?);
        }
        ico(*cp, &icons10, r, &white);
        hits.push((r, Hit::Win(k as u8)));
    }

    // ---- address row: back / forward / up / refresh, breadcrumbs, search ----
    rt.FillRectangle(&rect(0.0, TAB, cw, TOOL), &b(0x1F1F1F)?);
    rt.FillRectangle(&rect(0.0, TOOL, cw, TOOL + 1.0), &b(0x3A3A3A)?);
    let enabled = [!ui.back.is_empty(), !ui.fwd.is_empty(), ui.path != THIS_PC, true];
    for (k, (h, cp)) in [(Hit::Back, 0xE72Bu32), (Hit::Forward, 0xE72A), (Hit::Up, 0xE74A), (Hit::Refresh, 0xE72C)].iter().enumerate() {
        let r = rect(9.0 + k as f32 * 48.0, 46.0, 45.0 + k as f32 * 48.0, 82.0);
        if hover == Some(*h) && enabled[k] {
            rt.FillRoundedRectangle(&rrect(r, 4.0), &b(0x2D2D2D)?);
        }
        ico(*cp, &icons, r, if enabled[k] { &white } else { &off });
        hits.push((r, *h));
    }
    let crumb_box = rect(201.0, 48.0, cw - 273.0, 80.0);
    rt.FillRoundedRectangle(&rrect(crumb_box, 4.0), &b(0x2C2C2C)?);
    hits.push((crumb_box, Hit::CrumbBox));
    if !ui.editing {
        ico(0xE7F4, &icons, rect(crumb_box.left + 8.0, 48.0, crumb_box.left + 36.0, 80.0), &white);
        let mut x = crumb_box.left + 44.0;
        for (i, (label, _)) in crumbs(&ui.path, drive_label).iter().enumerate() {
            ico(0xE76C, &icons10, rect(x, 48.0, x + 16.0, 80.0), &white);
            x += 24.0;
            let w_ = width(label, &body14);
            if x + w_ > crumb_box.right - 30.0 {
                break;
            }
            let r = rect(x - 6.0, 52.0, x + w_ + 6.0, 76.0);
            if hover == Some(Hit::Crumb(i)) {
                rt.FillRoundedRectangle(&rrect(r, 4.0), &b(0x383838)?);
            }
            txt(label, &body14, rect(x, 48.0, x + w_ + 2.0, 80.0), &white);
            hits.push((r, Hit::Crumb(i)));
            x += w_ + 14.0;
        }
    }
    let search_box = rect(cw - 265.0, 48.0, cw - 15.0, 80.0);
    rt.FillRoundedRectangle(&rrect(search_box, 4.0), &b(0x2C2C2C)?);
    hits.push((search_box, Hit::Search));
    if ui.filter.is_empty() && !ui.searching {
        txt(&format!("Search {title}"), &body14, rect(search_box.left + 14.0, 48.0, search_box.right - 36.0, 80.0), &dim);
    }
    ico(0xE721, &icons10, rect(search_box.right - 34.0, 48.0, search_box.right - 6.0, 80.0), &white);

    // ---- command bar ----
    rt.FillRectangle(&rect(0.0, CMD - 1.0, cw, CMD), &b(0x3A3A3A)?);
    let in_folder = ui.path != THIS_PC && ui.path != HOME;
    let has_sel = ui.sel.is_some() && in_folder;
    let mut x = 3.0;
    let cmds: [(u8, u32, &str, bool); 10] = [(0, 0xECC8, "New", in_folder), (1, 0xE8C6, "", has_sel), (2, 0xE8C8, "", has_sel), (3, 0xE77F, "", in_folder), (4, 0xE8AC, "", has_sel), (8, 0xE72D, "", has_sel), (5, 0xE74D, "", has_sel), (6, 0xE8CB, "Sort", true), (7, 0xE8FD, "View", true), (9, 0xE712, "", true)];
    for (id, cp, label, on) in cmds {
        let w_ = if label.is_empty() { 44.0 } else { 44.0 + width(label, &body) + 16.0 };
        match id {
            1 => {
                rt.FillRectangle(&rect(x + 3.0, 100.0, x + 4.0, 124.0), &b(0x3A3A3A)?);
                x += 8.0;
            }
            6 => {
                rt.FillRectangle(&rect(x, 100.0, x + 1.0, 124.0), &b(0x3A3A3A)?);
                x += 3.0;
            }
            9 => x += 9.0,
            _ => {}
        }
        let r = rect(x, 94.0, x + w_, 130.0);
        if hover == Some(Hit::Cmd(id)) && on {
            rt.FillRoundedRectangle(&rrect(r, 4.0), &b(0x2D2D2D)?);
        }
        ico(cp, &icons, rect(x + 6.0, 94.0, x + 38.0, 130.0), if on { &white } else { &off });
        if !label.is_empty() {
            txt(label, &body, rect(x + 40.0, 94.0, x + w_, 130.0), if on { &white } else { &off });
            ico(0xE70D, &icons10, rect(x + w_ - 20.0, 94.0, x + w_ - 4.0, 130.0), if on { &dim } else { &off });
        }
        hits.push((r, Hit::Cmd(id)));
        x += w_ + 4.0;
    }

    // ---- Preview switch (right end of the command bar) and the pane itself ----
    let pr = rect(cw - 100.0, 94.0, cw - 8.0, 130.0);
    if hover == Some(Hit::Cmd(10)) {
        rt.FillRoundedRectangle(&rrect(pr, 4.0), &b(0x2D2D2D)?);
    }
    ico(0xE90D, &icons, rect(cw - 97.0, 94.0, cw - 65.0, 130.0), &white);
    txt("Preview", &body, rect(cw - 64.0, 94.0, cw, 130.0), &white);
    hits.push((pr, Hit::Cmd(10)));
    if !ui.no_preview {
        let px0 = cw - 276.0;
        rt.FillRectangle(&rect(px0, CMD, cw, ch - STATUS), &b(0x202020)?);
        rt.FillRectangle(&rect(px0, CMD, px0 + 1.0, ch - STATUS), &b(0x2B2B2B)?);
        let one = selected(ui);
        let target = (one.len() == 1).then(|| ui.entries.get(one[0])).flatten().filter(|e| !e.dir).map(|e| (full(&ui.path, e), e.name.clone()));
        let centered = fmt(12.0, w!("Segoe UI Variable Text"))?;
        centered.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
        match target {
            Some((p, name)) => {
                if ui.thumb.as_ref().map(|t| t.0 != p).unwrap_or(true) {
                    ui.thumb = thumb(&p, 240).map(|(w_, h_, px)| (p.clone(), w_, h_, px));
                }
                let mid = (CMD + ch - STATUS) / 2.0;
                if let Some((_, w_, h_, px)) = ui.thumb.as_ref() {
                    if let Ok(bm) = rt.CreateBitmap(D2D_SIZE_U { width: *w_, height: *h_ }, Some(px.as_ptr() as *const c_void), w_ * 4, &bp) {
                        let (x, y) = ((px0 + (276.0 - *w_ as f32) / 2.0).round(), (mid - *h_ as f32 / 2.0).round());
                        rt.DrawBitmap(&bm, Some(&rect(x, y, x + *w_ as f32, y + *h_ as f32)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
                    }
                }
                txt(&name, &centered, rect(px0 + 12.0, mid + 130.0, cw - 12.0, mid + 160.0), &white);
            }
            None => txt(if one.len() > 1 { "Select a single file to preview." } else { "Select a file to preview." }, &centered, rect(px0, CMD, cw, ch - STATUS + 6.0), &white),
        }
    }

    // ---- left pane ----
    rt.PushAxisAlignedClip(&rect(0.0, CMD, NAVW, ch - STATUS), D2D1_ANTIALIAS_MODE_ALIASED);
    let places = ui.places.clone();
    let nav_h = ch - STATUS - CMD;
    let nav_all = places.len() as f32 * 32.0 + 10.0;
    ui.nav_scroll = ui.nav_scroll.clamp(0.0, (nav_all - nav_h).max(0.0));
    let mut y = CMD + 6.0 - ui.nav_scroll;
    for (i, (label, path, kind)) in places.iter().enumerate() {
        if *kind == 9 {
            rt.FillRectangle(&rect(10.0, y + 15.0, NAVW - 28.0, y + 16.0), &b(0x3A3A3A)?);
            y += 32.0;
            continue;
        }
        let indent = if *kind == 2 { 8.0 } else { 0.0 };
        let r = rect(2.0, y, NAVW - 20.0, y + 32.0);
        // a drive lights up while you are somewhere inside it (unless one of your folders is the match)
        let inside = *kind == 2 && ui.path.starts_with(path.as_str()) && !places.iter().any(|(_, p, _)| *p == ui.path);
        let on = *path == ui.path || inside;
        if on || hover == Some(Hit::Place(i)) {
            rt.FillRoundedRectangle(&rrect(rect(r.left, r.top + 2.0, r.right, r.bottom - 2.0), 4.0), &b(if on { 0x2D2D2D } else { 0x262626 })?);
        }
        let shell_path = match path.as_str() {
            THIS_PC => "shell:MyComputerFolder",
            HOME => "shell:::{f874310e-b6b7-47dc-bc84-b9e6b38f5903}",
            _ if *kind == 0 && label == "Gallery" => "shell:::{e88865ea-0e1c-4e20-9aa6-edcd0212c87c}",
            other => other,
        };
        if path == THIS_PC {
            ico(0xE70D, &icons10, rect(8.0, y, 24.0, y + 32.0), &dim);
        } else if *kind == 2 {
            ico(0xE76C, &icons10, rect(16.0, y, 32.0, y + 32.0), &dim);
        } else if *kind == 4 {
            ico(0xE76C, &icons10, rect(8.0, y, 24.0, y + 32.0), &dim);
        }
        let ns_icon = NS_ICONS.lock().unwrap().iter().find(|(t, _)| *kind == 4 && t == path).map(|(_, i)| i.clone());
        let shell_path = ns_icon.as_deref().unwrap_or(shell_path);
        icon16(ui, &format!("place:{label}:{path}"), shell_path, 33.0 + indent, y + 8.0);
        gtxt(label, &ui9, rect(if *kind == 2 { 63.0 } else { 55.0 }, y, NAVW - if *kind == 1 { 32.0 } else { 12.0 }, y + 32.0), &white);
        if *kind == 1 {
            ico(0xE842, &icons12, rect(NAVW - 37.0, y, NAVW - 19.0, y + 32.0), &b(0x9F9F9F)?);
        }
        hits.push((r, Hit::Place(i)));
        y += 32.0;
    }
    rt.PopAxisAlignedClip();
    let bar = b(0x9F9F9F)?;
    if nav_all > nav_h {
        let (t0, t1) = (CMD + 17.0, ch - STATUS - 12.0);
        let (len, at) = ((t1 - t0) * nav_h / nav_all, (t1 - t0) * ui.nav_scroll / nav_all);
        rt.FillRectangle(&rect(NAVW - 11.0, t0 + at, NAVW - 9.0, t0 + at + len), &bar);
    }

    // ---- column headers ----
    for (i, (name, x0)) in COLS.iter().enumerate() {
        let x1 = if i + 1 < COLS.len() { COLS[i + 1].1 } else { COLS_END };
        let r = rect(*x0, CMD + 1.0, x1, LIST_TOP - 4.0);
        gtxt(name, &ui9, rect(x0 + if i == 0 { 16.0 } else { 6.0 }, CMD + 1.0, x1 - 4.0, LIST_TOP - 5.0), &b(if hover == Some(Hit::Column(i)) { 0xFFFFFF } else { 0xDEDEDE })?);
        if ui.sort.0 == i {
            ico(if ui.sort.1 { 0xE70E } else { 0xE70D }, &icons10, rect((x0 + x1) / 2.0 - 8.0, CMD - 2.0, (x0 + x1) / 2.0 + 8.0, CMD + 8.0), &dim);
        }
        rt.FillRectangle(&rect(x1 - 1.0, CMD + 2.0, x1, LIST_TOP - 6.0), &b(0x3A3A3A)?);
        hits.push((r, Hit::Column(i)));
    }

    // ---- rows ----
    let view_bot = ch - STATUS;
    let no_ext = hide_ext();
    let (n_items, lw) = (ui.entries.len(), list_width(ui, cw));
    let total = cell(ui.view, 0, n_items, lw).1;
    let max = (total - (view_bot - LIST_TOP)).max(0.0);
    ui.scroll = ui.scroll.clamp(0.0, max);
    let name_mid = fmt(12.0, w!("Segoe UI Variable Text"))?;
    name_mid.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
    rt.PushAxisAlignedClip(&rect(NAVW + 1.0, LIST_TOP, cw, view_bot), D2D1_ANTIALIAS_MODE_ALIASED);
    if ui.entries.is_empty() {
        txt(if ui.filter.is_empty() { "This folder is empty." } else { "No items match your search." }, &ui9, rect(COLS[0].1 + 16.0, LIST_TOP + 8.0, cw - 12.0, LIST_TOP + 36.0), &dim);
    }
    for i in 0..ui.entries.len() {
        let ((cx, cy, cwid, chgt), _) = cell(ui.view, i, n_items, lw);
        let (x, y) = (COLS[0].1 + cx, LIST_TOP + cy - ui.scroll);
        if y + chgt < LIST_TOP || y > view_bot {
            continue;
        }
        let e = ui.entries[i].clone();
        let r = rect(x, y, x + cwid, y + chgt);
        let is_sel = ui.sel == Some(i) || ui.more.contains(&i);
        if is_sel || hover == Some(Hit::Row(i)) {
            rt.FillRoundedRectangle(&rrect(rect(r.left, r.top + 1.0, r.right, r.bottom - 1.0), 3.0), &b(if is_sel { 0x383838 } else { 0x2B2B2B })?);
        }
        let p = full(&ui.path, &e);
        let ext = std::path::Path::new(&e.name).extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
        let key = if e.dir { if ui.path == THIS_PC || ui.path == HOME { p.clone() } else { "<dir>".into() } } else if matches!(ext.as_str(), "exe" | "lnk" | "ico" | "url") { p.clone() } else { format!(".{ext}") };
        let shown = if no_ext && !e.dir { std::path::Path::new(&e.name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| e.name.clone()) } else { e.name.clone() };
        if ui.view == 1 {
            // large icons: the picture itself for pictures, else the 48px icon; name centred underneath
            // ponytail: thumbnails are made as they come into view, on this thread; a folder of big photos stutters once
            let bk = if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico") { format!("big:{p}") } else { format!("big:{key}") };
            if !ui.big.contains_key(&bk) {
                let im = image(&p, 48, SIIGBF_RESIZETOFIT).unwrap_or((0, 0, Vec::new()));
                ui.big.insert(bk.clone(), im);
            }
            if let Some((w_, h_, px)) = ui.big.get(&bk).filter(|b| b.0 > 0) {
                if let Ok(bm) = rt.CreateBitmap(D2D_SIZE_U { width: *w_, height: *h_ }, Some(px.as_ptr() as *const c_void), w_ * 4, &bp) {
                    let (ix, iy) = ((x + (cwid - *w_ as f32) / 2.0).round(), (y + 6.0 + (48.0 - *h_ as f32) / 2.0).round());
                    rt.DrawBitmap(&bm, Some(&rect(ix, iy, ix + *w_ as f32, iy + *h_ as f32)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
                }
            }
            txt(&shown, &name_mid, rect(x + 4.0, y + 60.0, x + cwid - 4.0, y + chgt - 2.0), &white);
            hits.push((r, Hit::Row(i)));
            continue;
        }
        if ui.view == 2 {
            icon16(ui, &key, &p, x + 4.0, y + 4.0);
            gtxt(&shown, &ui9, rect(x + 24.0, y, x + cwid - 6.0, y + chgt), &white);
            hits.push((r, Hit::Row(i)));
            continue;
        }
        icon16(ui, &key, &p, COLS[0].1 + 4.0, y + 6.0);
        gtxt(&shown, &ui9, rect(COLS[0].1 + 24.0, y, COLS[1].1 - 8.0, y + ROW), &white);
        gtxt(&date_text(e.modified), &ui9, rect(COLS[1].1 + 6.0, y, COLS[2].1 - 8.0, y + ROW), &dim);
        gtxt(&e.kind, &ui9, rect(COLS[2].1 + 6.0, y, COLS[3].1 - 8.0, y + ROW), &dim);
        if !e.dir {
            gtxt(&size_text(e.size), &right, rect(COLS[3].1, y, COLS_END - 12.0, y + ROW), &dim);
        }
        hits.push((r, Hit::Row(i)));
    }
    rt.PopAxisAlignedClip();

    if max > 0.0 {
        let all = total;
        let (t0, t1) = (CMD + 17.0, view_bot - 12.0);
        let (len, at) = ((t1 - t0) * (view_bot - LIST_TOP) / all, (t1 - t0) * ui.scroll / all);
        let x = if ui.no_preview { cw - 10.0 } else { cw - 286.0 };
        rt.FillRectangle(&rect(x, t0 + at, x + 2.0, t0 + at + len), &bar);
    }

    // ---- status bar ----
    rt.FillRectangle(&rect(0.0, ch - STATUS, cw, ch), &b(0x1C1C1C)?);
    let n = ui.entries.len();
    let mut status = format!("{n} item{}  |", if n == 1 { "" } else { "s" });
    let k = selected(ui).len();
    if k > 0 {
        status.push_str(&format!("  {k} item{} selected  |", if k == 1 { "" } else { "s" }));
    }
    // the view this list is in (details), marked like Explorer's two corner buttons
    let vb = rect(cw - 50.0, ch - STATUS + 1.0, cw - 28.0, ch - 1.0);
    rt.DrawRectangle(&rect(vb.left + 0.5, vb.top + 0.5, vb.right - 0.5, vb.bottom - 0.5), &b(0x9F9F9F)?, 1.0, None);
    ico(0xE8FD, &icons10, vb, &white);
    ico(0xE91B, &icons10, rect(cw - 27.0, ch - STATUS + 1.0, cw - 5.0, ch - 1.0), &white);
    gtxt(&status, &ui9, rect(14.0, ch - STATUS, cw - 80.0, ch), &white);
    ui.hits = hits;
    Ok(())
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
        UI.with(|u| {
            let mut ui = u.borrow_mut();
            if ui.gfx.is_none() {
                ui.gfx = make_gfx().ok();
            }
            if let Some((rt, _)) = ui.gfx.clone() {
                let _ = rt.BindDC(mem, &rc);
                rt.SetDpi(dpi, dpi);
                rt.BeginDraw();
                let max = IsZoomed(hwnd).as_bool();
                let _ = draw(&mut ui, rc.right as f32 * 96.0 / dpi, rc.bottom as f32 * 96.0 / dpi, max);
                let _ = rt.EndDraw(None, None);
            }
        });
        let _ = BufferedPaintSetAlpha(pb, None, 255);
        let _ = EndBufferedPaint(pb, true);
    }
    let _ = EndPaint(hwnd, &ps);
}

unsafe fn make_gfx() -> windows::core::Result<(ID2D1DCRenderTarget, IDWriteFactory)> {
    let f: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
    let props = D2D1_RENDER_TARGET_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        ..Default::default()
    };
    let rt = f.CreateDCRenderTarget(&props)?;
    rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE); // opaque window, so text can match Explorer's
    Ok((rt, DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?))
}

// ======================= window =======================

fn hit(x: f32, y: f32) -> Option<Hit> {
    // try_borrow: asked while the state is in use, the answer is "nothing there" rather than a panic
    UI.with(|u| u.try_borrow().ok().and_then(|ui| ui.hits.iter().rev().find(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom).map(|(_, h)| *h)))
}

unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_KEYDOWN && wp.0 == VK_RETURN.0 as usize {
        let n = GetWindowTextLengthW(hwnd);
        let mut buf = vec![0u16; n as usize + 1];
        GetWindowTextW(hwnd, &mut buf);
        let p = String::from_utf16_lossy(&buf[..n as usize]).trim().trim_matches('"').to_string();
        UI.with(|u| {
            let mut ui = u.borrow_mut();
            match ui.renaming.take() {
                Some(i) => {
                    if let Some(e) = ui.entries.get(i).cloned() {
                        if !p.is_empty() && p != e.name {
                            let _ = std::fs::rename(full(&ui.path, &e), std::path::Path::new(&ui.path).join(&p));
                        }
                    }
                    load(&mut ui);
                }
                None => go(&mut ui, p, true),
            }
        });
        let main = HWND(WIN.load(Relaxed) as *mut c_void);
        let _ = SetFocus(Some(main));
        let _ = InvalidateRect(Some(main), None, false);
        return LRESULT(0);
    }
    if msg == WM_CHAR && wp.0 == 13 {
        return LRESULT(0);
    }
    let prev: WNDPROC = std::mem::transmute(EDIT_PROC.load(Relaxed));
    CallWindowProcW(prev, hwnd, msg, wp, lp)
}

pub unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // Re-entered while the window's state is in use: a handler called into Windows (a shell call, a
    // menu, a dialog) and that ran a message loop. Touching the state again would panic and close the
    // window, so this message gets default handling instead.
    if UI.with(|u| u.try_borrow_mut().is_err()) && !matches!(msg, WM_NCCREATE | WM_CREATE | WM_NCDESTROY | WM_DESTROY) {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    let dips = |lp: LPARAM| ((lp.0 & 0xFFFF) as i16 as f32 / s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s);
    let redraw = || {
        let _ = InvalidateRect(Some(hwnd), None, false);
    };
    match msg {
        WM_CREATE => {
            let inst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).map(|m| HINSTANCE(m.0)).ok();
            let e = CreateWindowExW(WINDOW_EX_STYLE(0), w!("EDIT"), w!(""), WS_CHILD | WINDOW_STYLE(ES_AUTOHSCROLL as u32), 0, 0, 10, 10, Some(hwnd), None, inst, None).unwrap_or_default();
            let se = CreateWindowExW(WINDOW_EX_STYLE(0), w!("EDIT"), w!(""), WS_CHILD | WINDOW_STYLE(ES_AUTOHSCROLL as u32), 0, 0, 10, 10, Some(hwnd), None, inst, None).unwrap_or_default();
            SEARCH.store(se.0 as isize, Relaxed);
            let font = CreateFontW(-(13.0 * s) as i32, 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI Variable Text"));
            SendMessageW(e, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            SendMessageW(se, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            EDIT_PROC.store(SetWindowLongPtrW(e, GWLP_WNDPROC, edit_proc as *const () as usize as isize), Relaxed);
            EDIT.store(e.0 as isize, Relaxed);
        }
        WM_SIZE => {
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let cwd = rc.right as f32 / s;
            let px = |v: f32| (v * s) as i32;
            if UI.with(|u| u.borrow().renaming.is_none()) {
                let _ = SetWindowPos(HWND(EDIT.load(Relaxed) as *mut c_void), None, px(213.0), px(55.0), px(cwd - 273.0 - 225.0), px(19.0), SWP_NOZORDER);
            }
            let _ = SetWindowPos(HWND(SEARCH.load(Relaxed) as *mut c_void), None, px(cwd - 251.0), px(55.0), px(200.0), px(19.0), SWP_NOZORDER);
            redraw();
        }
        WM_CTLCOLOREDIT => {
            let dc = HDC(wp.0 as *mut c_void);
            SetTextColor(dc, COLORREF(0xFFFFFF));
            SetBkColor(dc, COLORREF(0x2C2C2C));
            thread_local! { static BR: HBRUSH = unsafe { CreateSolidBrush(COLORREF(0x2C2C2C)) }; }
            return LRESULT(BR.with(|b| b.0 as isize));
        }
        // our own frame: the tab strip is the title bar, like Explorer's
        WM_NCCALCSIZE if wp.0 == 1 => {
            if IsZoomed(hwnd).as_bool() {
                let p = &mut *(lp.0 as *mut NCCALCSIZE_PARAMS);
                let f = GetSystemMetrics(SM_CXSIZEFRAME) + GetSystemMetrics(SM_CXPADDEDBORDER);
                p.rgrc[0].left += f;
                p.rgrc[0].top += f;
                p.rgrc[0].right -= f;
                p.rgrc[0].bottom -= f;
            }
            return LRESULT(0);
        }
        WM_NCHITTEST => {
            let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
            let _ = ScreenToClient(hwnd, &mut pt);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let e = (6.0 * s) as i32;
            if !IsZoomed(hwnd).as_bool() {
                let (l, t, r, bt) = (pt.x < e, pt.y < e, pt.x >= rc.right - e, pt.y >= rc.bottom - e);
                let code = match (l, t, r, bt) {
                    (true, true, _, _) => HTTOPLEFT,
                    (_, true, true, _) => HTTOPRIGHT,
                    (true, _, _, true) => HTBOTTOMLEFT,
                    (_, _, true, true) => HTBOTTOMRIGHT,
                    (true, ..) => HTLEFT,
                    (_, true, ..) => HTTOP,
                    (_, _, true, _) => HTRIGHT,
                    (.., true) => HTBOTTOM,
                    _ => HTCLIENT,
                };
                if code != HTCLIENT {
                    return LRESULT(code as isize);
                }
            }
            let (x, y) = (pt.x as f32 / s, pt.y as f32 / s);
            if y < TAB && hit(x, y).is_none() {
                return LRESULT(HTCAPTION as isize);
            }
            return LRESULT(HTCLIENT as isize);
        }
        WM_COMMAND => {
            // notifications from the two text boxes
            let (code, ctl) = ((wp.0 >> 16) as u32 & 0xFFFF, lp.0);
            if ctl == SEARCH.load(Relaxed) {
                let se = HWND(ctl as *mut c_void);
                if code == 0x0300 {
                    // EN_CHANGE: filter as you type
                    let n = GetWindowTextLengthW(se);
                    let mut buf = vec![0u16; n as usize + 1];
                    GetWindowTextW(se, &mut buf);
                    UI.with(|u| {
                        // changing folders clears this box from inside a borrow: nothing to do then
                        if let Ok(mut ui) = u.try_borrow_mut() {
                            ui.filter = String::from_utf16_lossy(&buf[..n as usize]);
                            refilter(&mut ui);
                            ui.scroll = 0.0;
                        }
                    });
                } else if code == 0x0200 {
                    // EN_KILLFOCUS: an empty box goes back to showing "Search <folder>"
                    UI.with(|u| {
                        if let Ok(mut ui) = u.try_borrow_mut() {
                            ui.searching = false;
                        }
                    });
                    if GetWindowTextLengthW(se) == 0 {
                        let _ = ShowWindow(se, SW_HIDE);
                    }
                }
                redraw();
            } else if ctl == EDIT.load(Relaxed) && code == 0x0200 {
                let _ = ShowWindow(HWND(ctl as *mut c_void), SW_HIDE);
                UI.with(|u| {
                    if let Ok(mut ui) = u.try_borrow_mut() {
                        ui.editing = false;
                        ui.renaming = None;
                    }
                });
                redraw();
            }
        }
        WM_MOUSEMOVE => {
            let (x, y) = dips(lp);
            let h = hit(x, y);
            if UI.with(|u| std::mem::replace(&mut u.borrow_mut().hover, h) != h) {
                let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                let _ = TrackMouseEvent(&mut tme);
                redraw();
            }
        }
        WM_MOUSELEAVE => {
            UI.with(|u| u.borrow_mut().hover = None);
            redraw();
        }
        WM_MOUSEWHEEL => {
            let d = ((wp.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
            let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
            let _ = ScreenToClient(hwnd, &mut pt);
            let over_nav = (pt.x as f32 / s) < NAVW;
            UI.with(|u| {
                let mut ui = u.borrow_mut();
                if over_nav {
                    ui.nav_scroll -= d * 96.0;
                } else {
                    ui.scroll -= d * ROW * 3.0;
                }
            });
            redraw();
        }
        WM_LBUTTONDOWN => {
            let (x, y) = dips(lp);
            let h0 = hit(x, y);
            if !matches!(h0, Some(Hit::CrumbBox | Hit::Search)) {
                let _ = SetFocus(Some(hwnd));
            }
            let mut after: Option<Hit> = None;
            UI.with(|u| {
                let mut ui = u.borrow_mut();
                match h0 {
                    Some(Hit::Crumb(i)) => {
                        if let Some((_, p)) = crumbs(&ui.path, drive_label).get(i).cloned() {
                            go(&mut ui, p, true);
                        }
                    }
                    Some(h @ (Hit::CrumbBox | Hit::Search | Hit::Cmd(_) | Hit::Win(_) | Hit::TabX(_))) => after = Some(h),
                    Some(Hit::Tab(k)) => switch_tab(&mut ui, k),
                    Some(Hit::NewTab) => new_tab(&mut ui),
                    Some(Hit::Row(i)) => {
                        let (a, m) = click_select(ui.sel, &ui.more, i, wp.0 & 0x8 != 0, wp.0 & 0x4 != 0);
                        ui.sel = a;
                        ui.more = m;
                    }
                    Some(Hit::Back) => {
                        if let Some(p) = ui.back.pop() {
                            let cur = ui.path.clone();
                            ui.fwd.push(cur);
                            go(&mut ui, p, false);
                        }
                    }
                    Some(Hit::Forward) => {
                        if let Some(p) = ui.fwd.pop() {
                            let cur = ui.path.clone();
                            ui.back.push(cur);
                            go(&mut ui, p, false);
                        }
                    }
                    Some(Hit::Up) => {
                        let p = parent(&ui.path);
                        go(&mut ui, p, true);
                    }
                    Some(Hit::Refresh) => load(&mut ui),
                    Some(Hit::Place(i)) => {
                        if let Some(p) = ui.places.get(i).map(|p| p.1.clone()) {
                            go(&mut ui, p, true);
                        }
                    }
                    Some(Hit::Column(i)) => {
                        ui.sort = if ui.sort.0 == i { (i, !ui.sort.1) } else { (i, true) };
                        let (c, a) = ui.sort;
                        sort(&mut ui.entries, c, a);
                        ui.sel = None;
                    }
                    None => {
                        ui.sel = None;
                        ui.more.clear();
                    }
                }
            });
            if let Some(h) = after {
                command(hwnd, h, s);
            }
            redraw();
        }
        WM_LBUTTONDBLCLK => {
            let (x, y) = dips(lp);
            if let Some(Hit::Row(i)) = hit(x, y) {
                UI.with(|u| open(&mut u.borrow_mut(), i));
                redraw();
            }
        }
        WM_RBUTTONUP => {
            let (x, y) = dips(lp);
            // what is under the pointer is worked out BEFORE the state is borrowed: doing it inside
            // borrowed the state twice and ended the program - every right-click closed File Explorer
            let under = hit(x, y);
            let (folder, item) = UI.with(|u| {
                let mut ui = u.borrow_mut();
                let item = match under {
                    Some(Hit::Row(i)) => {
                        ui.sel = Some(i);
                        ui.entries.get(i).map(|e| full(&ui.path, e))
                    }
                    _ => None,
                };
                (ui.path.clone(), item)
            });
            redraw();
            if (folder != THIS_PC && folder != HOME) || item.is_some() {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                context_menu(hwnd, &folder, item.as_deref(), pt);
                UI.with(|u| load(&mut u.borrow_mut())); // the menu may have renamed, deleted or created something
                redraw();
            }
        }
        WM_KEYDOWN => {
            UI.with(|u| {
                let mut ui = u.borrow_mut();
                let n = ui.entries.len();
                let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
                match VIRTUAL_KEY(wp.0 as u16) {
                    VIRTUAL_KEY(0x54) if ctrl => new_tab(&mut ui), // Ctrl+T
                    VIRTUAL_KEY(0x57) if ctrl => {
                        // Ctrl+W
                        let k = ui.tab;
                        if close_tab(&mut ui, k) {
                            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                        }
                    }
                    VK_DOWN => {
                        ui.sel = Some(ui.sel.map_or(0, |i| (i + 1).min(n.saturating_sub(1))));
                        ui.more.clear();
                    }
                    VK_UP => {
                        ui.sel = Some(ui.sel.map_or(0, |i| i.saturating_sub(1)));
                        ui.more.clear();
                    }
                    VK_RETURN => {
                        if let Some(i) = ui.sel {
                            open(&mut ui, i);
                        }
                    }
                    VK_BACK => {
                        let p = parent(&ui.path);
                        go(&mut ui, p, true);
                    }
                    VK_F2 if ui.sel.is_some() => {
                        drop(ui);
                        command(hwnd, Hit::Cmd(4), s);
                        return;
                    }
                    VK_F5 => load(&mut ui),
                    VIRTUAL_KEY(0x41) if GetKeyState(VK_CONTROL.0 as i32) < 0 => {
                        // Ctrl+A
                        ui.sel = (n > 0).then_some(0);
                        ui.more = (1..n).collect();
                    }
                    VK_DELETE => {
                        let items: Vec<String> = selected(&ui).iter().filter_map(|i| ui.entries.get(*i)).map(|e| full(&ui.path, e)).collect();
                        if !items.is_empty() {
                            let folder = ui.path.clone();
                            drop(ui);
                            shell_verb(hwnd, &folder, &items, b"delete\0");
                            UI.with(|u| load(&mut u.borrow_mut()));
                            let _ = InvalidateRect(Some(hwnd), None, false);
                            return;
                        }
                    }
                    _ => {}
                }
                // keep the selection in view
                if let Some(i) = ui.sel {
                    let mut rc = RECT::default();
                    let _ = GetClientRect(hwnd, &mut rc);
                    let view = rc.bottom as f32 / s - LIST_TOP - STATUS;
                    let ((_, y, _, hgt), _) = cell(ui.view, i, ui.entries.len(), list_width(&ui, rc.right as f32 / s));
                    if y < ui.scroll {
                        ui.scroll = y;
                    } else if y + hgt > ui.scroll + view {
                        ui.scroll = y + hgt - view;
                    }
                }
            });
            redraw();
        }
        WM_ERASEBKGND => return LRESULT(1),
        // for the checks: switch the view without opening the menu (wParam 0 details, 1 icons, 2 list)
        m if m == WM_APP + 77 => {
            UI.with(|u| { let mut ui = u.borrow_mut(); ui.view = (wp.0 as u8).min(2); ui.scroll = 0.0; });
            redraw();
        }
        WM_PAINT => paint(hwnd),
        WM_CLOSE => {
            let _ = ShowWindow(hwnd, SW_HIDE);
            UI.with(|u| {
                let mut ui = u.borrow_mut();
                ui.gfx = None;
                ui.icons.clear();
            });
            // hand the memory back: the window is gone, the app keeps running
            let _ = windows::Win32::System::ProcessStatus::K32EmptyWorkingSet(windows::Win32::System::Threading::GetCurrentProcess());
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

/// Buttons that need the window: text boxes, command bar, window buttons.
unsafe fn command(hwnd: HWND, h: Hit, s: f32) {
    let (folder, items, sel) = UI.with(|u| {
        let ui = u.borrow();
        (ui.path.clone(), selected(&ui).iter().filter_map(|i| ui.entries.get(*i)).map(|e| full(&ui.path, e)).collect::<Vec<String>>(), ui.sel)
    });
    let edit = HWND(EDIT.load(Relaxed) as *mut c_void);
    match h {
        Hit::CrumbBox => {
            // click the empty part of the address bar to type a path
            UI.with(|u| u.borrow_mut().editing = true);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let cwd = rc.right as f32 / s;
            let _ = SetWindowPos(edit, None, (213.0 * s) as i32, (55.0 * s) as i32, ((cwd - 273.0 - 225.0) * s) as i32, (19.0 * s) as i32, SWP_NOZORDER | SWP_SHOWWINDOW);
            let _ = SetWindowTextW(edit, &HSTRING::from(folder.as_str()));
            let _ = SetFocus(Some(edit));
            SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
        }
        Hit::Search => {
            UI.with(|u| u.borrow_mut().searching = true);
            let se = HWND(SEARCH.load(Relaxed) as *mut c_void);
            let _ = ShowWindow(se, SW_SHOW);
            let _ = SetFocus(Some(se));
        }
        Hit::Cmd(0) if folder != THIS_PC && folder != HOME => {
            let name = new_folder_name(&folder);
            let _ = std::fs::create_dir(std::path::Path::new(&folder).join(&name));
            UI.with(|u| {
                let mut ui = u.borrow_mut();
                load(&mut ui);
                ui.sel = ui.entries.iter().position(|e| e.name == name);
                ui.more.clear();
            });
            // like Windows: the new folder's name is ready to be typed over
            command(hwnd, Hit::Cmd(4), s);
        }
        Hit::Cmd(1) if !items.is_empty() => shell_verb(hwnd, &folder, &items, b"cut\0"),
        Hit::Cmd(2) if !items.is_empty() => shell_verb(hwnd, &folder, &items, b"copy\0"),
        Hit::Cmd(3) if folder != THIS_PC && folder != HOME => {
            shell_verb(hwnd, &folder, &[], b"paste\0");
            UI.with(|u| load(&mut u.borrow_mut()));
        }
        Hit::Cmd(4) => {
            // rename in place: the text box sits on the name
            if let (Some(i), Some(e)) = (sel, sel.and_then(|i| UI.with(|u| u.borrow().entries.get(i).cloned()))) {
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                let (ex, ey, ew) = UI.with(|u| {
                    let ui = u.borrow();
                    let ((cx, cy, cwid, chgt), _) = cell(ui.view, i, ui.entries.len(), list_width(&ui, rc.right as f32 / s));
                    let (x, y) = (COLS[0].1 + cx, LIST_TOP + cy - ui.scroll);
                    match ui.view {
                        1 => (x + 4.0, y + chgt - 28.0, cwid - 8.0),
                        2 => (x + 24.0, y + 2.0, cwid - 30.0),
                        _ => (COLS[0].1 + 24.0, y + 4.0, COLS[1].1 - COLS[0].1 - 32.0),
                    }
                });
                UI.with(|u| u.borrow_mut().renaming = Some(i));
                let _ = SetWindowPos(edit, None, (ex * s) as i32, (ey * s) as i32, (ew * s) as i32, (20.0 * s) as i32, SWP_NOZORDER | SWP_SHOWWINDOW);
                let _ = SetWindowTextW(edit, &HSTRING::from(e.name.as_str()));
                let _ = SetFocus(Some(edit));
                SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
            }
        }
        Hit::Cmd(5) if !items.is_empty() => {
            shell_verb(hwnd, &folder, &items, b"delete\0");
            UI.with(|u| load(&mut u.borrow_mut()));
        }
        Hit::Cmd(6) => {
            if let Ok(m) = CreatePopupMenu() {
                let (col, asc) = UI.with(|u| u.borrow().sort);
                for (k, (name, _)) in COLS.iter().enumerate() {
                    let _ = AppendMenuW(m, if k == col { MF_STRING | MF_CHECKED } else { MF_STRING }, k + 1, &HSTRING::from(*name));
                }
                let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
                let _ = AppendMenuW(m, if asc { MF_STRING | MF_CHECKED } else { MF_STRING }, 10, w!("Ascending"));
                let _ = AppendMenuW(m, if !asc { MF_STRING | MF_CHECKED } else { MF_STRING }, 11, w!("Descending"));
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let cmd = TrackPopupMenu(m, TPM_RETURNCMD, pt.x, pt.y, None, hwnd, None).0;
                let _ = DestroyMenu(m);
                UI.with(|u| {
                    let mut ui = u.borrow_mut();
                    match cmd {
                        1..=4 => ui.sort.0 = cmd as usize - 1,
                        10 => ui.sort.1 = true,
                        11 => ui.sort.1 = false,
                        _ => {}
                    }
                    let (c, a) = ui.sort;
                    sort(&mut ui.entries, c, a);
                });
            }
        }
        Hit::Cmd(8) if !items.is_empty() => shell_verb(hwnd, &folder, &items, b"Windows.ModernShare "),
        Hit::Cmd(10) => UI.with(|u| {
            let mut ui = u.borrow_mut();
            ui.no_preview = !ui.no_preview;
        }),
        Hit::Cmd(9) => {
            if let Ok(m) = CreatePopupMenu() {
                let _ = AppendMenuW(m, MF_STRING, 1, w!("Select all"));
                let _ = AppendMenuW(m, MF_STRING, 2, w!("Select none"));
                let _ = AppendMenuW(m, MF_STRING, 3, w!("Invert selection"));
                let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
                let _ = AppendMenuW(m, MF_STRING, 4, w!("Properties"));
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let cmd = TrackPopupMenu(m, TPM_RETURNCMD, pt.x, pt.y, None, hwnd, None).0;
                let _ = DestroyMenu(m);
                if cmd == 4 {
                    shell_verb(hwnd, &folder, &items, b"properties ");
                }
                UI.with(|u| {
                    let mut ui = u.borrow_mut();
                    let n = ui.entries.len();
                    let now = selected(&ui);
                    let mut want: Vec<usize> = match cmd {
                        1 => (0..n).collect(),
                        2 => Vec::new(),
                        3 => (0..n).filter(|i| !now.contains(i)).collect(),
                        _ => now,
                    };
                    ui.sel = if want.is_empty() { None } else { Some(want.remove(0)) };
                    ui.more = want;
                });
            }
        }
        Hit::Cmd(7) => {
            if let Ok(m) = CreatePopupMenu() {
                let (hid, ext) = (show_hidden(), !hide_ext());
                let view = UI.with(|u| u.borrow().view);
                for (id, v, label) in [(11usize, 1u8, w!("Large icons")), (12, 2, w!("List")), (10, 0, w!("Details"))] {
                    let _ = AppendMenuW(m, if view == v { MF_STRING | MF_CHECKED } else { MF_STRING }, id, label);
                }
                let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
                let _ = AppendMenuW(m, if ext { MF_STRING | MF_CHECKED } else { MF_STRING }, 1, w!("File name extensions"));
                let _ = AppendMenuW(m, if hid { MF_STRING | MF_CHECKED } else { MF_STRING }, 2, w!("Hidden items"));
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let cmd = TrackPopupMenu(m, TPM_RETURNCMD, pt.x, pt.y, None, hwnd, None).0;
                let _ = DestroyMenu(m);
                match cmd {
                    1 => crate::qsx::set_reg_dword(ADVANCED, w!("HideFileExt"), ext as u32),
                    2 => crate::qsx::set_reg_dword(ADVANCED, w!("Hidden"), if hid { 2 } else { 1 }),
                    10..=12 => UI.with(|u| { let mut ui = u.borrow_mut(); ui.view = [0, 1, 2][(cmd - 10) as usize]; ui.scroll = 0.0; }),
                    _ => {}
                }
                UI.with(|u| load(&mut u.borrow_mut()));
            }
        }
        Hit::Win(0) => {
            let _ = ShowWindow(hwnd, SW_MINIMIZE);
        }
        Hit::Win(1) => {
            let _ = ShowWindow(hwnd, if IsZoomed(hwnd).as_bool() { SW_RESTORE } else { SW_MAXIMIZE });
        }
        Hit::TabX(k) => {
            if UI.with(|u| close_tab(&mut u.borrow_mut(), k)) {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        Hit::Win(_) => {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        _ => {}
    }
}

pub unsafe fn show(inst: HINSTANCE, path: Option<String>) {
    let mut h = HWND(WIN.load(Relaxed) as *mut c_void);
    if h.is_invalid() {
        let wc = WNDCLASSW { style: CS_DBLCLKS, lpfnWndProc: Some(proc), hInstance: inst, lpszClassName: w!("shellbits_files"), hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
        RegisterClassW(&wc);
        let Ok(w_) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("shellbits_files"), w!("File Explorer"), WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT, 1120, 700, None, None, Some(inst), None) else { return };
        h = w_;
        WIN.store(h.0 as isize, Relaxed);
        let dark = 1u32;
        let _ = DwmSetWindowAttribute(h, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const c_void, 4);
        let _ = DwmExtendFrameIntoClientArea(h, &MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 1, cyBottomHeight: 0 });
        let _ = SetWindowPos(h, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER);
    }
    UI.with(|u| {
        let mut ui = u.borrow_mut();
        ui.places = places();
        if !IsWindowVisible(h).as_bool() || ui.tabs.is_empty() {
            ui.tabs = vec![Default::default()];
            ui.tab = 0;
            ui.back.clear();
            ui.fwd.clear();
        }
        let start = path.or_else(|| (IsWindowVisible(h).as_bool() && !ui.path.is_empty()).then(|| ui.path.clone())).or(Some(HOME.to_string())).or_else(|| known(&FOLDERID_Profile)).unwrap_or_else(|| THIS_PC.into());
        go(&mut ui, start, false);
    });
    let _ = ShowWindow(h, SW_SHOW);
    crate::flyout::force_foreground(h);
    let _ = InvalidateRect(Some(h), None, false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_lay_items_out() {
        // details: one per row, full width
        assert_eq!(cell(0, 3, 10, 500.0), ((0.0, 3.0 * ROW, 500.0, ROW), 10.0 * ROW));
        // large icons: 104 wide tiles, as many as fit (4 in 430), 10 items -> 3 rows
        assert_eq!(cell(1, 5, 10, 430.0), ((104.0, 112.0, 104.0, 112.0), 336.0));
        // list: 260 wide names; a narrow pane still shows one column
        assert_eq!(cell(2, 3, 4, 600.0), ((260.0, 24.0, 260.0, 24.0), 48.0));
        assert_eq!(cell(2, 3, 4, 100.0).0, (0.0, 72.0, 260.0, 24.0));
        assert_eq!(cell(1, 0, 0, 430.0).1, 0.0);
    }
    fn e(name: &str, dir: bool, size: u64) -> Entry {
        Entry { name: name.into(), dir, size, modified: size, kind: String::new() }
    }
    #[test]
    fn folders_first_then_column() {
        let mut v = vec![e("b.txt", false, 5), e("Zeta", true, 0), e("a.txt", false, 9), e("alpha", true, 0)];
        sort(&mut v, 0, true);
        assert_eq!(v.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["alpha", "Zeta", "a.txt", "b.txt"]);
        sort(&mut v, 3, false);
        assert_eq!(v[2].name, "a.txt"); // biggest file first, folders still on top
    }
    #[test]
    fn sizes_and_parents() {
        assert_eq!(size_text(1), "1 KB");
        assert_eq!(size_text(1536 * 1024), "1,536 KB");
        assert_eq!(parent("C:\\Users\\me"), "C:\\Users");
        assert_eq!(parent("C:\\"), THIS_PC);
    }
    #[test]
    fn breadcrumbs_and_new_folder_names() {
        let c = crumbs("C:\\Users\\me", |r| format!("Disk ({})", &r[..2]));
        assert_eq!(c.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["This PC", "Disk (C:)", "Users", "me"]);
        assert_eq!(c[3].1, "C:\\Users\\me");
        assert_eq!(c[1].1, "C:\\");
        let dir = std::env::temp_dir().join("shellbits-newfolder-test");
        let _ = std::fs::create_dir_all(dir.join("New folder"));
        assert_eq!(new_folder_name(&dir.display().to_string()), "New folder (2)");
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn closing_tabs() {
        assert_eq!(after_close(1, 0, 0), None);
        assert_eq!(after_close(3, 1, 1), Some(1)); // the next tab slides in
        assert_eq!(after_close(3, 2, 2), Some(1));
        assert_eq!(after_close(3, 2, 0), Some(1));
        assert_eq!(after_close(3, 0, 2), Some(0));
    }
    #[test]
    fn click_selection_rules() {
        assert_eq!(click_select(None, &[], 3, false, false), (Some(3), vec![]));
        assert_eq!(click_select(Some(3), &[], 5, true, false), (Some(3), vec![5]));
        assert_eq!(click_select(Some(3), &[5], 5, true, false), (Some(3), vec![]));
        assert_eq!(click_select(Some(2), &[], 5, false, true), (Some(2), vec![3, 4, 5]));
        assert_eq!(click_select(Some(5), &[], 2, false, true), (Some(5), vec![2, 3, 4]));
        assert_eq!(click_select(Some(3), &[5], 3, true, false), (Some(5), vec![]));
        assert_eq!(click_select(Some(3), &[4, 5], 9, false, false), (Some(9), vec![]));
    }
    #[test]
    fn lists_a_real_folder() {
        let dir = std::env::temp_dir().join("shellbits-files-test");
        let _ = std::fs::create_dir_all(dir.join("sub"));
        std::fs::write(dir.join("note.txt"), b"hello").unwrap();
        let v = list(&dir.display().to_string());
        assert!(v.iter().any(|x| x.name == "sub" && x.dir));
        assert!(v.iter().any(|x| x.name == "note.txt" && !x.dir && x.size == 5 && !x.kind.is_empty()));
        let _ = std::fs::remove_dir_all(dir);
    }
}

fn request_file() -> std::path::PathBuf {
    std::env::temp_dir().join("shellbits-open-folder.txt")
}

/// Ask the running Rust UI to open `path` in File Explorer (works from any thread or a second process).
pub fn request(path: &str) {
    let _ = std::fs::write(request_file(), path);
    unsafe {
        if let Ok(f) = FindWindowW(w!("shellbits_flyout"), None) {
            let _ = PostMessageW(Some(f), WM_HOTKEY, WPARAM(crate::FILES_ID as usize), LPARAM(0));
        }
    }
}

/// The folder a `request` asked for, if one is waiting.
pub fn take_request() -> Option<String> {
    let p = request_file();
    let s = std::fs::read_to_string(&p).ok()?;
    let _ = std::fs::remove_file(p);
    let s = s.trim().trim_matches('"').to_string();
    (!s.is_empty()).then_some(s)
}

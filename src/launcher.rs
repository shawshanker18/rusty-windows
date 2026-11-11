//! Windows 11 Start menu + taskbar search, measured 1:1 from the real ones on this PC (100% scale, DIPs).
//!
//! Start  (Win key / taskbar Start button): 832x864, centred, search band, Pinned (8 cols),
//!        Recent files (3x2), All apps list, footer with user + power.
//! Search (taskbar search box / Win+S): 832x864 anchored to the search box. You type *in the taskbar
//!        box* (our pill sits exactly on top of it). Empty: "Recent searches" with times.
//!        Typing: back arrow + filter chips, Best match / Apps / Settings / Documents / Folders on the
//!        left, details pane with Open / Options on the right. Typing in Start shows the same results.
//! Icons load on a background thread once, so scrolling never waits on the disk.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use windows::core::{w, Interface, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::flyout::{glyph, rect, rrect, utf16};

pub const WM_TOGGLE: u32 = WM_APP + 3; // wParam: 0 = Start, 1 = Search
pub const WM_PRELOAD: u32 = WM_APP + 4;
const WM_REFOCUS: u32 = WM_APP + 5;
const WM_LOADED: u32 = WM_APP + 6;
const T_SCROLL: usize = 10;
const T_OPEN: usize = 11;
pub static LAUNCHER: AtomicIsize = AtomicIsize::new(0);
pub static PILL: AtomicIsize = AtomicIsize::new(0);

// ---------------- measured layout (DIPs) ----------------
const W: f32 = 832.0;
const H: f32 = 864.0;
const GAP: f32 = 13.0; // panel bottom sits 13px above the taskbar
// Start
const BAND_H: f32 = 64.0;
const SBOX: D2D_RECT_F = D2D_RECT_F { left: 32.0, top: 17.0, right: 799.0, bottom: 47.0 };
const FOOT_Y: f32 = 798.0;
const COLS: usize = 8;
const TILE_W: f32 = 96.0;
const TILE_H: f32 = 84.0;
const GRID_X: f32 = 32.0;
const MAX_PINS: usize = 24;
const REC_COL_W: f32 = 256.0;
const REC_ROW_H: f32 = 56.0;
const ALL_ROW_H: f32 = 40.0;
// Search
const S_TOP: f32 = 64.0; // header / chips row height
const RS_ROW_H: f32 = 60.0; // "Recent searches" rows
const LEFT: D2D_RECT_F = D2D_RECT_F { left: 25.0, top: 0.0, right: 354.0, bottom: 0.0 };
const PANE: D2D_RECT_F = D2D_RECT_F { left: 375.0, top: 77.0, right: 799.0, bottom: 840.0 };
const BEST_H: f32 = 56.0;
const RES_ROW_H: f32 = 62.0;
const SEC_H: f32 = 44.0;
const WHEEL_STEP: f32 = 100.0;
const REFRESH_SECS: u64 = 60;

// ---------------- measured colours (opaque, dark theme) ----------------
const fn rgb(v: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: ((v >> 16) & 255) as f32 / 255.0, g: ((v >> 8) & 255) as f32 / 255.0, b: (v & 255) as f32 / 255.0, a: 1.0 }
}
const C_PANEL: D2D1_COLOR_F = rgb(0x2B2B2B);
const C_BAND: D2D1_COLOR_F = rgb(0x202020);
const C_BOX: D2D1_COLOR_F = rgb(0x1F1F1F);
const C_FOOT: D2D1_COLOR_F = rgb(0x232323);
const C_HOVER: D2D1_COLOR_F = rgb(0x353535);
const C_SEL: D2D1_COLOR_F = rgb(0x3B3B3B);
const C_TILE: D2D1_COLOR_F = rgb(0x393939);
const C_PANE: D2D1_COLOR_F = rgb(0x363636);
const C_PREVIEW: D2D1_COLOR_F = rgb(0x3B3B3B);
const C_PREVIEW_EDGE: D2D1_COLOR_F = rgb(0x454545);
const C_BUTTON: D2D1_COLOR_F = rgb(0x404040);
const C_CHIP: D2D1_COLOR_F = rgb(0x3A3A3A);
const C_CHIP_EDGE: D2D1_COLOR_F = rgb(0x4A4A4A);
const C_TEXT2: D2D1_COLOR_F = D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 0.786 };
const C_BORDER: D2D1_COLOR_F = D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 0.0698 };

// Segoe Fluent Icons
const G_SEARCH: u32 = 0xE721;
const G_POWER: u32 = 0xE7E8;
const G_USER: u32 = 0xE77B;
const G_BACK: u32 = 0xE72B;
const G_CHEVRON: u32 = 0xE76C;
const G_DOWN: u32 = 0xE70D;
const G_GEAR: u32 = 0xE713;
const G_LINK: u32 = 0xE703;
const G_FOLDER: u32 = 0xE8B7;

#[derive(Clone, Copy, PartialEq, Default)]
enum Mode {
    #[default]
    Start,
    Search,
}

#[derive(Clone, Copy, PartialEq)]
enum Cat {
    App,
    Setting,
    File,
    Folder,
    Photo,
}

#[derive(Clone)]
struct Entry {
    name: String,
    key: String,        // lowercase name, for matching
    target: String,     // what ShellExecute opens
    icon: String,       // shell parsing name for the icon ("" = use glyph)
    glyph: u32,         // fallback icon
    sub: &'static str,  // "App", "System", "File", "Folder", "Settings"
    cat: Cat,
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Entry(usize),
    Power,
    Thumb,
    Back,
    Chip(usize),
    Open,
    Options,
    ShowAll,
}

#[derive(Clone, Copy)]
enum Kind {
    Head(&'static str, bool), // title, has ">" chevron
    Tile(usize),
    Recent(usize),
    Row(usize),
    RecentSearch(usize),
    Best(usize),
    Result(usize),
}

struct Item {
    kind: Kind,
    r: D2D_RECT_F, // content coordinates
}

const CHIPS: [&str; 6] = ["All", "Apps", "Documents", "Settings", "Folders", "Photos"];

struct Gfx {
    rt: ID2D1DCRenderTarget,
    dw: IDWriteFactory,
    icon12: IDWriteTextFormat,
    icon16: IDWriteTextFormat,
    icon24: IDWriteTextFormat,
    body: IDWriteTextFormat,
    body_c: IDWriteTextFormat,
    small: IDWriteTextFormat,
    small_r: IDWriteTextFormat,
    body_r: IDWriteTextFormat,
    tile: IDWriteTextFormat,
    head: IDWriteTextFormat,
    title: IDWriteTextFormat,
}

#[derive(Default)]
struct State {
    mode: Mode,
    entries: Vec<Entry>, // apps, then settings, folders, recent files
    recent_files: Vec<(usize, u64)>, // entry index, modified unix secs
    loaded: Option<Instant>,
    icons: HashMap<String, Vec<u8>>, // ICON_PX premultiplied BGRA
    bitmaps: HashMap<String, ID2D1Bitmap>,
    bitmaps24: HashMap<String, ID2D1Bitmap>, // list rows / recent tiles draw 24px icons
    preview: Option<(String, ID2D1Bitmap)>,
    query: String,
    chip: usize,
    hits: Vec<usize>,
    sel: Option<usize>,
    hover: Option<Hit>,
    pins: Vec<String>, // targets
    counts: HashMap<String, (u32, u64)>, // target -> (launches, last unix secs)
    items: Vec<Item>,
    content_h: f32,
    scroll: f32,
    target: f32,
    drag: Option<(f32, f32)>,
    user: String,
    show_all_recent: bool,
    gfx: Option<Gfx>,
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
    static SHOWN_AT: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
    static OPEN_ANIM: std::cell::Cell<Option<(Instant, i32)>> = const { std::cell::Cell::new(None) };
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ======================= data: apps, settings, folders, recent files =======================

/// Settings pages people actually search for (opened via ms-settings: links).
const SETTINGS: [(&str, &str); 36] = [
    ("Settings", "ms-settings:"), ("Display settings", "ms-settings:display"), ("Sound settings", "ms-settings:sound"),
    ("Bluetooth & devices", "ms-settings:bluetooth"), ("Wi-Fi settings", "ms-settings:network-wifi"),
    ("Network & internet", "ms-settings:network"), ("Personalization", "ms-settings:personalization"),
    ("Background", "ms-settings:personalization-background"), ("Colors", "ms-settings:colors"),
    ("Themes", "ms-settings:themes"), ("Lock screen", "ms-settings:lockscreen"), ("Taskbar settings", "ms-settings:taskbar"),
    ("Start settings", "ms-settings:personalization-start"), ("Installed apps", "ms-settings:appsfeatures"),
    ("Default apps", "ms-settings:defaultapps"), ("Startup apps", "ms-settings:startupapps"),
    ("Accounts", "ms-settings:accounts"), ("Sign-in options", "ms-settings:signinoptions"),
    ("Date & time", "ms-settings:dateandtime"), ("Language & region", "ms-settings:regionlanguage"),
    ("Gaming", "ms-settings:gaming"), ("Game Mode", "ms-settings:gaming-gamemode"),
    ("Privacy & security", "ms-settings:privacy"), ("Windows Security", "ms-settings:windowsdefender"),
    ("Windows Update", "ms-settings:windowsupdate"), ("Storage", "ms-settings:storagesense"),
    ("Power & battery", "ms-settings:powersleep"), ("Battery saver", "ms-settings:batterysaver"),
    ("Notifications", "ms-settings:notifications"), ("Focus", "ms-settings:focus"),
    ("Mouse settings", "ms-settings:mousetouchpad"), ("Keyboard settings", "ms-settings:typing"),
    ("Printers & scanners", "ms-settings:printers"), ("About your PC", "ms-settings:about"),
    ("Night light", "ms-settings:nightlight"), ("Multitasking", "ms-settings:multitasking"),
];

unsafe fn take_pwstr(p: windows::core::PWSTR) -> String {
    let s = p.to_string().unwrap_or_default();
    CoTaskMemFree(Some(p.0 as *const c_void));
    s
}

unsafe fn enumerate_apps() -> windows::core::Result<Vec<Entry>> {
    let folder: IShellItem = SHGetKnownFolderItem(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None)?;
    let en: IEnumShellItems = folder.BindToHandler(None, &BHID_EnumItems)?;
    let mut out = Vec::new();
    loop {
        let mut one = [None];
        let mut n = 0;
        if en.Next(&mut one, Some(&mut n)).is_err() || n == 0 {
            break;
        }
        let Some(item) = one[0].take() else { break };
        let name = take_pwstr(item.GetDisplayName(SIGDN_NORMALDISPLAY)?);
        let id = take_pwstr(item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING)?);
        if name.is_empty() {
            continue;
        }
        // Windows labels its own tools "System" in search
        let system = id.starts_with("Microsoft.Windows.") || id.contains("\\system32\\") || id.contains("\\System32\\")
            || matches!(name.as_str(), "Settings" | "File Explorer" | "Control Panel" | "Task Manager" | "Run");
        let target = format!("shell:AppsFolder\\{id}");
        out.push(Entry { key: name.to_lowercase(), name, icon: target.clone(), target, glyph: 0, sub: if system { "System" } else { "App" }, cat: Cat::App });
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(out)
}

fn known_folders() -> Vec<Entry> {
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    ["Desktop", "Documents", "Downloads", "Pictures", "Music", "Videos"]
        .iter()
        .map(|n| {
            let p = format!("{home}\\{n}");
            Entry { name: n.to_string(), key: n.to_lowercase(), target: p.clone(), icon: p, glyph: G_FOLDER, sub: "Folder", cat: Cat::Folder }
        })
        .collect()
}

/// Windows' own "Recent" list (the .lnk files Explorer keeps), newest first.
fn recent_files() -> Vec<(Entry, u64)> {
    let dir = format!("{}\\Microsoft\\Windows\\Recent", std::env::var("APPDATA").unwrap_or_default());
    let mut v: Vec<(Entry, u64)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")))
        .filter_map(|e| {
            let t = e.metadata().ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
            let p = e.path();
            let name = p.file_stem()?.to_string_lossy().to_string();
            // folders show up as plain names; files have an extension
            let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).filter(|e| (1..=5).contains(&e.len()));
            let is_file = ext.is_some();
            let photo = ext.as_deref().is_some_and(|e| matches!(e, "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "heic" | "tif" | "tiff"));
            let ps = p.to_string_lossy().to_string();
            Some((Entry {
                key: name.to_lowercase(),
                name,
                target: ps.clone(),
                icon: ps,
                glyph: if is_file { 0xE8A5 } else { G_FOLDER },
                sub: if is_file { "File" } else { "Folder" },
                cat: if photo { Cat::Photo } else if is_file { Cat::File } else { Cat::Folder },
            }, t))
        })
        .collect();
    v.retain(|(e, _)| !e.name.starts_with("ms-") && !e.name.contains("--"));
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v.truncate(60);
    v
}

// ---------------- background loader ----------------

#[derive(Default)]
struct Inbox {
    data: Option<(Vec<Entry>, Vec<(usize, u64)>)>,
    icons: Vec<(String, Vec<u8>)>,
}
static INBOX: Mutex<Inbox> = Mutex::new(Inbox { data: None, icons: Vec::new() });
static LOADING: AtomicBool = AtomicBool::new(false);
static HAVE_ICON: Mutex<Option<HashSet<String>>> = Mutex::new(None);
const ICON_PX: u32 = 32;

/// Shell icon at `px`, from a big source, empty padding trimmed, high-quality cubic downscale.
pub(crate) unsafe fn icon_pixels(wic: &IWICImagingFactory, parsing: &str, px: u32) -> windows::core::Result<Vec<u8>> {
    let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(parsing), None)?;
    let f: IShellItemImageFactory = item.cast()?;
    let ask = if px <= 32 { 48 } else { px as i32 }; // bigger requests make Windows frame small icons
    let hbm = f.GetImage(SIZE { cx: ask, cy: ask }, SIIGBF_ICONONLY)?;
    let bmp = wic.CreateBitmapFromHBITMAP(hbm, HPALETTE::default(), WICBitmapUsePremultipliedAlpha);
    let _ = DeleteObject(hbm.into());
    let conv = wic.CreateFormatConverter()?;
    conv.Initialize(&bmp?, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
    let (mut cw, mut ch) = (0, 0);
    conv.GetSize(&mut cw, &mut ch)?;
    let mut big = vec![0u8; (cw * ch * 4) as usize];
    conv.CopyPixels(std::ptr::null(), cw * 4, &mut big)?;
    let (mut x0, mut y0, mut x1, mut y1) = (cw, ch, 0, 0);
    for y in 0..ch {
        for x in 0..cw {
            if big[((y * cw + x) * 4 + 3) as usize] > 8 {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
    }
    let src = wic.CreateBitmapFromMemory(cw, ch, &GUID_WICPixelFormat32bppPBGRA, cw * 4, &big)?;
    let side = x1.saturating_sub(x0).max(y1.saturating_sub(y0));
    let r = if side > 0 && side < cw * 3 / 4 {
        // old apps ship small icons that Windows centres on an empty 256px canvas
        let side = (side + side / 8).min(cw.min(ch));
        let cx = ((x0 + x1) / 2).saturating_sub(side / 2).min(cw - side);
        let cy = ((y0 + y1) / 2).saturating_sub(side / 2).min(ch - side);
        WICRect { X: cx as i32, Y: cy as i32, Width: side as i32, Height: side as i32 }
    } else {
        WICRect { X: 0, Y: 0, Width: cw as i32, Height: ch as i32 }
    };
    let clip = wic.CreateBitmapClipper()?;
    clip.Initialize(&src, &r)?;
    let scale = wic.CreateBitmapScaler()?;
    scale.Initialize(&clip, px, px, WICBitmapInterpolationModeHighQualityCubic)?;
    let mut out = vec![0u8; (px * px * 4) as usize];
    scale.CopyPixels(std::ptr::null(), px * 4, &mut out)?;
    Ok(out)
}

fn spawn_loader(notify: Option<HWND>) {
    if LOADING.swap(true, Relaxed) {
        return;
    }
    let h = notify.map(|h| h.0 as isize);
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        load_all(h.map(|h| HWND(h as *mut c_void)));
        LOADING.store(false, Relaxed);
    });
}

unsafe fn load_all(notify: Option<HWND>) {
    let post = || {
        if let Some(h) = notify {
            let _ = PostMessageW(Some(h), WM_LOADED, WPARAM(0), LPARAM(0));
        }
    };
    let mut entries = enumerate_apps().unwrap_or_default();
    for (name, uri) in SETTINGS {
        entries.push(Entry { name: name.into(), key: name.to_lowercase(), target: uri.into(), icon: String::new(), glyph: G_GEAR, sub: "Settings", cat: Cat::Setting });
    }
    entries.extend(known_folders());
    let mut recent = Vec::new();
    for (e, t) in recent_files() {
        recent.push((entries.len(), t));
        entries.push(e);
    }
    let icon_keys: Vec<String> = entries.iter().filter(|e| !e.icon.is_empty()).map(|e| e.icon.clone()).collect();
    INBOX.lock().unwrap().data = Some((entries, recent));
    post();
    let Ok(wic) = CoCreateInstance::<_, IWICImagingFactory>(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) else { return };
    let have = HAVE_ICON.lock().unwrap().clone().unwrap_or_default();
    let mut batch = Vec::new();
    for k in icon_keys {
        if have.contains(&k) {
            continue;
        }
        if let Ok(px) = icon_pixels(&wic, &k, ICON_PX) {
            batch.push((k, px));
        }
        if batch.len() >= 32 {
            INBOX.lock().unwrap().icons.append(&mut batch);
            post();
        }
    }
    INBOX.lock().unwrap().icons.append(&mut batch);
    post();
    if let Some(h) = notify {
        let _ = PostMessageW(Some(h), WM_LOADED, WPARAM(1), LPARAM(0)); // 1 = all done
    }
}

fn drain_inbox(st: &mut State) {
    let mut inbox = INBOX.lock().unwrap();
    if let Some((entries, recent)) = inbox.data.take() {
        let sel_t = st.sel.and_then(|i| st.entries.get(i)).map(|e| e.target.clone());
        st.entries = entries;
        st.recent_files = recent;
        st.counts = history_for(&st.entries);
        st.loaded = Some(Instant::now());
        if st.pins.is_empty() {
            st.pins = default_pins(&st.entries);
        }
        let (s, t) = (st.scroll, st.target);
        refilter(st);
        st.scroll = s.min(max_scroll(st));
        st.target = t.min(max_scroll(st));
        st.sel = sel_t.and_then(|t| st.entries.iter().position(|e| e.target == t)).or(st.sel);
    }
    let mut have = HAVE_ICON.lock().unwrap();
    let have = have.get_or_insert_with(HashSet::new);
    for (k, px) in inbox.icons.drain(..) {
        have.insert(k.clone());
        st.icons.insert(k, px);
    }
}

// ---------------- pins / history ----------------

fn data_file(name: &str) -> std::path::PathBuf {
    std::env::current_exe().map(|p| p.with_file_name(name)).unwrap_or_else(|_| name.into())
}

fn load_counts() -> HashMap<String, (u32, u64)> {
    std::fs::read_to_string(data_file("launches.txt"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            match f.as_slice() {
                [n, t, id] => Some((fix_target(id), (n.parse().ok()?, t.parse().ok()?))),
                [n, id] => Some((fix_target(id), (n.parse().ok()?, 0))),
                _ => None,
            }
        })
        .collect()
}

/// Older files stored bare app ids.
fn fix_target(id: &str) -> String {
    if id.contains(':') && !id.contains(":\\") { id.to_string() } else if id.contains('\\') && id.contains(":\\") && !id.starts_with("shell:") { format!("shell:AppsFolder\\{id}") } else { format!("shell:AppsFolder\\{id}") }
}

/// Windows' own launch history (what Start/Search use for "recent" and times): UserAssist, ROT13 names.
fn user_assist() -> HashMap<String, (u32, u64)> {
    use windows::Win32::System::Registry::*;
    let mut out = HashMap::new();
    for guid in ["{CEBFF5CD-ACE2-4F4F-9178-9926F41749EA}", "{F4E57C4B-2036-45F0-A9AB-443BCFE33D9F}"] {
        let path = HSTRING::from(format!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\UserAssist\\{guid}\\Count"));
        let mut key = HKEY::default();
        unsafe {
            if RegOpenKeyExW(HKEY_CURRENT_USER, &path, Some(0), KEY_READ, &mut key).is_err() {
                continue;
            }
            for i in 0.. {
                let mut name = [0u16; 1024];
                let mut nlen = name.len() as u32;
                let mut data = [0u8; 4096]; // one session value is ~1.6 KB
                let mut dlen = data.len() as u32;
                let r = RegEnumValueW(key, i, Some(windows::core::PWSTR(name.as_mut_ptr())), &mut nlen, None, None, Some(data.as_mut_ptr()), Some(&mut dlen));
                if r == ERROR_NO_MORE_ITEMS {
                    break;
                }
                if r.is_err() || dlen < 68 {
                    continue;
                }
                let raw = String::from_utf16_lossy(&name[..nlen as usize]);
                let id: String = raw.chars().map(|c| match c {
                    'a'..='z' => (((c as u8 - b'a' + 13) % 26) + b'a') as char,
                    'A'..='Z' => (((c as u8 - b'A' + 13) % 26) + b'A') as char,
                    _ => c,
                }).collect();
                let count = u32::from_le_bytes(data[4..8].try_into().unwrap());
                let ft = u64::from_le_bytes(data[60..68].try_into().unwrap());
                let unix = (ft / 10_000_000).saturating_sub(11_644_473_600);
                if count > 0 && unix > 0 {
                    out.insert(id, (count, unix));
                }
            }
            let _ = RegCloseKey(key);
        }
    }
    out
}

/// Our history, topped up with Windows' (newest time wins). Windows records Store apps by
/// app id, desktop apps by exe path and Start shortcuts by .lnk path, so match all three.
fn history_for(entries: &[Entry]) -> HashMap<String, (u32, u64)> {
    let mut h = load_counts();
    let by_target: HashMap<&str, usize> = entries.iter().enumerate().map(|(i, e)| (e.target.as_str(), i)).collect();
    let by_name: HashMap<String, usize> = entries.iter().enumerate().filter(|(_, e)| e.cat == Cat::App).map(|(i, e)| (e.key.clone(), i)).collect();
    for (raw, (n, t)) in user_assist() {
        let direct = format!("shell:AppsFolder\\{raw}");
        let stem = raw.rsplit(['\\', '/']).next().unwrap_or(&raw);
        let stem = stem.strip_suffix(".lnk").or_else(|| stem.strip_suffix(".exe")).unwrap_or(stem).to_lowercase();
        let hit = by_target.get(direct.as_str()).copied()
            .or_else(|| by_name.get(&stem).copied())
            .or_else(|| entries.iter().position(|e| e.cat == Cat::App && e.target.to_lowercase().ends_with(&raw.to_lowercase())));
        if let Some(i) = hit {
            let e = h.entry(entries[i].target.clone()).or_insert((0, 0));
            e.0 = e.0.max(n);
            e.1 = e.1.max(t);
        }
    }
    h
}

fn save_counts(c: &HashMap<String, (u32, u64)>) {
    let s: String = c.iter().map(|(id, (n, t))| format!("{n}\t{t}\t{id}\n")).collect();
    let _ = std::fs::write(data_file("launches.txt"), s);
}

fn load_pins() -> Vec<String> {
    std::fs::read_to_string(data_file("pins2.txt")).unwrap_or_default().lines().map(str::to_string).filter(|l| !l.is_empty()).collect()
}

fn save_pins(p: &[String]) {
    let _ = std::fs::write(data_file("pins2.txt"), p.join("\n"));
}

/// Your current Windows Start pins (read off your Start menu), then common apps.
fn default_pins(entries: &[Entry]) -> Vec<String> {
    let mut pins = Vec::new();
    for want in ["microsoft edge", "microsoft store", "photos", "settings", "calculator", "notepad", "snipping tool", "onenote", "file explorer"] {
        if let Some(e) = entries.iter().find(|e| e.cat == Cat::App && e.key == want) {
            pins.push(e.target.clone());
        }
    }
    pins
}

// ---------------- search ----------------

fn score(key: &str, q: &str) -> Option<u8> {
    if key.starts_with(q) {
        Some(0)
    } else if key.split([' ', '-', '_', '.', '(', '&']).any(|w| w.starts_with(q)) {
        Some(1)
    } else if key.contains(q) {
        Some(2)
    } else {
        None
    }
}

fn chip_allows(chip: usize, c: Cat) -> bool {
    match chip {
        1 => c == Cat::App,
        2 => c == Cat::File,
        3 => c == Cat::Setting,
        4 => c == Cat::Folder,
        5 => c == Cat::Photo,
        _ => true,
    }
}

fn refilter(st: &mut State) {
    let q = st.query.trim().to_lowercase();
    st.hits.clear();
    if !q.is_empty() {
        let mut hits: Vec<(u8, u32, u8, usize)> = st
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| chip_allows(st.chip, e.cat))
            .filter_map(|(i, e)| {
                let n = st.counts.get(&e.target).map(|c| c.0).unwrap_or(0);
                let cat = match e.cat { Cat::App => 0, Cat::Setting => 1, Cat::File | Cat::Photo => 2, Cat::Folder => 3 };
                Some((score(&e.key, &q)?, n, cat, i))
            })
            .collect();
        hits.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)).then(a.3.cmp(&b.3)));
        let mut seen = HashSet::new();
        st.hits = hits.into_iter().map(|h| h.3).filter(|&i| seen.insert(st.entries[i].target.clone())).collect();
    }
    st.sel = st.hits.first().copied();
    st.scroll = 0.0;
    st.target = 0.0;
    layout(st);
}

fn searching(st: &State) -> bool {
    !st.query.trim().is_empty()
}

/// Recent searches = what you launched from here, newest first (falls back to pins on first run).
fn recent_searches(st: &State) -> Vec<usize> {
    let mut v: Vec<(u64, usize)> = st
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.cat != Cat::File || true)
        .filter_map(|(i, e)| st.counts.get(&e.target).filter(|c| c.1 > 0).map(|c| (c.1, i)))
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    let mut seen = HashSet::new();
    let mut out: Vec<usize> = v.into_iter().map(|x| x.1).filter(|&i| seen.insert(st.entries[i].target.clone())).take(10).collect();
    if out.is_empty() {
        out = st.pins.iter().filter_map(|t| st.entries.iter().position(|e| &e.target == t)).take(10).collect();
    }
    out
}

fn view_rect(st: &State) -> D2D_RECT_F {
    match (st.mode, searching(st)) {
        (Mode::Start, _) => rect(0.0, BAND_H, W, FOOT_Y),
        (Mode::Search, false) => rect(0.0, S_TOP, W, H - 16.0),
        (Mode::Search, true) => rect(0.0, S_TOP + 13.0, LEFT.right + 6.0, H - 16.0),
    }
}

fn layout(st: &mut State) {
    let mut items = Vec::new();
    let mut y = 0.0;
    if searching(st) {
        // left column: Best match, then grouped sections (Apps / Settings / Documents / Folders)
        if let Some(&b) = st.hits.first() {
            items.push(Item { kind: Kind::Head("Best match", false), r: rect(LEFT.left + 5.0, y, LEFT.right, y + 40.0) });
            y += 42.0;
            items.push(Item { kind: Kind::Best(b), r: rect(LEFT.left, y, LEFT.right, y + BEST_H) });
            y += BEST_H;
        } else {
            items.push(Item { kind: Kind::Head("No results", false), r: rect(LEFT.left + 5.0, y, LEFT.right, y + 40.0) });
            y += 42.0;
        }
        let groups: [(&'static str, Cat); 5] = [("Apps", Cat::App), ("Settings", Cat::Setting), ("Documents", Cat::File), ("Folders", Cat::Folder), ("Photos", Cat::Photo)];
        let cap = if st.chip == 0 { 4 } else { usize::MAX };
        for (title, cat) in groups {
            let g: Vec<usize> = st.hits.iter().skip(1).copied().filter(|&i| st.entries[i].cat == cat).take(cap).collect();
            if g.is_empty() {
                continue;
            }
            items.push(Item { kind: Kind::Head(title, st.chip == 0), r: rect(LEFT.left + 8.0, y, LEFT.right - 8.0, y + SEC_H) });
            y += SEC_H;
            for i in g {
                items.push(Item { kind: Kind::Result(i), r: rect(LEFT.left, y, LEFT.right, y + RES_ROW_H - 2.0) });
                y += RES_ROW_H;
            }
        }
    } else if st.mode == Mode::Search {
        for i in recent_searches(st) {
            items.push(Item { kind: Kind::RecentSearch(i), r: rect(24.0, y, W - 24.0, y + RS_ROW_H) });
            y += RS_ROW_H;
        }
    } else {
        // Start: Pinned (8 cols), Recent (3 x 2), All
        y += 24.0;
        items.push(Item { kind: Kind::Head("Pinned", false), r: rect(63.0, y, W - 60.0, y + 30.0) });
        y += 38.0;
        let pins: Vec<usize> = st.pins.iter().filter_map(|t| st.entries.iter().position(|e| &e.target == t)).collect();
        for (n, &i) in pins.iter().enumerate() {
            let x = GRID_X + (n % COLS) as f32 * TILE_W;
            let ty = y + (n / COLS) as f32 * TILE_H;
            items.push(Item { kind: Kind::Tile(i), r: rect(x + 2.0, ty, x + TILE_W - 2.0, ty + TILE_H - 4.0) });
        }
        y += pins.len().div_ceil(COLS).max(1) as f32 * TILE_H + 33.0;
        items.push(Item { kind: Kind::Head("Recent", false), r: rect(63.0, y, W - 60.0, y + 30.0) });
        y += 45.0;
        let n_rec = if st.show_all_recent { 12 } else { 6 };
        let files: Vec<usize> = st.recent_files.iter().map(|x| x.0).filter(|&i| matches!(st.entries[i].cat, Cat::File | Cat::Photo)).take(n_rec).collect();
        for (n, &i) in files.iter().enumerate() {
            let x = 52.0 + (n % 3) as f32 * REC_COL_W;
            let ry = y + (n / 3) as f32 * REC_ROW_H;
            items.push(Item { kind: Kind::Recent(i), r: rect(x, ry, x + REC_COL_W - 8.0, ry + REC_ROW_H - 4.0) });
        }
        y += files.len().div_ceil(3).max(1) as f32 * REC_ROW_H + 27.0;
        items.push(Item { kind: Kind::Head("All", false), r: rect(63.0, y, W - 60.0, y + 30.0) });
        y += 38.0;
        for i in 0..st.entries.len() {
            if st.entries[i].cat == Cat::App {
                items.push(Item { kind: Kind::Row(i), r: rect(52.0, y, W - 52.0, y + ALL_ROW_H - 2.0) });
                y += ALL_ROW_H;
            }
        }
    }
    st.content_h = y + 12.0;
    st.items = items;
}

fn max_scroll(st: &State) -> f32 {
    let v = view_rect(st);
    (st.content_h - (v.bottom - v.top)).max(0.0)
}

fn thumb(st: &State) -> Option<D2D_RECT_F> {
    let v = view_rect(st);
    let vh = v.bottom - v.top;
    if st.content_h <= vh {
        return None;
    }
    let th = (vh * vh / st.content_h).max(32.0);
    let y = v.top + 4.0 + (st.scroll / max_scroll(st)) * (vh - 8.0 - th);
    let x = v.right - 10.0;
    Some(rect(x, y, x + 6.0, y + th))
}

// fixed (non-scrolling) hot spots
const POWER: D2D_RECT_F = D2D_RECT_F { left: 743.0, top: 816.0, right: 775.0, bottom: 848.0 };
const BACK: D2D_RECT_F = D2D_RECT_F { left: 21.0, top: 23.0, right: 53.0, bottom: 55.0 };
const OPEN_BTN: D2D_RECT_F = D2D_RECT_F { left: 399.0, top: 277.0, right: 519.0, bottom: 311.0 };
const OPTS_BTN: D2D_RECT_F = D2D_RECT_F { left: 524.0, top: 277.0, right: 645.0, bottom: 311.0 };
const SHOW_ALL: D2D_RECT_F = D2D_RECT_F { left: 690.0, top: 0.0, right: 780.0, bottom: 30.0 }; // content y set per layout

unsafe fn chip_rects(g: &Gfx) -> Vec<D2D_RECT_F> {
    let mut x = 63.0;
    CHIPS
        .iter()
        .map(|c| {
            let w = text_width(g, c, &g.body) + 24.0;
            let r = rect(x, 24.0, x + w, 54.0);
            x += w + 4.0;
            r
        })
        .collect()
}

unsafe fn text_width(g: &Gfx, s: &str, f: &IDWriteTextFormat) -> f32 {
    let t = utf16(s);
    let Ok(l) = g.dw.CreateTextLayout(&t, f, 1000.0, 100.0) else { return 0.0 };
    let mut m = DWRITE_TEXT_METRICS::default();
    let _ = l.GetMetrics(&mut m);
    m.widthIncludingTrailingWhitespace
}

fn inside(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

unsafe fn hit_test(st: &State, x: f32, y: f32) -> Option<Hit> {
    let s = searching(st);
    if st.mode == Mode::Start && inside(&POWER, x, y) {
        return Some(Hit::Power);
    }
    if st.mode == Mode::Search && s {
        if inside(&BACK, x, y) {
            return Some(Hit::Back);
        }
        if let Some(g) = &st.gfx {
            for (i, r) in chip_rects(g).iter().enumerate() {
                if inside(r, x, y) {
                    return Some(Hit::Chip(i));
                }
            }
        }
    }
    if s && st.sel.is_some() {
        let dy = if st.mode == Mode::Start { BAND_H - S_TOP } else { 0.0 };
        if inside(&OPEN_BTN, x, y - dy) {
            return Some(Hit::Open);
        }
        if inside(&OPTS_BTN, x, y - dy) {
            return Some(Hit::Options);
        }
    }
    if let Some(t) = thumb(st) {
        if x >= t.left - 6.0 && x < t.right + 4.0 && y >= t.top && y < t.bottom {
            return Some(Hit::Thumb);
        }
    }
    let v = view_rect(st);
    if !inside(&v, x, y) {
        return None;
    }
    let cy = y - v.top + st.scroll;
    for it in &st.items {
        match it.kind {
            Kind::Head("Recent", _) if st.mode == Mode::Start => {
                let r = rect(SHOW_ALL.left, it.r.top, SHOW_ALL.right, it.r.bottom);
                if inside(&r, x, cy) {
                    return Some(Hit::ShowAll);
                }
            }
            Kind::Tile(i) | Kind::Recent(i) | Kind::Row(i) | Kind::RecentSearch(i) | Kind::Best(i) | Kind::Result(i) if inside(&it.r, x, cy) => {
                return Some(Hit::Entry(i));
            }
            _ => {}
        }
    }
    None
}

// ======================= show / hide / launch =======================

pub unsafe fn preload(hwnd: HWND) {
    ST.with(|s| {
        let mut st = s.borrow_mut();
        st.counts = history_for(&st.entries);
        st.pins = load_pins();
        st.user = display_name();
        if st.gfx.is_none() {
            st.gfx = make_gfx().ok();
        }
    });
    spawn_loader(Some(hwnd));
}

pub(crate) fn display_name() -> String {
    unsafe {
        use windows::Win32::Security::Authentication::Identity::{GetUserNameExW, NameDisplay};
        let mut buf = [0u16; 256];
        let mut n = buf.len() as u32;
        if GetUserNameExW(NameDisplay, Some(windows::core::PWSTR(buf.as_mut_ptr())), &mut n) && n > 0 {
            return String::from_utf16_lossy(&buf[..n as usize]);
        }
    }
    std::env::var("USERNAME").unwrap_or_default()
}

fn pill() -> HWND {
    HWND(PILL.load(Relaxed) as *mut c_void)
}

unsafe fn toggle(hwnd: HWND, mode: Mode) {
    if IsWindowVisible(hwnd).as_bool() {
        let same = ST.with(|s| s.borrow().mode == mode);
        hide(hwnd);
        if same {
            return;
        }
    }
    let stale = ST.with(|s| {
        let mut st = s.borrow_mut();
        st.mode = mode;
        st.counts = history_for(&st.entries);
        st.query.clear();
        st.chip = 0;
        st.hover = None;
        st.show_all_recent = false;
        refilter(&mut st);
        st.loaded.is_none_or(|t| t.elapsed().as_secs() > REFRESH_SECS)
    });
    let mut wa = RECT::default();
    let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut wa as *mut _ as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    let (w, h) = ((W * s).round() as i32, (H * s).round() as i32);
    let sb = [0, 1, 2, 3].map(|i| ZONES[1][i].load(Relaxed));
    let x = if mode == Mode::Search && sb[2] > sb[0] {
        sb[0].min(wa.right - w) // Windows anchors search to the search box's left edge
    } else {
        (wa.left + wa.right - w) / 2
    };
    let rest = wa.bottom - h - (GAP * s).round() as i32;
    // rises from behind the taskbar like Windows' Start (curve measured from film: crate::flyout::open_offset)
    let first = (crate::flyout::open_offset(0.0) * s).round() as i32;
    let _ = SetWindowRgn(hwnd, Some(CreateRectRgn(0, 0, w, (wa.bottom - rest - first).clamp(0, h))), false);
    let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, rest + first, w, h, SWP_SHOWWINDOW);
    OPEN_ANIM.with(|a| a.set(Some((Instant::now(), rest))));
    SetTimer(Some(hwnd), T_OPEN, 8, None);
    SHOWN_AT.with(|t| t.set(Some(Instant::now())));
    if mode == Mode::Search && sb[2] > sb[0] {
        // our pill sits exactly on the taskbar search box: you type there, like Windows
        let _ = SetWindowPos(pill(), Some(HWND_TOPMOST), sb[0], sb[1], sb[2] - sb[0], sb[3] - sb[1], SWP_SHOWWINDOW | SWP_NOACTIVATE);
        let _ = InvalidateRect(Some(pill()), None, false);
    }
    crate::flyout::force_foreground(hwnd);
    let _ = SetFocus(Some(hwnd));
    let _ = InvalidateRect(Some(hwnd), None, false);
    if stale {
        spawn_loader(Some(hwnd));
    }
}

unsafe fn hide(hwnd: HWND) {
    let _ = ShowWindow(hwnd, SW_HIDE);
    let _ = ShowWindow(pill(), SW_HIDE);
    crate::flyout::give_back();
    let _ = windows::Win32::System::ProcessStatus::K32EmptyWorkingSet(windows::Win32::System::Threading::GetCurrentProcess());
}

/// Windows apps that have a Rust replacement: the id of the hotkey message that opens it.
/// `target` is whatever would be launched: a Start-menu id, a "shell:AppsFolder\\..." path, or a typed command.
pub(crate) fn rust_app(target: &str) -> Option<i32> {
    let t = target.trim().trim_matches('"').to_lowercase();
    let t = t.strip_prefix("shell:appsfolder\\").unwrap_or(&t);
    if t.starts_with("microsoft.windowscalculator_") || matches!(t, "calc" | "calc.exe") {
        Some(crate::CALC_ID)
    } else if t.starts_with("windows.immersivecontrolpanel_") || t == "ms-settings:" {
        Some(crate::SETTINGS_ID)
    } else if t == "microsoft.windows.explorer" || matches!(t, "explorer" | "explorer.exe") {
        Some(crate::FILES_ID)
    } else {
        None
    }
}

/// Open `target` in its Rust replacement if it has one. Returns false when Windows should open it as usual.
pub(crate) unsafe fn open_rust(target: &str) -> bool {
    let (Some(id), Ok(f)) = (rust_app(target), FindWindowW(w!("shellbits_flyout"), None)) else { return false };
    PostMessageW(Some(f), WM_HOTKEY, WPARAM(id as usize), LPARAM(0)).is_ok()
}

/// Start a program, file or link and make sure what it opens comes to the FRONT.
/// Windows only lets the process the user last interacted with put a window in front; a desktop
/// program started from the shell otherwise opened behind everything ("nothing happened").
/// So: allow any process to take the foreground, start it, then once its first window shows,
/// bring that window forward ourselves.
pub(crate) fn start_front(target: &str, args: &str, verb: &str) {
    let (target, args, verb) = (target.to_string(), args.to_string(), verb.to_string());
    std::thread::spawn(move || unsafe {
        use windows::Win32::System::Threading::{GetProcessId, WaitForInputIdle, INFINITE};
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        let (t, a, v) = (HSTRING::from(target.as_str()), HSTRING::from(args.as_str()), HSTRING::from(verb.as_str()));
        let mut sei = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI,
            lpVerb: if verb.is_empty() { PCWSTR::null() } else { PCWSTR(v.as_ptr()) },
            lpFile: PCWSTR(t.as_ptr()),
            lpParameters: if args.is_empty() { PCWSTR::null() } else { PCWSTR(a.as_ptr()) },
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        if ShellExecuteExW(&mut sei).is_err() || sei.hProcess.is_invalid() {
            return; // opened by an already running app (Store apps, folders, links): it brings itself forward
        }
        let pid = GetProcessId(sei.hProcess);
        let _ = AllowSetForegroundWindow(pid);
        let _ = WaitForInputIdle(sei.hProcess, 5000);
        let _ = INFINITE;
        // its first visible top-level window, looked for for up to 5 seconds
        for _ in 0..50 {
            if let Some(w) = window_of(pid) {
                if GetForegroundWindow() != w {
                    crate::flyout::force_foreground(w);
                }
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = CloseHandle(sei.hProcess);
    });
}

/// A visible, titled, top-level window belonging to process `pid`.
unsafe fn window_of(pid: u32) -> Option<HWND> {
    unsafe extern "system" fn each(h: HWND, lp: LPARAM) -> windows::core::BOOL {
        let want = &mut *(lp.0 as *mut (u32, Option<HWND>));
        let mut p = 0;
        GetWindowThreadProcessId(h, Some(&mut p));
        if p == want.0 && IsWindowVisible(h).as_bool() && GetWindowTextLengthW(h) > 0 && GetWindow(h, GW_OWNER).map_or(true, |o| o.is_invalid()) {
            want.1 = Some(h);
            return FALSE;
        }
        TRUE
    }
    let mut want: (u32, Option<HWND>) = (pid, None);
    let _ = EnumWindows(Some(each), LPARAM(&mut want as *mut _ as isize));
    want.1
}

unsafe fn launch(hwnd: HWND, i: usize, verb: PCWSTR) {
    let target = ST.with(|s| {
        let mut st = s.borrow_mut();
        let t = st.entries.get(i)?.target.clone();
        let c = st.counts.entry(t.clone()).or_default();
        c.0 += 1;
        c.1 = now();
        save_counts(&st.counts);
        Some(t)
    });
    // hide without handing the focus back to the app that had it before: what we start gets it
    let _ = ShowWindow(hwnd, SW_HIDE);
    let _ = ShowWindow(pill(), SW_HIDE);
    if let Some(t) = target {
        // "Run as administrator" always goes to Windows' own app
        let verb = verb.to_string().unwrap_or_default();
        if verb == "runas" || !open_rust(&t) {
            start_front(&t, "", &verb);
        }
    }
}

unsafe fn entry_menu(hwnd: HWND, i: usize, at_button: bool) {
    let (pinned, target, cat) = ST.with(|s| {
        let st = s.borrow();
        let e = &st.entries[i];
        (st.pins.contains(&e.target), e.target.clone(), e.cat)
    });
    let Ok(m) = CreatePopupMenu() else { return };
    let _ = AppendMenuW(m, MF_STRING, 1, w!("Open"));
    if cat == Cat::App {
        let _ = AppendMenuW(m, MF_STRING, 2, w!("Run as administrator"));
        let _ = AppendMenuW(m, MF_STRING, 3, if pinned { w!("Unpin from Start") } else { w!("Pin to Start") });
    }
    if cat == Cat::File || cat == Cat::Folder {
        let _ = AppendMenuW(m, MF_STRING, 4, w!("Open file location"));
    }
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let flags = if at_button { TPM_RETURNCMD | TPM_LEFTALIGN } else { TPM_RETURNCMD | TPM_RIGHTBUTTON };
    let cmd = TrackPopupMenu(m, flags, pt.x, pt.y, None, hwnd, None).0;
    let _ = DestroyMenu(m);
    match cmd {
        1 => launch(hwnd, i, w!("open")),
        2 => launch(hwnd, i, w!("runas")),
        3 => ST.with(|s| {
            let mut st = s.borrow_mut();
            if pinned {
                st.pins.retain(|p| p != &target);
            } else if st.pins.len() < MAX_PINS {
                st.pins.push(target);
            }
            save_pins(&st.pins);
            layout(&mut st);
        }),
        4 => {
            hide(hwnd);
            ShellExecuteW(None, w!("open"), w!("explorer.exe"), &HSTRING::from(format!("/select,\"{target}\"")), PCWSTR::null(), SW_SHOWNORMAL);
        }
        _ => {}
    }
    let _ = InvalidateRect(Some(hwnd), None, false);
}

unsafe fn power_menu(hwnd: HWND) {
    let Ok(m) = CreatePopupMenu() else { return };
    let _ = AppendMenuW(m, MF_STRING, 1, w!("Lock"));
    let _ = AppendMenuW(m, MF_STRING, 2, w!("Sleep"));
    let _ = AppendMenuW(m, MF_STRING, 3, w!("Shut down"));
    let _ = AppendMenuW(m, MF_STRING, 4, w!("Restart"));
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_BOTTOMALIGN | TPM_RIGHTALIGN, pt.x, pt.y, None, hwnd, None).0;
    let _ = DestroyMenu(m);
    if cmd == 0 {
        return;
    }
    hide(hwnd);
    let run = |args: &str| {
        ShellExecuteW(None, w!("open"), w!("shutdown.exe"), &HSTRING::from(args), PCWSTR::null(), SW_HIDE);
    };
    match cmd {
        1 => { let _ = windows::Win32::System::Shutdown::LockWorkStation(); }
        2 => { let _ = windows::Win32::System::Power::SetSuspendState(false, false, false); }
        3 => run("/s /t 0"),
        _ => run("/r /t 0"),
    }
}

fn scroll_to(hwnd: HWND, target: f32) {
    ST.with(|s| {
        let mut st = s.borrow_mut();
        st.target = target.clamp(0.0, max_scroll(&st));
    });
    unsafe {
        SetTimer(Some(hwnd), T_SCROLL, 8, None);
    }
}

fn reveal_sel(hwnd: HWND) {
    let t = ST.with(|s| {
        let st = s.borrow();
        let i = st.sel?;
        let v = view_rect(&st);
        let vh = v.bottom - v.top;
        let it = st.items.iter().find(|it| matches!(it.kind, Kind::Result(j) | Kind::Best(j) if j == i))?;
        if it.r.top - 40.0 < st.target {
            Some(it.r.top - 40.0)
        } else if it.r.bottom > st.target + vh {
            Some(it.r.bottom - vh)
        } else {
            None
        }
    });
    if let Some(t) = t {
        scroll_to(hwnd, t);
    }
}

fn redraw_all(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
        if IsWindowVisible(pill()).as_bool() {
            let _ = InvalidateRect(Some(pill()), None, false);
        }
    }
}

// ======================= window procedure =======================

pub unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let dips = |lp: LPARAM| {
        let s = GetDpiForWindow(hwnd) as f32 / 96.0;
        ((lp.0 & 0xFFFF) as i16 as f32 / s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s)
    };
    let redraw = || redraw_all(hwnd);
    match msg {
        WM_TOGGLE => toggle(hwnd, if wp.0 == 1 { Mode::Search } else { Mode::Start }),
        WM_PRELOAD => preload(hwnd),
        crate::flyout::WM_DISMISS => {
            if IsWindowVisible(hwnd).as_bool() {
                hide(hwnd);
            }
        }
        WM_LOADED => {
            ST.with(|s| drain_inbox(&mut s.borrow_mut()));
            redraw();
            if wp.0 == 1 && !IsWindowVisible(hwnd).as_bool() {
                // loading pulled in shell + imaging code; idle, so hand the pages back
                let _ = windows::Win32::System::ProcessStatus::K32EmptyWorkingSet(windows::Win32::System::Threading::GetCurrentProcess());
            }
        }
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            let fresh = SHOWN_AT.with(|t| t.get()).is_some_and(|t| t.elapsed().as_millis() < 400);
            if fresh {
                let _ = PostMessageW(Some(hwnd), WM_REFOCUS, WPARAM(0), LPARAM(0));
            } else if GetCapture() != hwnd {
                hide(hwnd);
            }
        }
        WM_REFOCUS => {
            crate::flyout::force_foreground(hwnd);
            let _ = SetFocus(Some(hwnd));
        }
        WM_TIMER => match wp.0 {
            T_SCROLL => {
                let done = ST.with(|s| {
                    let mut st = s.borrow_mut();
                    let d = st.target - st.scroll;
                    if d.abs() < 0.5 {
                        st.scroll = st.target;
                        true
                    } else {
                        st.scroll += d * 0.28;
                        false
                    }
                });
                if done {
                    let _ = KillTimer(Some(hwnd), T_SCROLL);
                }
                redraw();
            }
            T_OPEN => {
                let Some((t0, rest)) = OPEN_ANIM.with(|a| a.get()) else {
                    let _ = KillTimer(Some(hwnd), T_OPEN);
                    return LRESULT(0);
                };
                let s = GetDpiForWindow(hwnd) as f32 / 96.0;
                let off = (crate::flyout::open_offset(t0.elapsed().as_secs_f32() * 1000.0) * s).round() as i32;
                let mut r = RECT::default();
                let _ = GetWindowRect(hwnd, &mut r);
                let mut wa = RECT::default();
                let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut wa as *mut _ as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
                let (w, h) = (r.right - r.left, r.bottom - r.top);
                // the part below the top of the taskbar is cut off while it rises
                let _ = SetWindowRgn(hwnd, if off > 0 { Some(CreateRectRgn(0, 0, w, (wa.bottom - rest - off).clamp(0, h))) } else { None }, true);
                let _ = SetWindowPos(hwnd, None, r.left, rest + off, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
                if off == 0 {
                    OPEN_ANIM.with(|a| a.set(None));
                    let _ = KillTimer(Some(hwnd), T_OPEN);
                }
            }
            _ => {}
        },
        WM_CHAR => {
            let c = wp.0 as u32;
            let changed = ST.with(|s| {
                let mut st = s.borrow_mut();
                match c {
                    0x08 => { st.query.pop(); }
                    0x7F => {
                        let t = st.query.trim_end().rfind(' ').map(|i| i + 1).unwrap_or(0);
                        st.query.truncate(t);
                    }
                    c if c >= 0x20 => {
                        if let Some(ch) = char::from_u32(c) {
                            st.query.push(ch);
                        }
                    }
                    _ => return false,
                }
                refilter(&mut st);
                true
            });
            if changed {
                redraw();
            }
        }
        WM_KEYDOWN => {
            let vk = VIRTUAL_KEY(wp.0 as u16);
            match vk {
                VK_ESCAPE => {
                    let had = ST.with(|s| {
                        let mut st = s.borrow_mut();
                        let had = !st.query.is_empty();
                        if had {
                            st.query.clear();
                            refilter(&mut st);
                        }
                        had
                    });
                    if !had {
                        hide(hwnd);
                    }
                }
                VK_RETURN => {
                    if let Some(i) = ST.with(|s| { let st = s.borrow(); if searching(&st) { st.sel } else { None } }) {
                        launch(hwnd, i, w!("open"));
                    }
                    return LRESULT(0);
                }
                VK_DOWN | VK_UP => {
                    let moved = ST.with(|s| {
                        let mut st = s.borrow_mut();
                        let order: Vec<usize> = st.items.iter().filter_map(|it| match it.kind { Kind::Best(i) | Kind::Result(i) => Some(i), _ => None }).collect();
                        if order.is_empty() {
                            return false;
                        }
                        let pos = st.sel.and_then(|i| order.iter().position(|&h| h == i)).unwrap_or(0) as i32;
                        let n = (pos + if vk == VK_DOWN { 1 } else { -1 }).clamp(0, order.len() as i32 - 1);
                        st.sel = Some(order[n as usize]);
                        true
                    });
                    if moved {
                        reveal_sel(hwnd);
                    } else {
                        let t = ST.with(|s| s.borrow().target);
                        scroll_to(hwnd, t + if vk == VK_DOWN { ALL_ROW_H } else { -ALL_ROW_H });
                    }
                }
                VK_NEXT | VK_PRIOR => {
                    let t = ST.with(|s| s.borrow().target);
                    scroll_to(hwnd, t + if vk == VK_NEXT { 400.0 } else { -400.0 });
                }
                VK_HOME => scroll_to(hwnd, 0.0),
                VK_END => scroll_to(hwnd, f32::MAX),
                _ => return LRESULT(0),
            }
            redraw();
        }
        WM_MOUSEMOVE => {
            let (x, y) = dips(lp);
            let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            let changed = ST.with(|s| {
                let mut st = s.borrow_mut();
                if let Some((y0, s0)) = st.drag {
                    let v = view_rect(&st);
                    let th = thumb(&st).map(|t| t.bottom - t.top).unwrap_or(0.0);
                    let track = (v.bottom - v.top) - 8.0 - th;
                    let val = (s0 + (y - y0) / track.max(1.0) * max_scroll(&st)).clamp(0.0, max_scroll(&st));
                    st.scroll = val;
                    st.target = val;
                    return true;
                }
                let h = hit_test(&st, x, y);
                if let (Some(Hit::Entry(i)), true) = (h, searching(&st)) {
                    st.sel = Some(i); // hover previews it on the right, like Windows
                }
                std::mem::replace(&mut st.hover, h) != h
            });
            if changed {
                redraw();
            }
        }
        WM_MOUSELEAVE => {
            ST.with(|s| s.borrow_mut().hover = None);
            redraw();
        }
        WM_LBUTTONDOWN => {
            let (x, y) = dips(lp);
            let grab = ST.with(|s| {
                let mut st = s.borrow_mut();
                if hit_test(&st, x, y) == Some(Hit::Thumb) {
                    st.drag = Some((y, st.scroll));
                    true
                } else {
                    false
                }
            });
            if grab {
                SetCapture(hwnd);
            }
        }
        WM_LBUTTONUP => {
            let (x, y) = dips(lp);
            if ST.with(|s| s.borrow_mut().drag.take().is_some()) {
                let _ = ReleaseCapture();
                return LRESULT(0);
            }
            let (hit, sel) = ST.with(|s| { let st = s.borrow(); (hit_test(&st, x, y), st.sel) });
            match hit {
                Some(Hit::Entry(i)) => launch(hwnd, i, w!("open")),
                Some(Hit::Power) => power_menu(hwnd),
                Some(Hit::Open) => { if let Some(i) = sel { launch(hwnd, i, w!("open")); } }
                Some(Hit::Options) => { if let Some(i) = sel { entry_menu(hwnd, i, true); } }
                Some(Hit::Back) => ST.with(|s| { let mut st = s.borrow_mut(); st.query.clear(); st.chip = 0; refilter(&mut st); }),
                Some(Hit::Chip(c)) => ST.with(|s| { let mut st = s.borrow_mut(); st.chip = c; refilter(&mut st); }),
                Some(Hit::ShowAll) => ST.with(|s| { let mut st = s.borrow_mut(); st.show_all_recent = !st.show_all_recent; layout(&mut st); }),
                _ => {}
            }
            redraw();
        }
        WM_RBUTTONUP => {
            let (x, y) = dips(lp);
            if let Some(Hit::Entry(i)) = ST.with(|s| hit_test(&s.borrow(), x, y)) {
                entry_menu(hwnd, i, false);
            }
        }
        WM_MOUSEWHEEL => {
            let notches = ((wp.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
            let t = ST.with(|s| s.borrow().target);
            scroll_to(hwnd, t - notches * WHEEL_STEP);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => paint(hwnd, false),
        WM_SETTINGCHANGE | WM_DPICHANGED => ST.with(|s| {
            let mut st = s.borrow_mut();
            st.gfx = None;
            st.bitmaps.clear();
                st.bitmaps24.clear();
            st.preview = None;
        }),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

/// The taskbar search box overlay (you type into it while the search panel is open).
pub unsafe extern "system" fn pill_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint(hwnd, true);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
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
    let fmt = |name, size, weight, align: DWRITE_TEXT_ALIGNMENT| -> windows::core::Result<IDWriteTextFormat> {
        let t = dw.CreateTextFormat(name, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))?;
        t.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        t.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        t.SetTextAlignment(align)?;
        let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
        t.SetTrimming(&trim, &dw.CreateEllipsisTrimmingSign(&t)?)?;
        Ok(t)
    };
    let text = w!("Segoe UI Variable Text");
    let disp = w!("Segoe UI Variable Display");
    let icons = w!("Segoe Fluent Icons");
    let (l, c, r) = (DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_TRAILING);
    let (n, sb) = (DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD);
    Ok(Gfx {
        icon12: fmt(icons, 12.0, n, c)?,
        icon16: fmt(icons, 16.0, n, c)?,
        icon24: fmt(icons, 24.0, n, c)?,
        body: fmt(text, 14.0, n, l)?,
        body_c: fmt(text, 14.0, n, c)?,
        small: fmt(text, 12.0, n, l)?,
        small_r: fmt(text, 12.0, n, r)?,
        body_r: fmt(text, 14.0, n, r)?,
        tile: fmt(text, 12.0, n, c)?,
        head: fmt(text, 14.0, sb, l)?,
        title: fmt(disp, 20.0, sb, l)?,
        rt,
        dw,
    })
}

fn bitmap_from(rt: &ID2D1DCRenderTarget, px: &[u8], size: u32) -> Option<ID2D1Bitmap> {
    let bp = D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
    };
    unsafe { rt.CreateBitmap(D2D_SIZE_U { width: size, height: size }, Some(px.as_ptr() as *const c_void), size * 4, &bp).ok() }
}

/// "1 hour ago" / "19 hours ago" / "2 days ago" (search) and "22h ago" / "Yesterday" (Start).
fn ago(t: u64, short: bool) -> String {
    let d = now().saturating_sub(t);
    let (m, h, days) = (d / 60, d / 3600, d / 86400);
    let pl = |n: u64, w: &str| format!("{n} {w}{} ago", if n == 1 { "" } else { "s" });
    match (short, days, h, m) {
        (true, 0, 0, m) => format!("{}m ago", m.max(1)),
        (true, 0, h, _) => format!("{h}h ago"),
        (true, 1, _, _) => "Yesterday".into(),
        (true, d, _, _) => format!("{d}d ago"),
        (false, 0, 0, m) => pl(m.max(1), "minute"),
        (false, 0, h, _) => pl(h, "hour"),
        (false, d, _, _) => pl(d, "day"),
    }
}

unsafe fn draw(st: &mut State, mem: HDC, rc: &RECT, dpi: f32, is_pill: bool) -> windows::core::Result<()> {
    if st.gfx.is_none() {
        st.gfx = Some(make_gfx()?);
        st.bitmaps.clear();
                st.bitmaps24.clear();
        st.preview = None;
    }
    let rt = st.gfx.as_ref().unwrap().rt.clone();
    rt.BindDC(mem, rc)?;
    rt.SetDpi(dpi, dpi);
    if is_pill {
        return draw_pill(st, &rt, rc, dpi);
    }

    // ---- phase 1: make sure bitmaps exist for what's on screen (needs &mut st) ----
    let v = view_rect(st);
    let (top, bottom) = (st.scroll, st.scroll + (v.bottom - v.top));
    let mut keys: Vec<String> = st
        .items
        .iter()
        .filter(|it| it.r.bottom > top && it.r.top < bottom)
        .filter_map(|it| match it.kind {
            Kind::Tile(i) | Kind::Recent(i) | Kind::Row(i) | Kind::RecentSearch(i) | Kind::Best(i) | Kind::Result(i) => Some(st.entries[i].icon.clone()),
            _ => None,
        })
        .filter(|k| !k.is_empty())
        .collect();
    keys.dedup();
    for k in keys {
        if !st.bitmaps.contains_key(&k) {
            if let Some(px) = st.icons.get(&k) {
                if let Some(b) = bitmap_from(&rt, px, ICON_PX) {
                    st.bitmaps.insert(k.clone(), b);
                }
                if let Some(b) = bitmap_from(&rt, &crate::flyout::shrink(px, ICON_PX, 24), 24) {
                    st.bitmaps24.insert(k, b);
                }
            }
        }
    }
    // big icon for the details pane (loaded on demand at 64px)
    if let Some(i) = st.sel.filter(|_| searching(st)) {
        let k = st.entries[i].icon.clone();
        if !k.is_empty() && st.preview.as_ref().is_none_or(|p| p.0 != k) {
            let wic: Option<IWICImagingFactory> = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok();
            st.preview = wic.and_then(|w| icon_pixels(&w, &k, 64).ok()).and_then(|px| bitmap_from(&rt, &px, 64)).map(|b| (k, b));
        }
    }

    // ---- phase 2: draw ----
    let st = &*st;
    let g = st.gfx.as_ref().unwrap();
    rt.BeginDraw();
    rt.Clear(Some(&crate::flyout::base(C_PANEL)));
    let brush = |c: D2D1_COLOR_F| rt.CreateSolidColorBrush(&c, None);
    let white = |a: f32| rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a }, None);
    let text = |s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush| {
        rt.DrawText(&utf16(s), f, &r, b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL)
    };
    let icon = |cp: u32, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush| {
        rt.DrawText(&glyph(cp), f, &r, b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL)
    };
    let primary = white(1.0)?;
    let secondary = brush(C_TEXT2)?;
    let accent = brush(rgb(0x4CC2FF))?;
    let hover_b = brush(C_HOVER)?;
    let sel_b = brush(C_SEL)?;
    let tile_b = brush(C_TILE)?;
    let entry_icon = |e: &Entry, x: f32, y: f32, size: f32| {
        match if size == 24.0 { st.bitmaps24.get(&e.icon) } else { st.bitmaps.get(&e.icon) } {
            Some(b) => crate::flyout::draw_icon(&rt, b, x, y, size),
            None => {
                let gl = if e.glyph != 0 { e.glyph } else { 0xE74C };
                let f = if size >= 24.0 { &g.icon24 } else { &g.icon16 };
                icon(gl, f, rect(x, y, x + size, y + size), &primary);
            }
        }
    };
    let is_hover = |i: usize| matches!(st.hover, Some(Hit::Entry(j)) if j == i);
    let s = searching(st);

    // ---- fixed top area ----
    match st.mode {
        Mode::Start => {
            rt.FillRectangle(&rect(0.0, 0.0, W, BAND_H), &brush(C_BAND)?);
            rt.FillRoundedRectangle(&rrect(SBOX, 15.0), &brush(C_BOX)?);
            rt.DrawRoundedRectangle(&rrect(SBOX, 15.0), &brush(C_BORDER)?, 1.0, None);
            icon(G_SEARCH, &g.icon16, rect(SBOX.left + 14.0, SBOX.top, SBOX.left + 34.0, SBOX.bottom), &accent);
            let tx = SBOX.left + 43.0;
            let tb = rect(tx, SBOX.top, SBOX.right - 16.0, SBOX.bottom);
            if st.query.is_empty() {
                text("Search for apps, settings, and documents", &g.body, tb, &secondary);
                rt.FillRectangle(&rect(tx, SBOX.top + 7.0, tx + 1.0, SBOX.bottom - 7.0), &primary);
            } else {
                text(&st.query, &g.body, tb, &primary);
                let cx = (tx + text_width(g, &st.query, &g.body)).min(tb.right);
                rt.FillRectangle(&rect(cx, SBOX.top + 7.0, cx + 1.0, SBOX.bottom - 7.0), &primary);
            }
        }
        Mode::Search => {
            if s {
                let back_hover = st.hover == Some(Hit::Back);
                if back_hover {
                    rt.FillRoundedRectangle(&rrect(BACK, 4.0), &hover_b);
                }
                icon(G_BACK, &g.icon16, BACK, &primary);
                for (i, (r, name)) in chip_rects(g).iter().zip(CHIPS).enumerate() {
                    let on = st.chip == i;
                    let rr = rrect(*r, 15.0);
                    if on {
                        rt.FillRoundedRectangle(&rr, &accent);
                        text(name, &g.body_c, *r, &brush(rgb(0x000000))?);
                    } else {
                        rt.FillRoundedRectangle(&rr, &brush(if st.hover == Some(Hit::Chip(i)) { C_CHIP_EDGE } else { C_CHIP })?);
                        text(name, &g.body_c, *r, &primary);
                    }
                }
            } else {
                text("Recent searches", &g.title, rect(33.0, 20.0, 500.0, 54.0), &primary);
            }
            // account initial, linked devices, settings (top-right, like Windows)
            let initial: String = st.user.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "S".into());
            rt.FillEllipse(&D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: 702.0, Y: 39.0 }, radiusX: 12.0, radiusY: 12.0 }, &brush(C_CHIP)?);
            text(&initial, &g.body_c, rect(690.0, 27.0, 714.0, 51.0), &primary);
            icon(G_LINK, &g.icon16, rect(733.0, 27.0, 757.0, 51.0), &primary);
            icon(G_GEAR, &g.icon16, rect(778.0, 27.0, 802.0, 51.0), &primary);
        }
    }

    // ---- scrolling content ----
    rt.PushAxisAlignedClip(&v, D2D1_ANTIALIAS_MODE_ALIASED);
    let dy = v.top - st.scroll;
    for it in st.items.iter().filter(|it| it.r.bottom > top && it.r.top < bottom) {
        let r = rect(it.r.left, it.r.top + dy, it.r.right, it.r.bottom + dy);
        match it.kind {
            Kind::Head(t, chevron) => {
                text(t, &g.head, r, &primary);
                if chevron {
                    icon(G_CHEVRON, &g.icon12, rect(r.right - 20.0, r.top, r.right, r.bottom), &secondary);
                }
                if t == "Recent" && st.mode == Mode::Start {
                    let sa = rect(SHOW_ALL.left, r.top, SHOW_ALL.right, r.bottom);
                    if st.hover == Some(Hit::ShowAll) {
                        rt.FillRoundedRectangle(&rrect(sa, 4.0), &hover_b);
                    }
                    let label = if st.show_all_recent { "Show less" } else { "Show all" };
                    text(label, &g.body, rect(sa.left + 10.0, r.top, sa.right, r.bottom), &primary);
                    icon(G_CHEVRON, &g.icon12, rect(sa.right - 22.0, r.top, sa.right - 6.0, r.bottom), &primary);
                }
                if t == "All" && st.mode == Mode::Start {
                    text("View: List", &g.body_r, rect(r.right - 160.0, r.top, r.right - 30.0, r.bottom), &primary);
                    icon(G_DOWN, &g.icon12, rect(r.right - 26.0, r.top, r.right - 10.0, r.bottom), &primary);
                }
            }
            Kind::Tile(i) => {
                if is_hover(i) {
                    rt.FillRoundedRectangle(&rrect(r, 4.0), &hover_b);
                }
                let cx = (r.left + r.right) / 2.0;
                entry_icon(&st.entries[i], cx - 16.0, r.top + 13.0, 32.0);
                text(&st.entries[i].name, &g.tile, rect(r.left + 2.0, r.top + 48.0, r.right - 2.0, r.top + 68.0), &primary);
            }
            Kind::Recent(i) => {
                if is_hover(i) {
                    rt.FillRoundedRectangle(&rrect(r, 4.0), &hover_b);
                }
                entry_icon(&st.entries[i], r.left + 14.0, r.top + 10.0, 32.0);
                let t = st.recent_files.iter().find(|x| x.0 == i).map(|x| ago(x.1, true)).unwrap_or_default();
                text(&st.entries[i].name, &g.small, rect(r.left + 56.0, r.top + 6.0, r.right - 6.0, r.top + 26.0), &primary);
                text(&t, &g.small, rect(r.left + 56.0, r.top + 24.0, r.right - 6.0, r.top + 44.0), &secondary);
            }
            Kind::Row(i) => {
                if is_hover(i) {
                    rt.FillRoundedRectangle(&rrect(r, 4.0), &hover_b);
                }
                entry_icon(&st.entries[i], r.left + 12.0, r.top + (r.bottom - r.top - 24.0) / 2.0, 24.0);
                text(&st.entries[i].name, &g.body, rect(r.left + 48.0, r.top, r.right - 12.0, r.bottom), &primary);
            }
            Kind::RecentSearch(i) => {
                let e = &st.entries[i];
                if is_hover(i) {
                    rt.FillRoundedRectangle(&rrect(r, 4.0), &hover_b);
                }
                let tile = rect(33.0, r.top + 10.0, 73.0, r.top + 50.0);
                rt.FillRoundedRectangle(&rrect(tile, 4.0), &tile_b);
                entry_icon(e, 41.0, r.top + 18.0, 24.0);
                text(&e.name, &g.body, rect(85.0, r.top + 10.0, 600.0, r.top + 30.0), &primary);
                text(e.sub, &g.small, rect(85.0, r.top + 29.0, 600.0, r.top + 47.0), &secondary);
                if let Some((_, t)) = st.counts.get(&e.target).filter(|c| c.1 > 0) {
                    text(&ago(*t, false), &g.small_r, rect(600.0, r.top, 800.0, r.bottom), &secondary);
                }
            }
            Kind::Best(i) | Kind::Result(i) => {
                let e = &st.entries[i];
                let best = matches!(it.kind, Kind::Best(_));
                if st.sel == Some(i) {
                    rt.FillRoundedRectangle(&rrect(r, 4.0), &sel_b);
                    rt.FillRoundedRectangle(&rrect(rect(r.left, (r.top + r.bottom) / 2.0 - 8.0, r.left + 3.0, (r.top + r.bottom) / 2.0 + 8.0), 1.5), &accent);
                } else if is_hover(i) {
                    rt.FillRoundedRectangle(&rrect(r, 4.0), &hover_b);
                }
                let mid = (r.top + r.bottom) / 2.0;
                let tile = rect(r.left + 10.0, mid - 20.0, r.left + 50.0, mid + 20.0);
                rt.FillRoundedRectangle(&rrect(tile, 4.0), &tile_b);
                entry_icon(e, tile.left + 8.0, tile.top + 8.0, 24.0);
                if best {
                    text(&e.name, &g.body, rect(r.left + 62.0, mid - 19.0, r.right - 8.0, mid + 1.0), &primary);
                    text(e.sub, &g.small, rect(r.left + 62.0, mid + 1.0, r.right - 8.0, mid + 19.0), &secondary);
                } else {
                    text(&e.name, &g.body, rect(r.left + 62.0, r.top, r.right - 8.0, r.bottom), &primary);
                }
            }
        }
    }
    rt.PopAxisAlignedClip();

    // ---- details pane (search results) ----
    if s {
        let oy = if st.mode == Mode::Start { BAND_H - S_TOP } else { 0.0 };
        let p = rect(PANE.left, PANE.top + oy, PANE.right, if st.mode == Mode::Start { FOOT_Y - 16.0 } else { PANE.bottom });
        rt.FillRoundedRectangle(&rrect(p, 8.0), &brush(C_PANE)?);
        if let Some(i) = st.sel {
            let e = &st.entries[i];
            text(&e.name, &g.head, rect(399.0, 100.0 + oy, 775.0, 122.0 + oy), &primary);
            text(e.sub, &g.small, rect(399.0, 122.0 + oy, 775.0, 142.0 + oy), &secondary);
            let pv = rect(399.0, 153.0 + oy, 775.0, 262.0 + oy);
            rt.FillRoundedRectangle(&rrect(pv, 4.0), &brush(C_PREVIEW)?);
            rt.DrawRoundedRectangle(&rrect(pv, 4.0), &brush(C_PREVIEW_EDGE)?, 1.0, None);
            let (cx, cy) = ((pv.left + pv.right) / 2.0, (pv.top + pv.bottom) / 2.0);
            match &st.preview {
                Some((k, b)) if *k == e.icon => crate::flyout::draw_icon(&rt, b, cx - 32.0, cy - 32.0, 64.0),
                _ => icon(if e.glyph != 0 { e.glyph } else { 0xE74C }, &g.icon24, rect(cx - 32.0, cy - 32.0, cx + 32.0, cy + 32.0), &primary),
            }
            for (r, label, h) in [(OPEN_BTN, "Open", Hit::Open), (OPTS_BTN, "Options", Hit::Options)] {
                let r = rect(r.left, r.top + oy, r.right, r.bottom + oy);
                rt.FillRoundedRectangle(&rrect(r, 4.0), &brush(if st.hover == Some(h) { C_SEL } else { C_BUTTON })?);
                if h == Hit::Options {
                    text(label, &g.body, rect(r.left + 24.0, r.top, r.right - 30.0, r.bottom), &primary);
                    icon(G_DOWN, &g.icon12, rect(r.right - 34.0, r.top, r.right - 16.0, r.bottom), &primary);
                } else {
                    text(label, &g.body_c, r, &primary);
                }
            }
        }
    }

    // ---- scrollbar ----
    if let Some(t) = thumb(st) {
        let active = st.drag.is_some() || st.hover == Some(Hit::Thumb);
        let t = if active { t } else { rect(t.left + 2.0, t.top, t.right - 1.0, t.bottom) };
        rt.FillRoundedRectangle(&rrect(t, 3.0), &white(if active { 0.55 } else { 0.36 })?);
    }

    // ---- Start footer: user + power ----
    if st.mode == Mode::Start {
        rt.FillRectangle(&rect(0.0, FOOT_Y, W, H), &brush(C_FOOT)?);
        let fy = FOOT_Y + (H - FOOT_Y) / 2.0;
        rt.FillEllipse(&D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: 80.0, Y: fy }, radiusX: 16.0, radiusY: 16.0 }, &brush(rgb(0x8A8A8A))?);
        icon(G_USER, &g.icon16, rect(64.0, fy - 16.0, 96.0, fy + 16.0), &brush(rgb(0xE6E6E6))?);
        text(&st.user, &g.small, rect(108.0, fy - 12.0, W / 2.0, fy + 12.0), &primary);
        if st.hover == Some(Hit::Power) {
            rt.FillRoundedRectangle(&rrect(POWER, 4.0), &hover_b);
        }
        icon(G_POWER, &g.icon16, POWER, &primary);
    }
    rt.EndDraw(None, None)
}

unsafe fn draw_pill(st: &State, rt: &ID2D1DCRenderTarget, rc: &RECT, dpi: f32) -> windows::core::Result<()> {
    let g = st.gfx.as_ref().unwrap();
    let (w, h) = ((rc.right - rc.left) as f32 * 96.0 / dpi, (rc.bottom - rc.top) as f32 * 96.0 / dpi);
    rt.BeginDraw();
    rt.Clear(Some(&rgb(0x1C1C1C))); // taskbar behind the rounded box
    let r = rect(0.0, 0.0, w, h);
    rt.FillRoundedRectangle(&rrect(r, h / 2.0), &rt.CreateSolidColorBrush(&C_BOX, None)?);
    rt.DrawRoundedRectangle(&rrect(rect(0.5, 0.5, w - 0.5, h - 0.5), h / 2.0), &rt.CreateSolidColorBrush(&C_BORDER, None)?, 1.0, None);
    let white = rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }, None)?;
    rt.DrawText(&glyph(G_SEARCH), &g.icon16, &rect(10.0, 0.0, 28.0, h), &white, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
    let tx = 36.0;
    rt.DrawText(&utf16(&st.query), &g.body, &rect(tx, 0.0, w - 10.0, h), &white, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
    let cx = (tx + text_width(g, &st.query, &g.body)).min(w - 10.0);
    rt.FillRectangle(&rect(cx, 8.0, cx + 1.0, h - 8.0), &white);
    rt.EndDraw(None, None)
}

unsafe fn paint(hwnd: HWND, is_pill: bool) {
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
            if draw(&mut st, mem, &rc, dpi, is_pill).is_err() {
                st.gfx = None;
                st.bitmaps.clear();
                st.bitmaps24.clear();
                st.preview = None;
            }
        });
        let _ = EndBufferedPaint(pb, true);
    }
    let _ = EndPaint(hwnd, &ps);
}

// ======================= test snapshot (no window, nothing on screen) =======================

/// `shellbits.exe --snapshot out.bmp start|search [query] [scroll]`
pub unsafe fn snapshot(path: &str, mode: &str, query: &str, scroll: f32) -> windows::core::Result<()> {
    load_all(None);
    ST.with(|s| -> windows::core::Result<()> {
        let mut st = s.borrow_mut();
        st.counts = history_for(&st.entries);
        st.pins = load_pins();
        st.user = display_name();
        drain_inbox(&mut st);
        st.mode = if mode == "search" { Mode::Search } else { Mode::Start };
        st.query = query.to_string();
        refilter(&mut st);
        st.scroll = scroll.min(max_scroll(&st));
        let (w, h) = (W as i32, H as i32);
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, ..Default::default() },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let dib = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let dc = CreateCompatibleDC(None);
        let old = SelectObject(dc, dib.into());
        draw(&mut st, dc, &RECT { left: 0, top: 0, right: w, bottom: h }, 96.0, false)?;
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

// ======================= Win key, Win+S, taskbar Start button + search box =======================
// Win tap / Start button -> Start. Win+S / search box -> Search. Win+<anything else> stays Windows'.
// Trick (same as AutoHotkey): swallow the real Win-up, then inject a dummy key + Win-up so
// Explorer sees "Win was used" and doesn't open its own Start.

pub(crate) const MARK: usize = 0x5348_4C42; // tags our own injected input
static WIN_STATE: AtomicU8 = AtomicU8::new(0); // 0 up, 1 down alone, 2 down+used
static SWALLOW_S_UP: AtomicBool = AtomicBool::new(false);
static SWALLOW_A_UP: AtomicBool = AtomicBool::new(false);
static SWALLOW_N_UP: AtomicBool = AtomicBool::new(false);
static SWALLOW_X_UP: AtomicBool = AtomicBool::new(false);
static SWALLOW_I_UP: AtomicBool = AtomicBool::new(false);
static SWALLOW_TAB_UP: AtomicBool = AtomicBool::new(false);
static SWALLOW_Z_UP: AtomicBool = AtomicBool::new(false);
static SWALLOW_V_UP: AtomicBool = AtomicBool::new(false);
static SWALLOW_SPACE_UP: AtomicBool = AtomicBool::new(false);
static SWALLOW_DOT_UP: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static SWALLOW_VK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
const SHELL_KEYS: [(u32, bool); 10] = [(b'E' as u32, false), (b'I' as u32, false), (b'S' as u32, true), (b'D' as u32, false), (b'M' as u32, false), (b'L' as u32, false), (0x25, false), (0x26, false), (0x27, false), (0x28, false)];
static SWALLOW_R_UP: AtomicBool = AtomicBool::new(false);
static RCLICK_START: AtomicBool = AtomicBool::new(false);
static CLICK_ZONE: AtomicU8 = AtomicU8::new(0); // 0 none, else ZONES index + 1
// taskbar Start button, search box, network/volume/battery, clock/bell; screen pixels l,t,r,b (0s = not found)
pub(crate) static ZONES: [[std::sync::atomic::AtomicI32; 4]; 4] = [const { [const { std::sync::atomic::AtomicI32::new(0) }; 4] }; 4];

/// 1 = Quick Settings, 2 = notification center + calendar.
fn post_panel(which: u8) {
    unsafe {
        let (h, m) = if which == 1 {
            (crate::quick::QS.load(Relaxed), crate::quick::WM_QS_TOGGLE)
        } else {
            (crate::notify::NC.load(Relaxed), crate::notify::WM_NC_TOGGLE)
        };
        let _ = PostMessageW(Some(HWND(h as *mut c_void)), m, WPARAM(0), LPARAM(0));
    }
}

pub(crate) unsafe fn inject(keys: &[(u16, bool)]) {
    let v: Vec<INPUT> = keys
        .iter()
        .map(|&(vk, up)| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                dwExtraInfo: MARK,
                ..Default::default()
            } },
        })
        .collect();
    SendInput(&v, size_of::<INPUT>() as i32);
}

fn post_toggle(search: bool) {
    let h = HWND(LAUNCHER.load(Relaxed) as *mut c_void);
    unsafe {
        let _ = PostMessageW(Some(h), WM_TOGGLE, WPARAM(search as usize), LPARAM(0));
    }
}

/// Shift/Ctrl/Alt held: Win+Shift+S, Win+Ctrl+... etc. belong to Windows, not to us.
fn other_mods() -> bool {
    unsafe { [VK_SHIFT, VK_CONTROL, VK_MENU].iter().any(|k| GetAsyncKeyState(k.0 as i32) as u16 & 0x8000 != 0) }
}

/// Returns true if the event should be swallowed.
pub unsafe fn on_key(k: &KBDLLHOOKSTRUCT, up: bool) -> bool {
    KEY_EVENTS.fetch_add(1, Relaxed);
    if std::env::var_os("SHELLBITS_TOAST_TEST").is_some() && k.dwExtraInfo != MARK {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(std::env::temp_dir().join("shellbits-keys.txt")) {
            let _ = writeln!(f, "key vk={:#x} up={up} win_state={}", k.vkCode, WIN_STATE.load(Relaxed));
        }
    }
    if crate::snap::on_key(k, up) || crate::clip::on_key(k, up) || crate::emoji::on_key(k, up) {
        return true;
    }
    if crate::alttab::on_key(k, up) {
        return true;
    }
    if k.dwExtraInfo == MARK {
        return false;
    }
    let win = k.vkCode == VK_LWIN.0 as u32 || k.vkCode == VK_RWIN.0 as u32;
    if k.vkCode == b'A' as u32 {
        if !up && WIN_STATE.load(Relaxed) != 0 && !other_mods() {
            WIN_STATE.store(2, Relaxed);
            SWALLOW_A_UP.store(true, Relaxed);
            inject(&[(0xE8, false), (0xE8, true)]);
            post_panel(1);
            return true;
        }
        if up && SWALLOW_A_UP.swap(false, Relaxed) {
            return true;
        }
    }
    if k.vkCode == b'N' as u32 {
        if !up && WIN_STATE.load(Relaxed) != 0 && !other_mods() {
            WIN_STATE.store(2, Relaxed);
            SWALLOW_N_UP.store(true, Relaxed);
            inject(&[(0xE8, false), (0xE8, true)]);
            post_panel(2);
            return true;
        }
        if up && SWALLOW_N_UP.swap(false, Relaxed) {
            return true;
        }
    }
    // Win+Space -> input language list in Rust (each tap steps; letting go of Win switches)
    if k.vkCode == 0x20 && crate::lang::LANG.load(Relaxed) != 0 {
        if !up && WIN_STATE.load(Relaxed) != 0 && !other_mods() {
            WIN_STATE.store(2, Relaxed);
            SWALLOW_SPACE_UP.store(true, Relaxed);
            inject(&[(0xE8, false), (0xE8, true)]);
            crate::lang::step();
            return true;
        }
        if up && SWALLOW_SPACE_UP.swap(false, Relaxed) {
            return true;
        }
    }
    // Win+. or Win+; -> emoji panel in Rust
    if (k.vkCode == 0xBE || k.vkCode == 0xBA) && crate::emoji::EMOJI.load(Relaxed) != 0 {
        if !up && WIN_STATE.load(Relaxed) != 0 && !other_mods() {
            WIN_STATE.store(2, Relaxed);
            SWALLOW_DOT_UP.store(k.vkCode, Relaxed);
            inject(&[(0xE8, false), (0xE8, true)]);
            crate::emoji::toggle();
            return true;
        }
        if up && SWALLOW_DOT_UP.load(Relaxed) == k.vkCode && SWALLOW_DOT_UP.swap(0, Relaxed) != 0 {
            return true;
        }
    }
    // Win+V -> clipboard history in Rust
    if k.vkCode == 0x56 && crate::clip::CLIP.load(Relaxed) != 0 {
        if !up && WIN_STATE.load(Relaxed) != 0 && !other_mods() {
            WIN_STATE.store(2, Relaxed);
            SWALLOW_V_UP.store(true, Relaxed);
            inject(&[(0xE8, false), (0xE8, true)]);
            crate::clip::toggle();
            return true;
        }
        if up && SWALLOW_V_UP.swap(false, Relaxed) {
            return true;
        }
    }
    // the keyboard's Calculator key
    if k.vkCode == 0xB7 && k.dwExtraInfo != MARK {
        if !up {
            open_rust("calc");
        }
        return true;
    }
    // Win+Z -> Snap layouts in Rust
    if k.vkCode == 0x5A && crate::snap::SNAP.load(Relaxed) != 0 {
        if !up && WIN_STATE.load(Relaxed) != 0 && !other_mods() {
            WIN_STATE.store(2, Relaxed);
            SWALLOW_Z_UP.store(true, Relaxed);
            inject(&[(0xE8, false), (0xE8, true)]);
            crate::snap::toggle();
            return true;
        }
        if up && SWALLOW_Z_UP.swap(false, Relaxed) {
            return true;
        }
    }
    // Win+Tab -> Task View in Rust
    if k.vkCode == 0x09 && crate::alttab::ALTTAB.load(Relaxed) != 0 {
        if !up && WIN_STATE.load(Relaxed) != 0 && !other_mods() {
            WIN_STATE.store(2, Relaxed);
            SWALLOW_TAB_UP.store(true, Relaxed);
            inject(&[(0xE8, false), (0xE8, true)]);
            crate::alttab::task_view();
            return true;
        }
        if up && SWALLOW_TAB_UP.swap(false, Relaxed) {
            return true;
        }
    }
    // Win+I -> Settings in Rust, Win+E -> File Explorer in Rust
    for (key, id) in [(b'I', crate::SETTINGS_ID), (b'E', crate::FILES_ID)] {
        if k.vkCode == key as u32 {
            if !up && WIN_STATE.load(Relaxed) != 0 && !other_mods() {
                WIN_STATE.store(2, Relaxed);
                SWALLOW_I_UP.store(true, Relaxed);
                inject(&[(0xE8, false), (0xE8, true)]);
                if let Ok(f) = FindWindowW(w!("shellbits_flyout"), None) {
                    let _ = PostMessageW(Some(f), WM_HOTKEY, WPARAM(id as usize), LPARAM(0));
                }
                return true;
            }
            if up && SWALLOW_I_UP.swap(false, Relaxed) {
                return true;
            }
        }
    }
    // as the shell: the Win shortcuts Explorer would otherwise provide (Win+E, D, I, M, L, arrows, Shift+S)
    if crate::session::SHELL.load(Relaxed) && !win {
        if !up && WIN_STATE.load(Relaxed) != 0 {
            let shift = GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000 != 0;
            let ctrl_alt = [VK_CONTROL, VK_MENU].iter().any(|k| GetAsyncKeyState(k.0 as i32) as u16 & 0x8000 != 0);
            let vk = k.vkCode;
            if std::env::var_os("SHELLBITS_TOAST_TEST").is_some() {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(std::env::temp_dir().join("shellbits-keys.txt")) {
                    let _ = writeln!(f, "shell key vk={vk:#x} win_state={} shift={shift} ctrl_alt={ctrl_alt}", WIN_STATE.load(Relaxed));
                }
            }
            if !ctrl_alt && SHELL_KEYS.contains(&(vk, shift)) {
                WIN_STATE.store(2, Relaxed);
                SWALLOW_VK.store(vk, Relaxed);
                inject(&[(0xE8, false), (0xE8, true)]);
                // off the hook thread: opening apps can take a moment
                std::thread::spawn(move || {
                    crate::session::hotkey(vk, shift);
                });
                return true;
            }
        }
        if up && SWALLOW_VK.load(Relaxed) == k.vkCode && SWALLOW_VK.swap(0, Relaxed) != 0 {
            return true;
        }
    }
    // Win+X -> our power-user menu, Win+R -> our Run box
    for (key, flag, msg) in [(b'X', &SWALLOW_X_UP, crate::winx::WM_WINX_TOGGLE), (b'R', &SWALLOW_R_UP, crate::winx::WM_RUN_SHOW)] {
        if k.vkCode == key as u32 {
            if !up && WIN_STATE.load(Relaxed) != 0 && !other_mods() {
                WIN_STATE.store(2, Relaxed);
                flag.store(true, Relaxed);
                inject(&[(0xE8, false), (0xE8, true)]);
                crate::winx::post(msg, 0);
                return true;
            }
            if up && flag.swap(false, Relaxed) {
                return true;
            }
        }
    }
    if k.vkCode == b'S' as u32 {
        if !up && WIN_STATE.load(Relaxed) != 0 && !other_mods() {
            WIN_STATE.store(2, Relaxed);
            SWALLOW_S_UP.store(true, Relaxed);
            inject(&[(0xE8, false), (0xE8, true)]);
            post_toggle(true);
            return true;
        }
        if up && SWALLOW_S_UP.swap(false, Relaxed) {
            return true;
        }
    }
    match (win, up) {
        (true, false) => {
            if WIN_STATE.load(Relaxed) == 0 {
                WIN_STATE.store(1, Relaxed);
            }
            false
        }
        (true, true) => {
            crate::lang::commit(); // Win released while the input-language list is up: switch to the chosen one
            let alone = WIN_STATE.swap(0, Relaxed) == 1;
            if alone {
                inject(&[(0xE8, false), (0xE8, true), (k.vkCode as u16, true)]);
                post_toggle(false);
            }
            alone
        }
        (false, false) => {
            if WIN_STATE.load(Relaxed) == 1 {
                WIN_STATE.store(2, Relaxed);
            }
            false
        }
        _ => false,
    }
}

/// Close any open panel the click landed outside of (a panel and its companion count as one).
pub(crate) unsafe fn dismiss_outside(pt: POINT) {
    let h = |a: &AtomicIsize| HWND(a.load(Relaxed) as *mut c_void);
    let groups = [
        [h(&LAUNCHER), h(&PILL)],
        [h(&crate::quick::QS), h(&crate::quick::MEDIA)],
        [h(&crate::notify::NC), h(&crate::notify::CAL)],
        [h(&crate::winx::MENU), h(&crate::winx::SUB)],
        [h(&crate::tray::FLOW), h(&crate::tray::FLOW)],
        [h(&crate::alttab::ALTTAB), h(&crate::alttab::ALTTAB)],
        [h(&crate::snap::SNAP), h(&crate::snap::SNAP)],
        [h(&crate::snap::ASSIST), h(&crate::snap::ASSIST)],
        [h(&crate::clip::CLIP), h(&crate::clip::CLIP)],
        [h(&crate::emoji::EMOJI), h(&crate::emoji::EMOJI)],
    ];
    // clicks on our own taskbar zones toggle the panel themselves
    let on_zone = ZONES.iter().any(|z| {
        let r = [0, 1, 2, 3].map(|i| z[i].load(Relaxed));
        r[2] > r[0] && (r[0]..r[2]).contains(&pt.x) && (r[1]..r[3]).contains(&pt.y)
    });
    if on_zone {
        return;
    }
    // the same goes for our own taskbar (shell mode): its click handler opens AND closes the panel.
    // Closing here on the press made the release open it again, so a second click never closed it.
    let bar = HWND(crate::taskbar::TASKBAR.load(Relaxed) as *mut c_void);
    let mut br = RECT::default();
    if IsWindowVisible(bar).as_bool() && GetWindowRect(bar, &mut br).is_ok() && PtInRect(&br, pt).as_bool() {
        return;
    }
    for g in groups {
        if !IsWindowVisible(g[0]).as_bool() {
            continue;
        }
        let inside = g.iter().any(|w| {
            let mut r = RECT::default();
            IsWindowVisible(*w).as_bool() && GetWindowRect(*w, &mut r).is_ok() && PtInRect(&r, pt).as_bool()
        });
        if !inside {
            let _ = PostMessageW(Some(g[0]), crate::flyout::WM_DISMISS, WPARAM(0), LPARAM(0));
        }
    }
}

/// Low-level mouse hook: Start button -> our Start, search box -> our search.
pub static MOUSE_EVENTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
pub static KEY_EVENTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub unsafe extern "system" fn mouse_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    MOUSE_EVENTS.fetch_add(1, Relaxed);
    if code == HC_ACTION as i32 {
        let m = &*(lp.0 as *const MSLLHOOKSTRUCT);
        let msg = wp.0 as u32;
        if matches!(msg, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN) {
            dismiss_outside(m.pt);
        }
        if msg == WM_LBUTTONDOWN {
            for (n, z) in ZONES.iter().enumerate() {
                let r = [0, 1, 2, 3].map(|i| z[i].load(Relaxed));
                if r[2] > r[0] && (r[0]..r[2]).contains(&m.pt.x) && (r[1]..r[3]).contains(&m.pt.y) {
                    CLICK_ZONE.store(n as u8 + 1, Relaxed);
                    return LRESULT(1);
                }
            }
        }
        // right-click the Start button -> Win+X menu at the cursor
        if msg == WM_RBUTTONDOWN {
            let r = [0, 1, 2, 3].map(|i| ZONES[0][i].load(Relaxed));
            if r[2] > r[0] && (r[0]..r[2]).contains(&m.pt.x) && (r[1]..r[3]).contains(&m.pt.y) {
                RCLICK_START.store(true, Relaxed);
                return LRESULT(1);
            }
        }
        if msg == WM_RBUTTONUP && RCLICK_START.swap(false, Relaxed) {
            inject(&[(0xE8, false), (0xE8, true)]);
            crate::winx::post(crate::winx::WM_WINX_TOGGLE, 1);
            return LRESULT(1);
        }
        if msg == WM_LBUTTONUP {
            let z = CLICK_ZONE.swap(0, Relaxed);
            if z != 0 {
                inject(&[(0xE8, false), (0xE8, true)]); // earns us the right to take focus
                match z {
                    1 | 2 => post_toggle(z == 2),
                    3 => post_panel(1),
                    _ => post_panel(2),
                }
                return LRESULT(1);
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

thread_local! {
    static UIA: RefCell<Option<windows::Win32::UI::Accessibility::IUIAutomation>> = const { RefCell::new(None) };
}

/// `--zones-selftest`: where UI Automation says the taskbar's buttons are, one "name left top right bottom"
/// per line (zeros when it cannot see one). The test scripts use this to know where to click.
pub unsafe fn zone_report() -> String {
    use windows::Win32::UI::Accessibility::*;
    let mut out = String::new();
    let uia: windows::core::Result<IUIAutomation> = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER);
    let (Ok(uia), Ok(tray)) = (uia, FindWindowW(w!("Shell_TrayWnd"), None)) else { return "no UI Automation or no taskbar".into() };
    for (name, prop) in [("StartButton", UIA_AutomationIdPropertyId), ("SearchButton", UIA_AutomationIdPropertyId), ("SystemTray.OmniButtonLeft", UIA_ClassNamePropertyId), ("SystemTray.OmniButtonRight", UIA_ClassNamePropertyId), ("SystemTray.OmniButtonCenter", UIA_ClassNamePropertyId)] {
        let r = (|| -> windows::core::Result<RECT> {
            let cond = uia.CreatePropertyCondition(prop, &windows::Win32::System::Variant::VARIANT::from(windows::core::BSTR::from(name)))?;
            uia.ElementFromHandle(tray)?.FindFirst(TreeScope_Descendants, &cond)?.CurrentBoundingRectangle()
        })()
        .unwrap_or_default();
        out += &format!("{name} {} {} {} {}
", r.left, r.top, r.right, r.bottom);
    }
    out
}

/// Where the taskbar Start button, search box, tray status icons and clock are now (they move).
pub unsafe fn update_search_rect() {
    use windows::Win32::UI::Accessibility::*;
    // (property, values): a zone is the union of every element matching one of the values
    let zones: [(UIA_PROPERTY_ID, &[&str]); 4] = [
        (UIA_AutomationIdPropertyId, &["StartButton"]),
        (UIA_AutomationIdPropertyId, &["SearchButton"]),
        (UIA_ClassNamePropertyId, &["SystemTray.AccentButton", "SystemTray.OmniButtonCenter"]),
        (UIA_ClassNamePropertyId, &["SystemTray.OmniButtonLeft", "SystemTray.OmniButtonRight"]),
    ];
    for (zone, (prop, vals)) in ZONES.iter().zip(zones) {
        let r = UIA.with(|c| -> windows::core::Result<RECT> {
            let mut c = c.borrow_mut();
            if c.is_none() {
                *c = Some(CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?);
            }
            let uia = c.as_ref().unwrap();
            let tray = FindWindowW(w!("Shell_TrayWnd"), None)?;
            let root = uia.ElementFromHandle(tray)?;
            let mut u = RECT { left: i32::MAX, top: i32::MAX, right: i32::MIN, bottom: i32::MIN };
            for v in vals {
                let cond = uia.CreatePropertyCondition(prop, &windows::Win32::System::Variant::VARIANT::from(windows::core::BSTR::from(*v)))?;
                let all = root.FindAll(TreeScope_Descendants, &cond)?;
                for i in 0..all.Length()? {
                    let r = all.GetElement(i)?.CurrentBoundingRectangle()?;
                    if r.right > r.left {
                        u = RECT { left: u.left.min(r.left), top: u.top.min(r.top), right: u.right.max(r.right), bottom: u.bottom.max(r.bottom) };
                    }
                }
            }
            if u.right <= u.left { Ok(RECT::default()) } else { Ok(u) }
        });
        let r = r.unwrap_or_default();
        for (a, v) in zone.iter().zip([r.left, r.top, r.right, r.bottom]) {
            a.store(v, Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::rust_app;
    #[test]
    fn apps_with_a_rust_replacement() {
        assert_eq!(rust_app(r"shell:AppsFolder\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"), Some(crate::CALC_ID));
        assert_eq!(rust_app("Calc.exe"), Some(crate::CALC_ID));
        assert_eq!(rust_app("windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel"), Some(crate::SETTINGS_ID));
        assert_eq!(rust_app("Microsoft.Windows.Explorer"), Some(crate::FILES_ID));
        assert_eq!(rust_app(r"C:\Windows\explorer.exe"), None); // a full path is left to Windows
        assert_eq!(rust_app("ms-settings:display"), None); // a specific Settings page too
        assert_eq!(rust_app("chrome"), None);
    }
}

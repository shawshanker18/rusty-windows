//! Settings in Rust (Win+I / `shellbits --settings`): Windows 11's layout (320px nav with icons,
//! page title, rounded #2B2B2B cards with a control on the right) and controls that change the real
//! system setting. Every value is read from Windows when a page opens; nothing is hard-coded.
//! Pages not rebuilt yet are one click away through each page's "More ... settings" row.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};
use std::sync::Mutex;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::flyout::{glyph, rect, rrect, utf16};
use crate::quick::rgb;

pub static WIN: AtomicIsize = AtomicIsize::new(0);
const WM_VALUES: u32 = WM_APP + 120;
const NAV: f32 = 320.0;
const ROW_H: f32 = 68.0;

#[derive(Clone, Copy)]
enum Kind {
    Toggle(fn() -> bool, fn(bool)),
    Slider(fn() -> f32, fn(f32)), // 0..1
    Info(fn() -> String),
    Link(&'static str), // opens this page of Windows' own Settings
    Sub(&'static str),  // opens one of our own sub-pages
    Expand(&'static str), // a Link drawn with a down chevron (a section the real page unfolds in place)
    Header,             // a section title between cards, not a card
    Section,            // the same, with the tighter spacing some pages use (Privacy & security)
    Choice(fn() -> String, fn() -> Vec<String>, fn(usize)), // drop-down: current value, the options, pick one
    State(fn() -> bool, &'static str), // shows On/Off with a switch; changing it happens on that Windows page
    Flat(fn() -> String, fn() -> Vec<String>, fn(usize)), // like Choice, but shown as plain text with a chevron
    Big(fn() -> String),                   // the large battery percentage at the top of Power & battery
    Note(fn() -> bool, &'static str),      // an information bar, shown only while its condition holds
    Tiles(fn() -> Vec<(String, String, bool, String)>), // spec tiles: label, big value, -, bottom line
    Facts(fn() -> Vec<(String, String, bool, String)>), // a card of "name  value" lines with a Copy button
    RenamePc,                                            // device name + model with the Rename this PC button
    ToggleMore(fn() -> bool, fn(bool), &'static str), // a switch plus a chevron that opens more settings
    NetHero(fn() -> (String, bool)),       // the connection summary at the top of Network & internet: name, wired?
    BtTiles(fn() -> Vec<crate::qsx::Item>), // paired devices as tiles, then an "Add device" tile
    Center(&'static str, &'static str),    // a thin bar with a centred link: text, page it opens
    Space(f32),                            // extra air between cards
    LinkBtn(&'static str, &'static str, &'static str), // a Link with an accent button before the chevron: page, button text, button's page
    ThemeHero(fn() -> Vec<(String, String, bool, String)>), // desktop preview + Windows' themes to pick from: theme file, its wallpaper, is current
    TimeHero(fn() -> Vec<(String, String, bool, String)>), // clock, date, time zone and region at the top of Time & language
    UpdateHero(fn() -> bool),              // Windows Update's status block; the value is "a restart is waiting"
    When(fn() -> bool, &'static str),      // an expander card that only exists while its condition holds
    HomeHero(fn() -> Vec<(String, String, bool, String)>),    // this PC, its connection and update state, across the top of Home
    AccountHero(fn() -> Vec<(String, String, bool, String)>), // picture, name, email and account type at the top of Accounts
    SortBy,                                // "Sort by:" with its drop-down, above the app list
    Senders(fn() -> Vec<crate::qsx::Item>, fn(&str, bool)), // one card per item, each with its own switch: the list, and what flipping one does
    Actions(fn() -> Vec<crate::qsx::Item>, &'static str, fn(&str, &str)), // one card per item with a button: the list, the button's text, what it does (item id, item name)
    Devices(fn() -> Vec<crate::qsx::Item>, &'static str), // sound devices to pick from, then a "pair a new device" line with this label
}

/// Added to a Row's icon when the glyph comes from Settings' own icon font instead of Segoe Fluent Icons.
const SET: u32 = 0x10_0000;
thread_local! {
    // "Settings Fluent Icons": the font the real Settings app draws its card icons with (not installed system-wide)
    static SET_FACE: std::cell::OnceCell<Option<IDWriteFontFace>> = const { std::cell::OnceCell::new() };
}
unsafe fn settings_face(dw: &IDWriteFactory) -> Option<IDWriteFontFace> {
    SET_FACE.with(|c| {
        c.get_or_init(|| {
            let path = format!("{}\\SystemResources\\Windows.UI.SettingsAppThreshold\\SystemSettings\\Assets\\Fonts\\SettingsIcons.ttf", std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into()));
            let file = dw.CreateFontFileReference(&HSTRING::from(path), None).ok()?;
            dw.CreateFontFace(DWRITE_FONT_FACE_TYPE_TRUETYPE, &[Some(file)], 0, DWRITE_FONT_SIMULATIONS_NONE).ok()
        })
        .clone()
    })
}

struct Row {
    icon: u32, // Segoe Fluent Icons glyph, 0 = none
    label: &'static str,
    sub: &'static str,
    kind: Kind,
}

struct Page {
    name: &'static str,
    icon: u32,
    parent: &'static str, // "" = shown in the left navigation
    rows: Vec<Row>,
}

#[derive(Clone, Default)]
struct Val {
    on: bool,
    f: f32,
    s: String,
    list: Vec<(String, String, bool, String)>, // devices: name, adapter, is the default, endpoint id
}

/// "Speakers (Realtek(R) Audio)" -> ("Speakers", "Realtek(R) Audio")
pub fn split_device(name: &str) -> (String, String) {
    match name.split_once(" (") {
        Some((a, b)) => (a.to_string(), b.strip_suffix(')').unwrap_or(b).to_string()),
        None => (name.to_string(), String::new()),
    }
}

const PUSH: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\PushNotifications");
const SENDERS: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Notifications\\Settings";
static BY_NAME: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn toasts() -> bool {
    reg_u32(HKEY_CURRENT_USER, PUSH, w!("ToastEnabled")) != Some(0)
}
fn set_toasts(on: bool) {
    let v = on as u32;
    unsafe {
        let _ = RegSetKeyValueW(HKEY_CURRENT_USER, PUSH, w!("ToastEnabled"), REG_DWORD.0, Some(&v as *const _ as *const c_void), 4);
    }
}
/// Most recent first (or by name), given (name, last notification time).
pub fn sort_senders(v: &mut [(String, u64)], by_name: bool) {
    if by_name {
        v.sort_by_key(|a| a.0.to_lowercase());
    } else {
        v.sort_by(|a, b| b.1.cmp(&a.1));
    }
}
/// Apps that have sent notifications, with whether each is allowed to: Windows' own per-app list.
fn senders() -> Vec<crate::qsx::Item> {
    use windows::Win32::UI::Shell::{IShellItem, SHCreateItemFromParsingName, SIGDN_NORMALDISPLAY};
    let mut found: Vec<(String, u64)> = Vec::new();
    unsafe {
        let mut k = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(SENDERS), Some(0), KEY_READ, &mut k).is_err() {
            return Vec::new();
        }
        for i in 0.. {
            let mut name = [0u16; 512];
            let mut n = name.len() as u32;
            if RegEnumKeyExW(k, i, Some(windows::core::PWSTR(name.as_mut_ptr())), &mut n, None, None, None, None).is_err() {
                break;
            }
            let id = String::from_utf16_lossy(&name[..n as usize]);
            let (mut last, mut sz) = (0u64, 8u32);
            let _ = RegGetValueW(HKEY_CURRENT_USER, &HSTRING::from(format!("{SENDERS}\\{id}")), w!("LastNotificationAddedTime"), RRF_RT_REG_QWORD, None, Some(&mut last as *mut _ as *mut c_void), Some(&mut sz));
            found.push((id, last));
        }
        let _ = RegCloseKey(k);
        // only the ones Windows can name (the rest are internal senders the real page hides too)
        let mut named: Vec<(String, u64, String)> = found
            .into_iter()
            .filter_map(|(id, last)| {
                let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(format!("shell:AppsFolder\\{id}")), None).ok()?;
                let pw = item.GetDisplayName(SIGDN_NORMALDISPLAY).ok()?;
                let name = pw.to_string().unwrap_or_default();
                windows::Win32::System::Com::CoTaskMemFree(Some(pw.0 as *const c_void));
                (!name.is_empty()).then_some((name, last, id))
            })
            .collect();
        let mut order: Vec<(String, u64)> = named.iter().map(|n| (n.2.clone(), n.1)).collect();
        if BY_NAME.load(Relaxed) {
            named.sort_by_key(|a| a.0.to_lowercase());
        } else {
            sort_senders(&mut order, false);
            named.sort_by_key(|n| order.iter().position(|o| o.0 == n.2));
        }
        named
            .into_iter()
            .map(|(name, _, id)| {
                let on = reg_u32(HKEY_CURRENT_USER, PCWSTR(HSTRING::from(format!("{SENDERS}\\{id}")).as_ptr()), w!("Enabled")) != Some(0);
                crate::qsx::Item { name, key: id, on, ..Default::default() }
            })
            .collect()
    }
}
/// Where a list item's icon comes from: a startup entry names its program, anything else is an app id.
fn icon_source(id: &str) -> String {
    match id.split('\t').nth(1).filter(|_| id.starts_with("startup\t") || id.starts_with("app\t")) {
        Some(file) => file.to_string(),
        None => format!("shell:AppsFolder\\{id}"),
    }
}

const RUN: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const APPROVED: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved";

/// Windows' "is this startup item switched on" record: 12 bytes whose first byte is even when it is
/// on (02, 06) and odd when it was switched off (03, 07). No record at all means on.
pub fn startup_on(record: &[u8]) -> bool {
    record.first().is_none_or(|b| b & 1 == 0)
}

/// The program a startup command line starts: `"C:\a b\x.exe" --flag` -> `C:\a b\x.exe`.
pub fn exe_of(command: &str) -> String {
    let c = command.trim();
    if let Some(rest) = c.strip_prefix('"') {
        return rest.split('"').next().unwrap_or("").to_string();
    }
    match c.to_lowercase().find(".exe") {
        Some(i) => c[..i + 4].to_string(),
        None => c.split_whitespace().next().unwrap_or("").to_string(),
    }
}

unsafe fn reg_values(root: HKEY, key: &str) -> Vec<(String, String)> {
    use windows::Win32::System::Registry::RegEnumValueW;
    let mut out = Vec::new();
    let mut k = HKEY::default();
    if RegOpenKeyExW(root, &HSTRING::from(key), Some(0), KEY_READ, &mut k).is_err() {
        return out;
    }
    for i in 0.. {
        let (mut name, mut data) = ([0u16; 512], [0u8; 4096]);
        let (mut n, mut d) = (name.len() as u32, data.len() as u32);
        if RegEnumValueW(k, i, Some(windows::core::PWSTR(name.as_mut_ptr())), &mut n, None, None, Some(data.as_mut_ptr()), Some(&mut d)).is_err() {
            break;
        }
        let text: Vec<u16> = data[..d as usize].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|c| *c != 0).collect();
        out.push((String::from_utf16_lossy(&name[..n as usize]), String::from_utf16_lossy(&text)));
    }
    let _ = RegCloseKey(k);
    out
}

unsafe fn approved(root: HKEY, list: &str, name: &str) -> bool {
    use windows::Win32::System::Registry::RRF_RT_REG_BINARY;
    let (mut rec, mut n) = ([0u8; 16], 16u32);
    let found = RegGetValueW(root, &HSTRING::from(format!("{APPROVED}\\{list}")), &HSTRING::from(name), RRF_RT_REG_BINARY, None, Some(rec.as_mut_ptr() as *mut c_void), Some(&mut n)).is_ok();
    startup_on(if found { &rec[..n as usize] } else { &[] })
}

/// What starts when you sign in: your own Run entries, your Startup folder, and the PC-wide Run
/// entries, each with Windows' own on/off record (the one Task Manager and Settings flip).
// ponytail: packaged (Store) apps' startup tasks are not listed; they need the StartupTask API per package
fn startup_apps() -> Vec<crate::qsx::Item> {
    use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    let mut out = Vec::new();
    unsafe {
        for (root, tag, note) in [(HKEY_CURRENT_USER, "cu", ""), (HKEY_LOCAL_MACHINE, "lm", "All users")] {
            for (name, command) in reg_values(root, RUN) {
                let on = approved(root, "Run", &name);
                let sub = match (on, note) { (true, "") => "On".to_string(), (false, "") => "Off".to_string(), (true, n) => format!("On - {n}"), (false, n) => format!("Off - {n}") };
                out.push(crate::qsx::Item { key: format!("startup\t{}\t{tag}:{name}", exe_of(&command)), name, sub, on, ..Default::default() });
            }
        }
        let folder = format!("{}\\Microsoft\\Windows\\Start Menu\\Programs\\Startup", std::env::var("APPDATA").unwrap_or_default());
        for f in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
            let file = f.file_name().to_string_lossy().to_string();
            if file.eq_ignore_ascii_case("desktop.ini") || f.path().is_dir() {
                continue;
            }
            let on = approved(HKEY_CURRENT_USER, "StartupFolder", &file);
            let name = f.path().file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or(file.clone());
            out.push(crate::qsx::Item { key: format!("startup\t{}\tfolder:{file}", f.path().display()), name, sub: if on { "On".into() } else { "Off".into() }, on, ..Default::default() });
        }
    }
    out.sort_by_key(|i| i.name.to_lowercase());
    out
}

/// Flip one startup item, the way Windows does: write its 12-byte record (02 = on, 03 = off).
/// PC-wide entries need administrator rights; without them the write fails and nothing changes.
fn set_startup(id: &str, on: bool) {
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, REG_BINARY};
    let Some((kind, name)) = id.rsplit('\t').next().and_then(|k| k.split_once(':')) else { return };
    let (root, list) = match kind { "cu" => (HKEY_CURRENT_USER, "Run"), "lm" => (HKEY_LOCAL_MACHINE, "Run"), _ => (HKEY_CURRENT_USER, "StartupFolder") };
    let mut rec = [0u8; 12];
    rec[0] = if on { 2 } else { 3 };
    unsafe {
        let _ = RegSetKeyValueW(root, &HSTRING::from(format!("{APPROVED}\\{list}")), &HSTRING::from(name), REG_BINARY.0, Some(rec.as_ptr() as *const c_void), 12);
    }
}

const UNINSTALL: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall";
const UNINSTALL32: &str = "SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall";

/// The second line of an app's card, like Windows': "1.2.3  |  Publisher  |  12/31/2026  |  45.2 MB",
/// leaving out whatever the app did not record. `date` is the registry's YYYYMMDD.
pub fn app_line(version: &str, publisher: &str, date: &str, size_kb: u32) -> String {
    let mut parts: Vec<String> = [version, publisher].iter().filter(|p| !p.trim().is_empty()).map(|p| p.trim().to_string()).collect();
    if date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) {
        parts.push(format!("{}/{}/{}", date[4..6].trim_start_matches('0'), date[6..8].trim_start_matches('0'), &date[..4]));
    }
    if size_kb >= 512 { // smaller than half a megabyte would read "0 MB"
        parts.push(size_label(size_kb as u64 * 1024));
    }
    parts.join("  |  ")
}

/// The command that removes an app. Windows Installer entries are recorded as "install/repair"
/// (`MsiExec.exe /I{...}`); removing is the same command with /X.
pub fn uninstall_command(recorded: &str) -> String {
    let c = recorded.trim();
    if c.to_lowercase().starts_with("msiexec") { c.replacen("/I{", "/X{", 1).replacen("/i{", "/X{", 1) } else { c.to_string() }
}

/// A DisplayIcon value as a file: `"C:\x.exe",0` -> `C:\x.exe`.
pub fn icon_file(display_icon: &str) -> String {
    let t = display_icon.trim().trim_matches('"');
    match t.rsplit_once(',') {
        Some((file, index)) if index.trim().trim_start_matches('-').bytes().all(|b| b.is_ascii_digit()) => file.trim().trim_matches('"').to_string(),
        _ => t.to_string(),
    }
}

/// The programs Windows lists under Installed apps, from its uninstall records (this user's and the
/// PC's, 64- and 32-bit), without system components and patches, by name.
// ponytail: Store (packaged) apps are not listed yet; they come from the package manager, not these records
fn installed_apps() -> Vec<crate::qsx::Item> {
    use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    let mut out: Vec<crate::qsx::Item> = Vec::new();
    unsafe {
        for (root, base) in [(HKEY_LOCAL_MACHINE, UNINSTALL), (HKEY_LOCAL_MACHINE, UNINSTALL32), (HKEY_CURRENT_USER, UNINSTALL)] {
            let mut k = HKEY::default();
            if RegOpenKeyExW(root, &HSTRING::from(base), Some(0), KEY_READ, &mut k).is_err() {
                continue;
            }
            for i in 0.. {
                let mut name = [0u16; 512];
                let mut n = name.len() as u32;
                if RegEnumKeyExW(k, i, Some(windows::core::PWSTR(name.as_mut_ptr())), &mut n, None, None, None, None).is_err() {
                    break;
                }
                let key = HSTRING::from(format!("{base}\\{}", String::from_utf16_lossy(&name[..n as usize])));
                let text = |v: PCWSTR| reg_str(root, PCWSTR(key.as_ptr()), v);
                let num = |v: PCWSTR| reg_u32(root, PCWSTR(key.as_ptr()), v).unwrap_or(0);
                let (title, remove) = (text(w!("DisplayName")), text(w!("UninstallString")));
                if title.is_empty() || remove.is_empty() || num(w!("SystemComponent")) == 1 || !text(w!("ParentKeyName")).is_empty() || out.iter().any(|a| a.name == title) {
                    continue;
                }
                out.push(crate::qsx::Item {
                    sub: app_line(&text(w!("DisplayVersion")), &text(w!("Publisher")), &text(w!("InstallDate")), num(w!("EstimatedSize"))),
                    key: format!("app\t{}\t{}", icon_file(&text(w!("DisplayIcon"))), uninstall_command(&remove)),
                    name: title,
                    ..Default::default()
                });
            }
            let _ = RegCloseKey(k);
        }
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

/// Uninstall: ask first (as Windows does), then start the app's own uninstaller, which shows its own steps.
fn uninstall_app(id: &str, name: &str) {
    use windows::Win32::System::Threading::{CreateProcessW, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTUPINFOW};
    let Some(command) = id.split('\t').nth(2).filter(|c| !c.is_empty()) else { return };
    unsafe {
        let ask = HSTRING::from(format!("This app and its related info will be uninstalled.\n\n{name}"));
        if MessageBoxW(None, &ask, w!("Uninstall"), MB_OKCANCEL | MB_ICONQUESTION | MB_SETFOREGROUND) != IDOK {
            return;
        }
        let mut cmd: Vec<u16> = command.encode_utf16().chain([0]).collect();
        let si = STARTUPINFOW { cb: size_of::<STARTUPINFOW>() as u32, ..Default::default() };
        let mut pi = PROCESS_INFORMATION::default();
        if CreateProcessW(None, Some(windows::core::PWSTR(cmd.as_mut_ptr())), None, None, false, PROCESS_CREATION_FLAGS(0), None, None, &si, &mut pi).is_ok() {
            let _ = CloseHandle(pi.hThread);
            let _ = CloseHandle(pi.hProcess);
        } else {
            // uninstallers that need administrator rights cannot be started this way: let the shell ask
            windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), w!("cmd.exe"), &HSTRING::from(format!("/c {command}")), PCWSTR::null(), SW_HIDE);
        }
    }
}

fn set_sender(id: &str, on: bool) {
    let v = on as u32;
    unsafe {
        let _ = RegSetKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(format!("{SENDERS}\\{id}")), w!("Enabled"), REG_DWORD.0, Some(&v as *const _ as *const c_void), 4);
    }
}


static VALUES: Mutex<(usize, Vec<Val>)> = Mutex::new((usize::MAX, Vec::new()));

// ======================= backends =======================

fn reg_u32(root: HKEY, key: PCWSTR, name: PCWSTR) -> Option<u32> {
    let (mut v, mut n) = (0u32, 4u32);
    unsafe { RegGetValueW(root, key, name, RRF_RT_REG_DWORD, None, Some(&mut v as *mut _ as *mut c_void), Some(&mut n)).ok().ok()? };
    Some(v)
}

fn reg_str(root: HKEY, key: PCWSTR, name: PCWSTR) -> String {
    let mut buf = [0u16; 256];
    let mut n = (buf.len() * 2) as u32;
    unsafe {
        if RegGetValueW(root, key, name, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut c_void), Some(&mut n)).is_err() {
            return String::new();
        }
    }
    String::from_utf16_lossy(&buf[..(n as usize / 2).saturating_sub(1)]).trim().to_string()
}

const PERSONALIZE: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");

fn set_personalize(name: PCWSTR, v: u32) {
    unsafe {
        let _ = RegSetKeyValueW(HKEY_CURRENT_USER, PERSONALIZE, name, REG_DWORD.0, Some(&v as *const _ as *const c_void), 4);
        // tell every app the colours changed, like Settings does
        let what = w!("ImmersiveColorSet");
        let _ = SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, WPARAM(0), LPARAM(what.as_ptr() as isize), SMTO_ABORTIFHUNG, 200, None);
    }
}

/// Value of `key=` in an .ini-style theme file's text.
pub fn ini_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        k.trim().eq_ignore_ascii_case(key).then(|| v.trim().to_string())
    })
}
/// Windows' built-in themes: (theme file, wallpaper file, is the current theme, "").
fn themes() -> Vec<(String, String, bool, String)> {
    let root = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let current = reg_str(HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes"), w!("CurrentTheme")).to_lowercase();
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(format!("{root}\\Resources\\Themes")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("theme"))).collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|f| {
            // theme files are UTF-16 or ANSI
            let bytes = std::fs::read(&f).ok()?;
            let text = if bytes.starts_with(&[0xFF, 0xFE]) { String::from_utf16_lossy(&bytes[2..].chunks(2).map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)])).collect::<Vec<_>>()) } else { String::from_utf8_lossy(&bytes).to_string() };
            let wall = ini_value(&text, "Wallpaper")?.replace("%SystemRoot%", &root).replace("%systemroot%", &root);
            let path = f.display().to_string();
            let dark = ini_value(&text, "SystemMode").is_some_and(|m| m.eq_ignore_ascii_case("dark"));
            Some((path.clone(), wall, path.to_lowercase() == current, format!("{}:{}", if dark { "dark" } else { "light" }, ini_value(&text, "ColorizationColor").map(|c| c[c.len().saturating_sub(6)..].to_string()).unwrap_or_default())))
        })
        .collect()
}
fn color_mode() -> String {
    if dark() { "Dark".into() } else { "Light".into() }
}
fn color_modes() -> Vec<String> {
    vec!["Light".into(), "Dark".into()]
}
fn set_color_mode(i: usize) {
    set_dark(i == 1);
}

fn dark() -> bool {
    reg_u32(HKEY_CURRENT_USER, PERSONALIZE, w!("AppsUseLightTheme")) == Some(0)
}
fn set_dark(on: bool) {
    set_personalize(w!("AppsUseLightTheme"), !on as u32);
    set_personalize(w!("SystemUsesLightTheme"), !on as u32);
}
fn transparency() -> bool {
    reg_u32(HKEY_CURRENT_USER, PERSONALIZE, w!("EnableTransparency")) != Some(0)
}
fn set_transparency(on: bool) {
    set_personalize(w!("EnableTransparency"), on as u32);
}

fn volume() -> f32 {
    unsafe { crate::quick::endpoint().and_then(|e| e.GetMasterVolumeLevelScalar()).unwrap_or(0.0) }
}
fn set_volume(v: f32) {
    unsafe {
        if let Ok(e) = crate::quick::endpoint() {
            let _ = e.SetMasterVolumeLevelScalar(v.clamp(0.0, 1.0), std::ptr::null());
        }
    }
}
fn muted() -> bool {
    unsafe { crate::quick::endpoint().and_then(|e| e.GetMute()).map(|b| b.as_bool()).unwrap_or(false) }
}
fn set_muted(on: bool) {
    unsafe {
        if let Ok(e) = crate::quick::endpoint() {
            let _ = e.SetMute(on, std::ptr::null());
        }
    }
}
const SCALES: [u32; 12] = [100, 125, 150, 175, 200, 225, 250, 300, 350, 400, 450, 500];

/// Where the recommended scale sits in SCALES, given the current scale and how many steps Windows
/// says it is away from the recommended one.
pub fn recommended_index(current_percent: u32, steps_from_recommended: i32) -> usize {
    let cur = SCALES.iter().position(|s| *s >= current_percent).unwrap_or(0) as i32;
    (cur - steps_from_recommended).clamp(0, SCALES.len() as i32 - 1) as usize
}
fn scale_now() -> (u32, usize) {
    unsafe {
        let pct = windows::Win32::UI::HiDpi::GetDpiForSystem() * 100 / 96;
        let mut rel = 0i32;
        // 0x9E / 0x9F: the scale override Windows' own Settings page reads and writes (steps from recommended)
        let _ = SystemParametersInfoW(SYSTEM_PARAMETERS_INFO_ACTION(0x9E), 0, Some(&mut rel as *mut _ as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        (pct, recommended_index(pct, rel))
    }
}
fn scale_label(i: usize, rec: usize) -> String {
    format!("{}%{}", SCALES[i], if i == rec { " (Recommended)" } else { "" })
}
fn scale() -> String {
    let (pct, rec) = scale_now();
    scale_label(SCALES.iter().position(|s| *s >= pct).unwrap_or(0), rec)
}
fn scales() -> Vec<String> {
    let rec = scale_now().1;
    (0..=(rec + 3).min(SCALES.len() - 1)).map(|i| scale_label(i, rec)).collect()
}
fn set_scale(i: usize) {
    let rec = scale_now().1 as i32;
    unsafe {
        let _ = SystemParametersInfoW(SYSTEM_PARAMETERS_INFO_ACTION(0x9F), (i as i32 - rec) as u32, None, SPIF_UPDATEINIFILE);
    }
}

fn current_mode() -> DEVMODEW {
    let mut m = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
    unsafe {
        let _ = EnumDisplaySettingsW(PCWSTR::null(), ENUM_CURRENT_SETTINGS, &mut m);
    }
    m
}
/// Every size the main display offers, biggest first.
fn modes() -> Vec<(u32, u32)> {
    let mut v = Vec::new();
    let mut m = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
    let mut i = 0;
    unsafe {
        while EnumDisplaySettingsW(PCWSTR::null(), ENUM_DISPLAY_SETTINGS_MODE(i), &mut m).as_bool() {
            if !v.contains(&(m.dmPelsWidth, m.dmPelsHeight)) {
                v.push((m.dmPelsWidth, m.dmPelsHeight));
            }
            i += 1;
        }
    }
    v.sort_by(|a, b| (b.0 * b.1).cmp(&(a.0 * a.1)));
    v
}
fn resolution() -> String {
    let m = current_mode();
    format!("{} \u{D7} {}", m.dmPelsWidth, m.dmPelsHeight)
}
fn resolutions() -> Vec<String> {
    modes().iter().map(|(w_, h)| format!("{w_} \u{D7} {h}")).collect()
}
/// Try a display change the way Windows does: apply it, ask, and go back unless you say keep (15 s).
fn try_mode(m: DEVMODEW) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxTimeoutW(hwnd: *mut c_void, text: PCWSTR, caption: PCWSTR, kind: u32, lang: u16, ms: u32) -> i32;
    }
    unsafe {
        if ChangeDisplaySettingsExW(PCWSTR::null(), Some(&m), None, CDS_TYPE(0), None) != DISP_CHANGE_SUCCESSFUL {
            return;
        }
        let keep = MessageBoxTimeoutW(std::ptr::null_mut(), w!("Keep these display settings?\n\nReverting to the previous settings in 15 seconds."), w!("Settings"), 0x4 | 0x20 | 0x40000 | 0x100, 0, 15000) == 6;
        if keep {
            let _ = ChangeDisplaySettingsExW(PCWSTR::null(), Some(&m), None, CDS_UPDATEREGISTRY, None);
        } else {
            let _ = ChangeDisplaySettingsExW(PCWSTR::null(), None, None, CDS_TYPE(0), None);
        }
    }
}
fn set_resolution(i: usize) {
    if let Some((w_, h)) = modes().get(i).copied() {
        let mut m = current_mode();
        if (m.dmPelsWidth, m.dmPelsHeight) != (w_, h) {
            (m.dmPelsWidth, m.dmPelsHeight) = (w_, h);
            m.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT;
            try_mode(m);
        }
    }
}
const ORIENTATIONS: [&str; 4] = ["Landscape", "Portrait", "Landscape (flipped)", "Portrait (flipped)"];
fn orientation_index() -> usize {
    unsafe { current_mode().Anonymous1.Anonymous2.dmDisplayOrientation.0 as usize % 4 }
}
fn orientation() -> String {
    ORIENTATIONS[orientation_index()].to_string()
}
fn orientations() -> Vec<String> {
    ORIENTATIONS.iter().map(|s| s.to_string()).collect()
}
fn set_orientation(i: usize) {
    let old = orientation_index();
    if i == old || i > 3 {
        return;
    }
    let mut m = current_mode();
    if (i + old) % 2 == 1 {
        (m.dmPelsWidth, m.dmPelsHeight) = (m.dmPelsHeight, m.dmPelsWidth); // turning a quarter swaps width and height
    }
    m.Anonymous1.Anonymous2.dmDisplayOrientation = DEVMODE_DISPLAY_ORIENTATION(i as u32);
    m.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYORIENTATION;
    try_mode(m);
}
/// Night light on/off, read from where Windows keeps it.
/// ponytail: read-only; the switch opens Windows' night light page, because writing this blob is undocumented
fn night_light() -> bool {
    let key = w!("Software\\Microsoft\\Windows\\CurrentVersion\\CloudStore\\Store\\DefaultAccount\\Current\\default$windows.data.bluelightreduction.bluelightreductionstate\\windows.data.bluelightreduction.bluelightreductionstate");
    let mut buf = [0u8; 128];
    let mut n = 128u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, key, w!("Data"), RRF_RT_REG_BINARY, None, Some(buf.as_mut_ptr() as *mut c_void), Some(&mut n)).is_ok() && n > 18 && buf[18] == 0x15 }
}

fn mouse_speed() -> f32 {
    let mut v = 10i32;
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETMOUSESPEED, 0, Some(&mut v as *mut _ as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
    (v - 1) as f32 / 19.0
}
fn set_mouse_speed(f: f32) {
    let v = (1.0 + f * 19.0).round() as isize;
    unsafe {
        let _ = SystemParametersInfoW(SPI_SETMOUSESPEED, 0, Some(v as *mut c_void), SPIF_UPDATEINIFILE | SPIF_SENDCHANGE);
    }
}

fn radio(kind: windows::Devices::Radios::RadioKind) -> bool {
    use windows::Devices::Radios::RadioState;
    crate::quick::radios().unwrap_or_default().iter().any(|r| r.Kind().ok() == Some(kind) && r.State().ok() == Some(RadioState::On))
}
fn wifi() -> bool {
    radio(windows::Devices::Radios::RadioKind::WiFi)
}
fn set_wifi(on: bool) {
    crate::quick::set_radio(windows::Devices::Radios::RadioKind::WiFi, on);
}
fn bluetooth() -> bool {
    radio(windows::Devices::Radios::RadioKind::Bluetooth)
}
fn set_bluetooth(on: bool) {
    crate::quick::set_radio(windows::Devices::Radios::RadioKind::Bluetooth, on);
}
fn airplane() -> bool {
    crate::qsx::airplane() == Some(true)
}
/// The connection carrying your internet: its name (Wi-Fi network or adapter) and whether it is wired.
/// Empty name = not connected.
fn connection() -> (String, bool) {
    use windows::Win32::NetworkManagement::IpHelper::{GetAdaptersAddresses, GAA_FLAG_INCLUDE_GATEWAYS, IP_ADAPTER_ADDRESSES_LH};
    use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    let mut size = 64 * 1024u32;
    let mut buf = vec![0u8; size as usize];
    unsafe {
        if GetAdaptersAddresses(0, GAA_FLAG_INCLUDE_GATEWAYS, None, Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH), &mut size) != 0 {
            return Default::default();
        }
        let mut p = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !p.is_null() {
            let a = &*p;
            if a.OperStatus == IfOperStatusUp && !a.FirstGatewayAddress.is_null() {
                let wifi = a.IfType == 71;
                let ssid = if wifi { crate::quick::wifi_ssid() } else { String::new() };
                return (if ssid.is_empty() { a.FriendlyName.to_string().unwrap_or_default() } else { ssid }, !wifi);
            }
            p = a.Next;
        }
    }
    Default::default()
}
/// "Discoverable as "NAME"" - built once, the PC name doesn't change while we run.
fn discoverable() -> &'static str {
    static S: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    S.get_or_init(|| format!("Discoverable as \"{}\"", device_name()))
}
#[allow(dead_code)]
fn bt_device() -> String {
    let d = crate::quick::bt_device();
    if d.is_empty() { "No device connected".into() } else { d }
}

fn saver() -> bool {
    crate::qsx::saver_on()
}
fn set_saver(on: bool) {
    crate::qsx::set_saver(on);
}
fn battery() -> String {
    crate::quick::battery().map(|(p, _)| format!("{p}%")).unwrap_or_else(|| "No battery".into())
}
/// Windows shows this bar when power settings are set by policy.
fn power_managed() -> bool {
    let mut k = HKEY::default();
    unsafe {
        let ok = RegOpenKeyExW(HKEY_LOCAL_MACHINE, w!("SOFTWARE\\Policies\\Microsoft\\Power\\PowerSettings"), Some(0), KEY_READ, &mut k).is_ok();
        if ok {
            let _ = RegCloseKey(k);
        }
        ok
    }
}
// Windows' three power modes (overlay schemes), in the order its drop-down lists them
const POWER_MODES: [(&str, u128); 3] = [("Best Power Efficiency", 0x961cc777_2547_4f9d_8174_7d86181b8a7a), ("Balanced", 0), ("Best Performance", 0xded574b5_45a0_4f42_8737_46345c09c238)];
#[link(name = "powrprof", kind = "raw-dylib")]
unsafe extern "system" {
    fn PowerGetEffectiveOverlayScheme(scheme: *mut windows::core::GUID) -> u32;
    fn PowerSetActiveOverlayScheme(scheme: *const windows::core::GUID) -> u32;
}
fn power_mode() -> String {
    let mut g = windows::core::GUID::zeroed();
    unsafe { PowerGetEffectiveOverlayScheme(&mut g) };
    POWER_MODES.iter().find(|m| m.1 == g.to_u128()).map(|m| m.0).unwrap_or("Balanced").to_string()
}
fn power_modes() -> Vec<String> {
    POWER_MODES.iter().map(|m| m.0.to_string()).collect()
}
fn set_power_mode(i: usize) {
    if let Some(m) = POWER_MODES.get(i) {
        unsafe { PowerSetActiveOverlayScheme(&windows::core::GUID::from_u128(m.1)) };
    }
}
fn battery_percent_shown() -> bool {
    reg_u32(HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced"), w!("IsBatteryPercentageEnabled")) == Some(1)
}
fn set_battery_percent(on: bool) {
    let v = on as u32;
    unsafe {
        let _ = RegSetKeyValueW(HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced"), w!("IsBatteryPercentageEnabled"), REG_DWORD.0, Some(&v as *const _ as *const c_void), 4);
    }
}
/// "On", or the battery level at which energy saver switches itself on.
fn saver_text() -> String {
    use windows::Win32::System::Power::{PowerGetActiveScheme, PowerReadDCValueIndex};
    if saver() {
        return "On".into();
    }
    unsafe {
        let mut scheme = std::ptr::null_mut();
        let mut pct = 0u32;
        if PowerGetActiveScheme(None, &mut scheme).is_ok() {
            let sub = windows::core::GUID::from_u128(0xde830923_a562_41af_a086_e3a2c6bad2da);
            let key = windows::core::GUID::from_u128(0xe69653ca_cf7f_4f05_aa73_cb833fa90ad4);
            let _ = PowerReadDCValueIndex(None, Some(scheme), Some(&sub), Some(&key), &mut pct);
            let _ = LocalFree(Some(HLOCAL(scheme as *mut c_void)));
        }
        match pct {
            0 => "Never".into(),
            100 => "Always on".into(),
            p => format!("Turns on at {p}%"),
        }
    }
}
fn saver_choices() -> Vec<String> {
    vec![if saver() { "Turn off now".into() } else { "Turn on now".into() }]
}
fn toggle_saver(_: usize) {
    set_saver(!saver());
}

fn access(i: usize) -> bool {
    crate::qsx::access_states().get(i).is_some_and(|a| a.on)
}
fn sticky() -> bool {
    access(5)
}
fn set_sticky(on: bool) {
    crate::qsx::toggle_access(5, !on);
}
fn mono() -> bool {
    access(3)
}
fn set_mono(on: bool) {
    crate::qsx::toggle_access(3, !on);
}

const CPU0: PCWSTR = w!("HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0");
const NT: PCWSTR = w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion");
type Line = (String, String, bool, String);

fn cpu_speed() -> String {
    // Windows rounds to a tenth and prints two decimals ("3.20 GHz")
    format!("{:.2} GHz", (reg_u32(HKEY_LOCAL_MACHINE, CPU0, w!("~MHz")).unwrap_or(0) as f64 / 100.0).round() / 10.0)
}
/// Installed and usable memory in GB.
fn ram_gb() -> (f64, f64) {
    use windows::Win32::System::SystemInformation::{GetPhysicallyInstalledSystemMemory, GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    let mut kb = 0u64;
    unsafe {
        let _ = GlobalMemoryStatusEx(&mut m);
        let _ = GetPhysicallyInstalledSystemMemory(&mut kb);
    }
    let usable = m.ullTotalPhys as f64 / 1073741824.0;
    (if kb > 0 { kb as f64 / 1048576.0 } else { usable }, usable)
}
pub fn size_label(bytes: u64) -> String {
    if bytes >= 1 << 30 { format!("{} GB", (bytes as f64 / 1073741824.0).round()) } else { format!("{} MB", (bytes as f64 / 1048576.0).round()) }
}
/// Graphics adapters and their dedicated memory, biggest first.
fn gpus() -> Vec<(String, u64)> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    let mut v: Vec<(String, u64)> = Vec::new();
    unsafe {
        if let Ok(f) = CreateDXGIFactory1::<IDXGIFactory1>() {
            let mut i = 0;
            while let Ok(a) = f.EnumAdapters1(i) {
                i += 1;
                if let Ok(d) = a.GetDesc1() {
                    let name = String::from_utf16_lossy(&d.Description[..d.Description.iter().position(|c| *c == 0).unwrap_or(0)]);
                    if d.Flags & 2 == 0 && !v.iter().any(|g| g.0 == name) {
                        v.push((name, d.DedicatedVideoMemory as u64)); // 2 = software adapter
                    }
                }
            }
        }
    }
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}
/// Used and total bytes across the fixed drives.
fn storage() -> (u64, u64) {
    use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives};
    let (mut used, mut total) = (0u64, 0u64);
    unsafe {
        let mask = GetLogicalDrives();
        for i in 0..26 {
            let root = HSTRING::from(format!("{}:\\", (b'A' + i) as char));
            if mask & (1 << i) != 0 && GetDriveTypeW(&root) == 3 {
                let (mut free, mut all) = (0u64, 0u64);
                if GetDiskFreeSpaceExW(&root, None, Some(&mut all), Some(&mut free)).is_ok() {
                    used += all - free;
                    total += all;
                }
            }
        }
    }
    (used, disks().max(total))
}
/// Size of every fixed disk added up (what Windows calls your storage, partitions you can't see included).
fn disks() -> u64 {
    use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    let mut total = 0u64;
    for i in 0..8 {
        unsafe {
            let Ok(h) = CreateFileW(&HSTRING::from(format!(r"\\.\PhysicalDrive{i}")), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None) else { break };
            let mut out = [0u8; 64]; // DISK_GEOMETRY_EX: media type at 8, disk size at 24
            let mut got = 0u32;
            if windows::Win32::System::IO::DeviceIoControl(h, 0x700A0, None, 0, Some(out.as_mut_ptr() as *mut c_void), 64, Some(&mut got), None).is_ok() && out[8] == 12 {
                total += u64::from_le_bytes(out[24..32].try_into().unwrap());
            }
            let _ = CloseHandle(h);
        }
    }
    total
}
fn storage_text() -> String {
    let (used, total) = storage();
    format!("{} of {} used", size_label(used), size_label(total))
}
fn spec_tiles() -> Vec<Line> {
    let g = gpus();
    let (installed, _) = ram_gb();
    let mut v = vec![("Processor".to_string(), processor().trim().to_string(), false, cpu_speed()), ("Installed RAM".to_string(), format!("{installed:.1} GB"), false, String::new())];
    if let Some(top) = g.first() {
        v.push(("Graphics card".to_string(), size_label(top.1), false, if g.len() > 1 { "Multiple GPUs installed".to_string() } else { top.0.clone() }));
    }
    v.push(("Storage".to_string(), size_label(storage().1), false, storage_text()));
    v
}
fn device_info() -> Vec<Line> {
    let (installed, usable) = ram_gb();
    let line = |k: &str, v: String| (k.to_string(), v, false, String::new());
    vec![
        line("Device name", device_name().to_lowercase()),
        line("Processor", format!("{} ({})", processor().trim(), cpu_speed())),
        line("Installed RAM", format!("{installed:.1} GB ({usable:.1} GB usable)")),
        line("Graphics card", gpus().iter().map(|g| format!("{} ({})", g.0, size_label(g.1))).collect::<Vec<_>>().join("\n")),
        line("Storage", storage_text()),
        line("Device ID", reg_str(HKEY_LOCAL_MACHINE, w!("SOFTWARE\\Microsoft\\SQMClient"), w!("MachineId")).trim_matches(|c| c == '{' || c == '}').to_string()),
        line("Product ID", reg_str(HKEY_LOCAL_MACHINE, NT, w!("ProductId"))),
        line("System type", format!("64-bit operating system, {}-based processor", if std::env::var("PROCESSOR_ARCHITECTURE").is_ok_and(|a| a == "ARM64") { "ARM" } else { "x64" })),
    ]
}
/// Days since 1970 -> (year, month, day).
pub fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}
fn windows_specs() -> Vec<Line> {
    let line = |k: &str, v: String| (k.to_string(), v, false, String::new());
    let (y, m, d) = civil(reg_u32(HKEY_LOCAL_MACHINE, NT, w!("InstallDate")).unwrap_or(0) as i64 / 86400);
    vec![
        line("Edition", reg_str(HKEY_LOCAL_MACHINE, NT, w!("ProductName")).replace("Windows 10", "Windows 11")),
        line("Version", reg_str(HKEY_LOCAL_MACHINE, NT, w!("DisplayVersion"))),
        line("Installed on", format!("{m}/{d}/{y}")),
        line("OS build", format!("{}.{}", reg_str(HKEY_LOCAL_MACHINE, NT, w!("CurrentBuild")), reg_u32(HKEY_LOCAL_MACHINE, NT, w!("UBR")).unwrap_or(0))),
    ]
}
/// Put text on the clipboard (the Copy buttons).
pub(crate) fn copy_text(text: &str) {
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    let w: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    unsafe {
        if OpenClipboard(None).is_ok() {
            let _ = EmptyClipboard();
            if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2) {
                let p = GlobalLock(h) as *mut u16;
                if !p.is_null() {
                    std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                    let _ = GlobalUnlock(h);
                    let _ = SetClipboardData(13, Some(HANDLE(h.0))); // CF_UNICODETEXT
                }
            }
            let _ = CloseClipboard();
        }
    }
}

fn device_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_default()
}
fn processor() -> String {
    reg_str(HKEY_LOCAL_MACHINE, w!("HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0"), w!("ProcessorNameString"))
}
/// [(time, date), ("Time zone", display name), ("Region", country)] in your own formats.
fn time_summary() -> Vec<(String, String, bool, String)> {
    use windows::Win32::Globalization::{GetDateFormatEx, GetGeoInfoEx, GetTimeFormatEx, GetUserDefaultGeoName, DATE_LONGDATE, GEO_FRIENDLYNAME, TIME_NOSECONDS};
    let text = |buf: &[u16], n: i32| String::from_utf16_lossy(&buf[..(n.max(1) - 1) as usize]);
    let mut buf = [0u16; 128];
    unsafe {
        let n = GetTimeFormatEx(PCWSTR::null(), TIME_NOSECONDS, None, PCWSTR::null(), Some(&mut buf));
        let time = text(&buf, n);
        let n = GetDateFormatEx(PCWSTR::null(), DATE_LONGDATE, None, PCWSTR::null(), Some(&mut buf), PCWSTR::null());
        let date = text(&buf, n);
        let key = reg_str(HKEY_LOCAL_MACHINE, w!("SYSTEM\\CurrentControlSet\\Control\\TimeZoneInformation"), w!("TimeZoneKeyName"));
        let zone = reg_str(HKEY_LOCAL_MACHINE, PCWSTR(HSTRING::from(format!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Time Zones\\{key}")).as_ptr()), w!("Display"));
        let mut geo = [0u16; 16];
        let _ = GetUserDefaultGeoName(&mut geo);
        let n = GetGeoInfoEx(PCWSTR(geo.as_ptr()), GEO_FRIENDLYNAME, Some(&mut buf));
        let region = text(&buf, n);
        vec![(time, date, false, String::new()), ("Time zone".into(), if zone.is_empty() { time_zone() } else { zone }, false, String::new()), ("Region".into(), region, false, String::new())]
    }
}
/// Windows has installed updates and is waiting for a restart.
fn restart_pending() -> bool {
    let mut k = HKEY::default();
    unsafe {
        let ok = RegOpenKeyExW(HKEY_LOCAL_MACHINE, w!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\WindowsUpdate\\Auto Update\\RebootRequired"), Some(0), KEY_READ, &mut k).is_ok();
        if ok {
            let _ = RegCloseKey(k);
        }
        // newer builds stage the update and offer "Update and restart" instead of setting that key
        ok || reg_u32(HKEY_LOCAL_MACHINE, w!(r"SOFTWARE\Microsoft\WindowsUpdate\Orchestrator"), w!("EnhancedShutdownEnabled")) == Some(1)
    }
}
fn early_updates() -> bool {
    reg_u32(HKEY_LOCAL_MACHINE, w!("SOFTWARE\\Microsoft\\WindowsUpdate\\UX\\Settings"), w!("IsContinuousInnovationOptedIn")) == Some(1)
}
fn time_zone() -> String {
    let mut tz = windows::Win32::System::Time::TIME_ZONE_INFORMATION::default();
    unsafe { windows::Win32::System::Time::GetTimeZoneInformation(&mut tz) };
    String::from_utf16_lossy(&tz.StandardName[..tz.StandardName.iter().position(|&c| c == 0).unwrap_or(0)])
}
/// "Administrator" or "Standard user", from how Windows issued this sign-in's token.
fn account_type() -> String {
    use windows::Win32::Security::{GetTokenInformation, TokenElevationType, TOKEN_QUERY};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut tok = HANDLE::default();
        let (mut kind, mut n) = (0u32, 0u32);
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok).is_ok() {
            let _ = GetTokenInformation(tok, TokenElevationType, Some(&mut kind as *mut _ as *mut c_void), 4, &mut n);
            let _ = CloseHandle(tok);
        }
        // 2 = full administrator token, 3 = an administrator's limited token, 1 = an ordinary account
        if kind >= 2 { "Administrator".into() } else { "Standard user".into() }
    }
}
fn account_summary() -> Vec<(String, String, bool, String)> {
    vec![(crate::launcher::display_name(), account_email(), false, account_type())]
}
fn home_summary() -> Vec<(String, String, bool, String)> {
    let (net, _) = connection();
    let line = |a: String, b: String| (a, b, false, String::new());
    vec![
        line(device_name().to_lowercase(), model()),
        line(if net.is_empty() { "Not connected".into() } else { net.clone() }, if net.is_empty() { String::new() } else { "Connected".into() }),
        line("Windows Update".into(), if restart_pending() { "Restart required".into() } else { "You're up to date".into() }),
    ]
}
fn rust_ui_ram() -> String {
    use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    let mut c = PROCESS_MEMORY_COUNTERS::default();
    unsafe {
        let _ = K32GetProcessMemoryInfo(windows::Win32::System::Threading::GetCurrentProcess(), &mut c, size_of::<PROCESS_MEMORY_COUNTERS>() as u32);
    }
    format!("{:.0} MB in use, {:.0} MB committed", c.WorkingSetSize as f64 / 1048576.0, c.PagefileUsage as f64 / 1048576.0)
}
fn is_shell() -> bool {
    crate::session::installed()
}
fn set_shell(on: bool) {
    let _ = if on { crate::session::install() } else { crate::session::restore() };
}

fn model() -> String {
    reg_str(HKEY_LOCAL_MACHINE, w!("HARDWARE\\DESCRIPTION\\System\\BIOS"), w!("SystemProductName"))
}

/// The Microsoft account this PC is signed in with (shown under your name), if any.
fn account_email() -> String {
    unsafe {
        let mut k = HKEY::default();
        // the account Windows itself is signed in with (other apps' accounts are stored elsewhere)
        if RegOpenKeyExW(HKEY_USERS, w!(".DEFAULT\\Software\\Microsoft\\IdentityCRL\\StoredIdentities"), Some(0), KEY_READ, &mut k).is_err() {
            return String::new();
        }
        let mut name = [0u16; 256];
        let mut n = name.len() as u32;
        let ok = RegEnumKeyExW(k, 0, Some(windows::core::PWSTR(name.as_mut_ptr())), &mut n, None, None, None, None).is_ok();
        let _ = RegCloseKey(k);
        if ok { String::from_utf16_lossy(&name[..n as usize]) } else { String::new() }
    }
}

fn dnd() -> bool {
    crate::notify::dnd_get()
}

fn pages() -> Vec<Page> {
    use Kind::*;
    let r = |icon, label, sub, kind| Row { icon, label, sub, kind };
    let top = |name, icon, rows| Page { name, icon, parent: "", rows };
    let sub = |name, parent, rows| Page { name, icon: 0, parent, rows };
    vec![
        top("Home", 0xE80F, vec![
            r(0, "", "", HomeHero(home_summary)),
            r(0, "Recommended settings", "", Header),
            r(0xEDA2, "Storage", "", Link("ms-settings:storagesense")),
            r(0xE71D, "Installed apps", "", Link("ms-settings:appsfeatures")),
            r(0, "Rust UI", "", Header),
            r(0xE950, "Use Rust UI as the Windows shell", "Replaces Explorer's taskbar and desktop from your next sign-in", Toggle(is_shell, set_shell)),
            r(0xE9D9, "Memory used by Rust UI", "", Info(rust_ui_ram)),
        ]),
        top("System", 0xE770, vec![
            r(0xE7F8, "Display", "Monitors, brightness, night light, display profile", Sub("Display")),
            r(0xE767, "Sound", "Volume levels, output, input, sound devices", Sub("Sound")),
            r(0xEA8F, "Notifications", "Alerts from apps and system, do not disturb", Sub("Notifications")),
            r(SET | 0xE5C4, "Focus", "Reduce distractions", Link("ms-settings:quiethours")),
            r(0xE7E8, "Power & battery", "Screen and sleep, battery usage, power mode, energy saver", Sub("Power & battery")),
            r(0xEDA2, "Storage", "Storage space, drives, configuration rules", Link("ms-settings:storagesense")),
            r(0xE72D, "Share", "Share files, photos, links and more", Link("ms-settings:crossdevice")),
            r(0xE7C4, "Multitasking", "Snap windows, desktops, task switching", Link("ms-settings:multitasking")),
            r(0xE946, "About", "Device specifications, rename PC, Windows specifications", Sub("About")),
        ]),
        sub("Display", "System", {
            // ponytail: no Brightness card here, like the real page on this PC (it lives in Quick Settings);
            // add it back when we know the rule Windows uses to show it
            let mut v = vec![r(0, "Brightness & color", "", Header)];
            v.extend([
                r(0xF08C, "Night light", "Use warmer colors to help block blue light", State(night_light, "ms-settings:nightlight")),
                r(0xEF3C, "Color profile", "Determine how colors appear on your screen", Link("ms-settings:display")),
                r(SET | 0xEA7F, "HDR", "More about HDR", Link("ms-settings:display")),
                r(0, "Scale & layout", "", Header),
                r(SET | 0xE58E, "Scale", "Change the size of text, apps, and other items", Choice(scale, scales, set_scale)),
                r(0xE799, "Display resolution", "Adjust the resolution to fit your connected display", Choice(resolution, resolutions, set_resolution)),
                r(0xE8B4, "Display orientation", "", Choice(orientation, orientations, set_orientation)),
                r(0xEBC6, "Multiple displays", "Choose the presentation mode for your displays", Expand("ms-settings:display")),
                r(0, "Related settings", "", Header),
                r(0xE7F4, "Advanced display", "Display information, refresh rate", Link("ms-settings:display-advanced")),
                r(0xE7FC, "Graphics", "", Link("ms-settings:display-advancedgraphics")),
            ]);
            v
        }),
        sub("Sound", "System", vec![
            r(0, "Output", "", Header),
            r(0xE767, "Choose where to play sound", "Apps might have their own settings", Devices(crate::qsx::outputs, "Pair a new output device")),
            r(0, "Volume", "", Slider(volume, set_volume)),
            r(0, "Mono audio", "Combine left and right audio channels into one", Toggle(mono, set_mono)),
            r(0, "Input", "", Header),
            r(0xE720, "Choose a device for speaking or recording", "Apps might have their own settings", Devices(crate::qsx::inputs, "Pair a new input device")),
            r(0, "Advanced", "", Header),
            r(0xE74F, "Mute", "", Toggle(muted, set_muted)),
            r(0xE9E9, "Volume mixer", "App volume mix, app input & output devices", Link("ms-settings:apps-volume")),
            r(0, "More sound settings", "", Link("ms-settings:sound")),
        ]),
        sub("Notifications", "System", vec![
            r(0xEA8F, "Notifications", "Get notifications from apps and other senders", Toggle(toasts, set_toasts)),
            r(0xF4BD, "Do not disturb", "Notifications will be sent directly to notification center", Toggle(dnd, crate::notify::dnd_set)),
            r(SET | 0xECC1, "Turn on do not disturb automatically", "", Expand("ms-settings:notifications")),
            r(0xE8D0, "Set priority notifications", "", Link("ms-settings:notifications")),
            r(SET | 0xE5C4, "Focus", "Session duration, hide badges on apps", Link("ms-settings:quiethours")),
            r(0, "Notifications from apps and other senders", "", Header),
            r(0, "Sort by:", "", SortBy),
            r(0, "", "", Senders(senders, set_sender)),
        ]),
        sub("Power & battery", "System", vec![
            r(0, "", "", Big(battery)),
            r(0xE946, "", "", Note(power_managed, "Some of these settings are managed by your organization.")),
            r(0xE8BE, "Energy recommendations", "Lower your carbon footprint by applying these recommendations", Link("ms-settings:energyrecommendations")),
            r(0xEC48, "Power Mode", "Optimize your device based on power use and performance", Flat(power_mode, power_modes, set_power_mode)),
            r(0xEE3F, "Screen, sleep, & hibernate timeouts", "Choose what happens when your device is idle for a specified time", Expand("ms-settings:powersleep")),
            r(0xEBB2, "Battery percentage", "Display battery percentage on taskbar", Toggle(battery_percent_shown, set_battery_percent)),
            r(0xEBBA, "Energy saver", "Reduce power consumption and increase battery life by limiting some background activities", Flat(saver_text, saver_choices, toggle_saver)),
            r(SET | 0xE9E6, "Battery usage", "", Expand("ms-settings:batterysaver-usagedetails")),
            r(0xE762, "Lid & power button controls", "Choose what happens when you interact with your device's physical controls", Expand("ms-settings:powersleep")),
        ]),
        sub("About", "System", vec![
            r(0, "", "", Tiles(spec_tiles)),
            r(0, "", "", RenamePc),
            r(0xE946, "Device info", "", Facts(device_info)),
            r(0xE770, "Windows specifications", "", Facts(windows_specs)),
            r(0, "Product key and activation", "Change product key or upgrade your edition of Windows", Link("ms-settings:activation")),
        ]),
        top("Bluetooth & devices", 0xE702, vec![
            r(0, "", "", BtTiles(crate::qsx::bt_devices)),
            r(0, "", "", Center("View more devices", "ms-settings:connecteddevices")),
            r(0xE702, "Bluetooth", discoverable(), Toggle(bluetooth, set_bluetooth)),
            r(0, "", "", Space(16.33)),
            r(0xE772, "Devices", "Mouse, keyboard, pen, audio, displays and docks, other devices", LinkBtn("ms-settings:connecteddevices", "Add device", "ms-settings-connectabledevices:devicediscovery")),
            r(0xE749, "Printers & scanners", "Preferences", Link("ms-settings:printers")),
            r(0xE1C9, "Mobile devices", "Instantly access your mobile devices from your PC", Link("ms-settings:mobile-devices")),
            r(0xE722, "Cameras", "Connected cameras, default image settings", Link("ms-settings:camera")),
            r(0xE962, "Mouse", "Buttons, mouse pointer speed, scrolling", Link("ms-settings:mousetouchpad")),
            r(0xE962, "Mouse pointer speed", "", Slider(mouse_speed, set_mouse_speed)),
            r(0xEFA5, "Touchpad", "Taps, gestures, scrolling, zooming", Link("ms-settings:devices-touchpad")),
        ]),
        top("Network & internet", 0xE701, vec![
            r(0, "", "", NetHero(connection)),
            r(0xE701, "Wi-Fi", "Connect, manage known networks, metered network", ToggleMore(wifi, set_wifi, "ms-settings:network-wifi")),
            r(0xE839, "Ethernet", "Authentication, IP and DNS settings, metered network", Link("ms-settings:network-ethernet")),
            r(0xE705, "VPN", "Add, connect, manage", Link("ms-settings:network-vpn")),
            r(0xE704, "Mobile hotspot", "Share your internet connection", Link("ms-settings:network-mobilehotspot")),
            r(0xE709, "Airplane mode", "Stop wireless communication", ToggleMore(airplane, crate::qsx::set_airplane, "ms-settings:network-airplanemode")),
            r(0xE968, "Proxy", "Proxy server for Wi-Fi and Ethernet connections", Link("ms-settings:network-proxy")),
            r(0xE83C, "Dial-up", "Set up a dial-up internet connection", Link("ms-settings:network-dialup")),
            r(0xE774, "Advanced network settings", "View all network adapters, network reset", Link("ms-settings:network-advancedsettings")),
        ]),
        top("Personalization", 0xE771, vec![
            r(0, "", "", ThemeHero(themes)),
            r(0xEB9F, "Background", "Background image, color, slideshow", Link("ms-settings:personalization-background")),
            r(0xE790, "Colors", "Accent color, transparency effects, color theme", Sub("Colors")),
            r(0xE771, "Themes", "Install, create, manage", Link("ms-settings:themes")),
            r(SET | 0xF431, "Dynamic Lighting", "Connected devices, effects, app settings", Link("ms-settings:personalization-lighting")),
            r(0xEE3F, "Lock screen", "Lock screen images, apps, animations", Link("ms-settings:lockscreen")),
            r(0xE144, "Text input", "Touch keyboard, voice typing, emoji and more, input method editor", Link("ms-settings:personalization-textinput")),
            r(0xE8FC, "Start", "Recent apps and items, folders", Link("ms-settings:personalization-start")),
            r(0xE90E, "Taskbar", "Taskbar behaviors, system pins", Link("ms-settings:taskbar")),
            r(0xE8D2, "Fonts", "Install, manage", Link("ms-settings:fonts")),
            r(0xE7F8, "Device usage", "Select all the ways you plan to use your device", Link("ms-settings:deviceusage")),
        ]),
        sub("Colors", "Personalization", vec![
            r(0xE790, "Choose your mode", "Change the colors that appear in Windows and your apps", Choice(color_mode, color_modes, set_color_mode)),
            r(0xEF1F, "Transparency effects", "Windows and surfaces appear translucent", Toggle(transparency, set_transparency)),
            r(0xE790, "Accent color", "", Link("ms-settings:colors")),
        ]),
        sub("Installed apps", "Apps", vec![
            r(0, "", "", Actions(installed_apps, "Uninstall", uninstall_app)),
        ]),
        sub("Startup", "Apps", vec![
            r(0, "Startup apps", "", Header),
            r(0, "", "", Senders(startup_apps, set_startup)),
        ]),
        top("Apps", 0xE71D, vec![
            r(0xE71D, "Installed apps", "Uninstall and manage apps on your PC", Sub("Installed apps")),
            r(SET | 0xEFDD, "Advanced app settings", "Choose where to get apps, archive apps, uninstall updates", Link("ms-settings:appsfeatures-app")),
            r(SET | 0xE42A, "Default apps", "Defaults for file and link types, other defaults", Link("ms-settings:defaultapps")),
            r(0xF277, "Actions", "Windows can recommend actions from these apps.", Link("ms-settings:appactions")),
            r(0xE826, "Offline maps", "Downloads, storage location, map updates", Link("ms-settings:maps")),
            r(SET | 0xF6AB, "Apps for websites", "Websites that can open in an app instead of a browser", Link("ms-settings:appsforwebsites")),
            r(0xE116, "Video playback", "Video adjustments, HDR streaming, battery options", Link("ms-settings:videoplayback")),
            r(0xE7B5, "Startup", "Apps that start automatically when you sign in", Sub("Startup")),
            r(0xEA6C, "Resume", "Continue work across devices", Link("ms-settings:crossdevice")),
        ]),
        top("Accounts", 0xE77B, vec![
            r(0, "", "", AccountHero(account_summary)),
            r(0, "Account settings", "", Header),
            r(0xE77B, "Your info", "Profile photo", Link("ms-settings:yourinfo")),
            r(0xE192, "Sign-in options", "Windows Hello, security key, password, dynamic lock", Link("ms-settings:signinoptions")),
            r(0xE715, "Email & accounts", "Accounts used by email, calendar, and contacts", Link("ms-settings:emailandaccounts")),
            r(0xE716, "Family", "Manage your family group, edit account types and device permissions", Link("ms-settings:family-group")),
            r(0xE753, "Windows backup", "Back up your files, apps, preferences to restore them across devices", Link("ms-settings:backup")),
            r(0xE8FA, "Other users", "Device access, work or school users, kiosk assigned access", Link("ms-settings:otherusers")),
            r(0xE821, "Access work or school", "Organization resources like email, apps, and network", Link("ms-settings:workplace")),
            r(0xE8D7, "Passkeys", "Use your face, fingerprint, or PIN to sign in to apps and websites", Link("ms-settings:savedpasskeys")),
        ]),
        top("Time & language", 0xE775, vec![
            r(0, "", "", TimeHero(time_summary)),
            r(SET | 0xE4AB, "Date & time", "Time zones, automatic clock settings, calendar display", Link("ms-settings:dateandtime")),
            r(0xF2B7, "Language & region", "Windows display language, preferred languages, regional formats", Link("ms-settings:regionlanguage")),
            r(0xE144, "Typing", "Touch keyboard, text suggestions, preferences", Link("ms-settings:typing")),
            r(0xEFA9, "Speech", "Speech language, speech recognition microphone setup, voices", Link("ms-settings:speech")),
        ]),
        top("Gaming", 0xE7FC, vec![
            r(SET | 0xF28C, "XBOX mode", "Enable, accessibility control hints", Link("ms-settings:gaming-fullscreenexperience")),
            r(SET | 0xF192, "Game Bar", "Controller and keyboard shortcuts", Link("ms-settings:gaming-gamebar")),
            r(0xF7EE, "Captures", "Save location, recording preferences", Link("ms-settings:gaming-gamedvr")),
            r(0xE7FC, "Game Mode", "Optimize your PC for play", Link("ms-settings:gaming-gamemode")),
        ]),
        top("Accessibility", 0xE776, vec![
            r(0, "Vision", "", Header),
            r(0xE8E9, "Text size", "Text size that appears throughout Windows and your apps", Link("ms-settings:easeofaccess-display")),
            r(0xE794, "Visual effects", "Scroll bars, transparency, animations, notification timeout", Link("ms-settings:easeofaccess-visualeffects")),
            r(0xE7C9, "Mouse pointer and touch", "Mouse pointer color, size", Link("ms-settings:easeofaccess-mousepointer")),
            r(SET | 0xE330, "Text cursor", "Appearance and thickness, text cursor indicator", Link("ms-settings:easeofaccess-cursor")),
            r(0xE8A3, "Magnifier", "Magnifier reading, zoom increment", Link("ms-settings:easeofaccess-magnifier")),
            r(0xE790, "Color filters", "Colorblindness filters, grayscale, inverted", Link("ms-settings:easeofaccess-colorfilter")),
            r(0xE81E, "Screen tint", "Add a subtle color overlay to make your screen more comfortable to view", Link("ms-settings:easeofaccess-colorfilter")),
            r(0xE7A1, "Contrast themes", "Color themes for low vision, light sensitivity", Link("ms-settings:easeofaccess-highcontrast")),
            r(0xED4D, "Narrator", "Voice, verbosity, keyboard, braille", Link("ms-settings:easeofaccess-narrator")),
            r(0, "Hearing", "", Header),
            r(0xE767, "Audio", "Mono audio, audio notifications", Link("ms-settings:easeofaccess-audio")),
            r(0xE7F0, "Captions", "Styles, live captions", Link("ms-settings:easeofaccess-closedcaptioning")),
            r(0, "Interaction", "", Header),
            r(0xE720, "Speech", "Voice access, voice typing, Windows Speech Recognition", Link("ms-settings:easeofaccess-speechrecognition")),
            r(0xE765, "Keyboard", "Sticky, Filter, and Toggle keys, on-screen keyboard", Link("ms-settings:easeofaccess-keyboard")),
            r(0xE765, "Sticky keys", "Press one key at a time for keyboard shortcuts", Toggle(sticky, set_sticky)),
            r(0xE962, "Mouse", "Mouse keys, speed, acceleration", Link("ms-settings:easeofaccess-mouse")),
            r(0xE7B3, "Eye control", "Eye tracker, text-to-speech", Link("ms-settings:easeofaccess-eyecontrol")),
        ]),
        top("Privacy & security", 0xE72E, vec![
            r(0, "Security", "", Section),
            r(0xEA18, "Windows Security", "Antivirus, browser, firewall, and network protection for your device", Link("windowsdefender:")),
            r(0xE707, "Find my device", "Track your device if you think you've lost it", Link("ms-settings:findmydevice")),
            r(0, "Windows permissions", "", Section),
            r(0xE72E, "Recommendations & offers", "Advertising ID, personalized offers, local content, app launches, settings suggestions, productivity tools", Link("ms-settings:privacy")),
            r(0xEFA9, "Speech", "Online speech recognition for dictation and other voice-based interactions", Link("ms-settings:privacy-speech")),
            r(0xEADF, "Inking & typing personalization", "Custom dictionary, words in your dictionary", Link("ms-settings:privacy-speechtyping")),
            r(0xE9D9, "Diagnostics & feedback", "Diagnostic data, inking and typing data, feedback frequency", Link("ms-settings:privacy-feedback")),
            r(0xE773, "Search", "Search history, search apps, cloud content search, search indexing", Link("ms-settings:search")),
            r(0, "App permissions", "", Section),
            r(0xE81D, "Location", "", Link("ms-settings:privacy-location")),
            r(0xE722, "Camera", "", Link("ms-settings:privacy-webcam")),
            r(0xE720, "Microphone", "", Link("ms-settings:privacy-microphone")),
            r(0xEA8F, "Notifications", "", Link("ms-settings:privacy-notifications")),
        ]),
        top("Windows Update", 0xE895, vec![
            r(0, "", "", UpdateHero(restart_pending)),
            r(0, "Available updates", "These updates will complete automatically.", When(restart_pending, "ms-settings:windowsupdate")),
            r(0, "More options", "", Header),
            // ponytail: read-only; this switch is a machine-wide setting that needs administrator rights to change
            r(0xE789, "Get the latest updates as soon as they're available", "Be among the first to get the latest non-security updates, fixes, and improvements as they roll out.", State(early_updates, "ms-settings:windowsupdate")),
            r(0xE769, "Pause updates", "Select the date to pause updates until.", Link("ms-settings:windowsupdate")),
            r(0xE81C, "Update history", "", Link("ms-settings:windowsupdate-history")),
            r(0xE9F5, "Advanced options", "Delivery optimization, optional updates, active hours, other update settings", Link("ms-settings:windowsupdate-options")),
            r(0xF1AD, "Windows Insider Program", "Get preview builds of Windows to share feedback on new features and updates", Link("ms-settings:windowsinsider")),
        ]),
    ]
}

// ======================= window (measured from Windows' Settings at 100%) =======================

const TOPBAR: f32 = 48.0;
const CARD_X: f32 = 325.0;
const NAV_Y: f32 = 143.0;

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Nav(usize),    // page index
    Row(usize),
    Back,
    Win(u8),       // minimise / maximise / close
    Result(usize), // page index of a search result
    Rename,
    Radio(usize, usize), // row, device
    Pair(usize),
    Mute,
    Sender(usize, usize), // row, app
    Sort,
    Copy(usize),
    Open(&'static str), // opens this page of Windows' own Settings
    Tile(usize),        // a device tile's Connect button
    Theme(usize),       // a theme thumbnail
}

#[derive(Default)]
struct Ui {
    page: usize,
    hover: Option<Hit>,
    drag: Option<usize>,
    vals: Vec<Val>,
    query: String,
    hits: Vec<(D2D_RECT_F, Hit)>,
    gfx: Option<(ID2D1DCRenderTarget, IDWriteFactory)>,
}

thread_local! {
    static SCROLL: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
    static UI: RefCell<Ui> = RefCell::new(Ui { page: 1, ..Default::default() });
    static PAGES: Vec<Page> = pages();
}

fn page_index(name: &str) -> Option<usize> {
    PAGES.with(|p| p.iter().position(|x| x.name == name))
}

/// Read every value on a page off the UI thread (some of Windows' answers take a moment).
fn load(page: usize) {
    std::thread::spawn(move || unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        let kinds: Vec<Kind> = pages().get(page).map(|p| p.rows.iter().map(|r| r.kind).collect()).unwrap_or_default();
        let mut slow = String::new(); // which value took long to read, for %TEMP%\shellbits-settings-load.txt
        let mut index = 0;
        let vals = kinds
            .into_iter()
            .map(|k| {
                let t = std::time::Instant::now();
                if std::env::var("SHELLBITS_TRACE").is_ok() {
                    let _ = std::fs::write(std::env::temp_dir().join("shellbits-settings-row.txt"), format!("page {page} reading row {index}"));
                }
                let v = read_value(k);
                if t.elapsed().as_millis() > 300 {
                    slow += &format!("page {page} row {index}: {} ms

", t.elapsed().as_millis());
                }
                index += 1;
                v
            })
            .collect();
        if !slow.is_empty() {
            let _ = std::fs::write(std::env::temp_dir().join("shellbits-settings-load.txt"), slow);
        }
        *VALUES.lock().unwrap() = (page, vals);
        let _ = PostMessageW(Some(HWND(WIN.load(Relaxed) as *mut c_void)), WM_VALUES, WPARAM(0), LPARAM(0));
    });
}

fn read_value(k: Kind) -> Val {
    match k {
        Kind::Toggle(get, _) | Kind::ToggleMore(get, ..) => Val { on: get(), ..Default::default() },
        Kind::NetHero(get) => {
            let (s, on) = get();
            Val { s, on, ..Default::default() }
        }
        Kind::Slider(get, _) => Val { f: get(), on: std::ptr::fn_addr_eq(get, volume as fn() -> f32) && muted(), ..Default::default() },
        Kind::Info(get) | Kind::Choice(get, ..) | Kind::Flat(get, ..) => Val { s: get(), ..Default::default() },
        Kind::Big(get) => Val { s: get(), on: crate::quick::battery().is_some_and(|b| b.1), ..Default::default() },
        Kind::Note(shown, _) => Val { list: if shown() { vec![Default::default()] } else { Vec::new() }, ..Default::default() },
        Kind::State(get, _) | Kind::UpdateHero(get) => Val { on: get(), ..Default::default() },
        Kind::When(shown, _) => Val { list: if shown() { vec![Default::default()] } else { Vec::new() }, ..Default::default() },
        Kind::Tiles(get) | Kind::Facts(get) | Kind::ThemeHero(get) | Kind::TimeHero(get) | Kind::HomeHero(get) | Kind::AccountHero(get) => Val { list: get(), ..Default::default() },
        Kind::BtTiles(get) => Val { list: get().into_iter().map(|d| (d.name, String::new(), d.on, d.key)).collect(), ..Default::default() },
        Kind::Actions(get, ..) => Val { list: get().into_iter().map(|d| (d.name, d.sub, d.on, d.key)).collect(), ..Default::default() },
        Kind::Senders(get, _) => Val {
            list: get().into_iter().map(|d| (d.name, if !d.sub.is_empty() { d.sub } else if d.on { "Banners, Sounds".to_string() } else { "Off".to_string() }, d.on, d.key)).collect(),
            ..Default::default()
        },
        Kind::Devices(get, _) => Val {
            list: get().into_iter().map(|d| { let (a, b) = split_device(&d.name); (a, b, d.on, d.key) }).collect(),
            ..Default::default()
        },
        _ => Val::default(),
    }
}

fn first_row_y(page: &Page) -> f32 {
    // measured on the real pages: where the first row sits depends on what the page opens with
    let first = page.rows.first().map(|r| r.kind);
    match (page.name, page.parent.is_empty(), first) {
        ("System", ..) => 223.0,
        (_, true, Some(Kind::NetHero(_) | Kind::BtTiles(_) | Kind::ThemeHero(_))) => 112.0,
        (_, true, Some(Kind::UpdateHero(_))) => 117.0,
        (_, true, Some(Kind::AccountHero(_))) => 110.0,
        (_, true, Some(Kind::Header | Kind::Section)) => 128.0,
        (_, true, _) => 117.0,
        (_, false, Some(Kind::Header)) => 123.0,
        (_, false, _) => 112.0,
    }
}

/// Top of every row (before scrolling) and the y just past the last one. Cards are ROW_H tall with a
/// 4px gap; a section title takes 16px at the top of a page and 56px between cards, as in Settings.
fn row_tops(kinds: &[Kind], counts: &[usize], start: f32) -> (Vec<f32>, f32) {
    let mut y = start;
    let mut tops = Vec::new();
    for (i, k) in kinds.iter().enumerate() {
        tops.push(y);
        y += row_height(*k, i, counts) + 4.0;
    }
    (tops, y)
}

/// A card is ROW_H; a device card adds 70 per device and a 50px "pair" line; a section title is
/// 12 at the top of a page and 52 between cards.
fn row_height(k: Kind, i: usize, counts: &[usize]) -> f32 {
    match k {
        Kind::Header if i == 0 => 12.0,
        Kind::Header => 52.0,
        Kind::Section if i == 0 => 12.0,
        Kind::Section => 36.0,
        Kind::Devices(..) => ROW_H + 70.0 * counts.get(i).copied().unwrap_or(0) as f32 + 50.0,
        Kind::SortBy => 37.0,
        Kind::HomeHero(_) => 77.0,
        Kind::AccountHero(_) => 84.0,
        Kind::UpdateHero(_) => 101.0,
        Kind::When(..) => if counts.get(i).copied().unwrap_or(0) > 0 { ROW_H } else { -4.0 },
        Kind::TimeHero(_) => 88.0,
        Kind::ThemeHero(_) => 237.0,
        Kind::BtTiles(_) => 227.0, // 221 tall, 10 of air to the bar below
        Kind::Center(..) => 41.0, // 35 tall, 10 of air below
        Kind::Space(h) => h - 4.0,
        Kind::NetHero(_) => 115.0,
        // tiles are 140 tall, three to a row, 14 apart; then 52 of air before the next card
        Kind::Tiles(_) => (counts.get(i).copied().unwrap_or(0).div_ceil(3) as f32 * 154.0 - 14.0).max(0.0) + 52.0,
        // 68 for the heading, then 25 per line (a line that wraps onto more lines is counted by its newlines), 20 of padding
        Kind::Facts(_) => ROW_H + 25.0 * counts.get(i).copied().unwrap_or(0) as f32 + 20.0,
        Kind::Big(_) => 95.0,
        Kind::Toggle(..) | Kind::ToggleMore(..) | Kind::State(..) => ROW_H + 0.67, // cards holding a switch are two thirds of a pixel taller in Settings
        Kind::Note(..) => if counts.get(i).copied().unwrap_or(0) > 0 { 52.0 } else { -4.0 },
        Kind::Senders(..) | Kind::Actions(..) => (73.0 * counts.get(i).copied().unwrap_or(0) as f32 - 5.0).max(0.0),
        _ => ROW_H,
    }
}

thread_local! {
    // theme wallpapers, shrunk: width, height, pixels
    static THUMBS: RefCell<std::collections::HashMap<String, (u32, u32, Vec<u8>)>> = RefCell::new(std::collections::HashMap::new());
}
thread_local! {
    static APP_ICONS: RefCell<std::collections::HashMap<String, Vec<u8>>> = RefCell::new(std::collections::HashMap::new());
}
thread_local! {
    // how many devices each row of the open page lists (0 for ordinary rows)
    static COUNTS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}
fn set_counts(vals: &[Val]) {
    COUNTS.with(|c| *c.borrow_mut() = vals.iter().map(|v| v.list.len() + v.list.iter().map(|l| l.1.matches('\n').count()).sum::<usize>()).collect());
}

fn row_rect(page: &Page, i: usize, cw: f32) -> D2D_RECT_F {
    let kinds: Vec<Kind> = page.rows.iter().map(|r| r.kind).collect();
    let counts = COUNTS.with(|c| c.borrow().clone());
    let y = row_tops(&kinds, &counts, first_row_y(page)).0[i] - SCROLL.with(|s| s.get());
    rect(CARD_X, y, cw - 25.0, y + row_height(kinds[i], i, &counts))
}

fn slider_x(r: &D2D_RECT_F) -> (f32, f32) {
    (r.right - 216.0, r.right - 16.0)
}

/// Rows whose label or description contains the query: (page index, row label, page name).
fn search(pages: &[Page], q: &str) -> Vec<(usize, &'static str, &'static str)> {
    let q = q.to_lowercase();
    pages
        .iter()
        .enumerate()
        .flat_map(|(i, p)| p.rows.iter().filter(|r| r.label.to_lowercase().contains(&q) || r.sub.to_lowercase().contains(&q)).map(move |r| (i, r.label, p.name)))
        .take(8)
        .collect()
}

unsafe fn draw(ui: &mut Ui, cw: f32, ch: f32, maximized: bool) -> windows::core::Result<()> {
    let Some((rt, dw)) = ui.gfx.clone() else { return Ok(()) };
    let mut hits: Vec<(D2D_RECT_F, Hit)> = Vec::new();
    rt.Clear(Some(&rgb(0x202020)));
    let b = |c: u32| rt.CreateSolidColorBrush(&rgb(c), None);
    let white = b(0xFFFFFF)?;
    let dim = b(0xCFCFCF)?;
    let accent = b(0x4CC2FF)?;
    let fmt = |size: f32, weight, family: PCWSTR| -> windows::core::Result<IDWriteTextFormat> {
        let t = dw.CreateTextFormat(family, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))?;
        t.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        t.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        Ok(t)
    };
    let ui_font = w!("Segoe UI Variable Text");
    let (n, sb) = (DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD);
    let body = fmt(14.0, n, ui_font)?;
    let body_sb = fmt(14.0, sb, ui_font)?;
    let small = fmt(12.0, n, w!("Segoe UI Variable Small"))?; // the 12px optical size, a touch wider, as Settings uses
    let title = fmt(28.0, sb, w!("Segoe UI Variable Display"))?;
    let h20 = fmt(20.0, sb, w!("Segoe UI Variable Display"))?;
    let icons = fmt(16.0, n, w!("Segoe Fluent Icons"))?;
    icons.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
    let icons20 = fmt(20.0, n, w!("Segoe Fluent Icons"))?;
    icons20.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
    let icons10 = fmt(10.0, n, w!("Segoe Fluent Icons"))?;
    let icons12 = fmt(12.0, n, w!("Segoe Fluent Icons"))?;
    icons12.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
    icons10.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
    let right = fmt(14.0, n, ui_font)?;
    let header = fmt(14.0, sb, ui_font)?;
    let h18 = fmt(18.0, sb, w!("Segoe UI Variable Display"))?;
    let big = fmt(54.0, sb, w!("Segoe UI Variable Display"))?;
    let icons40 = fmt(40.0, n, w!("Segoe Fluent Icons"))?;
    right.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
    let txt = |s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, br: &ID2D1SolidColorBrush| rt.DrawText(&utf16(s), f, &r, br, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
    let ico = |cp: u32, f: &IDWriteTextFormat, r: D2D_RECT_F, br: &ID2D1SolidColorBrush| rt.DrawText(&glyph(cp), f, &r, br, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
    let hover = ui.hover;
    let set_face = settings_face(&dw);
    // a card's icon, 20px, centred in `r`
    let card_icon = |cp: u32, r: D2D_RECT_F| {
        let face = if cp & SET != 0 { set_face.as_ref() } else { None };
        let Some(face) = face else { return ico(cp & !SET, &icons20, r, &white) };
        let code = cp & !SET;
        let mut gi = 0u16;
        let mut fm = DWRITE_FONT_METRICS::default();
        face.GetMetrics(&mut fm);
        if face.GetGlyphIndices(&code, 1, &mut gi).is_err() || gi == 0 {
            return;
        }
        let size = 20.0f32;
        let (advance, offset) = (0.0f32, DWRITE_GLYPH_OFFSET::default());
        let run = DWRITE_GLYPH_RUN { fontFace: std::mem::ManuallyDrop::new(Some(face.clone())), fontEmSize: size, glyphCount: 1, glyphIndices: &gi, glyphAdvances: &advance, glyphOffsets: &offset, isSideways: false.into(), bidiLevel: 0 };
        let origin = windows_numerics::Vector2 { X: ((r.left + r.right) / 2.0 - size / 2.0).round(), Y: ((r.top + r.bottom) / 2.0 - size / 2.0).round() + size * fm.ascent as f32 / fm.designUnitsPerEm as f32 };
        rt.DrawGlyphRun(origin, &run, &white, DWRITE_MEASURING_MODE_NATURAL);
        drop(std::mem::ManuallyDrop::into_inner(run.fontFace));
    };

    PAGES.with(|pages| -> windows::core::Result<()> {
        let page = &pages[ui.page];
        let top_of = if page.parent.is_empty() { ui.page } else { page_index(page.parent).unwrap_or(ui.page) };

        // ---- title bar: back, "Settings", search, window buttons ----
        let back = rect(8.0, 9.0, 40.0, 41.0);
        let can_back = !page.parent.is_empty() || !ui.query.is_empty();
        if can_back && hover == Some(Hit::Back) {
            rt.FillRoundedRectangle(&rrect(back, 4.0), &b(0x2D2D2D)?);
        }
        ico(0xE72B, &icons10, back, &b(if can_back { 0xFFFFFF } else { 0x6B6B6B })?);
        hits.push((back, Hit::Back));
        txt("Settings", &small, rect(49.0, 8.0, 200.0, 40.0), &white);
        let sbx = rect(CARD_X, 9.0, 839.0, 39.0);
        rt.FillRoundedRectangle(&rrect(sbx, 15.0), &b(0x2D2D2D)?);
        rt.DrawRoundedRectangle(&rrect(rect(sbx.left + 0.5, sbx.top + 0.5, sbx.right - 0.5, sbx.bottom - 0.5), 15.0), &b(0x353535)?, 1.0, None);
        ico(0xE721, &icons12, rect(sbx.left + 7.0, sbx.top - 1.0, sbx.left + 31.0, sbx.bottom - 1.0), &white);
        if ui.query.is_empty() {
            txt("Find a setting", &body, rect(sbx.left + 40.0, sbx.top - 2.0, sbx.right - 12.0, sbx.bottom - 2.0), &b(0x9A9A9A)?);
        } else {
            txt(&ui.query, &body, rect(sbx.left + 40.0, sbx.top, sbx.right - 12.0, sbx.bottom), &white);
            rt.FillRectangle(&rect(sbx.left + 14.0, sbx.bottom - 1.5, sbx.right - 14.0, sbx.bottom), &accent);
        }
        for (k, cp) in [0xE921u32, if maximized { 0xE923 } else { 0xE922 }, 0xE8BB].iter().enumerate() {
            let r = rect(cw - 46.0 * (3 - k) as f32, 0.0, cw - 46.0 * (2 - k) as f32, 32.0);
            if hover == Some(Hit::Win(k as u8)) {
                rt.FillRectangle(&r, &b(if k == 2 { 0xC42B1C } else { 0x2D2D2D })?);
            }
            ico(*cp, &icons10, r, &white);
            hits.push((r, Hit::Win(k as u8)));
        }

        // ---- profile + nav ----
        let c = windows_numerics::Vector2 { X: 46.0, Y: 87.0 };
        rt.FillEllipse(&D2D1_ELLIPSE { point: c, radiusX: 30.0, radiusY: 30.0 }, &b(0xFAFAFA)?);
        rt.FillEllipse(&D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: 46.0, Y: 79.0 }, radiusX: 9.0, radiusY: 9.0 }, &b(0x8A8A8A)?);
        rt.FillRoundedRectangle(&rrect(rect(32.0, 91.0, 60.0, 103.0), 5.0), &b(0x8A8A8A)?);
        let email = account_email();
        if email.is_empty() {
            txt(&crate::launcher::display_name(), &body_sb, rect(88.0, 70.0, 310.0, 104.0), &white);
        } else {
            txt(&crate::launcher::display_name(), &body_sb, rect(88.0, 69.0, 310.0, 89.0), &white);
            txt(&email, &fmt(12.0, n, ui_font)?, rect(88.0, 87.0, 310.0, 105.0), &white);
        }
        let mut k = 0;
        for (i, p) in pages.iter().enumerate() {
            if !p.parent.is_empty() {
                continue;
            }
            let r = rect(16.0, NAV_Y + k as f32 * 40.0, 296.0, NAV_Y + 36.0 + k as f32 * 40.0);
            k += 1;
            if (i == top_of && ui.query.is_empty()) || hover == Some(Hit::Nav(i)) {
                rt.FillRoundedRectangle(&rrect(r, 4.0), &b(0x2D2D2D)?);
            }
            if i == top_of && ui.query.is_empty() {
                rt.FillRoundedRectangle(&rrect(rect(r.left, r.top + 10.0, r.left + 3.0, r.bottom - 10.0), 1.5), &accent);
            }
            ico(p.icon, &icons, rect(r.left + 8.0, r.top, r.left + 32.0, r.bottom), &white);
            txt(p.name, &body, rect(r.left + 44.0, r.top, r.right - 8.0, r.bottom), &white);
            hits.push((r, Hit::Nav(i)));
        }

        // ---- search results ----
        if !ui.query.is_empty() {
            txt(&format!("Results for \u{201C}{}\u{201D}", ui.query), &title, rect(CARD_X, 49.0, cw - 40.0, 97.0), &white);
            let res = search(pages, &ui.query);
            if res.is_empty() {
                txt("No settings match. Press Esc to clear.", &body, rect(CARD_X, 123.0, cw - 25.0, 150.0), &dim);
            }
            for (j, (pi, label, pname)) in res.iter().enumerate() {
                let y = 123.0 + j as f32 * (ROW_H + 4.0);
                let r = rect(CARD_X, y, cw - 25.0, y + ROW_H);
                rt.FillRoundedRectangle(&rrect(r, 4.0), &b(if hover == Some(Hit::Result(*pi)) { 0x323232 } else { 0x2B2B2B })?);
                txt(label, &body, rect(r.left + 20.0, r.top + 12.0, r.right - 60.0, r.top + 34.0), &white);
                txt(pname, &small, rect(r.left + 20.0, r.top + 34.0, r.right - 60.0, r.top + 54.0), &dim);
                ico(0xE76C, &icons12, rect(r.right - 36.0, r.top, r.right - 12.0, r.bottom), &white);
                hits.push((r, Hit::Result(*pi)));
            }
            ui.hits = hits;
            return Ok(());
        }

        // ---- page title (breadcrumb on sub-pages) ----
        if page.parent.is_empty() {
            txt(page.name, &title, rect(CARD_X, 49.0, cw - 40.0, 97.0), &white);
        } else {
            let l = dw.CreateTextLayout(&utf16(page.parent), &title, 600.0, 48.0)?;
            let mut m = DWRITE_TEXT_METRICS::default();
            l.GetMetrics(&mut m)?;
            txt(page.parent, &title, rect(CARD_X, 49.0, cw - 40.0, 97.0), &dim);
            ico(0xE76C, &icons, rect(CARD_X + m.width + 6.0, 53.0, CARD_X + m.width + 34.0, 97.0), &dim);
            txt(page.name, &title, rect(CARD_X + m.width + 40.0, 49.0, cw - 40.0, 97.0), &white);
        }
        if page.name == "System" {
            rt.FillRoundedRectangle(&rrect(rect(324.0, 123.0, 444.0, 191.0), 4.0), &b(0x000000)?);
            txt(&device_name().to_lowercase(), &h20, rect(460.0, 129.0, 800.0, 153.0), &white);
            txt(&model(), &body, rect(460.0, 152.0, 800.0, 172.0), &white);
            let rr = rect(461.0, 171.0, 521.0, 191.0);
            txt("Rename", &body, rr, &b(if hover == Some(Hit::Rename) { 0xB5F0FF } else { 0x99EBFF })?);
            hits.push((rr, Hit::Rename));
        }

        // ---- cards ----
        let clip_top = first_row_y(page) - 18.0;
        // measured: sub-pages sit their icons and text 2px further in; top-level card lists sit them 1px higher
        let dx = if page.parent.is_empty() { 0.0 } else { 2.0 };
        let dy = if page.parent.is_empty() && matches!(first_row_y(page) as i32, 117 | 128 | 223) && !matches!(page.rows.first().map(|r| r.kind), Some(Kind::UpdateHero(_))) { -1.0 } else { 0.0 };

        rt.PushAxisAlignedClip(&rect(NAV, clip_top, cw, ch), D2D1_ANTIALIAS_MODE_ALIASED);
        for (i, row) in page.rows.iter().enumerate() {
            // Settings lays rows out on fractions of a pixel: boxes snap to the nearest pixel, text snaps down
            let rf = row_rect(page, i, cw);
            let r = rect(rf.left, rf.top.round(), rf.right, rf.bottom.round());
            let dy = dy + rf.top.floor() - rf.top.round();
            if r.top > ch || r.bottom < clip_top {
                continue;
            }
            if matches!(row.kind, Kind::Header | Kind::Section) {
                // the title sits 17px above the next card
                txt(row.label, &header, rect(r.left + 1.0, r.bottom - 26.0, r.right, r.bottom - 4.0), &white);
                continue;
            }
            if let Kind::Big(_) = row.kind {
                let v = ui.vals.get(i).cloned().unwrap_or_default();
                let l = dw.CreateTextLayout(&utf16(&v.s), &big, 600.0, 80.0)?;
                let mut m = DWRITE_TEXT_METRICS::default();
                l.GetMetrics(&mut m)?;
                txt(&v.s, &big, rect(r.left - 2.0, r.top, r.left + 600.0, r.top + 78.0), &white);
                let level: u32 = v.s.trim_end_matches('%').parse().unwrap_or(0);
                ico(if v.on { 0xEBAB } else { 0xEBA0 } + (level + 5) / 10, &icons40, rect(r.left + m.width + 14.0, r.top + 6.0, r.left + m.width + 74.0, r.top + 78.0), &white);
                continue;
            }
            if let Kind::Note(_, text) = row.kind {
                if r.bottom - r.top > 10.0 {
                    let c = rect(r.left, r.top, r.right, r.top + 49.0);
                    rt.FillRoundedRectangle(&rrect(c, 4.0), &b(0x272727)?);
                    rt.DrawRoundedRectangle(&rrect(rect(c.left + 0.5, c.top + 0.5, c.right - 0.5, c.bottom - 0.5), 4.0), &b(0x1D1D1D)?, 1.0, None);
                    ico(0xF167, &icons, rect(c.left + 12.0, c.top, c.left + 36.0, c.bottom), &accent);
                    txt(text, &body, rect(c.left + 46.0, c.top, c.right - 20.0, c.bottom), &white);
                }
                continue;
            }
            if let Kind::Tiles(_) = row.kind {
                let list = ui.vals.get(i).map(|v| v.list.clone()).unwrap_or_default();
                let wrap = fmt(16.0, sb, ui_font)?;
                wrap.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
                wrap.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
                for (k, (label, value, _, foot)) in list.iter().enumerate() {
                    let (x, y) = (r.left + (k % 3) as f32 * 212.0, r.top + (k / 3) as f32 * 154.0);
                    let c = rect(x, y, x + 198.0, y + 140.0);
                    rt.FillRoundedRectangle(&rrect(c, 6.0), &b(0x2B2B2B)?);
                    ico([0xE950, 0xE964, 0xE7F4, 0xEDA2][k.min(3)], &icons10, rect(x + 14.0, y + 12.0, x + 30.0, y + 32.0), &dim);
                    txt(label, &small, rect(x + 36.0, y + 12.0, x + 190.0, y + 32.0), &dim);
                    txt(value, &wrap, rect(x + 16.0, y + 38.0, x + 186.0, y + 100.0), &white);
                    txt(foot, &small, rect(x + 16.0, y + 106.0, x + 190.0, y + 128.0), &dim);
                }
                continue;
            }
            if matches!(row.kind, Kind::RenamePc) {
                rt.FillRoundedRectangle(&rrect(r, 4.0), &b(0x2B2B2B)?);
                rt.DrawRoundedRectangle(&rrect(rect(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5), 4.0), &b(0x1D1D1D)?, 1.0, None);
                txt(&device_name().to_lowercase(), &body, rect(r.left + 16.0, r.top + 15.0, r.right - 200.0, r.top + 37.0), &white);
                txt(&model(), &small, rect(r.left + 16.0, r.top + 35.0, r.right - 200.0, r.top + 53.0), &dim);
                let br = rect(r.right - 146.0, r.top + 18.0, r.right - 16.0, r.top + 50.0);
                rt.FillRoundedRectangle(&rrect(br, 4.0), &b(if hover == Some(Hit::Rename) { 0x3D3D3D } else { 0x373737 })?);
                let centered = fmt(14.0, n, ui_font)?;
                centered.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                txt("Rename this PC", &centered, br, &white);
                hits.push((br, Hit::Rename));
                continue;
            }
            if let Kind::Facts(_) = row.kind {
                let list = ui.vals.get(i).map(|v| v.list.clone()).unwrap_or_default();
                rt.FillRoundedRectangle(&rrect(r, 4.0), &b(0x272727)?);
                rt.FillRoundedRectangle(&rrect(rect(r.left, r.top, r.right, r.top + ROW_H), 4.0), &b(0x2B2B2B)?);
                rt.DrawRoundedRectangle(&rrect(rect(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5), 4.0), &b(0x1D1D1D)?, 1.0, None);
                rt.FillRectangle(&rect(r.left, r.top + ROW_H, r.right, r.top + ROW_H + 1.0), &b(0x1D1D1D)?);
                card_icon(row.icon, rect(r.left + 14.0 + dx, r.top + 1.0 + dy, r.left + 42.0 + dx, r.top + ROW_H + 1.0 + dy));
                txt(row.label, &body, rect(r.left + 58.0, r.top, r.right - 200.0, r.top + ROW_H), &white);
                let br = rect(r.right - 134.0, r.top + 18.0, r.right - 44.0, r.top + 50.0);
                rt.FillRoundedRectangle(&rrect(br, 4.0), &b(if hover == Some(Hit::Copy(i)) { 0x3D3D3D } else { 0x373737 })?);
                let centered = fmt(14.0, n, ui_font)?;
                centered.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                txt("Copy", &centered, br, &white);
                hits.push((br, Hit::Copy(i)));
                ico(0xE70E, &icons10, rect(r.right - 36.0, r.top, r.right - 12.0, r.top + ROW_H), &white);
                let top_align = fmt(14.0, n, ui_font)?;
                top_align.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
                let mut y = r.top + ROW_H + 9.0;
                for (key, value, _, _) in &list {
                    let lines = 1 + value.matches('\n').count();
                    txt(key, &top_align, rect(r.left + 58.0, y, r.left + 170.0, y + 22.0), &white);
                    for (k, part) in value.split('\n').enumerate() {
                        txt(part, &top_align, rect(r.left + 172.0, y + 19.0 * k as f32, r.right - 20.0, y + 19.0 * k as f32 + 22.0), &dim);
                    }
                    y += 25.0 + 19.0 * (lines - 1) as f32;
                }
                continue;
            }
            if let Kind::NetHero(_) = row.kind {
                let v = ui.vals.get(i).cloned().unwrap_or_default();
                let up = !v.s.is_empty();
                let big_icon = fmt(72.0, n, w!("Segoe Fluent Icons"))?;
                ico(if !up { 0xF384 } else if v.on { 0xE839 } else { 0xE701 }, &big_icon, rect(r.left - 4.0, r.top - 6.0, r.left + 96.0, r.top + 94.0), &b(0x0078D4)?);
                txt(if up { &v.s } else { "Not connected" }, &h20, rect(r.left + 114.0, r.top + 18.0, r.left + 400.0, r.top + 46.0), &white);
                if up {
                    ico(0xE774, &icons12, rect(r.left + 112.0, r.top + 48.0, r.left + 132.0, r.top + 68.0), &b(0x4CC2FF)?);
                    txt("Connected", &body, rect(r.left + 134.0, r.top + 46.0, r.left + 400.0, r.top + 70.0), &dim);
                }
                for (x, glyph_, label, uri) in [(418.0, 0xE946, "Properties", if v.on { "ms-settings:network-ethernet" } else { "ms-settings:network-wifi" }), (636.0, 0xEB05, "Data usage", "ms-settings:datausage")] {
                    let hr = rect(r.left + x - 8.0, r.top + 14.0, r.left + x + 190.0, r.top + 74.0);
                    if hover == Some(Hit::Open(uri)) {
                        rt.FillRoundedRectangle(&rrect(hr, 4.0), &b(0x2B2B2B)?);
                    }
                    ico(glyph_, &icons, rect(r.left + x, r.top + 30.0, r.left + x + 24.0, r.top + 58.0), &white);
                    txt(label, &body_sb, rect(r.left + x + 32.0, r.top + 30.0, r.left + x + 190.0, r.top + 58.0), &white);
                    hits.push((hr, Hit::Open(uri)));
                }
                ico(0xE76C, &icons12, rect(r.right - 30.0, r.top + 30.0, r.right - 6.0, r.top + 58.0), &white);
                continue;
            }
            if let Kind::BtTiles(_) = row.kind {
                let list = ui.vals.get(i).map(|v| v.list.clone()).unwrap_or_default();
                let centered = fmt(14.0, n, ui_font)?;
                centered.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                let centered_sb = fmt(14.0, sb, ui_font)?;
                centered_sb.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                let big_icon = fmt(44.0, n, w!("Segoe Fluent Icons"))?;
                big_icon.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                // as many tiles as fit; the last one is always "Add device"
                let fit = (((r.right - r.left + 6.0) / 224.0).floor() as usize).max(1);
                let shown = list.len().min(fit - 1);
                for k in 0..=shown {
                    let x = r.left + k as f32 * 224.0;
                    let c = rect(x, r.top, x + 218.0, r.top + 221.0);
                    rt.FillRoundedRectangle(&rrect(c, 6.0), &b(0x2B2B2B)?);
                    rt.DrawRoundedRectangle(&rrect(rect(c.left + 0.5, c.top + 0.5, c.right - 0.5, c.bottom - 0.5), 6.0), &b(0x1D1D1D)?, 1.0, None);
                    if k == shown {
                        ico(0xE710, &big_icon, rect(c.left, c.top + 60.0, c.right, c.top + 130.0), &white);
                        txt("Add device", &centered_sb, rect(c.left, c.top + 128.0, c.right, c.top + 150.0), &white);
                        hits.push((c, Hit::Open("ms-settings-connectabledevices:devicediscovery")));
                        continue;
                    }
                    let (name, _, on, _) = &list[k];
                    ico(0xE712, &icons, rect(c.right - 40.0, c.top + 8.0, c.right - 10.0, c.top + 32.0), &white);
                    ico(0xE702, &big_icon, rect(c.left, c.top + 28.0, c.right, c.top + 92.0), &white);
                    txt(name, &centered_sb, rect(c.left + 8.0, c.top + 104.0, c.right - 8.0, c.top + 126.0), &white);
                    txt(if *on { "Connected" } else { "Not connected" }, &centered, rect(c.left + 8.0, c.top + 123.0, c.right - 8.0, c.top + 145.0), &dim);
                    if !*on {
                        let br = rect(c.left + 71.0, c.top + 153.0, c.left + 147.0, c.top + 185.0);
                        rt.FillRoundedRectangle(&rrect(br, 4.0), &b(if hover == Some(Hit::Tile(k)) { 0x3D3D3D } else { 0x373737 })?);
                        txt("Connect", &centered, br, &white);
                        hits.push((br, Hit::Tile(k)));
                    }
                }
                continue;
            }
            if let Kind::Center(text, uri) = row.kind {
                let r = rect(r.left, r.top, r.right, r.top + 35.0);
                rt.FillRoundedRectangle(&rrect(r, 4.0), &b(if hover == Some(Hit::Open(uri)) { 0x323232 } else { 0x2B2B2B })?);
                rt.DrawRoundedRectangle(&rrect(rect(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5), 4.0), &b(0x1D1D1D)?, 1.0, None);
                let centered = fmt(14.0, n, ui_font)?;
                centered.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                txt(text, &centered, r, &b(0x99EBFF)?);
                hits.push((r, Hit::Open(uri)));
                continue;
            }
            if matches!(row.kind, Kind::Space(_)) {
                continue;
            }
            if let Kind::ThemeHero(_) = row.kind {
                let list = ui.vals.get(i).map(|v| v.list.clone()).unwrap_or_default();
                // the desktop preview: a small window on your background
                let pv = rect(r.left - 1.0, r.top + 41.0, r.left + 249.0, r.top + 181.0);
                rt.FillRoundedRectangle(&rrect(pv, 8.0), &b(0x000000)?);
                let win = rect(pv.left + 137.0, pv.top + 16.0, pv.left + 233.0, pv.top + 111.0);
                rt.FillRectangle(&win, &b(0x2B2B2B)?);
                for k in 0..4 {
                    let y = win.top + 11.0 + 9.0 * k as f32;
                    rt.FillRectangle(&rect(win.left + 12.0, y, win.right - if k == 3 { 42.0 } else { 12.0 }, y + 1.0), &b(0xFFFFFF)?);
                }
                rt.FillRoundedRectangle(&rrect(rect(win.right - 36.0, win.bottom - 22.0, win.right - 12.0, win.bottom - 14.0), 2.0), &accent);
                txt("Select a theme to apply", &body_sb, rect(r.left + 297.0, r.top - 4.0, r.right, r.top + 20.0), &white);
                let bp = D2D1_BITMAP_PROPERTIES { pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }, dpiX: 96.0, dpiY: 96.0 };
                for (k, (_, wall, current, tone)) in list.iter().take(6).enumerate() {
                    let (x, y) = (r.left + 300.0 + (k % 3) as f32 * 120.0, r.top + 32.0 + (k / 3) as f32 * 98.0);
                    let c = rect(x, y, x + 112.0, y + 90.0);
                    let (w_, h_, px) = THUMBS.with(|m| m.borrow_mut().entry(wall.clone()).or_insert_with(|| crate::files::image(wall, 224, windows::Win32::UI::Shell::SIIGBF_THUMBNAILONLY).unwrap_or_default()).clone());
                    if w_ > 0 && px.len() == (w_ * h_ * 4) as usize {
                        if let Ok(bm) = rt.CreateBitmap(D2D_SIZE_U { width: w_, height: h_ }, Some(px.as_ptr() as *const c_void), w_ * 4, &bp) {
                            // the real page squeezes the whole picture into the tile
                            let src = rect(0.0, 0.0, w_ as f32, h_ as f32);
                            rt.DrawBitmap(&bm, Some(&c), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, Some(&src));
                        }
                    } else {
                        rt.FillRoundedRectangle(&rrect(c, 4.0), &b(0x2B2B2B)?);
                    }
                    // the little window each theme shows, in that theme's light or dark tone
                    let mock = rect(c.left + 66.0, c.top + 44.0, c.left + 108.0, c.top + 86.0);
                    rt.FillRoundedRectangle(&rrect(mock, 4.0), &b(if tone.starts_with("dark") { 0x1F2A44 } else { 0xE9EEF6 })?);
                    let chip = tone.split_once(':').and_then(|t| u32::from_str_radix(t.1, 16).ok()).unwrap_or(0x0078D4); // the theme's own accent colour
                    rt.FillRoundedRectangle(&rrect(rect(mock.left + 9.0, mock.bottom - 15.0, mock.right - 9.0, mock.bottom - 8.0), 2.0), &b(chip)?);
                    if *current || hover == Some(Hit::Theme(k)) {
                        rt.DrawRoundedRectangle(&rrect(rect(c.left - 1.0, c.top - 1.0, c.right + 1.0, c.bottom + 1.0), 4.0), &accent, 2.0, None);
                    }
                    hits.push((c, Hit::Theme(k)));
                }
                continue;
            }
            if let Kind::TimeHero(_) = row.kind {
                let list = ui.vals.get(i).map(|v| v.list.clone()).unwrap_or_default();
                if let Some((time, date, _, _)) = list.first() {
                    let clock = fmt(40.0, sb, w!("Segoe UI Variable Display"))?;
                    txt(time, &clock, rect(r.left - 2.0, r.top - 6.0, r.left + 300.0, r.top + 46.0), &white);
                    txt(date, &body, rect(r.left, r.top + 51.0, r.left + 380.0, r.top + 73.0), &dim);
                }
                for (k, (label, value, _, _)) in list.iter().enumerate().skip(1) {
                    let x = r.left + if k == 1 { 400.0 } else { 646.0 };
                    ico(if k == 1 { 0xE121 } else { 0xE774 }, &icons, rect(x, r.top + 11.0, x + 24.0, r.top + 39.0), &white);
                    txt(label, &body_sb, rect(x + 36.0, r.top + 5.0, x + 240.0, r.top + 27.0), &white);
                    txt(value, &small, rect(x + 36.0, r.top + 26.0, x + 240.0, r.top + 44.0), &white);
                }
                continue;
            }
            if let Kind::UpdateHero(_) = row.kind {
                let v = ui.vals.get(i).cloned().unwrap_or_default();
                let big_icon = fmt(64.0, n, w!("Segoe Fluent Icons"))?;
                big_icon.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                ico(0xE895, &big_icon, rect(r.left, r.top - 2.0, r.left + 96.0, r.top + 86.0), &b(0x0078D4)?);
                txt(if v.on { "Restart required" } else { "You're up to date" }, &h18, rect(r.left + 114.0, r.top + 13.0, r.right - 200.0, r.top + 41.0), &white);
                txt(if v.on { "Your device will restart outside of active hours." } else { "Windows checks for updates on its own." }, &body, rect(r.left + 114.0, r.top + 38.0, r.right - 200.0, r.top + 60.0), &dim);
                let label = if v.on { "Restart now" } else { "Check for updates" };
                let l = dw.CreateTextLayout(&utf16(label), &body, 400.0, 32.0)?;
                let mut m = DWRITE_TEXT_METRICS::default();
                l.GetMetrics(&mut m)?;
                let br = rect(r.right - m.width.ceil() - 24.0, r.top + 27.0, r.right, r.top + 59.0);
                // ponytail: the button opens Windows' own Update page; restarting or scanning from here comes later
                rt.FillRoundedRectangle(&rrect(br, 4.0), &b(if hover == Some(Hit::Open("ms-settings:windowsupdate-action")) { 0x47B1E8 } else { 0x4CC2FF })?);
                let centered = fmt(14.0, n, ui_font)?;
                centered.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                txt(label, &centered, br, &b(0x000000)?);
                hits.push((br, Hit::Open("ms-settings:windowsupdate-action")));
                continue;
            }
            if let Kind::When(_, uri) = row.kind {
                if r.bottom - r.top < 10.0 {
                    continue;
                }
                rt.FillRoundedRectangle(&rrect(r, 4.0), &b(if hover == Some(Hit::Open(uri)) { 0x323232 } else { 0x2B2B2B })?);
                rt.DrawRoundedRectangle(&rrect(rect(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5), 4.0), &b(0x1D1D1D)?, 1.0, None);
                txt(row.label, &body, rect(r.left + 16.0, r.top + 15.0, r.right - 60.0, r.top + 37.0), &white);
                txt(row.sub, &small, rect(r.left + 16.0, r.top + 35.0, r.right - 60.0, r.top + 53.0), &dim);
                ico(0xE70D, &icons10, rect(r.right - 36.0, r.top, r.right - 12.0, r.bottom), &white);
                hits.push((r, Hit::Open(uri)));
                continue;
            }
            if let Kind::HomeHero(_) = row.kind {
                let list = ui.vals.get(i).map(|v| v.list.clone()).unwrap_or_default();
                rt.FillRoundedRectangle(&rrect(rect(r.left, r.top + 2.0, r.left + 120.0, r.top + 70.0), 4.0), &b(0x000000)?);
                if let Some((name, model_, _, _)) = list.first() {
                    txt(name, &h20, rect(r.left + 136.0, r.top + 6.0, r.left + 420.0, r.top + 32.0), &white);
                    txt(model_, &body, rect(r.left + 136.0, r.top + 30.0, r.left + 420.0, r.top + 50.0), &white);
                    let rr = rect(r.left + 136.0, r.top + 49.0, r.left + 196.0, r.top + 69.0);
                    txt("Rename", &body, rr, &b(if hover == Some(Hit::Rename) { 0xB5F0FF } else { 0x99EBFF })?);
                    hits.push((rr, Hit::Rename));
                }
                for (k, (title_, state, _, _)) in list.iter().enumerate().skip(1) {
                    let x = r.left + if k == 1 { 460.0 } else { 610.0 };
                    let uri = if k == 1 { "ms-settings:network" } else { "ms-settings:windowsupdate" };
                    ico(if k == 1 { 0xE839 } else { 0xE895 }, &icons20, rect(x, r.top + 16.0, x + 28.0, r.top + 52.0), &b(0x4CC2FF)?);
                    txt(title_, &body_sb, rect(x + 38.0, r.top + 14.0, x + 240.0, r.top + 36.0), &white);
                    txt(state, &small, rect(x + 38.0, r.top + 34.0, x + 240.0, r.top + 52.0), &dim);
                    hits.push((rect(x, r.top + 8.0, x + 140.0, r.top + 60.0), Hit::Open(uri)));
                }
                continue;
            }
            if let Kind::AccountHero(_) = row.kind {
                if let Some((name, email, _, kind_)) = ui.vals.get(i).and_then(|v| v.list.first().cloned()) {
                    let c = windows_numerics::Vector2 { X: r.left + 49.0, Y: r.top + 49.0 };
                    rt.FillEllipse(&D2D1_ELLIPSE { point: c, radiusX: 48.0, radiusY: 48.0 }, &b(0xFAFAFA)?);
                    rt.FillEllipse(&D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: c.X, Y: c.Y - 12.0 }, radiusX: 15.0, radiusY: 15.0 }, &b(0x8A8A8A)?);
                    rt.FillRoundedRectangle(&rrect(rect(c.X - 23.0, c.Y + 7.0, c.X + 23.0, c.Y + 27.0), 8.0), &b(0x8A8A8A)?);
                    txt(&name.to_uppercase(), &h18, rect(r.left + 110.0, r.top + 14.0, r.right, r.top + 40.0), &white);
                    txt(&email, &body, rect(r.left + 110.0, r.top + 37.0, r.right, r.top + 57.0), &white);
                    txt(&kind_, &body, rect(r.left + 110.0, r.top + 56.0, r.right, r.top + 76.0), &white);
                }
                continue;
            }
            if matches!(row.kind, Kind::SortBy) {
                txt(row.label, &body, rect(r.left, r.top, r.left + 60.0, r.top + 30.0), &dim);
                let bx = rect(r.left + 53.0, r.top, r.left + 183.0, r.top + 30.0);
                rt.FillRoundedRectangle(&rrect(bx, 4.0), &b(if hover == Some(Hit::Sort) { 0x3D3D3D } else { 0x2D2D2D })?);
                txt(if BY_NAME.load(Relaxed) { "Name" } else { "Most recent" }, &body, rect(bx.left + 12.0, bx.top, bx.right - 30.0, bx.bottom), &white);
                ico(0xE70D, &icons10, rect(bx.right - 32.0, bx.top, bx.right - 8.0, bx.bottom), &dim);
                hits.push((bx, Hit::Sort));
                continue;
            }
            if let Kind::Senders(..) | Kind::Actions(..) = row.kind {
                let button = if let Kind::Actions(_, label, _) = row.kind { Some(label) } else { None };
                let list = ui.vals.get(i).map(|v| v.list.clone()).unwrap_or_default();
                let bp = D2D1_BITMAP_PROPERTIES { pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }, dpiX: 96.0, dpiY: 96.0 };
                for (k, (name, what, on, id)) in list.iter().enumerate() {
                    let c = rect(r.left, r.top + 73.0 * k as f32, r.right, r.top + 73.0 * k as f32 + ROW_H);
                    if c.top > ch || c.bottom < clip_top {
                        continue;
                    }
                    rt.FillRoundedRectangle(&rrect(c, 4.0), &b(if hover == Some(Hit::Sender(i, k)) { 0x323232 } else { 0x2B2B2B })?);
                    rt.DrawRoundedRectangle(&rrect(rect(c.left + 0.5, c.top + 0.5, c.right - 0.5, c.bottom - 0.5), 4.0), &b(0x1D1D1D)?, 1.0, None);
                    let px = APP_ICONS.with(|m| m.borrow_mut().entry(id.clone()).or_insert_with(|| crate::files::image(&icon_source(id), 24, windows::Win32::UI::Shell::SIIGBF_ICONONLY).filter(|i| i.0 == 24 && i.1 == 24).map(|i| i.2).unwrap_or_default()).clone());
                    if px.len() == 24 * 24 * 4 {
                        if let Ok(bm) = rt.CreateBitmap(D2D_SIZE_U { width: 24, height: 24 }, Some(px.as_ptr() as *const c_void), 96, &bp) {
                            crate::flyout::draw_icon(&rt, &bm, c.left + 16.0, c.top + 22.0, 24.0);
                        }
                    }
                    txt(name, &body, rect(c.left + 58.0, c.top + 15.0, c.right - 140.0, c.top + 37.0), &white);
                    txt(what, &small, rect(c.left + 58.0, c.top + 35.0, c.right - 140.0, c.top + 53.0), &dim);
                    if let Some(label) = button {
                        // a quiet button at the right, like the buttons on Windows' own cards
                        let bx = rect(c.right - 112.0, c.top + 18.0, c.right - 16.0, c.top + 50.0);
                        rt.FillRoundedRectangle(&rrect(bx, 4.0), &b(if hover == Some(Hit::Sender(i, k)) { 0x3D3D3D } else { 0x373737 })?);
                        rt.DrawRoundedRectangle(&rrect(rect(bx.left + 0.5, bx.top + 0.5, bx.right - 0.5, bx.bottom - 0.5), 4.0), &b(0x424242)?, 1.0, None);
                        let mid = dw.CreateTextFormat(w!("Segoe UI Variable Text"), None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 14.0, w!(""))?;
                        mid.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                        mid.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                        txt(label, &mid, bx, &white);
                        hits.push((bx, Hit::Sender(i, k)));
                        continue;
                    }
                    txt(if *on { "On" } else { "Off" }, &right, rect(c.right - 190.0, c.top, c.right - 96.0, c.bottom), &white);
                    crate::quick::switch(&rt, c.right - 84.0, c.top + 24.0, *on)?;
                    ico(0xE76C, &icons12, rect(c.right - 36.0, c.top, c.right - 12.0, c.bottom), &white);
                    hits.push((c, Hit::Sender(i, k)));
                }
                continue;
            }
            let clickable = matches!(row.kind, Kind::Link(_) | Kind::LinkBtn(..) | Kind::Expand(_) | Kind::Sub(_) | Kind::Choice(..) | Kind::Flat(..) | Kind::State(..));
            let hov = hover == Some(Hit::Row(i)) && clickable;
            rt.FillRoundedRectangle(&rrect(r, 4.0), &b(if hov { 0x323232 } else { 0x2B2B2B })?);
            rt.DrawRoundedRectangle(&rrect(rect(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5), 4.0), &b(0x1D1D1D)?, 1.0, None);
            let tx = if row.icon != 0 {
                card_icon(row.icon, rect(r.left + 14.0 + dx, r.top + 1.0 + dy, r.left + 42.0 + dx, r.top + ROW_H + 1.0 + dy));
                r.left + 58.0 + dx
            } else {
                r.left + 16.0
            };
            if row.sub.is_empty() {
                txt(row.label, &body, rect(tx, r.top + dy, r.right - 260.0, r.top + ROW_H + dy), &white);
            } else {
                txt(row.label, &body, rect(tx, r.top + 15.0 + dy, r.right - 60.0, r.top + 37.0 + dy), &white);
                txt(row.sub, &small, rect(tx, r.top + 35.0 + dy, r.right - 60.0, r.top + 53.0 + dy), &dim);
            }
            let v = ui.vals.get(i).cloned().unwrap_or_default();
            let loaded = i < ui.vals.len();
            match row.kind {
                Kind::Toggle(..) => {
                    txt(if v.on { "On" } else { "Off" }, &right, rect(r.right - 160.0, r.top, r.right - 71.0, r.bottom), &white);
                    crate::quick::switch(&rt, r.right - 59.0, r.top + 24.0, v.on)?;
                }
                Kind::Slider(..) => {
                    let (x0, x1) = slider_x(&r);
                    let y = (r.top + r.bottom) / 2.0;
                    let x = x0 + (x1 - x0) * v.f.clamp(0.0, 1.0);
                    rt.FillRoundedRectangle(&rrect(rect(x0, y - 2.0, x1, y + 2.0), 2.0), &b(0x9E9E9E)?);
                    rt.FillRoundedRectangle(&rrect(rect(x0, y - 2.0, x, y + 2.0), 2.0), &accent);
                    let pt = windows_numerics::Vector2 { X: x, Y: y };
                    rt.FillEllipse(&D2D1_ELLIPSE { point: pt, radiusX: 10.0, radiusY: 10.0 }, &b(0x454545)?);
                    rt.FillEllipse(&D2D1_ELLIPSE { point: pt, radiusX: 6.0, radiusY: 6.0 }, &accent);
                    txt(&format!("{:.0}", v.f * 100.0), &right, rect(x0 - 60.0, r.top, x0 - 8.0, r.bottom), &white);
                    if row.label == "Volume" {
                        // the speaker beside the number mutes, like the real page
                        let mr = rect(x0 - 62.0, r.top + 18.0, x0 - 30.0, r.top + 50.0);
                        ico(if v.on { 0xE74F } else { 0xE767 }, &icons, mr, &white);
                        hits.push((r, Hit::Row(i)));
                        hits.push((mr, Hit::Mute));
                        continue;
                    }
                }
                Kind::Info(_) => txt(if loaded { &v.s } else { "\u{2026}" }, &right, rect(r.left + 260.0, r.top, r.right - 24.0, r.bottom), &dim),
                Kind::Link(_) | Kind::Sub(_) => ico(0xE76C, &icons12, rect(r.right - 36.0, r.top, r.right - 12.0, r.bottom), &white),
                Kind::Expand(_) => ico(0xE70D, &icons10, rect(r.right - 36.0, r.top, r.right - 12.0, r.bottom), &white),
                Kind::State(..) => {
                    txt(if v.on { "On" } else { "Off" }, &right, rect(r.right - 190.0, r.top, r.right - 96.0, r.bottom), &white);
                    crate::quick::switch(&rt, r.right - 84.0, r.top + 24.0, v.on)?;
                    ico(0xE76C, &icons12, rect(r.right - 36.0, r.top, r.right - 12.0, r.bottom), &white);
                }
                Kind::Choice(..) => {
                    let value = if loaded { v.s.as_str() } else { "\u{2026}" };
                    let l = dw.CreateTextLayout(&utf16(value), &body, 600.0, 32.0)?;
                    let mut m = DWRITE_TEXT_METRICS::default();
                    l.GetMetrics(&mut m)?;
                    let bx = rect(r.right - 16.0 - (m.width + 12.0 + 44.0).ceil(), r.top + 18.0, r.right - 16.0, r.top + 50.0);
                    rt.FillRoundedRectangle(&rrect(bx, 4.0), &b(if hov { 0x3D3D3D } else { 0x373737 })?);
                    txt(value, &body, rect(bx.left + 12.0, bx.top, bx.right - 32.0, bx.bottom), &white);
                    ico(0xE70D, &icons10, rect(bx.right - 34.0, bx.top, bx.right - 10.0, bx.bottom), &dim);
                }
                Kind::Flat(..) => {
                    txt(if loaded { &v.s } else { "\u{2026}" }, &right, rect(r.left + 300.0, r.top, r.right - 45.0, r.bottom), &dim);
                    ico(0xE70D, &icons10, rect(r.right - 36.0, r.top, r.right - 12.0, r.bottom), &white);
                }
                Kind::ToggleMore(..) => {
                    txt(if v.on { "On" } else { "Off" }, &right, rect(r.right - 190.0, r.top, r.right - 96.0, r.bottom), &white);
                    crate::quick::switch(&rt, r.right - 84.0, r.top + 24.0, v.on)?;
                    let cr = rect(r.right - 40.0, r.top, r.right, r.bottom);
                    ico(0xE76C, &icons12, rect(r.right - 36.0, r.top, r.right - 12.0, r.bottom), &white);
                    if let Kind::ToggleMore(_, _, uri) = row.kind {
                        hits.push((r, Hit::Row(i)));
                        hits.push((cr, Hit::Open(uri)));
                        continue;
                    }
                }
                Kind::LinkBtn(_, text, uri) => {
                    ico(0xE76C, &icons12, rect(r.right - 36.0, r.top, r.right - 12.0, r.bottom), &white);
                    let br = rect(r.right - 174.0, r.top + 18.0, r.right - 44.0, r.top + 50.0);
                    rt.FillRoundedRectangle(&rrect(br, 4.0), &b(if hover == Some(Hit::Open(uri)) { 0x47B1E8 } else { 0x4CC2FF })?);
                    let centered = fmt(14.0, n, ui_font)?;
                    centered.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                    txt(text, &centered, br, &b(0x000000)?);
                    hits.push((r, Hit::Row(i)));
                    hits.push((br, Hit::Open(uri)));
                    continue;
                }
                Kind::HomeHero(_) | Kind::AccountHero(_) | Kind::UpdateHero(_) | Kind::When(..) | Kind::TimeHero(_) | Kind::ThemeHero(_) | Kind::NetHero(_) | Kind::BtTiles(_) | Kind::Center(..) | Kind::Space(_) => {}
                Kind::Header | Kind::Section | Kind::SortBy | Kind::Senders(..) | Kind::Actions(..) | Kind::Big(_) | Kind::Note(..) | Kind::Tiles(_) | Kind::Facts(_) | Kind::RenamePc => {}
                Kind::Devices(_, pair) => {
                    let line = b(0x1D1D1D)?;
                    let inner = b(0x272727)?;
                    let current = v.list.iter().find(|d| d.2).map(|d| d.0.as_str()).unwrap_or("");
                    txt(current, &right, rect(r.left + 300.0, r.top, r.right - 45.0, r.top + ROW_H), &dim);
                    ico(0xE70E, &icons10, rect(r.right - 36.0, r.top, r.right - 12.0, r.top + ROW_H), &white);
                    let mut y = r.top + ROW_H;
                    for (k, (name, adapter, on, _)) in v.list.iter().enumerate() {
                        let dr = rect(r.left + 1.0, y, r.right - 1.0, y + 70.0);
                        rt.FillRectangle(&dr, &b(if hover == Some(Hit::Radio(i, k)) { 0x2F2F2F } else { 0x272727 })?);
                        rt.FillRectangle(&rect(r.left, y, r.right, y + 1.0), &line);
                        let c = windows_numerics::Vector2 { X: r.left + 68.0, Y: y + 26.0 };
                        if *on {
                            rt.FillEllipse(&D2D1_ELLIPSE { point: c, radiusX: 9.5, radiusY: 9.5 }, &accent);
                            rt.FillEllipse(&D2D1_ELLIPSE { point: c, radiusX: 4.5, radiusY: 4.5 }, &b(0x1C1C1C)?);
                        } else {
                            rt.DrawEllipse(&D2D1_ELLIPSE { point: c, radiusX: 9.0, radiusY: 9.0 }, &b(0xC5C5C5)?, 1.0, None);
                        }
                        txt(name, &body, rect(r.left + 86.0, y + 7.0, r.right - 60.0, y + 29.0), &white);
                        txt(adapter, &small, rect(r.left + 86.0, y + 27.0, r.right - 60.0, y + 45.0), &dim);
                        txt(if *on { "Default device" } else { "Enabled" }, &small, rect(r.left + 86.0, y + 43.0, r.right - 60.0, y + 61.0), &dim);
                        ico(0xE76C, &icons12, rect(r.right - 36.0, y, r.right - 12.0, y + 70.0), &white);
                        hits.push((r, Hit::Row(i)));
                        hits.push((dr, Hit::Radio(i, k)));
                        y += 70.0;
                    }
                    rt.FillRectangle(&rect(r.left + 1.0, y + 1.0, r.right - 1.0, r.bottom - 1.0), &inner);
                    rt.FillRectangle(&rect(r.left, y, r.right, y + 1.0), &line);
                    txt(pair, &body, rect(r.left + 58.0, y, r.right - 190.0, y + 50.0), &white);
                    let br = rect(r.right - 174.0, y + 9.0, r.right - 44.0, y + 41.0);
                    rt.FillRoundedRectangle(&rrect(br, 4.0), &b(if hover == Some(Hit::Pair(i)) { 0x3D3D3D } else { 0x373737 })?);
                    let centered = fmt(14.0, n, ui_font)?;
                    centered.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                    txt("Add device", &centered, br, &white);
                    hits.push((br, Hit::Pair(i)));
                    continue;
                }
            }
            hits.push((r, Hit::Row(i)));
        }
        rt.PopAxisAlignedClip();
        ui.hits = hits;
        Ok(())
    })
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
        let max = IsZoomed(hwnd).as_bool();
        UI.with(|u| {
            let mut ui = u.borrow_mut();
            if ui.gfx.is_none() {
                ui.gfx = make_gfx().ok();
            }
            if let Some((rt, _)) = ui.gfx.clone() {
                let _ = rt.BindDC(mem, &rc);
                rt.SetDpi(dpi, dpi);
                rt.BeginDraw();
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
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
        ..Default::default()
    };
    let rt = f.CreateDCRenderTarget(&props)?;
    rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
    Ok((rt, DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?))
}

fn apply_slider(i: usize, f: f32, commit: bool) {
    UI.with(|u| {
        let mut ui = u.borrow_mut();
        if let Some(v) = ui.vals.get_mut(i) {
            v.f = f;
        }
        let (kind, label) = PAGES.with(|p| (p[ui.page].rows[i].kind, p[ui.page].rows[i].label));
        if let Kind::Slider(_, set) = kind {
            // volume follows the drag; slower settings (brightness) apply when you let go
            if commit || label == "Volume" {
                std::thread::spawn(move || set(f));
            }
        }
    });
}

fn go(page: usize) {
    UI.with(|u| {
        let mut ui = u.borrow_mut();
        ui.page = page;
        ui.vals.clear();
        ui.query.clear();
    });
    SCROLL.with(|sc| sc.set(0.0));
    COUNTS.with(|c| c.borrow_mut().clear());
    load(page);
}

fn hit(x: f32, y: f32) -> Option<Hit> {
    UI.with(|u| u.try_borrow().ok().and_then(|ui| ui.hits.iter().rev().find(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom).map(|(_, h)| *h)))
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
    let cw = || {
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        rc.right as f32 / s
    };
    let redraw = || {
        let _ = InvalidateRect(Some(hwnd), None, false);
    };
    let row_at = |i: usize| UI.with(|u| PAGES.with(|p| row_rect(&p[u.borrow().page], i, cw())));
    match msg {
        // our own title bar, like Settings': the whole window is client area
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
            if y < TOPBAR && hit(x, y).is_none() && !(CARD_X..839.0).contains(&x) {
                return LRESULT(HTCAPTION as isize);
            }
            return LRESULT(HTCLIENT as isize);
        }
        WM_VALUES => {
            let (page, vals) = VALUES.lock().unwrap().clone();
            UI.with(|u| {
                let mut ui = u.borrow_mut();
                if ui.page == page && ui.drag.is_none() {
                    set_counts(&vals);
                    ui.vals = vals;
                }
            });
            redraw();
        }
        WM_CHAR => {
            // typing anywhere searches, like the Find a setting box
            UI.with(|u| {
                let mut ui = u.borrow_mut();
                match wp.0 as u32 {
                    8 => {
                        ui.query.pop();
                    }
                    27 => ui.query.clear(),
                    c if c >= 32 => {
                        if let Some(ch) = char::from_u32(c) {
                            ui.query.push(ch);
                        }
                    }
                    _ => {}
                }
            });
            redraw();
        }
        WM_MOUSEMOVE => {
            let (x, y) = dips(lp);
            if let Some(i) = UI.with(|u| u.borrow().drag) {
                let (x0, x1) = slider_x(&row_at(i));
                apply_slider(i, ((x - x0) / (x1 - x0)).clamp(0.0, 1.0), false);
                redraw();
            } else {
                let h = hit(x, y);
                if UI.with(|u| std::mem::replace(&mut u.borrow_mut().hover, h) != h) {
                    let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    let _ = TrackMouseEvent(&mut tme);
                    redraw();
                }
            }
        }
        WM_MOUSELEAVE => {
            UI.with(|u| u.borrow_mut().hover = None);
            redraw();
        }
        WM_LBUTTONDOWN => {
            let (x, y) = dips(lp);
            let _ = SetFocus(Some(hwnd));
            if let Some(Hit::Row(i)) = hit(x, y) {
                let kind = UI.with(|u| PAGES.with(|p| p[u.borrow().page].rows[i].kind));
                let (x0, x1) = slider_x(&row_at(i));
                if matches!(kind, Kind::Slider(..)) && x >= x0 - 12.0 && x <= x1 + 12.0 {
                    SetCapture(hwnd);
                    UI.with(|u| u.borrow_mut().drag = Some(i));
                    apply_slider(i, ((x - x0) / (x1 - x0)).clamp(0.0, 1.0), false);
                    redraw();
                }
            }
        }
        WM_LBUTTONUP => {
            let (x, y) = dips(lp);
            if let Some(i) = UI.with(|u| u.borrow_mut().drag.take()) {
                let _ = ReleaseCapture();
                let (x0, x1) = slider_x(&row_at(i));
                apply_slider(i, ((x - x0) / (x1 - x0)).clamp(0.0, 1.0), true);
                redraw();
                return LRESULT(0);
            }
            match hit(x, y) {
                Some(Hit::Nav(i)) | Some(Hit::Result(i)) => go(i),
                Some(Hit::Back) => {
                    let parent = UI.with(|u| {
                        let mut ui = u.borrow_mut();
                        if !ui.query.is_empty() {
                            ui.query.clear();
                            return None;
                        }
                        PAGES.with(|p| page_index(p[ui.page].parent))
                    });
                    if let Some(p) = parent {
                        go(p);
                    }
                }
                Some(Hit::Win(0)) => {
                    let _ = ShowWindow(hwnd, SW_MINIMIZE);
                }
                Some(Hit::Win(1)) => {
                    let _ = ShowWindow(hwnd, if IsZoomed(hwnd).as_bool() { SW_RESTORE } else { SW_MAXIMIZE });
                }
                Some(Hit::Win(_)) => {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                Some(Hit::Radio(i, k)) => {
                    let (page, id) = UI.with(|u| {
                        let ui = u.borrow();
                        (ui.page, ui.vals.get(i).and_then(|v| v.list.get(k)).map(|d| d.3.clone()))
                    });
                    if let Some(id) = id {
                        std::thread::spawn(move || {
                            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
                            crate::qsx::set_output(&id);
                            load(page); // show what Windows actually did
                        });
                    }
                }
                Some(Hit::Open(uri)) => {
                    windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), &HSTRING::from(uri), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
                }
                Some(Hit::Tile(_)) => {
                    // ponytail: pairing/connecting goes through Windows' own Bluetooth page for now
                    windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), w!("ms-settings:bluetooth"), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
                }
                Some(Hit::Theme(k)) => {
                    // opening a .theme file is how Windows applies it
                    let file = UI.with(|u| u.borrow().vals.iter().find_map(|v| v.list.get(k).filter(|l| l.0.to_lowercase().ends_with(".theme")).map(|l| l.0.clone())));
                    if let Some(f) = file {
                        windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), &HSTRING::from(f), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
                    }
                }
                Some(Hit::Copy(i)) => {
                    let text = UI.with(|u| u.borrow().vals.get(i).map(|v| v.list.iter().map(|l| format!("{}\t{}", l.0, l.1.replace('\n', ", "))).collect::<Vec<_>>().join("\r\n")).unwrap_or_default());
                    copy_text(&text);
                }
                Some(Hit::Sort) => {
                    BY_NAME.store(!BY_NAME.load(Relaxed), Relaxed);
                    load(UI.with(|u| u.borrow().page));
                }
                Some(Hit::Sender(i, k)) if matches!(pages().get(UI.with(|u| u.borrow().page)).and_then(|p| p.rows.get(i)).map(|r| r.kind), Some(Kind::Actions(..))) => {
                    // a card's button: do its action for that item, then read the list again
                    let (page, item) = UI.with(|u| { let ui = u.borrow(); (ui.page, ui.vals.get(i).and_then(|v| v.list.get(k)).map(|d| (d.3.clone(), d.0.clone()))) });
                    if let (Some((id, name)), Some(Kind::Actions(_, _, act))) = (item, pages().get(page).and_then(|p| p.rows.get(i)).map(|r| r.kind)) {
                        std::thread::spawn(move || {
                            act(&id, &name);
                            load(page);
                        });
                    }
                }
                Some(Hit::Sender(i, k)) => {
                    let (page, app) = UI.with(|u| {
                        let mut ui = u.borrow_mut();
                        let app = ui.vals.get_mut(i).and_then(|v| v.list.get_mut(k)).map(|d| {
                            d.2 = !d.2;
                            (d.3.clone(), d.2)
                        });
                        (ui.page, app)
                    });
                    let set = pages().get(page).and_then(|p| p.rows.get(i)).and_then(|r| if let Kind::Senders(_, set) = r.kind { Some(set) } else { None });
                    if let (Some((id, on)), Some(set)) = (app, set) {
                        std::thread::spawn(move || {
                            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
                            set(&id, on);
                            load(page);
                        });
                    }
                }
                Some(Hit::Mute) => {
                    let page = UI.with(|u| u.borrow().page);
                    std::thread::spawn(move || {
                        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
                        set_muted(!muted());
                        load(page);
                    });
                }
                Some(Hit::Pair(_)) => {
                    windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), w!("ms-settings-connectabledevices:devicediscovery"), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
                }
                Some(Hit::Rename) => {
                    windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), w!("ms-settings:about"), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
                }
                Some(Hit::Row(i)) => {
                    let (page, kind, on) = UI.with(|u| {
                        let ui = u.borrow();
                        (ui.page, PAGES.with(|p| p[ui.page].rows[i].kind), ui.vals.get(i).is_some_and(|v| v.on))
                    });
                    match kind {
                        Kind::Toggle(_, set) | Kind::ToggleMore(_, set, _) => {
                            UI.with(|u| {
                                if let Some(v) = u.borrow_mut().vals.get_mut(i) {
                                    v.on = !on;
                                }
                            });
                            std::thread::spawn(move || {
                                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
                                set(!on);
                                std::thread::sleep(std::time::Duration::from_millis(900));
                                load(page); // show what Windows actually did
                            });
                        }
                        Kind::Link(uri) | Kind::Expand(uri) | Kind::LinkBtn(uri, ..) => {
                            windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), &HSTRING::from(uri), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
                        }
                        Kind::Sub(name) => {
                            if let Some(p) = page_index(name) {
                                go(p);
                            }
                        }
                        Kind::State(_, uri) => {
                            windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), &HSTRING::from(uri), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
                        }
                        Kind::Choice(_, options, set) | Kind::Flat(_, options, set) => {
                            if let Ok(m) = CreatePopupMenu() {
                                let now = UI.with(|u| u.borrow().vals.get(i).map(|v| v.s.clone()).unwrap_or_default());
                                for (k, o) in options().iter().enumerate() {
                                    let _ = AppendMenuW(m, if *o == now { MF_STRING | MF_CHECKED } else { MF_STRING }, k + 1, &HSTRING::from(o.as_str()));
                                }
                                let mut pt = POINT::default();
                                let _ = GetCursorPos(&mut pt);
                                let cmd = TrackPopupMenu(m, TPM_RETURNCMD, pt.x, pt.y, None, hwnd, None).0;
                                let _ = DestroyMenu(m);
                                if cmd > 0 {
                                    std::thread::spawn(move || {
                                        set(cmd as usize - 1);
                                        load(page); // show what Windows actually did
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }
                None => {}
            }
            redraw();
        }
        WM_MOUSEWHEEL => {
            let d = ((wp.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let need = UI.with(|u| PAGES.with(|p| { let pg = &p[u.borrow().page]; row_tops(&pg.rows.iter().map(|r| r.kind).collect::<Vec<_>>(), &COUNTS.with(|c| c.borrow().clone()), first_row_y(pg)).1 + 24.0 }));
            let max = (need - rc.bottom as f32 / s).max(0.0);
            SCROLL.with(|sc| sc.set((sc.get() - d * 60.0).clamp(0.0, max)));
            redraw();
        }
        WM_SIZE => redraw(),
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => paint(hwnd),
        WM_CLOSE => {
            let _ = ShowWindow(hwnd, SW_HIDE);
            UI.with(|u| u.borrow_mut().gfx = None);
            // hand the memory back: the window is gone, the app keeps running
            let _ = windows::Win32::System::ProcessStatus::K32EmptyWorkingSet(windows::Win32::System::Threading::GetCurrentProcess());
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

pub unsafe fn show(inst: HINSTANCE) {
    let mut h = HWND(WIN.load(Relaxed) as *mut c_void);
    if h.is_invalid() {
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: inst, lpszClassName: w!("shellbits_settings"), hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
        RegisterClassW(&wc);
        let Ok(w_) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("shellbits_settings"), w!("Settings"), WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT, 1180, 790, None, None, Some(inst), None) else { return };
        h = w_;
        WIN.store(h.0 as isize, Relaxed);
        let dark = 1u32;
        let _ = DwmSetWindowAttribute(h, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const c_void, 4);
        let _ = DwmExtendFrameIntoClientArea(h, &MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 1, cyBottomHeight: 0 });
        let _ = SetWindowPos(h, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER);
    }
    load(UI.with(|u| u.borrow().page));
    let _ = ShowWindow(h, SW_SHOW);
    crate::flyout::force_foreground(h);
}

/// `shellbits --snapshot-settings out.bmp [page name]`: one page with live values, drawn off-screen
/// at the size of the reference capture (1164 x 774).
pub unsafe fn snapshot(path: &str, page: &str) -> windows::core::Result<()> {
    let page = page_index(page).unwrap_or(1);
    load(page);
    for _ in 0..250 {
        // keep handling messages while we wait: some Windows queries answer through this thread
        let mut m = MSG::default();
        while PeekMessageW(&mut m, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&m);
            DispatchMessageW(&m);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        if VALUES.lock().unwrap().0 == page {
            break;
        }
    }
    let (w_, h) = (1164, 774);
    UI.with(|u| -> windows::core::Result<()> {
        let mut ui = u.borrow_mut();
        ui.page = page;
        ui.vals = VALUES.lock().unwrap().1.clone();
        set_counts(&ui.vals);
        ui.gfx = Some(make_gfx()?);
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w_, biHeight: -h, biPlanes: 1, biBitCount: 32, ..Default::default() },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let dib = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let dc = CreateCompatibleDC(None);
        let old = SelectObject(dc, dib.into());
        let rt = ui.gfx.as_ref().unwrap().0.clone();
        rt.BindDC(dc, &RECT { left: 0, top: 0, right: w_, bottom: h })?;
        rt.BeginDraw();
        draw(&mut ui, w_ as f32, h as f32, false)?;
        rt.EndDraw(None, None)?;
        let _ = GdiFlush();
        let px = std::slice::from_raw_parts(bits as *const u8, (w_ * h * 4) as usize);
        let mut out = Vec::with_capacity(54 + px.len());
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&((54 + px.len()) as u32).to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&w_.to_le_bytes());
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
    fn installed_app_records() {
        assert_eq!(app_line("1.2.3", "Acme", "20261105", 2048), "1.2.3  |  Acme  |  11/5/2026  |  2 MB");
        assert_eq!(app_line("", "Acme", "", 0), "Acme");
        assert_eq!(app_line("7", "", "2026", 0), "7"); // a date that is not YYYYMMDD is left out
        assert_eq!(uninstall_command("MsiExec.exe /I{ABC-1}"), "MsiExec.exe /X{ABC-1}");
        assert_eq!(uninstall_command(r#""C:\P F\unins000.exe" /SILENT"#), r#""C:\P F\unins000.exe" /SILENT"#);
        assert_eq!(icon_file(r#""C:\P F\app.exe",0"#), r"C:\P F\app.exe");
        assert_eq!(icon_file(r"C:\a,b\app.ico"), r"C:\a,b\app.ico"); // a comma that is part of the path
        assert_eq!(icon_source("app\tC:/x.ico\tC:/unins.exe"), "C:/x.ico");
    }

    #[test]
    fn startup_records_and_commands() {
        assert!(startup_on(&[])); // never switched: on
        assert!(startup_on(&[2, 0, 0, 0]) && startup_on(&[6, 0, 0, 0]));
        assert!(!startup_on(&[3, 0, 0, 0]) && !startup_on(&[7, 0, 0, 0]));
        assert_eq!(exe_of(r#""C:\Program Files\A B\x.exe" --hidden"#), r"C:\Program Files\A B\x.exe");
        assert_eq!(exe_of(r"C:\WINDOWS\system32\SecurityHealthSystray.exe"), r"C:\WINDOWS\system32\SecurityHealthSystray.exe");
        assert_eq!(exe_of(r"C:\tools\run.EXE -background"), r"C:\tools\run.EXE");
        assert_eq!(icon_source("startup\tC:/x.exe\tcu:X"), "C:/x.exe");
        assert_eq!(icon_source("Microsoft.App_1!App"), r"shell:AppsFolder\Microsoft.App_1!App");
    }
    #[test]
    fn layout_and_scale_math() {
        use Kind::*;
        fn s() -> String {
            String::new()
        }
        // Display page: title, 3 cards, title, card -> the positions measured on the real page (minus its 8px frame)
        let (tops, end) = row_tops(&[Header, Info(s), Info(s), Info(s), Header, Info(s)], &[], 123.0);
        assert_eq!(tops, vec![123.0, 139.0, 211.0, 283.0, 355.0, 411.0]);
        assert_eq!(end, 483.0);
        // Sound page: title, a device card listing 4 devices, then Volume -> 140..537 and 541, as measured
        fn none() -> Vec<crate::qsx::Item> {
            Vec::new()
        }
        let (tops, _) = row_tops(&[Header, Devices(none, ""), Info(s)], &[0, 4, 0], 124.0);
        assert_eq!(tops, vec![124.0, 140.0, 542.0]);
        assert_eq!(split_device("Speakers (Realtek(R) Audio)"), ("Speakers".to_string(), "Realtek(R) Audio".to_string()));
        assert_eq!(split_device("Headset"), ("Headset".to_string(), String::new()));
        let mut v = vec![("b".to_string(), 5), ("A".to_string(), 9), ("c".to_string(), 1)];
        sort_senders(&mut v, false);
        assert_eq!(v[0].0, "A");
        sort_senders(&mut v, true);
        assert_eq!(v.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["A", "b", "c"]);
        // Power page: battery %, the policy bar, first card -> 112, 212, 268 as measured; without the bar -> 212
        fn yes() -> bool {
            true
        }
        assert_eq!(row_tops(&[Big(s), Note(yes, ""), Info(s)], &[0, 1, 0], 112.0).0, vec![112.0, 211.0, 267.0]);
        assert_eq!(row_tops(&[Big(s), Note(yes, ""), Info(s)], &[0, 0, 0], 112.0).0, vec![112.0, 211.0, 211.0]);
        assert!(power_modes().contains(&power_mode()));
        // About page: 4 tiles (two rows), rename card, device info -> 112, 462, 534 as measured
        fn none4() -> Vec<Line> {
            Vec::new()
        }
        assert_eq!(row_tops(&[Tiles(none4), RenamePc, Facts(none4)], &[4, 0, 9], 112.0).0, vec![112.0, 462.0, 534.0]);
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(19782), (2024, 2, 29));
        assert_eq!(size_label(6 << 30), "6 GB");
        assert_eq!(size_label(486 << 20), "486 MB");
        // Bluetooth page: tiles, the bar, Bluetooth switch, air, Devices -> 112, 343, 382, ~455, 472 (real: 112, 343, 388?, 477)
        fn devs() -> Vec<crate::qsx::Item> {
            Vec::new()
        }
        fn on() -> bool {
            true
        }
        fn set(_: bool) {}
        let t = row_tops(&[BtTiles(devs), Center("", ""), Toggle(on, set), Space(17.0), Info(s)], &[], 112.0).0;
        assert_eq!(&t[..3], &[112.0, 343.0, 388.0]);
        assert_eq!(ini_value("[Theme]\r\nDisplayName=x\r\nWallpaper=%SystemRoot%\\web\\a.jpg\r\n", "wallpaper").as_deref(), Some("%SystemRoot%\\web\\a.jpg"));
        assert!(themes().len() >= 2, "Windows ships at least the light and dark themes");
        assert_eq!(recommended_index(100, 0), 0);
        assert_eq!(recommended_index(150, 1), 1); // at 150%, one step above recommended -> 125% is recommended
        assert_eq!(recommended_index(100, -2), 2);
        assert!(resolutions().contains(&resolution())); // the live list offers the mode we're in
        assert!(scales().contains(&scale()));
    }
    #[test]
    fn every_sub_page_link_resolves() {
        let p = pages();
        for page in &p {
            assert!(page.parent.is_empty() || p.iter().any(|x| x.name == page.parent), "parent of {}", page.name);
            for r in &page.rows {
                if let Kind::Sub(name) = r.kind {
                    assert!(p.iter().any(|x| x.name == name), "missing page {name}");
                }
            }
        }
    }
    #[test]
    fn search_finds_rows() {
        let p = pages();
        assert!(search(&p, "night").iter().any(|r| r.1 == "Night light"));
        assert!(search(&p, "zzzz").is_empty());
    }
}

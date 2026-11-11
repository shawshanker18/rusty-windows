//! Windows 11 taskbar, measured 1:1 from the real one on this PC (100% scale, DIPs):
//! 48px tall, centred group [Start 45][2][Search 220x32][2][apps 44 each], tray on the right:
//! [chevron 32][ENG/IN 44][4][network 24][4][volume 24][4][battery 28][8][clock 70][4][bell 24][4][show desktop 12].
//! Preview only for now (`shellbits --taskbar`): it sits on top of Explorer's taskbar until the
//! rest of the shell (tray hosting, flyouts) exists and Explorer can be switched off.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};

use windows::core::{w, BOOL, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::flyout::{glyph, rect, rrect, utf16};

pub static TASKBAR: AtomicIsize = AtomicIsize::new(0);
const T_TICK: usize = 20;

const H: f32 = 48.0;
const START_W: f32 = 45.0;
const SEARCH_W: f32 = 220.0;
const APP_W: f32 = 44.0;
const GAP: f32 = 2.0;

const fn rgb(v: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: ((v >> 16) & 255) as f32 / 255.0, g: ((v >> 8) & 255) as f32 / 255.0, b: (v & 255) as f32 / 255.0, a: 1.0 }
}
const C_BAR: D2D1_COLOR_F = rgb(0x202020);
const C_EDGE: D2D1_COLOR_F = rgb(0x434343);
const C_PILL: D2D1_COLOR_F = rgb(0x404040);
const C_PILL_EDGE: D2D1_COLOR_F = rgb(0x4A4A4A);
const C_ACTIVE: D2D1_COLOR_F = rgb(0x2D2D2D);
const C_ACTIVE_EDGE: D2D1_COLOR_F = rgb(0x363636);
const C_HOVER: D2D1_COLOR_F = rgb(0x2A2A2A);
const C_RUN: D2D1_COLOR_F = rgb(0x8D8D8D);
const C_ACCENT: D2D1_COLOR_F = rgb(0x4CC2FF);
const C_SEARCH_TEXT: D2D1_COLOR_F = rgb(0xCFCFCF);

// tray (from the right edge): (id, width, gap after previous)
#[derive(Clone, Copy, PartialEq)]
enum Tray {
    Icon(usize), // an app's promoted tray icon (index into tray::ICONS)
    Chevron,
    Input,
    Network,
    Volume,
    Battery,
    Clock,
    Bell,
    Desktop,
}
const TRAY: [(Tray, f32, f32); 8] = [
    (Tray::Chevron, 32.0, 0.0),
    (Tray::Input, 44.0, 0.0),
    (Tray::Network, 24.0, 4.0),
    (Tray::Volume, 24.0, 4.0),
    (Tray::Battery, 28.0, 4.0),
    (Tray::Clock, 70.0, 8.0),
    (Tray::Bell, 24.0, 4.0),
    (Tray::Desktop, 12.0, 4.0),
];

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Start,
    Search,
    App(usize),
    Tray(Tray),
}

struct App {
    key: String,       // AUMID or exe path
    icon: String,      // shell parsing name for its icon
    windows: Vec<HWND>,
}

#[derive(Default)]
struct State {
    apps: Vec<App>,
    order: Vec<String>, // keys in the order they first appeared (like Windows)
    icons: HashMap<String, Vec<u8>>,
    active: Option<String>,
    hover: Option<Hit>,
    pins: Option<Vec<(String, String)>>, // (key, launch target), read once from the pins file
    gfx: Option<(ID2D1DCRenderTarget, IDWriteTextFormat, IDWriteTextFormat, IDWriteTextFormat, IDWriteTextFormat)>,
}

thread_local! {
    static ST: std::cell::RefCell<State> = std::cell::RefCell::new(State::default());
}

// ======================= running apps =======================

unsafe extern "system" fn enum_cb(h: HWND, lp: LPARAM) -> BOOL {
    let v = &mut *(lp.0 as *mut Vec<HWND>);
    if !IsWindowVisible(h).as_bool() || GetWindowTextLengthW(h) == 0 {
        return TRUE;
    }
    let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
    let app = ex & WS_EX_APPWINDOW.0 != 0;
    let owned = GetWindow(h, GW_OWNER).is_ok_and(|o| !o.is_invalid());
    if !app && (ex & WS_EX_TOOLWINDOW.0 != 0 || owned) {
        return TRUE;
    }
    let mut cloaked = 0u32;
    let _ = DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut c_void, 4);
    if cloaked != 0 {
        return TRUE;
    }
    v.push(h);
    TRUE
}

/// The taskbar groups windows by AppUserModelID (what Windows uses), else by exe path.
pub(crate) unsafe fn app_key(h: HWND) -> (String, String) {
    if let Ok(store) = SHGetPropertyStoreForWindow::<IPropertyStore>(h) {
        if let Ok(v) = store.GetValue(&windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID) {
            let s = v.to_string();
            if !s.is_empty() {
                return (s.clone(), format!("shell:AppsFolder\\{s}"));
            }
        }
    }
    let mut pid = 0;
    GetWindowThreadProcessId(h, Some(&mut pid));
    if let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
        let mut buf = [0u16; 1024];
        let mut n = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n).is_ok();
        let _ = CloseHandle(p);
        if ok {
            let path = String::from_utf16_lossy(&buf[..n as usize]);
            return (path.to_lowercase(), path);
        }
    }
    (format!("hwnd:{:?}", h.0), String::new())
}

fn pins_path() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join("shellbits").join("taskbar-pins.txt")
}

/// One pin per line: key, a tab, the launch target.
fn parse_pins(text: &str) -> Vec<(String, String)> {
    text.lines().filter_map(|l| l.split_once('\t')).filter(|(k, t)| !k.is_empty() && !t.is_empty()).map(|(k, t)| (k.to_string(), t.trim().to_string())).collect()
}

/// Pin or unpin an app; returns the new list.
fn toggle_pin(mut pins: Vec<(String, String)>, key: &str, target: &str) -> Vec<(String, String)> {
    match pins.iter().position(|p| p.0 == key) {
        Some(i) => { pins.remove(i); }
        None => pins.push((key.to_string(), target.to_string())),
    }
    pins
}

/// Pinned apps come first, in pin order, whether they are running or not (like Windows).
fn merge_pins(mut apps: Vec<App>, pins: &[(String, String)]) -> Vec<App> {
    let mut out = Vec::new();
    for (key, target) in pins {
        match apps.iter().position(|a| &a.key == key) {
            Some(i) => out.push(apps.remove(i)),
            None => out.push(App { key: key.clone(), icon: target.clone(), windows: vec![] }),
        }
    }
    out.extend(apps);
    out
}

fn refresh(st: &mut State) {
    unsafe {
        let mut wins: Vec<HWND> = Vec::new();
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut wins as *mut _ as isize));
        let mut apps: Vec<App> = Vec::new();
        for h in wins {
            let (key, icon) = app_key(h);
            if let Some(a) = apps.iter_mut().find(|a| a.key == key) {
                a.windows.push(h);
            } else {
                apps.push(App { key, icon, windows: vec![h] });
            }
        }
        let mut new: Vec<(u64, String)> = apps.iter().filter(|a| !st.order.contains(&a.key)).map(|a| (started(a.windows[0]), a.key.clone())).collect();
        new.sort();
        st.order.extend(new.into_iter().map(|x| x.1));
        let order = st.order.clone();
        apps.sort_by_key(|a| order.iter().position(|k| k == &a.key).unwrap_or(usize::MAX));
        let pins = st.pins.get_or_insert_with(|| parse_pins(&std::fs::read_to_string(pins_path()).unwrap_or_default())).clone();
        let apps = merge_pins(apps, &pins);
        let fg = GetForegroundWindow();
        st.active = apps.iter().find(|a| a.windows.contains(&fg)).map(|a| a.key.clone()).or(st.active.take().filter(|_| false));
        // icons for new apps (24px at 100%)
        if apps.iter().any(|a| !a.icon.is_empty() && !st.icons.contains_key(&a.icon)) {
            if let Ok(wic) = CoCreateInstance::<_, IWICImagingFactory>(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) {
                for a in &apps {
                    if !a.icon.is_empty() && !st.icons.contains_key(&a.icon) {
                        if let Ok(px) = crate::launcher::icon_pixels(&wic, &a.icon, 24) {
                            st.icons.insert(a.icon.clone(), px);
                        }
                    }
                }
            }
        }
        st.apps = apps;
    }
}

// ======================= layout =======================

fn center_x(st: &State, w: f32) -> f32 {
    let total = START_W + GAP + SEARCH_W + GAP + APP_W * st.apps.len() as f32;
    ((w - total) / 2.0).round()
}

fn tray_rects(w: f32) -> Vec<(Tray, D2D_RECT_F)> {
    // laid out right-to-left from the screen edge; the third column is the gap to the left neighbour.
    // Promoted app icons (24 wide, like Windows 11) sit between the ^ chevron and the language.
    let mut x = w;
    let mut res = Vec::new();
    for &(t, wd, gap) in TRAY.iter().rev() {
        if t == Tray::Chevron {
            for i in crate::tray::split().0.into_iter().rev() {
                res.push((Tray::Icon(i), rect(x - 24.0, 0.0, x, H)));
                x -= 24.0;
            }
        }
        res.push((t, rect(x - wd, 0.0, x, H)));
        x -= wd + gap;
    }
    res.reverse();
    res
}

fn hit_test(st: &State, w: f32, x: f32, y: f32) -> Option<Hit> {
    if !(0.0..H).contains(&y) {
        return None;
    }
    let x0 = center_x(st, w);
    if (x0..x0 + START_W).contains(&x) {
        return Some(Hit::Start);
    }
    let sx = x0 + START_W + GAP;
    if (sx..sx + SEARCH_W).contains(&x) && (8.0..40.0).contains(&y) {
        return Some(Hit::Search);
    }
    let ax = sx + SEARCH_W + GAP;
    for i in 0..st.apps.len() {
        let l = ax + i as f32 * APP_W;
        if (l..l + APP_W).contains(&x) {
            return Some(Hit::App(i));
        }
    }
    tray_rects(w).into_iter().find(|(_, r)| x >= r.left && x < r.right).map(|(t, _)| Hit::Tray(t))
}

// ======================= system state for the tray =======================

fn clock_text() -> (String, String) {
    unsafe {
        let st = windows::Win32::System::SystemInformation::GetLocalTime();
        let h12 = match st.wHour % 12 { 0 => 12, h => h };
        let ampm = if st.wHour < 12 { "AM" } else { "PM" };
        (format!("{h12}:{:02} {ampm}", st.wMinute), format!("{}/{}/{}", st.wMonth, st.wDay, st.wYear))
    }
}

fn battery() -> Option<(u8, bool)> {
    unsafe {
        let mut s = windows::Win32::System::Power::SYSTEM_POWER_STATUS::default();
        windows::Win32::System::Power::GetSystemPowerStatus(&mut s).ok()?;
        (s.BatteryFlag & 128 == 0 && s.BatteryLifePercent <= 100).then_some((s.BatteryLifePercent, s.ACLineStatus == 1))
    }
}

fn input_lang() -> (String, String) {
    unsafe {
        let hkl = GetKeyboardLayout(GetWindowThreadProcessId(GetForegroundWindow(), None));
        let lang = (hkl.0 as usize & 0xFFFF) as u32;
        let mut name = [0u16; 16];
        let n = windows::Win32::Globalization::LCIDToLocaleName(lang, Some(&mut name), 0);
        let loc = String::from_utf16_lossy(&name[..(n.max(1) - 1) as usize]); // e.g. "en-IN"
        let (l, r) = loc.split_once('-').unwrap_or((&loc, ""));
        let l3 = match l { "en" => "ENG", "hi" => "HIN", "fr" => "FRA", "de" => "DEU", "es" => "ESP", o => o };
        // second line: the region of your installed input language for this language
        // (e.g. English (India) -> IN), from the same list the language switcher uses
        // Windows shows the keyboard layout's region; substituted layouts (e.g. d0010409 -> 00004009,
        // English (India)) are listed under HKCU\Keyboard Layout\Substitutes
        let region = layout_region(hkl.0 as usize as u32)
            .or_else(|| installed_regions().into_iter().find(|(lng, _)| lng == l).map(|(_, r)| r))
            .unwrap_or_else(|| r.to_string());
        (l3.to_uppercase(), region.to_uppercase())
    }
}

/// Installed input languages, in order (HKCU\Keyboard Layout\Preload): ("en", "IN"), ...
fn installed_regions() -> Vec<(String, String)> {
    use windows::Win32::System::Registry::*;
    let mut out = Vec::new();
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, w!(r"Keyboard Layout\Preload"), Some(0), KEY_READ, &mut key).is_err() {
            return out;
        }
        for i in 1..10 {
            let mut buf = [0u16; 16];
            let mut len = (buf.len() * 2) as u32;
            let name = windows::core::HSTRING::from(i.to_string());
            if RegQueryValueExW(key, &name, None, None, Some(buf.as_mut_ptr() as *mut u8), Some(&mut len)).is_err() {
                break;
            }
            let hex = String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]);
            let Ok(v) = u32::from_str_radix(hex.trim(), 16) else { continue };
            let mut name = [0u16; 16];
            let n = windows::Win32::Globalization::LCIDToLocaleName(v & 0xFFFF, Some(&mut name), 0);
            let loc = String::from_utf16_lossy(&name[..(n.max(1) - 1) as usize]);
            if let Some((l, r)) = loc.split_once('-') {
                out.push((l.to_string(), r.to_string()));
            }
        }
        let _ = RegCloseKey(key);
    }
    out
}

/// Region of a substituted keyboard layout (what the tray's second line shows).
fn layout_region(hkl: u32) -> Option<String> {
    use windows::Win32::System::Registry::*;
    unsafe {
        let mut key = HKEY::default();
        RegOpenKeyExW(HKEY_CURRENT_USER, w!(r"Keyboard Layout\Substitutes"), Some(0), KEY_READ, &mut key).ok().ok()?;
        // try the full HKL id, then any substitute for this language (first preload wins)
        let mut names = vec![format!("{hkl:08x}")];
        names.push(format!("d001{:04x}", hkl & 0xFFFF));
        let mut out = None;
        for n in names {
            let mut buf = [0u16; 16];
            let mut len = (buf.len() * 2) as u32;
            if RegQueryValueExW(key, &windows::core::HSTRING::from(n), None, None, Some(buf.as_mut_ptr() as *mut u8), Some(&mut len)).is_ok() {
                let hex = String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]);
                if let Ok(v) = u32::from_str_radix(hex.trim(), 16) {
                    let mut name = [0u16; 16];
                    let k = windows::Win32::Globalization::LCIDToLocaleName(v & 0xFFFF, Some(&mut name), 0);
                    let loc = String::from_utf16_lossy(&name[..(k.max(1) - 1) as usize]);
                    out = loc.split_once('-').map(|(_, r)| r.to_string());
                    break;
                }
            }
        }
        let _ = RegCloseKey(key);
        out
    }
}

/// When a process started (Windows orders taskbar buttons by launch).
fn started(h: HWND) -> u64 {
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return u64::MAX };
        let (mut c, mut e, mut k, mut u) = Default::default();
        let ok = GetProcessTimes(p, &mut c, &mut e, &mut k, &mut u).is_ok();
        let _ = CloseHandle(p);
        if ok { (c.dwHighDateTime as u64) << 32 | c.dwLowDateTime as u64 } else { u64::MAX }
    }
}

// ======================= drawing =======================

unsafe fn gfx(st: &mut State) -> windows::core::Result<()> {
    if st.gfx.is_some() {
        return Ok(());
    }
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
        Ok(t)
    };
    let text = w!("Segoe UI Variable Text");
    let icons = w!("Segoe Fluent Icons");
    st.gfx = Some((
        rt,
        fmt(icons, 16.0, DWRITE_TEXT_ALIGNMENT_CENTER)?,
        fmt(text, 14.0, DWRITE_TEXT_ALIGNMENT_LEADING)?,
        fmt(text, 12.0, DWRITE_TEXT_ALIGNMENT_CENTER)?,
        fmt(text, 12.0, DWRITE_TEXT_ALIGNMENT_TRAILING)?,
    ));
    Ok(())
}

unsafe fn draw(st: &mut State, mem: HDC, rc: &RECT, dpi: f32) -> windows::core::Result<()> {
    gfx(st)?;
    let (rt, icon16, body, small_c, small_r) = st.gfx.clone().unwrap();
    rt.BindDC(mem, rc)?;
    rt.SetDpi(dpi, dpi);
    let w = (rc.right - rc.left) as f32 * 96.0 / dpi;
    rt.BeginDraw();
    rt.Clear(Some(&C_BAR));
    let brush = |c: D2D1_COLOR_F| rt.CreateSolidColorBrush(&c, None);
    let white = brush(rgb(0xFFFFFF))?;
    let text = |s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush| {
        rt.DrawText(&utf16(s), f, &r, b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL)
    };
    let icon = |cp: u32, r: D2D_RECT_F, b: &ID2D1SolidColorBrush| {
        rt.DrawText(&glyph(cp), &icon16, &r, b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL)
    };
    rt.FillRectangle(&rect(0.0, 0.0, w, 1.0), &brush(C_EDGE)?); // top hairline

    // ---- Start: the Windows 11 logo (4 tiles, light-to-deep blue gradient) ----
    let x0 = center_x(st, w);
    if st.hover == Some(Hit::Start) {
        rt.FillRoundedRectangle(&rrect(rect(x0 + 2.0, 4.0, x0 + START_W - 2.0, 44.0), 4.0), &brush(C_HOVER)?);
    }
    let stops = [
        D2D1_GRADIENT_STOP { position: 0.0, color: rgb(0x6CD1FF) },
        D2D1_GRADIENT_STOP { position: 1.0, color: rgb(0x0B86DF) },
    ];
    let coll = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
    let (lx, ly) = (x0 + 11.0, 12.0);
    let grad = rt.CreateLinearGradientBrush(
        &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
            startPoint: windows_numerics::Vector2 { X: lx, Y: ly },
            endPoint: windows_numerics::Vector2 { X: lx + 23.0, Y: ly + 24.0 },
        },
        None,
        &coll,
    )?;
    for (dx, dy) in [(0.0, 0.0), (12.0, 0.0), (0.0, 12.0), (12.0, 12.0)] {
        rt.FillRoundedRectangle(&rrect(rect(lx + dx, ly + dy, lx + dx + 11.0, ly + dy + 11.0), 1.0), &grad);
    }

    // ---- Search pill ----
    let sx = x0 + START_W + GAP;
    let sr = rect(sx, 8.0, sx + SEARCH_W, 40.0);
    rt.FillRoundedRectangle(&rrect(sr, 16.0), &brush(if st.hover == Some(Hit::Search) { C_PILL_EDGE } else { C_PILL })?);
    rt.DrawRoundedRectangle(&rrect(rect(sr.left + 0.5, sr.top + 0.5, sr.right - 0.5, sr.bottom - 0.5), 15.5), &brush(C_PILL_EDGE)?, 1.0, None);
    let grey = brush(C_SEARCH_TEXT)?;
    icon(0xE721, rect(sx + 10.0, 8.0, sx + 30.0, 40.0), &grey);
    text("Search", &body, rect(sx + 37.0, 8.0, sx + SEARCH_W - 10.0, 40.0), &grey);

    // ---- running apps ----
    let ax = sx + SEARCH_W + GAP;
    let bp = D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
    };
    for (i, a) in st.apps.iter().enumerate() {
        let l = ax + i as f32 * APP_W;
        let active = st.active.as_deref() == Some(a.key.as_str());
        let hover = st.hover == Some(Hit::App(i));
        if active || hover {
            let r = rect(l + 2.0, 4.0, l + APP_W - 2.0, 44.0);
            rt.FillRoundedRectangle(&rrect(r, 4.0), &brush(if hover && !active { C_HOVER } else { C_ACTIVE })?);
            rt.DrawRoundedRectangle(&rrect(rect(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5), 4.0), &brush(C_ACTIVE_EDGE)?, 1.0, None);
        }
        if let Some(px) = st.icons.get(&a.icon) {
            if let Ok(b) = rt.CreateBitmap(D2D_SIZE_U { width: 24, height: 24 }, Some(px.as_ptr() as *const c_void), 24 * 4, &bp) {
                crate::flyout::draw_icon(&rt, &b, l + 10.0, 12.0, 24.0);
            }
        } else {
            icon(0xE737, rect(l + 10.0, 12.0, l + 34.0, 36.0), &white);
        }
        let cx = l + APP_W / 2.0;
        let (dw, col) = if active { (16.0, C_ACCENT) } else { (6.0, C_RUN) };
        if !a.windows.is_empty() { // a pinned app that is not running has no mark
            rt.FillRoundedRectangle(&rrect(rect(cx - dw / 2.0, 41.0, cx + dw / 2.0, 44.0), 1.5), &brush(col)?);
        }
    }

    // ---- tray ----
    let rects = tray_rects(w);
    let get = |t: Tray| rects.iter().find(|x| x.0 == t).unwrap().1;
    // the network / volume / battery group highlights as one button, like Windows
    let grp = rect(get(Tray::Network).left - 4.0, 4.0, get(Tray::Battery).right + 4.0, 44.0);
    for (t, r) in &rects {
        let hov = st.hover == Some(Hit::Tray(*t));
        let in_grp = matches!(t, Tray::Network | Tray::Volume | Tray::Battery);
        let grp_hov = matches!(st.hover, Some(Hit::Tray(Tray::Network | Tray::Volume | Tray::Battery)));
        if hov && !in_grp && *t != Tray::Desktop {
            rt.FillRoundedRectangle(&rrect(rect(r.left, 4.0, r.right, 44.0), 4.0), &brush(C_HOVER)?);
        }
        if in_grp && grp_hov && *t == Tray::Network {
            rt.FillRoundedRectangle(&rrect(grp, 4.0), &brush(C_HOVER)?);
        }
    }
    icon(0xE70E, get(Tray::Chevron), &white);
    let bp = D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
    };
    for (t, r) in &rects {
        if let Tray::Icon(i) = t {
            if let Some(px) = crate::tray::pixels(*i).filter(|p| p.len() == 16 * 16 * 4) {
                if let Ok(b) = rt.CreateBitmap(D2D_SIZE_U { width: 16, height: 16 }, Some(px.as_ptr() as *const c_void), 64, &bp) {
                    crate::flyout::draw_icon(&rt, &b, r.left + 4.0, 16.0, 16.0);
                }
            }
        }
    }
    let (l1, l2) = input_lang();
    let ir = get(Tray::Input);
    text(&l1, &small_c, rect(ir.left, 8.0, ir.right, 25.0), &white);
    text(&l2, &small_c, rect(ir.left, 23.0, ir.right, 40.0), &white);
    icon(0xE839, get(Tray::Network), &white);
    icon(0xE995, get(Tray::Volume), &white);
    // battery: outline, green fill while charging, like Windows 11
    let br = get(Tray::Battery);
    let (bx, by) = ((br.left + br.right) / 2.0 - 10.0, 18.5);
    if let Some((pct, charging)) = battery() {
        let body = rect(bx, by, bx + 18.0, by + 11.0);
        rt.DrawRoundedRectangle(&rrect(body, 2.0), &white, 1.0, None);
        rt.FillRoundedRectangle(&rrect(rect(bx + 18.5, by + 3.5, bx + 20.0, by + 7.5), 0.5), &white);
        let fill = rect(bx + 2.0, by + 2.0, bx + 2.0 + 14.0 * pct as f32 / 100.0, by + 9.0);
        rt.FillRoundedRectangle(&rrect(fill, 1.0), &brush(if charging { rgb(0x8BD48A) } else { rgb(0xFFFFFF) })?);
        if charging {
            // white lightning bolt with a dark outline, centred on the battery
            let (cx, cy) = (bx + 9.0, by + 5.5);
            let f: ID2D1Factory = rt.GetFactory()?;
            let path = f.CreatePathGeometry()?;
            let sink = path.Open()?;
            let pts = [(1.5, -6.0), (-3.0, 0.8), (-0.2, 0.8), (-1.5, 6.0), (3.0, -0.8), (0.2, -0.8)];
            sink.BeginFigure(windows_numerics::Vector2 { X: cx + pts[0].0, Y: cy + pts[0].1 }, D2D1_FIGURE_BEGIN_FILLED);
            for p in &pts[1..] {
                sink.AddLine(windows_numerics::Vector2 { X: cx + p.0, Y: cy + p.1 });
            }
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close()?;
            rt.DrawGeometry(&path, &brush(rgb(0x202020))?, 1.5, None);
            rt.FillGeometry(&path, &white, None);
        }
    }
    let (t1, t2) = clock_text();
    let cr = get(Tray::Clock);
    text(&t1, &small_r, rect(cr.left, 8.0, cr.right - 4.0, 25.0), &white);
    text(&t2, &small_r, rect(cr.left, 23.0, cr.right - 4.0, 40.0), &white);
    // notifications bell (Do not disturb shows little z's)
    let bl = get(Tray::Bell);
    icon(0xF2A8, bl, &white);
    // show desktop: a hairline at the far right edge
    let dr = get(Tray::Desktop);
    rt.FillRectangle(&rect(dr.left + 2.0, 12.0, dr.left + 3.0, 36.0), &brush(if st.hover == Some(Hit::Tray(Tray::Desktop)) { rgb(0x8D8D8D) } else { C_BAR })?);
    rt.EndDraw(None, None)
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
        ST.with(|s| {
            let mut st = s.borrow_mut();
            if draw(&mut st, mem, &rc, GetDpiForWindow(hwnd) as f32).is_err() {
                st.gfx = None;
            }
        });
        let _ = EndBufferedPaint(pb, true);
    }
    let _ = EndPaint(hwnd, &ps);
}

// ======================= behaviour =======================

unsafe fn press_win(vk: u16) {
    // hand off to Windows' own flyouts until shellbits has its own
    let k = |vk: u16, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, ..Default::default() } },
    };
    SendInput(&[k(0x5B, false), k(vk, false), k(vk, true), k(0x5B, true)], size_of::<INPUT>() as i32);
}

unsafe fn activate(st: &State, i: usize) {
    let Some(a) = st.apps.get(i) else { return };
    let Some(&h) = a.windows.first() else {
        // pinned, not running: start it
        windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), &windows::core::HSTRING::from(a.icon.as_str()), None, None, SW_SHOWNORMAL);
        return;
    };
    if st.active.as_deref() == Some(a.key.as_str()) && !IsIconic(h).as_bool() {
        let _ = ShowWindow(h, SW_MINIMIZE);
    } else {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let _ = SetForegroundWindow(h);
    }
}

pub unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let width = || {
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        rc.right as f32 * 96.0 / GetDpiForWindow(hwnd) as f32
    };
    let dips = |lp: LPARAM| {
        let s = GetDpiForWindow(hwnd) as f32 / 96.0;
        ((lp.0 & 0xFFFF) as i16 as f32 / s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s)
    };
    match msg {
        WM_CREATE => {
            ST.with(|s| refresh(&mut s.borrow_mut()));
            SetTimer(Some(hwnd), T_TICK, 1000, None);
        }
        WM_TIMER => {
            ST.with(|s| refresh(&mut s.borrow_mut()));
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        WM_MOUSEMOVE => {
            let (x, y) = dips(lp);
            let w = width();
            let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            if ST.with(|s| { let mut st = s.borrow_mut(); let h = hit_test(&st, w, x, y); std::mem::replace(&mut st.hover, h) != h }) {
                let _ = InvalidateRect(Some(hwnd), None, false);
                // hover preview for the app under the mouse
                let target = ST.with(|s| {
                    let st = s.borrow();
                    match st.hover {
                        Some(Hit::App(i)) => st.apps.get(i).and_then(|a| a.windows.first().copied()).map(|win| {
                            let sc = GetDpiForWindow(hwnd) as f32 / 96.0;
                            let mut wr = RECT::default();
                            let _ = GetWindowRect(hwnd, &mut wr);
                            let cx = center_x(&st, w) + START_W + GAP + SEARCH_W + GAP + (i as f32 + 0.5) * APP_W;
                            (win, wr.left + (cx * sc) as i32, wr.top)
                        }),
                        _ => None,
                    }
                });
                crate::peek::hover(target);
            }
        }
        WM_MOUSELEAVE => {
            crate::peek::hover(None);
            ST.with(|s| s.borrow_mut().hover = None);
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        WM_LBUTTONUP => {
            let (x, y) = dips(lp);
            let w = width();
            let hit = ST.with(|s| hit_test(&s.borrow(), w, x, y));
            let launcher = HWND(crate::launcher::LAUNCHER.load(Relaxed) as *mut c_void);
            match hit {
                Some(Hit::Start) => { let _ = PostMessageW(Some(launcher), crate::launcher::WM_TOGGLE, WPARAM(0), LPARAM(0)); }
                Some(Hit::Search) => { let _ = PostMessageW(Some(launcher), crate::launcher::WM_TOGGLE, WPARAM(1), LPARAM(0)); }
                Some(Hit::App(i)) => {
                    crate::peek::hover(None);
                    ST.with(|s| activate(&s.borrow(), i))
                }
                Some(Hit::Tray(Tray::Network | Tray::Volume | Tray::Battery)) => {
                    let qs = HWND(crate::quick::QS.load(Relaxed) as *mut c_void);
                    let _ = PostMessageW(Some(qs), crate::quick::WM_QS_TOGGLE, WPARAM(0), LPARAM(0));
                }
                Some(Hit::Tray(Tray::Clock | Tray::Bell)) => { let _ = PostMessageW(Some(HWND(crate::notify::NC.load(Relaxed) as *mut c_void)), crate::notify::WM_NC_TOGGLE, WPARAM(0), LPARAM(0)); }                  // notifications + calendar
                Some(Hit::Tray(Tray::Input)) => press_win(VK_SPACE.0),                                // language switcher
                Some(Hit::Tray(Tray::Desktop)) => {
                    if crate::session::SHELL.load(Relaxed) { crate::session::show_desktop(true) } else { press_win(b'D' as u16) }
                }
                Some(Hit::Tray(Tray::Chevron)) => {
                    if crate::tray::HOST.load(Relaxed) != 0 {
                        crate::tray::toggle_flow(tray_screen_rect(hwnd, Tray::Chevron));
                    } else {
                        press_win(b'B' as u16); // Explorer still owns the tray: open its overflow
                    }
                }
                Some(Hit::Tray(Tray::Icon(i))) => {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    crate::tray::click(i, 0, pt.x, pt.y);
                }
                // an empty part of the bar: close whatever panel is open, like a click anywhere else
                None => crate::launcher::dismiss_outside(POINT { x: -32000, y: -32000 }),
            }
        }
        WM_LBUTTONDBLCLK => {
            let (x, y) = dips(lp);
            let w = width();
            if let Some(Hit::Tray(Tray::Icon(i))) = ST.with(|s| hit_test(&s.borrow(), w, x, y)) {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                crate::tray::click(i, 2, pt.x, pt.y);
            }
        }
        WM_RBUTTONUP => {
            let (x, y) = dips(lp);
            let w = width();
            if let Some(Hit::Tray(Tray::Icon(i))) = ST.with(|s| hit_test(&s.borrow(), w, x, y)) {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                crate::tray::click(i, 1, pt.x, pt.y);
                return LRESULT(0);
            }
            if let Some(Hit::App(i)) = ST.with(|s| hit_test(&s.borrow(), w, x, y)) {
                let (wins, launch, key, pinned) = ST.with(|s| {
                    let st = s.borrow();
                    st.apps.get(i).map(|a| (a.windows.clone(), a.icon.clone(), a.key.clone(), st.pins.iter().flatten().any(|p| p.0 == a.key))).unwrap_or_default()
                });
                crate::peek::hover(None);
                if let Ok(m) = CreatePopupMenu() {
                    // like Windows' jump list: the app's recent items, the app itself (opens a new window), then Close
                    let recent = wins.first().map(|h| jump_list(&app_id(*h, &launch), 8)).unwrap_or_default();
                    if !recent.is_empty() {
                        let _ = AppendMenuW(m, MF_STRING | MF_GRAYED, 0, w!("Recent"));
                        for (k, (label, _)) in recent.iter().enumerate() {
                            let _ = AppendMenuW(m, MF_STRING, 100 + k, &windows::core::HSTRING::from(label.as_str()));
                        }
                        let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
                    }
                    let name = wins.first().map(|h| app_name(*h, &launch)).unwrap_or_default();
                    if !launch.is_empty() && !name.is_empty() {
                        let _ = AppendMenuW(m, MF_STRING, 2, &windows::core::HSTRING::from(name));
                        let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
                    }
                    if !launch.is_empty() {
                        let _ = AppendMenuW(m, MF_STRING, 3, if pinned { w!("Unpin from taskbar") } else { w!("Pin to taskbar") });
                    }
                    if !wins.is_empty() {
                        let _ = AppendMenuW(m, MF_STRING, 1, if wins.len() > 1 { w!("Close all windows") } else { w!("Close window") });
                    }
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    crate::flyout::force_foreground(hwnd); // menus need a foreground owner to close properly
                    match TrackPopupMenu(m, TPM_RETURNCMD | TPM_BOTTOMALIGN, pt.x, pt.y, None, hwnd, None).0 {
                        1 => {
                            for h in wins {
                                let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
                            }
                        }
                        2 => {
                            windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), &windows::core::HSTRING::from(launch.as_str()), None, None, SW_SHOWNORMAL);
                        }
                        3 => {
                            let pins = toggle_pin(ST.with(|s| s.borrow().pins.clone().unwrap_or_default()), &key, &launch);
                            let text: String = pins.iter().map(|(k, t)| format!("{k}\t{t}\r\n")).collect();
                            let _ = std::fs::create_dir_all(pins_path().parent().unwrap());
                            let _ = std::fs::write(pins_path(), text);
                            ST.with(|s| { let mut st = s.borrow_mut(); st.pins = Some(pins); refresh(&mut st); });
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                        n if n >= 100 => {
                            if let Some((_, path)) = recent.get(n as usize - 100) {
                                windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), &windows::core::HSTRING::from(path.as_str()), None, None, SW_SHOWNORMAL);
                            }
                        }
                        _ => {}
                    }
                    let _ = DestroyMenu(m);
                }
            }
        }
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => paint(hwnd),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

/// Live preview: our taskbar on top of Explorer's (Esc-free: quit shellbits with Ctrl+Alt+Q).
pub unsafe fn show_preview(inst: HINSTANCE) -> windows::core::Result<HWND> {
    let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
    let h = CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
        w!("shellbits_taskbar"), w!(""), WS_POPUP | WS_VISIBLE,
        0, sh - 48, sw, 48, None, None, Some(inst), None,
    )?;
    TASKBAR.store(h.0 as isize, Relaxed);
    let _ = crate::peek::create(inst);
    crate::tray::LISTENER.store(h.0 as isize, Relaxed); // repaint when tray icons change
    Ok(h)
}

/// `shellbits --snapshot-taskbar out.bmp`: render the taskbar with live data to a file.
pub unsafe fn snapshot(path: &str) -> windows::core::Result<()> {
    ST.with(|s| -> windows::core::Result<()> {
        let mut st = s.borrow_mut();
        refresh(&mut st);
        let (w, h) = (GetSystemMetrics(SM_CXSCREEN), 48);
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, ..Default::default() },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let dib = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let dc = CreateCompatibleDC(None);
        let old = SelectObject(dc, dib.into());
        draw(&mut st, dc, &RECT { left: 0, top: 0, right: w, bottom: h }, 96.0)?;
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


/// Screen-pixel rectangle of a tray slot (anchors the overflow flyout).
unsafe fn tray_screen_rect(hwnd: HWND, t: Tray) -> RECT {
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    let mut cr = RECT::default();
    let _ = GetClientRect(hwnd, &mut cr);
    let r = tray_rects(cr.right as f32 / s).into_iter().find(|x| x.0 == t).map(|x| x.1).unwrap_or_default();
    let mut wr = RECT::default();
    let _ = GetWindowRect(hwnd, &mut wr);
    RECT { left: wr.left + (r.left * s) as i32, top: wr.top + (r.top * s) as i32, right: wr.left + (r.right * s) as i32, bottom: wr.top + (r.bottom * s) as i32 }
}

/// Shell mode: keep maximized windows off the taskbar (Explorer does this as an app bar).
pub unsafe fn reserve_space() {
    let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
    let h = TASKBAR.load(Relaxed);
    let s = GetDpiForWindow(HWND(h as *mut c_void)) as f32 / 96.0;
    let mut wa = RECT { left: 0, top: 0, right: sw, bottom: sh - (H * s).round() as i32 };
    let _ = SystemParametersInfoW(SPI_SETWORKAREA, 0, Some(&mut wa as *mut _ as *mut c_void), SPIF_SENDCHANGE);
    // minimised windows belong on the taskbar, not as title strips above it (Explorer sets this too)
    let mut mm = MINIMIZEDMETRICS { cbSize: size_of::<MINIMIZEDMETRICS>() as u32, ..Default::default() };
    let _ = SystemParametersInfoW(SPI_GETMINIMIZEDMETRICS, mm.cbSize, Some(&mut mm as *mut _ as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    mm.iArrange = MINIMIZEDMETRICS_ARRANGE(mm.iArrange.0 | ARW_HIDE);
    let _ = SystemParametersInfoW(SPI_SETMINIMIZEDMETRICS, mm.cbSize, Some(&mut mm as *mut _ as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
}

/// The name Windows shows for an app: its Start-menu name, else the exe's description, else the file name.
unsafe fn app_name(h: HWND, launch: &str) -> String {
    if let Some(aumid) = launch.strip_prefix("shell:AppsFolder\\") {
        let n = crate::flyout::app_display_name(aumid);
        if !n.is_empty() {
            return n;
        }
    }
    let _ = h;
    std::path::Path::new(launch).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}


/// The identity Windows files an app's jump list under: the one its window declares, else the one in
/// its Start-menu entry ("shell:AppsFolder\<id>"). Empty when the app has neither.
// ponytail: apps with no declared identity get no recent list here; Windows derives one from the exe path
pub unsafe fn app_id(h: HWND, launch: &str) -> String {
    use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, PSGetPropertyKeyFromName, SHGetPropertyStoreForWindow};
    let mut key = windows::Win32::Foundation::PROPERTYKEY::default();
    if PSGetPropertyKeyFromName(w!("System.AppUserModel.ID"), &mut key).is_ok() {
        if let Ok(store) = SHGetPropertyStoreForWindow::<IPropertyStore>(h) {
            if let Ok(v) = store.GetValue(&key) {
                let id = v.to_string();
                if !id.is_empty() {
                    return id;
                }
            }
        }
    }
    // a Store-style app: Windows knows the identity of its process
    let mut pid = 0u32;
    GetWindowThreadProcessId(h, Some(&mut pid));
    if let Ok(p) = windows::Win32::System::Threading::OpenProcess(windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
        let mut buf = [0u16; 256];
        let mut n = buf.len() as u32;
        let ok = windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId(p, &mut n, Some(windows::core::PWSTR(buf.as_mut_ptr()))).is_ok();
        let _ = CloseHandle(p);
        if ok && n > 1 {
            return String::from_utf16_lossy(&buf[..n as usize - 1]);
        }
    }
    launch.strip_prefix("shell:AppsFolder\\").unwrap_or("").to_string()
}

/// An app's recent items as Windows keeps them for its jump list: (name to show, what to open).
pub unsafe fn jump_list(app_id: &str, max: u32) -> Vec<(String, String)> {
    use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::Common::IObjectArray;
    use windows::Win32::UI::Shell::{ApplicationDocumentLists, IApplicationDocumentLists, IShellItem, IShellLinkW, ADLT_RECENT, SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_NORMALDISPLAY};
    let mut out = Vec::new();
    if app_id.is_empty() {
        return out;
    }
    let Ok(lists) = CoCreateInstance::<_, IApplicationDocumentLists>(&ApplicationDocumentLists, None, CLSCTX_INPROC_SERVER) else { return out };
    if lists.SetAppID(&windows::core::HSTRING::from(app_id)).is_err() {
        return out;
    }
    let Ok(items) = lists.GetList::<IObjectArray>(ADLT_RECENT, max) else { return out };
    for i in 0..items.GetCount().unwrap_or(0) {
        // files and folders come as shell items, an app's own entries as shortcuts
        if let Ok(item) = items.GetAt::<IShellItem>(i) {
            let text = |kind| {
                item.GetDisplayName(kind).ok().map(|p| {
                    let s = p.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(p.0 as *const c_void));
                    s
                })
            };
            if let (Some(name), Some(path)) = (text(SIGDN_NORMALDISPLAY), text(SIGDN_DESKTOPABSOLUTEPARSING)) {
                out.push((name, path));
            }
        } else if let Ok(link) = items.GetAt::<IShellLinkW>(i) {
            let mut buf = [0u16; 520];
            if link.GetPath(&mut buf, std::ptr::null_mut(), 0).is_ok() {
                let path = String::from_utf16_lossy(&buf[..buf.iter().position(|c| *c == 0).unwrap_or(0)]);
                let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.clone());
                if !path.is_empty() {
                    out.push((name, path));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins() {
        let p = parse_pins("a\tshell:AppsFolder\\a\r\nbroken line\n\tno key\nb\tC:\\b.exe\n");
        assert_eq!(p, vec![("a".to_string(), "shell:AppsFolder\\a".to_string()), ("b".to_string(), "C:\\b.exe".to_string())]);
        // pinning adds at the end, pinning again removes
        let q = toggle_pin(p.clone(), "c", "c.exe");
        assert_eq!(q.len(), 3);
        assert_eq!(toggle_pin(q, "a", "").iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["b", "c"]);
        // running apps z and b; pins a and b: pinned first in pin order, a without a window, then the rest
        let run = |k: &str| App { key: k.into(), icon: String::new(), windows: vec![HWND(1 as *mut c_void)] };
        let m = merge_pins(vec![run("z"), run("b")], &p);
        assert_eq!(m.iter().map(|a| (a.key.as_str(), a.windows.len())).collect::<Vec<_>>(), [("a", 0), ("b", 1), ("z", 1)]);
    }
}

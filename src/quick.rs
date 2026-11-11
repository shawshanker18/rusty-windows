//! Windows 11 Quick Settings (+ the media card above it), measured from the real one (DIPs):
//! panel 356x386, 16px from the right edge, 13px above the taskbar, #292929, 1px #3C3C3C border.
//! Tiles 96x48 at x 24/132/240, rows y 23/119, labels at y 88/184. Divider y 213, sliders y 250/303,
//! footer from y 338 (#212121). Media card 356x176 sits 14px above.
//! Real controls: Wi-Fi / Bluetooth radios (Windows.Devices.Radios), volume (Core Audio),
//! brightness (WMI), Live captions, media session (play/pause/skip).

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};
use std::sync::Mutex;

use windows::core::{w, BSTR, HSTRING, PCWSTR};
use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager as Gsmtc;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eMultimedia, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::System::Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::flyout::{glyph, rect, rrect, utf16};

pub const WM_QS_TOGGLE: u32 = WM_APP + 20;
const WM_QS_DATA: u32 = WM_APP + 21;
pub static QS: AtomicIsize = AtomicIsize::new(0);
pub static MEDIA: AtomicIsize = AtomicIsize::new(0);

const W: f32 = 356.0;
const H: f32 = 386.0;
const MH: f32 = 176.0; // media card
const RIGHT: f32 = 16.0;
const BOTTOM: f32 = 13.0;
const MEDIA_GAP: f32 = 14.0;

pub(crate) const fn rgb(v: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: ((v >> 16) & 255) as f32 / 255.0, g: ((v >> 8) & 255) as f32 / 255.0, b: (v & 255) as f32 / 255.0, a: 1.0 }
}
const C_PANEL: D2D1_COLOR_F = rgb(0x292929);
const C_FOOT: D2D1_COLOR_F = rgb(0x212121);
const C_DIV: D2D1_COLOR_F = rgb(0x3B3B3B);
const C_TILE: D2D1_COLOR_F = rgb(0x363636);
const C_TILE_HOVER: D2D1_COLOR_F = rgb(0x3D3D3D);
const C_TILE_EDGE: D2D1_COLOR_F = rgb(0x3E3E3E);
const C_ON: D2D1_COLOR_F = rgb(0x4CC2FF);
const C_ON_HOVER: D2D1_COLOR_F = rgb(0x48B1E8);
const C_TRACK: D2D1_COLOR_F = rgb(0x9E9E9E);
const C_TRACK_OFF: D2D1_COLOR_F = rgb(0x5F5F5F);
const C_THUMB: D2D1_COLOR_F = rgb(0x454545);
const C_DIM: D2D1_COLOR_F = rgb(0x6E6E6E);

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Tile(usize, bool), // tile index, true = the chevron half
    Bright,
    Vol,
    Mute,
    Output,
    Battery,
    Settings,
    Back,
    PageToggle,
    Row(usize),
    RowBtn(usize),
    Footer,
    Prev,
    Play,
    Next,
    MediaApp,
}

/// Live system state, filled by a worker thread (radios / Wi-Fi / Bluetooth are slow-ish calls).
#[derive(Clone, Default)]
struct Sys {
    wifi_on: Option<bool>,
    ssid: String,
    bt_on: Option<bool>,
    bt_device: String,
    brightness: Option<u32>,
    battery: Option<(u8, bool)>,
    media: Option<Media>,
    airplane: Option<bool>,
    saver: bool,
}

#[derive(Clone, Copy, PartialEq, Default, Debug)]
enum Page {
    #[default]
    Main,
    Wifi,
    Bt,
    Output,
    Access,
    Perf,
    Temps,
}
static PAGE_ITEMS: Mutex<Option<(Page, Vec<crate::qsx::Item>)>> = Mutex::new(None);

#[derive(Clone, Default)]
struct Media {
    app: String,
    app_icon: String,
    title: String,
    artist: String,
    playing: bool,
    can_prev: bool,
    can_next: bool,
    thumb: Option<(u32, u32, Vec<u8>)>,
}

static SYS: Mutex<Option<Sys>> = Mutex::new(None);

#[derive(Default)]
struct State {
    sys: Sys,
    volume: f32,
    muted: bool,
    hover: Option<Hit>,
    drag: Option<Hit>,
    gfx: Option<Gfx>,
    icons: std::collections::HashMap<String, Vec<u8>>,
    page: Page,
    items: Vec<crate::qsx::Item>,
    sel: Option<usize>,
    pscroll: f32,
}

struct Gfx {
    rt: ID2D1DCRenderTarget,
    icon16: IDWriteTextFormat,
    icon12: IDWriteTextFormat,
    small_c: IDWriteTextFormat,
    small_r: IDWriteTextFormat,
    small: IDWriteTextFormat,
    body: IDWriteTextFormat,
    body_sb: IDWriteTextFormat,
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
    static SHOWN_AT: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
}
const WM_QS_REFOCUS: u32 = WM_APP + 22;

// ======================= system calls =======================

pub(crate) unsafe fn endpoint() -> windows::core::Result<IAudioEndpointVolume> {
    let e: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    e.GetDefaultAudioEndpoint(eRender, eMultimedia)?.Activate(CLSCTX_ALL, None)
}

pub(crate) fn radios() -> windows::core::Result<Vec<windows::Devices::Radios::Radio>> {
    use windows::Devices::Radios::Radio;
    let _ = Radio::RequestAccessAsync()?.get();
    Ok(Radio::GetRadiosAsync()?.get()?.into_iter().collect())
}

pub(crate) fn set_radio(kind: windows::Devices::Radios::RadioKind, on: bool) {
    use windows::Devices::Radios::RadioState;
    std::thread::spawn(move || {
        if let Ok(rs) = radios() {
            for r in rs.iter().filter(|r| r.Kind().ok() == Some(kind)) {
                let _ = r.SetStateAsync(if on { RadioState::On } else { RadioState::Off }).and_then(|a| a.get());
            }
        }
        collect_async();
    });
}

/// Connected Wi-Fi network name (WLAN API).
pub(crate) fn wifi_ssid() -> String {
    use windows::Win32::NetworkManagement::WiFi::*;
    unsafe {
        let mut ver = 0;
        let mut h = HANDLE::default();
        if WlanOpenHandle(2, None, &mut ver, &mut h) != 0 {
            return String::new();
        }
        let mut out = String::new();
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        if WlanEnumInterfaces(h, None, &mut list) == 0 && !list.is_null() {
            let l = &*list;
            let infos = std::slice::from_raw_parts(l.InterfaceInfo.as_ptr(), l.dwNumberOfItems as usize);
            for i in infos {
                if i.isState != wlan_interface_state_connected {
                    continue;
                }
                let mut size = 0;
                let mut data: *mut c_void = std::ptr::null_mut();
                if WlanQueryInterface(h, &i.InterfaceGuid, wlan_intf_opcode_current_connection, None, &mut size, &mut data, None) == 0 && !data.is_null() {
                    let a = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
                    let ssid = &a.wlanAssociationAttributes.dot11Ssid;
                    out = String::from_utf8_lossy(&ssid.ucSSID[..ssid.uSSIDLength as usize]).to_string();
                    WlanFreeMemory(data);
                }
            }
            WlanFreeMemory(list as *const c_void);
        }
        WlanCloseHandle(h, None);
        out
    }
}

/// First connected Bluetooth device's name (classic or Low Energy), as Settings lists them.
pub(crate) fn bt_device() -> String {
    // the connected device, from the instant device-tree listing (the old query here took 30 s and held up every open)
    crate::qsx::bt_devices().into_iter().find(|d| d.on).map(|d| d.name).unwrap_or_default()
}

/// Laptop panel brightness through WMI (external monitors don't report one).
unsafe fn wmi() -> windows::core::Result<windows::Win32::System::Wmi::IWbemServices> {
    use windows::Win32::System::Wmi::*;
    let loc: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)?;
    let svc = loc.ConnectServer(&BSTR::from("root\\wmi"), &BSTR::new(), &BSTR::new(), &BSTR::new(), 0, &BSTR::new(), None)?;
    CoSetProxyBlanket(&svc, RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE, None, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE, None, EOAC_NONE)?;
    Ok(svc)
}

/// Model id of the main display's monitor, e.g. "CMN1521" (laptop panel) or "GSM5BBF" (LG).
unsafe fn primary_monitor_model() -> Option<String> {
    use windows::Win32::Graphics::Gdi::{EnumDisplayDevicesW, DISPLAY_DEVICEW, DISPLAY_DEVICE_PRIMARY_DEVICE};
    for i in 0..8 {
        let mut a = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
        if !EnumDisplayDevicesW(None, i, &mut a, 0).as_bool() {
            break;
        }
        if (a.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE).0 == 0 {
            continue;
        }
        let mut m = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
        if EnumDisplayDevicesW(PCWSTR(a.DeviceName.as_ptr()), 0, &mut m, 0).as_bool() {
            let id = String::from_utf16_lossy(&m.DeviceID).trim_end_matches('\0').to_string(); // MONITOR\GSM5BBF\{...}
            return id.split('\\').nth(1).map(str::to_string);
        }
    }
    None
}

pub(crate) unsafe fn get_brightness() -> Option<u32> {
    use windows::Win32::System::Wmi::*;
    let svc = wmi().ok()?;
    let en = svc.ExecQuery(&BSTR::from("WQL"), &BSTR::from("SELECT CurrentBrightness, InstanceName FROM WmiMonitorBrightness"), WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY, None).ok()?;
    let mut objs = [None];
    let mut n = 0;
    let _ = en.Next(WBEM_INFINITE, &mut objs, &mut n);
    let obj = objs[0].take()?;
    let mut name = windows::Win32::System::Variant::VARIANT::default();
    obj.Get(w!("InstanceName"), 0, &mut name, None, None).ok()?;
    let inst = BSTR::try_from(&name).map(|b| b.to_string()).unwrap_or_default(); // DISPLAY\CMN1521\...
    let panel = inst.split('\\').nth(1).unwrap_or("").to_string();
    if primary_monitor_model().is_some_and(|m| !m.eq_ignore_ascii_case(&panel)) {
        return None; // main screen is an external monitor: Windows greys the slider out
    }
    let mut v = windows::Win32::System::Variant::VARIANT::default();
    obj.Get(w!("CurrentBrightness"), 0, &mut v, None, None).ok()?;
    u32::try_from(&v).ok()
}

pub(crate) fn set_brightness(level: u32) {
    std::thread::spawn(move || unsafe {
        use windows::Win32::System::Wmi::*;
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let r = (|| -> windows::core::Result<()> {
            let svc = wmi()?;
            let en = svc.ExecQuery(&BSTR::from("WQL"), &BSTR::from("SELECT * FROM WmiMonitorBrightnessMethods"), WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY, None)?;
            let mut objs = [None];
            let mut n = 0;
            let _ = en.Next(WBEM_INFINITE, &mut objs, &mut n);
            let inst = objs[0].take().ok_or(windows::core::Error::from(E_FAIL))?;
            let mut path = windows::Win32::System::Variant::VARIANT::default();
            inst.Get(w!("__PATH"), 0, &mut path, None, None)?;
            let mut class = None;
            svc.GetObject(&BSTR::from("WmiMonitorBrightnessMethods"), WBEM_FLAG_RETURN_WBEM_COMPLETE, None, Some(&mut class), None)?;
            let class = class.ok_or(windows::core::Error::from(E_FAIL))?;
            let mut inp = None;
            class.GetMethod(w!("WmiSetBrightness"), 0, &mut inp, std::ptr::null_mut())?;
            let params = inp.ok_or(windows::core::Error::from(E_FAIL))?.SpawnInstance(0)?;
            params.Put(w!("Timeout"), 0, &windows::Win32::System::Variant::VARIANT::from(0u32), 0)?;
            params.Put(w!("Brightness"), 0, &windows::Win32::System::Variant::VARIANT::from(level.min(100) as u8), 0)?;
            let p = BSTR::try_from(&path)?;
            svc.ExecMethod(&p, &BSTR::from("WmiSetBrightness"), Default::default(), None, &params, None, None)?;
            Ok(())
        })();
        let _ = r;
    });
}

pub(crate) fn battery() -> Option<(u8, bool)> {
    unsafe {
        let mut s = windows::Win32::System::Power::SYSTEM_POWER_STATUS::default();
        windows::Win32::System::Power::GetSystemPowerStatus(&mut s).ok()?;
        (s.BatteryFlag & 128 == 0 && s.BatteryLifePercent <= 100).then_some((s.BatteryLifePercent, s.ACLineStatus == 1))
    }
}

/// Current media session (what Windows shows in the card above Quick Settings).
unsafe fn media() -> Option<Media> {
    let s = Gsmtc::RequestAsync().ok()?.get().ok()?.GetCurrentSession().ok()?;
    let p = s.TryGetMediaPropertiesAsync().ok()?.get().ok()?;
    let info = s.GetPlaybackInfo().ok()?;
    let ctl = info.Controls().ok()?;
    let aumid = s.SourceAppUserModelId().ok()?.to_string();
    let app = crate::flyout::app_display_name(&aumid);
    let thumb = p.Thumbnail().ok().and_then(|t| crate::flyout::decode_thumb(&t).ok());
    Some(Media {
        app,
        app_icon: format!("shell:AppsFolder\\{aumid}"),
        title: p.Title().ok()?.to_string(),
        artist: p.Artist().map(|a| a.to_string()).unwrap_or_default(),
        playing: info.PlaybackStatus().ok()? == windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing,
        can_prev: ctl.IsPreviousEnabled().unwrap_or(false),
        can_next: ctl.IsNextEnabled().unwrap_or(false),
        thumb,
    })
}

fn collect() -> Sys {
    use windows::Devices::Radios::{RadioKind, RadioState};
    let rs = radios().unwrap_or_default();
    let state = |k: RadioKind| rs.iter().find(|r| r.Kind().ok() == Some(k)).map(|r| r.State().ok() == Some(RadioState::On));
    Sys {
        wifi_on: state(RadioKind::WiFi),
        ssid: wifi_ssid(),
        bt_on: state(RadioKind::Bluetooth),
        bt_device: bt_device(),
        brightness: unsafe { get_brightness() },
        battery: battery(),
        media: unsafe { media() },
        airplane: crate::qsx::airplane(),
        saver: crate::qsx::saver_on(),
    }
}

static COLLECTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn collect_async() {
    // one at a time: a Windows query that stalls must not pile up a new thread on every open
    if COLLECTING.swap(true, Relaxed) {
        return;
    }
    std::thread::spawn(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let s = collect();
        COLLECTING.store(false, Relaxed);
        *SYS.lock().unwrap() = Some(s);
        for h in [QS.load(Relaxed), MEDIA.load(Relaxed)] {
            let _ = PostMessageW(Some(HWND(h as *mut c_void)), WM_QS_DATA, WPARAM(0), LPARAM(0));
        }
    });
}

fn media_cmd(id: i32) {
    std::thread::spawn(move || {
        let r = (|| -> windows::core::Result<()> {
            let s = Gsmtc::RequestAsync()?.get()?.GetCurrentSession()?;
            match id {
                1 => { s.TryTogglePlayPauseAsync()?.get()?; }
                2 => { s.TrySkipNextAsync()?.get()?; }
                _ => { s.TrySkipPreviousAsync()?.get()?; }
            }
            Ok(())
        })();
        let _ = r;
        std::thread::sleep(std::time::Duration::from_millis(300));
        collect_async();
    });
}

pub(crate) fn open(uri: &str) {
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(uri), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

// ======================= layout =======================

const TILE_X: [f32; 3] = [24.0, 132.0, 240.0];
const TILE_Y: [f32; 2] = [23.0, 119.0];
const SL_X0: f32 = 57.0;
const SL_X1: f32 = 301.0;
const BRIGHT_Y: f32 = 250.0;
const VOL_Y: f32 = 303.0;
const FOOT_Y: f32 = 338.0;

fn tile_rect(i: usize) -> D2D_RECT_F {
    let (x, y) = (TILE_X[i % 3], TILE_Y[i / 3]);
    rect(x, y, x + 96.0, y + 48.0)
}

fn is_split(i: usize) -> bool {
    i < 2 // Wi-Fi, Bluetooth
}

fn qs_hit(st: &State, x: f32, y: f32) -> Option<Hit> {
    if st.page != Page::Main {
        return page_hit(st, x, y);
    }
    for i in 0..6 {
        let r = tile_rect(i);
        if x >= r.left && x < r.right && y >= r.top && y < r.bottom {
            return Some(Hit::Tile(i, is_split(i) && x >= r.left + 48.0));
        }
    }
    if (BRIGHT_Y - 14.0..BRIGHT_Y + 14.0).contains(&y) && (SL_X0 - 10.0..SL_X1 + 10.0).contains(&x) {
        return Some(Hit::Bright);
    }
    if (VOL_Y - 14.0..VOL_Y + 14.0).contains(&y) {
        if (18.0..48.0).contains(&x) {
            return Some(Hit::Mute);
        }
        if (SL_X0 - 10.0..SL_X1 + 10.0).contains(&x) {
            return Some(Hit::Vol);
        }
        if (310.0..346.0).contains(&x) {
            return Some(Hit::Output);
        }
    }
    if y >= FOOT_Y {
        if (16.0..100.0).contains(&x) {
            return Some(Hit::Battery);
        }
        if (316.0..340.0).contains(&x) {
            return Some(Hit::Settings);
        }
    }
    None
}

fn media_hit(x: f32, y: f32) -> Option<Hit> {
    if (120.0..152.0).contains(&y) {
        for (cx, h) in [(113.5, Hit::Prev), (179.5, Hit::Play), (245.5, Hit::Next)] {
            if (cx - 20.0..cx + 20.0).contains(&x) {
                return Some(h);
            }
        }
    }
    if (14.0..38.0).contains(&y) && (16.0..200.0).contains(&x) {
        return Some(Hit::MediaApp);
    }
    None
}

fn slider_value(x: f32) -> f32 {
    ((x - SL_X0) / (SL_X1 - SL_X0)).clamp(0.0, 1.0)
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
    Ok(Gfx {
        icon16: fmt(icons, 16.0, n, c)?,
        icon12: fmt(icons, 12.0, n, c)?,
        small_c: fmt(text, 12.0, n, c)?,
        small_r: fmt(text, 12.0, n, DWRITE_TEXT_ALIGNMENT_TRAILING)?,
        small: fmt(text, 12.0, n, l)?,
        body: fmt(text, 14.0, n, l)?,
        body_sb: fmt(text, 14.0, sb, l)?,
        rt,
    })
}

unsafe fn draw_qs(st: &State, g: &Gfx) -> windows::core::Result<()> {
    if st.page != Page::Main {
        return draw_page(st, g);
    }
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
    let s = &st.sys;
    let wifi_label = match (s.wifi_on, s.ssid.is_empty()) {
        (Some(true), false) => s.ssid.clone(),
        (Some(true), true) => "Available".into(),
        _ => "Wi-Fi".into(),
    };
    let bt_label = match (s.bt_on, s.bt_device.is_empty()) {
        (Some(true), false) => s.bt_device.clone(),
        _ => "Bluetooth".into(),
    };
    let tiles: [(u32, String, bool); 6] = [
        (0xE701, wifi_label, s.wifi_on == Some(true)),
        (0xE702, bt_label, s.bt_on == Some(true)),
        (0xE709, "Airplane mode".into(), s.airplane == Some(true)),
        (0xEC4A, perf_label(&crate::perf::latest()), false),
        (0xE863, "Energy saver".into(), s.saver),
        (0xE9CA, temps_label(&crate::perf::latest()), false),
    ];
    for (i, (gl, label, on)) in tiles.iter().enumerate() {
        let r = tile_rect(i);
        let hov = matches!(st.hover, Some(Hit::Tile(j, _)) if j == i);
        let fill = if *on { if hov { C_ON_HOVER } else { C_ON } } else if hov { C_TILE_HOVER } else { C_TILE };
        rt.FillRoundedRectangle(&rrect(r, 4.0), &brush(fill)?);
        if !*on {
            rt.DrawRoundedRectangle(&rrect(rect(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5), 4.0), &brush(C_TILE_EDGE)?, 1.0, None);
        }
        let fg = if *on { &black } else { &white };
        if is_split(i) {
            icon(*gl, &g.icon16, rect(r.left, r.top, r.left + 48.0, r.bottom), fg);
            rt.FillRectangle(&rect(r.left + 48.0, r.top, r.left + 49.0, r.bottom), &brush(if *on { rgb(0x3E9FD3) } else { rgb(0x2E2E2E) })?);
            icon(0xE76C, &g.icon12, rect(r.left + 48.0, r.top, r.right, r.bottom), fg);
        } else if i == 3 || i == 5 {
            icon(*gl, &g.icon16, rect(r.left + 28.0, r.top, r.left + 52.0, r.bottom), fg);
            icon(0xE76C, &g.icon12, rect(r.left + 50.0, r.top, r.left + 64.0, r.bottom), fg);
        } else {
            icon(*gl, &g.icon16, r, fg);
        }
        let ly = TILE_Y[i / 3] + 65.0;
        text(label, &g.small_c, rect(r.left - 6.0, ly - 9.0, r.right + 6.0, ly + 9.0), &white);
    }
    // edit-page dots + chevron at the right edge
    for (y, a) in [(99.0, 1.0), (114.0, 0.6)] {
        rt.FillEllipse(&D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: 346.0, Y: y }, radiusX: 2.0, radiusY: 2.0 }, &rt.CreateSolidColorBrush(&D2D1_COLOR_F { a, ..rgb(0xFFFFFF) }, None)?)
    }
    {
        let f: ID2D1Factory = rt.GetFactory()?;
        let path = f.CreatePathGeometry()?;
        let sink = path.Open()?;
        sink.BeginFigure(windows_numerics::Vector2 { X: 342.0, Y: 126.0 }, D2D1_FIGURE_BEGIN_FILLED);
        sink.AddLine(windows_numerics::Vector2 { X: 350.0, Y: 126.0 });
        sink.AddLine(windows_numerics::Vector2 { X: 346.0, Y: 131.0 });
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        sink.Close()?;
        rt.FillGeometry(&path, &white, None);
    }

    rt.FillRectangle(&rect(0.0, 213.0, W, 214.0), &brush(C_DIV)?);
    // sliders
    let slider = |y: f32, v: Option<f32>, accent: bool, active: bool| -> windows::core::Result<()> {
        let on = v.is_some();
        let v = v.unwrap_or(0.0);
        let x = SL_X0 + (SL_X1 - SL_X0) * v;
        rt.FillRoundedRectangle(&rrect(rect(SL_X0, y - 2.0, SL_X1, y + 2.0), 2.0), &brush(if on { C_TRACK } else { C_TRACK_OFF })?);
        if accent && v > 0.0 {
            rt.FillRoundedRectangle(&rrect(rect(SL_X0, y - 2.0, x, y + 2.0), 2.0), &brush(C_ON)?);
        }
        let pt = windows_numerics::Vector2 { X: x, Y: y };
        rt.FillEllipse(&D2D1_ELLIPSE { point: pt, radiusX: 10.0, radiusY: 10.0 }, &brush(C_THUMB)?);
        rt.DrawEllipse(&D2D1_ELLIPSE { point: pt, radiusX: 10.0, radiusY: 10.0 }, &brush(rgb(0x505050))?, 1.0, None);
        let r = if active { 7.0 } else { 5.0 };
        rt.FillEllipse(&D2D1_ELLIPSE { point: pt, radiusX: r, radiusY: r }, &brush(if accent && on { C_ON } else { C_TRACK_OFF })?);
        Ok(())
    };
    icon(0xE706, &g.icon16, rect(21.0, BRIGHT_Y - 12.0, 45.0, BRIGHT_Y + 12.0), &brush(if s.brightness.is_some() { rgb(0xFFFFFF) } else { C_DIM })?);
    slider(BRIGHT_Y, s.brightness.map(|b| b as f32 / 100.0), true, matches!(st.hover.or(st.drag), Some(Hit::Bright)))?;
    let vol_glyph = if st.muted { 0xE74F } else { [0xE992, 0xE993, 0xE994, 0xE995][match (st.volume * 100.0).round() as i32 { 0 => 0, 1..=33 => 1, 34..=66 => 2, _ => 3 }] };
    if st.hover == Some(Hit::Mute) {
        rt.FillRoundedRectangle(&rrect(rect(19.0, VOL_Y - 14.0, 47.0, VOL_Y + 14.0), 4.0), &brush(C_TILE)?);
    }
    icon(vol_glyph, &g.icon16, rect(21.0, VOL_Y - 12.0, 45.0, VOL_Y + 12.0), &white);
    slider(VOL_Y, Some(if st.muted { 0.0 } else { st.volume }), true, matches!(st.hover.or(st.drag), Some(Hit::Vol)))?;
    if st.hover == Some(Hit::Output) {
        rt.FillRoundedRectangle(&rrect(rect(310.0, VOL_Y - 14.0, 346.0, VOL_Y + 14.0), 4.0), &brush(C_TILE)?);
    }
    icon(0xE9E9, &g.icon16, rect(312.0, VOL_Y - 12.0, 330.0, VOL_Y + 12.0), &white);
    icon(0xE76C, &g.icon12, rect(328.0, VOL_Y - 12.0, 342.0, VOL_Y + 12.0), &white);

    // footer
    // live: a light shade over the see-through panel (Windows' footer is a few levels darker than the
    // body and tinted by what is behind); snapshots have no backdrop, so the flat colour stands in
    let foot = if crate::flyout::LIVE.load(Relaxed) { D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.10 } } else { C_FOOT };
    rt.FillRectangle(&rect(0.0, FOOT_Y, W, H), &brush(foot)?);
    let fy = FOOT_Y + 24.0;
    if let Some((pct, charging)) = s.battery {
        let (bx, by) = (26.0, fy - 5.5);
        rt.DrawRoundedRectangle(&rrect(rect(bx, by, bx + 18.0, by + 11.0), 2.0), &white, 1.0, None);
        rt.FillRoundedRectangle(&rrect(rect(bx + 18.5, by + 3.5, bx + 20.0, by + 7.5), 0.5), &white);
        rt.FillRoundedRectangle(&rrect(rect(bx + 2.0, by + 2.0, bx + 2.0 + 14.0 * pct as f32 / 100.0, by + 9.0), 1.0), &brush(if charging { rgb(0x8BD48A) } else { rgb(0xFFFFFF) })?);
        if charging {
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
            rt.DrawGeometry(&path, &brush(C_FOOT)?, 1.5, None);
            rt.FillGeometry(&path, &white, None);
        }
        text(&format!("{pct}%"), &g.small, rect(48.0, fy - 10.0, 120.0, fy + 10.0), &white);
    }
    if st.hover == Some(Hit::Settings) {
        rt.FillRoundedRectangle(&rrect(rect(316.0, fy - 14.0, 340.0, fy + 14.0), 4.0), &brush(C_TILE)?);
    }
    icon(0xE713, &g.icon16, rect(316.0, fy - 12.0, 340.0, fy + 12.0), &white);
    Ok(())
}

unsafe fn draw_media(st: &State, g: &Gfx) -> windows::core::Result<()> {
    let rt = &g.rt;
    rt.Clear(Some(&crate::flyout::base(C_PANEL)));
    let Some(m) = &st.sys.media else { return Ok(()) };
    let brush = |c: D2D1_COLOR_F| rt.CreateSolidColorBrush(&c, None);
    let white = brush(rgb(0xFFFFFF))?;
    let text = |s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush| {
        rt.DrawText(&utf16(s), f, &r, b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL)
    };
    let bp = D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
    };
    if let Some(px) = st.icons.get(&m.app_icon) {
        if let Ok(b) = rt.CreateBitmap(D2D_SIZE_U { width: 16, height: 16 }, Some(px.as_ptr() as *const c_void), 64, &bp) {
            crate::flyout::draw_icon(rt, &b, 24.0, 18.0, 16.0);
        }
    }
    text(&m.app, &g.body, rect(56.0, 14.0, 300.0, 38.0), &white);
    text(&m.title, &g.body_sb, rect(24.0, 57.0, 262.0, 79.0), &white);
    text(&m.artist, &g.body, rect(24.0, 76.0, 262.0, 96.0), &white);
    if let Some((tw, th, px)) = &m.thumb {
        if let Ok(b) = rt.CreateBitmap(D2D_SIZE_U { width: *tw, height: *th }, Some(px.as_ptr() as *const c_void), tw * 4, &bp) {
            // fit into a 58px-wide box on the right, keep aspect, rounded corners
            let w = 58.0;
            let h = (w * *th as f32 / *tw as f32).min(58.0);
            let r = rect(334.0 - w, 74.0 - h / 2.0, 334.0, 74.0 + h / 2.0);
            let bb = rt.CreateBitmapBrush(&b, None, None)?;
            bb.SetTransform(&windows_numerics::Matrix3x2 { M11: w / *tw as f32, M12: 0.0, M21: 0.0, M22: h / *th as f32, M31: r.left, M32: r.top });
            rt.FillRoundedRectangle(&rrect(r, 4.0), &bb);
        }
    }
    let dim = brush(C_DIM)?;
    for (cx, gl, h, enabled) in [(113.5, 0xF8AC, Hit::Prev, m.can_prev), (179.5, if m.playing { 0xF8AE } else { 0xF5B0 }, Hit::Play, true), (245.5, 0xF8AD, Hit::Next, m.can_next)] {
        if st.hover == Some(h) && enabled {
            rt.FillRoundedRectangle(&rrect(rect(cx - 20.0, 120.0, cx + 20.0, 152.0), 4.0), &brush(C_TILE)?);
        }
        rt.DrawText(&glyph(gl), &g.icon16, &rect(cx - 20.0, 120.0, cx + 20.0, 152.0), if enabled { &white } else { &dim }, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
    }
    Ok(())
}

unsafe fn paint(hwnd: HWND, media: bool) {
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
                if media { draw_media(&st, g)?; } else { draw_qs(&st, g)?; }
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
    if let Ok(v) = endpoint() {
        st.volume = v.GetMasterVolumeLevelScalar().unwrap_or(0.0);
        st.muted = v.GetMute().map(|b| b.as_bool()).unwrap_or(false);
    }
    if let Some(s) = SYS.lock().unwrap().clone() {
        st.sys = s;
    }
    st.sys.battery = battery();
    // media app icon (16px)
    if let Some(m) = &st.sys.media {
        if !st.icons.contains_key(&m.app_icon) {
            if let Ok(wic) = CoCreateInstance::<_, IWICImagingFactory>(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) {
                if let Ok(px) = crate::launcher::icon_pixels(&wic, &m.app_icon, 16) {
                    st.icons.insert(m.app_icon.clone(), px);
                }
            }
        }
    }
}

unsafe fn place(qs: HWND) {
    let mut wa = RECT::default();
    let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut wa as *mut _ as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    let s = GetDpiForWindow(qs) as f32 / 96.0;
    let (w, h, mh) = ((W * s).round() as i32, (H * s).round() as i32, (MH * s).round() as i32);
    let x = wa.right - w - (RIGHT * s).round() as i32;
    let y = wa.bottom - h - (BOTTOM * s).round() as i32;
    let _ = SetWindowPos(qs, Some(HWND_TOPMOST), x, y, w, h, SWP_SHOWWINDOW);
    let media = HWND(MEDIA.load(Relaxed) as *mut c_void);
    let has_media = ST.with(|s| s.borrow().sys.media.is_some());
    if has_media {
        let _ = SetWindowPos(media, Some(HWND_TOPMOST), x, y - mh - (MEDIA_GAP * s).round() as i32, w, mh, SWP_SHOWWINDOW | SWP_NOACTIVATE);
    } else {
        let _ = ShowWindow(media, SW_HIDE);
    }
}

pub unsafe fn toggle(qs: HWND) {
    if IsWindowVisible(qs).as_bool() {
        hide(qs);
        return;
    }
    ST.with(|s| refresh_local(&mut s.borrow_mut()));
    place(qs);
    SHOWN_AT.with(|t| t.set(Some(std::time::Instant::now())));
    crate::flyout::force_foreground(qs);
    let _ = UpdateWindow(qs);
    crate::flyout::slide(&[qs, HWND(MEDIA.load(Relaxed) as *mut c_void)], true);
    collect_async(); // radios, Wi-Fi name, Bluetooth device, brightness, media: arrive in a moment
    crate::perf::start(post_qs);
}

unsafe fn hide(qs: HWND) {
    ST.with(|s| { let mut st = s.borrow_mut(); st.page = Page::Main; st.items.clear(); });
    crate::perf::stop();
    crate::flyout::slide(&[qs, HWND(MEDIA.load(Relaxed) as *mut c_void)], false);
    let _ = ShowWindow(qs, SW_HIDE);
    let _ = ShowWindow(HWND(MEDIA.load(Relaxed) as *mut c_void), SW_HIDE);
    crate::flyout::give_back();
    let _ = windows::Win32::System::ProcessStatus::K32EmptyWorkingSet(windows::Win32::System::Threading::GetCurrentProcess());
}

unsafe fn set_volume(v: f32) {
    if let Ok(e) = endpoint() {
        let _ = e.SetMasterVolumeLevelScalar(v, std::ptr::null());
        if v > 0.0 {
            let _ = e.SetMute(false, std::ptr::null());
        }
    }
}

pub unsafe extern "system" fn qs_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let dips = |lp: LPARAM| {
        let s = GetDpiForWindow(hwnd) as f32 / 96.0;
        ((lp.0 & 0xFFFF) as i16 as f32 / s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s)
    };
    let redraw = || {
        let _ = InvalidateRect(Some(hwnd), None, false);
    };
    match msg {
        WM_QS_TOGGLE => toggle(hwnd),
        crate::flyout::WM_DISMISS => hide(hwnd),
        WM_QS_DATA => {
            if let Some((p, items)) = PAGE_ITEMS.lock().unwrap().take() {
                ST.with(|s| { let mut st = s.borrow_mut(); if st.page == p { st.items = items; } });
            }
            ST.with(|s| refresh_local(&mut s.borrow_mut()));
            if IsWindowVisible(hwnd).as_bool() {
                place(hwnd);
            }
            redraw();
        }
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            // the Win key's release can steal focus right after we open: take it back once
            let fresh = SHOWN_AT.with(|t| t.get()).is_some_and(|t| t.elapsed().as_millis() < 400);
            let to = HWND(lp.0 as *mut c_void);
            if fresh {
                let _ = PostMessageW(Some(hwnd), WM_QS_REFOCUS, WPARAM(0), LPARAM(0));
            } else if to.0 as isize != MEDIA.load(Relaxed) && GetCapture() != hwnd {
                hide(hwnd);
            }
        }
        WM_QS_REFOCUS => crate::flyout::force_foreground(hwnd),
        WM_KEYDOWN if wp.0 as u16 == windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE.0 => {
            if ST.with(|s| s.borrow().page) != Page::Main {
                ST.with(|s| s.borrow_mut().page = Page::Main);
                redraw();
            } else {
                hide(hwnd);
            }
        }
        WM_MOUSEMOVE => {
            let (x, y) = dips(lp);
            let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            let drag = ST.with(|s| s.borrow().drag);
            match drag {
                Some(Hit::Vol) => {
                    let v = slider_value(x);
                    set_volume(v);
                    ST.with(|s| { let mut st = s.borrow_mut(); st.volume = v; st.muted = false; });
                    redraw();
                }
                Some(Hit::Bright) => {
                    let v = (slider_value(x) * 100.0).round() as u32;
                    ST.with(|s| s.borrow_mut().sys.brightness = Some(v));
                    redraw();
                }
                _ => {
                    if ST.with(|s| { let mut st = s.borrow_mut(); let h = qs_hit(&st, x, y); std::mem::replace(&mut st.hover, h) != h }) {
                        redraw();
                    }
                }
            }
        }
        WM_MOUSELEAVE => {
            ST.with(|s| s.borrow_mut().hover = None);
            redraw();
        }
        WM_LBUTTONDOWN => {
            let (x, y) = dips(lp);
            let h = ST.with(|s| qs_hit(&s.borrow(), x, y));
            if matches!(h, Some(Hit::Vol | Hit::Bright)) {
                SetCapture(hwnd);
                ST.with(|s| s.borrow_mut().drag = h);
                let _ = PostMessageW(Some(hwnd), WM_MOUSEMOVE, wp, lp);
            }
        }
        WM_LBUTTONUP => {
            let (x, y) = dips(lp);
            if let Some(d) = ST.with(|s| s.borrow_mut().drag.take()) {
                let _ = ReleaseCapture();
                if d == Hit::Bright {
                    set_brightness((slider_value(x) * 100.0).round() as u32);
                }
                redraw();
                return LRESULT(0);
            }
            let (wifi, bt) = ST.with(|s| { let st = s.borrow(); (st.sys.wifi_on, st.sys.bt_on) });
            use windows::Devices::Radios::RadioKind;
            let hit = ST.with(|s| qs_hit(&s.borrow(), x, y));
            if ST.with(|s| s.borrow().page) != Page::Main {
                LAST_X.with(|c| c.set(x));
                page_click(hwnd, hit);
                redraw();
                return LRESULT(0);
            }
            match hit {
                Some(Hit::Tile(0, false)) => { set_radio(RadioKind::WiFi, wifi != Some(true)); ST.with(|s| s.borrow_mut().sys.wifi_on = Some(wifi != Some(true))); }
                Some(Hit::Tile(0, true)) => open_page(hwnd, Page::Wifi),
                Some(Hit::Tile(1, false)) => { set_radio(RadioKind::Bluetooth, bt != Some(true)); ST.with(|s| s.borrow_mut().sys.bt_on = Some(bt != Some(true))); }
                Some(Hit::Tile(1, true)) => open_page(hwnd, Page::Bt),
                Some(Hit::Tile(2, _)) => {
                    let on = ST.with(|s| s.borrow().sys.airplane) != Some(true);
                    crate::qsx::set_airplane(on);
                    ST.with(|s| s.borrow_mut().sys.airplane = Some(on));
                    later(1500);
                }
                Some(Hit::Tile(3, _)) => open_page(hwnd, Page::Perf),
                Some(Hit::Tile(4, _)) => {
                    let on = !ST.with(|s| s.borrow().sys.saver);
                    ST.with(|s| s.borrow_mut().sys.saver = on);
                    std::thread::spawn(move || {
                        if !crate::qsx::set_saver(on) {
                            open("ms-settings:batterysaver"); // this PC only allows it on battery
                        }
                        collect_async();
                    });
                }
                Some(Hit::Tile(5, _)) => open_page(hwnd, Page::Temps),
                Some(Hit::Mute) => {
                    if let Ok(e) = endpoint() {
                        let m = e.GetMute().map(|b| b.as_bool()).unwrap_or(false);
                        let _ = e.SetMute(!m, std::ptr::null());
                        ST.with(|s| s.borrow_mut().muted = !m);
                    }
                }
                Some(Hit::Output) => open_page(hwnd, Page::Output),
                Some(Hit::Battery) => { hide(hwnd); open("ms-settings:batterysaver"); }
                Some(Hit::Settings) => { hide(hwnd); open("ms-settings:"); }
                _ => {}
            }
            redraw();
        }
        WM_MOUSEWHEEL if ST.with(|s| s.borrow().page) != Page::Main => {
            let notches = ((wp.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
            ST.with(|s| {
                let mut st = s.borrow_mut();
                let total = rows(&st).last().map(|r| r.0 + r.1).unwrap_or(0.0);
                st.pscroll = (st.pscroll - notches * 48.0).clamp(0.0, (total - (FOOT_Y - HDR)).max(0.0));
            });
            redraw();
        }
        WM_MOUSEWHEEL => {
            // like Windows: scrolling over the panel nudges the volume
            let notches = ((wp.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
            let v = ST.with(|s| (s.borrow().volume + notches * 0.02).clamp(0.0, 1.0));
            set_volume(v);
            ST.with(|s| s.borrow_mut().volume = v);
            redraw();
        }
        WM_MOUSEACTIVATE => return LRESULT(MA_ACTIVATE as isize),
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => paint(hwnd, false),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

pub unsafe extern "system" fn media_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let dips = |lp: LPARAM| {
        let s = GetDpiForWindow(hwnd) as f32 / 96.0;
        ((lp.0 & 0xFFFF) as i16 as f32 / s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s)
    };
    match msg {
        WM_QS_DATA => { let _ = InvalidateRect(Some(hwnd), None, false); }
        WM_MOUSEMOVE => {
            let (x, y) = dips(lp);
            let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            if ST.with(|s| { let mut st = s.borrow_mut(); let h = media_hit(x, y); std::mem::replace(&mut st.hover, h) != h }) {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
        WM_MOUSELEAVE => { ST.with(|s| s.borrow_mut().hover = None); let _ = InvalidateRect(Some(hwnd), None, false); }
        WM_LBUTTONUP => {
            let (x, y) = dips(lp);
            match media_hit(x, y) {
                Some(Hit::Play) => media_cmd(1),
                Some(Hit::Next) => media_cmd(2),
                Some(Hit::Prev) => media_cmd(3),
                Some(Hit::MediaApp) => {
                    if let Some(m) = ST.with(|s| s.borrow().sys.media.clone()) {
                        hide(HWND(QS.load(Relaxed) as *mut c_void));
                        open(&m.app_icon);
                    }
                }
                _ => {}
            }
        }
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => paint(hwnd, true),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

/// `shellbits --snapshot-qs out.bmp`: media card + Quick Settings with live data, stacked like on screen.
pub unsafe fn snapshot(path: &str, page: &str) -> windows::core::Result<()> {
    let page = match page { "wifi" => Page::Wifi, "bt" => Page::Bt, "output" => Page::Output, "access" => Page::Access, "perf" => Page::Perf, "temps" => Page::Temps, _ => Page::Main };
    crate::perf::start(|| {});
    std::thread::sleep(std::time::Duration::from_millis(2300)); // two samples, so CPU/disk deltas exist
    crate::perf::stop();
    std::thread::sleep(std::time::Duration::from_millis(1200)); // let the sampler leave PDH before we exit
    let items = std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        match page {
            Page::Wifi => crate::qsx::wifi_networks(),
            Page::Bt => crate::qsx::bt_devices(),
            Page::Output => crate::qsx::outputs_and_apps(),
            Page::Access => crate::qsx::access_states(),
            Page::Perf => ["CPU", "Memory", "Disk (C:)", "GPU"].iter().map(|n| crate::qsx::Item { name: n.to_string(), ..Default::default() }).collect(),
            Page::Temps => ["CPU", "GPU", "Max fans (Cooler Boost)"].iter().map(|n| crate::qsx::Item { name: n.to_string(), ..Default::default() }).collect(),
            Page::Main => Vec::new(),
        }
    })
    .join()
    .unwrap_or_default();
    // system queries block on async WinRT calls: run them on a free-threaded worker like live mode does
    // a stalled Windows query (radios, Bluetooth, media) must not hang the snapshot: wait at most 8s
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _ = tx.send(collect());
    });
    let sys = rx.recv_timeout(std::time::Duration::from_secs(8)).unwrap_or_default();
    *SYS.lock().unwrap() = Some(sys);
    ST.with(|s| -> windows::core::Result<()> {
        let mut st = s.borrow_mut();
        refresh_local(&mut st);
        st.page = page;
        st.items = items;
        st.sel = (page == Page::Wifi && !st.items.is_empty()).then_some(0);
        st.gfx = Some(make_gfx()?);
        let (w, h) = (W as i32, (MH + MEDIA_GAP + H) as i32);
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, ..Default::default() },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let dib = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let dc = CreateCompatibleDC(None);
        let old = SelectObject(dc, dib.into());
        let g = st.gfx.as_ref().unwrap();
        let top = RECT { left: 0, top: 0, right: w, bottom: MH as i32 };
        g.rt.BindDC(dc, &top)?;
        g.rt.SetDpi(96.0, 96.0);
        g.rt.BeginDraw();
        draw_media(&st, g)?;
        g.rt.EndDraw(None, None)?;
        let _ = SetViewportOrgEx(dc, 0, (MH + MEDIA_GAP) as i32, None);
        let bottom = RECT { left: 0, top: 0, right: w, bottom: H as i32 };
        g.rt.BindDC(dc, &bottom)?;
        g.rt.BeginDraw();
        draw_qs(&st, g)?;
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

// ======================= sub-pages (Wi-Fi, Bluetooth, sound output, accessibility) =======================

const HDR: f32 = 64.0;
const MIX_X0: f32 = 200.0; // volume mixer bar
const MIX_X1: f32 = 330.0;
thread_local! {
    static LAST_X: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
}

fn post_qs() {
    unsafe {
        let _ = PostMessageW(Some(HWND(QS.load(Relaxed) as *mut c_void)), WM_QS_DATA, WPARAM(0), LPARAM(0));
    }
}

fn push_items(p: Page, v: Vec<crate::qsx::Item>) {
    *PAGE_ITEMS.lock().unwrap() = Some((p, v));
    post_qs();
}

/// Re-read everything after a toggle has had time to land.
fn later(ms: u64) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(ms));
        collect_async();
    });
}

/// (re)load a page's rows in the background
fn load_page(p: Page, delay: u64) {
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        std::thread::sleep(std::time::Duration::from_millis(delay));
        match p {
            Page::Wifi => {
                push_items(p, crate::qsx::wifi_networks());
                crate::qsx::wifi_scan();
                std::thread::sleep(std::time::Duration::from_millis(3500));
                push_items(p, crate::qsx::wifi_networks());
            }
            Page::Bt => push_items(p, crate::qsx::bt_devices()),
            Page::Output => push_items(p, crate::qsx::outputs_and_apps()),
            Page::Access => push_items(p, crate::qsx::access_states()),
            _ => {}
        }
    });
}

unsafe fn open_page(hwnd: HWND, p: Page) {
    ST.with(|s| {
        let mut st = s.borrow_mut();
        st.page = p;
        st.sel = None;
        st.pscroll = 0.0;
        st.hover = None;
        st.items = match p {
            Page::Output => crate::qsx::outputs_and_apps(),
            Page::Access => crate::qsx::access_states(),
            Page::Perf => ["CPU", "Memory", "Disk (C:)", "GPU"].iter().map(|n| crate::qsx::Item { name: n.to_string(), ..Default::default() }).collect(),
            Page::Temps => ["CPU", "GPU", "Max fans (Cooler Boost)"].iter().map(|n| crate::qsx::Item { name: n.to_string(), ..Default::default() }).collect(),
            _ => Vec::new(),
        };
    });
    if matches!(p, Page::Wifi | Page::Bt) {
        load_page(p, 0);
    }
    let _ = InvalidateRect(Some(hwnd), None, false);
}

fn row_h(p: Page) -> f32 {
    if matches!(p, Page::Wifi | Page::Bt | Page::Perf | Page::Temps) { 56.0 } else { 48.0 }
}

/// (top, height) of every row in list coordinates; the selected Wi-Fi/Bluetooth row grows a button.
fn rows(st: &State) -> Vec<(f32, f32)> {
    let rh = row_h(st.page);
    let mut y = 0.0;
    (0..st.items.len())
        .map(|i| {
            let h = rh + if st.sel == Some(i) && matches!(st.page, Page::Wifi | Page::Bt) { 44.0 } else { 0.0 };
            let r = (y, h);
            y += h;
            r
        })
        .collect()
}

fn page_hit(st: &State, x: f32, y: f32) -> Option<Hit> {
    if (12.0..44.0).contains(&x) && (16.0..48.0).contains(&y) {
        return Some(Hit::Back);
    }
    if matches!(st.page, Page::Wifi | Page::Bt) && (288.0..336.0).contains(&x) && (18.0..46.0).contains(&y) {
        return Some(Hit::PageToggle);
    }
    if y >= FOOT_Y {
        return (16.0..260.0).contains(&x).then_some(Hit::Footer);
    }
    if y < HDR {
        return None;
    }
    let ly = y - HDR + st.pscroll;
    for (i, (top, h)) in rows(st).into_iter().enumerate() {
        if ly >= top && ly < top + h {
            if h > row_h(st.page) && ly >= top + row_h(st.page) && (200.0..340.0).contains(&x) {
                return Some(Hit::RowBtn(i));
            }
            return Some(Hit::Row(i));
        }
    }
    None
}

unsafe fn page_click(hwnd: HWND, hit: Option<Hit>) {
    use windows::Devices::Radios::RadioKind;
    let (page, item, wifi, bt) = ST.with(|s| {
        let st = s.borrow();
        let i = match hit { Some(Hit::Row(i) | Hit::RowBtn(i)) => st.items.get(i).cloned(), _ => None };
        (st.page, i, st.sys.wifi_on, st.sys.bt_on)
    });
    match hit {
        Some(Hit::Back) => ST.with(|s| s.borrow_mut().page = Page::Main),
        Some(Hit::PageToggle) => {
            let (kind, on) = if page == Page::Wifi { (RadioKind::WiFi, wifi != Some(true)) } else { (RadioKind::Bluetooth, bt != Some(true)) };
            set_radio(kind, on);
            ST.with(|s| {
                let mut st = s.borrow_mut();
                if page == Page::Wifi { st.sys.wifi_on = Some(on) } else { st.sys.bt_on = Some(on) }
                st.items.clear();
            });
            if on {
                load_page(page, 1500);
            }
        }
        Some(Hit::Footer) => {
            hide(hwnd);
            open(match page {
                Page::Perf | Page::Temps => "taskmgr.exe",
                Page::Wifi => "ms-settings:network-wifi",
                Page::Bt => "ms-settings:bluetooth",
                Page::Output => "ms-settings:sound",
                _ => "ms-settings:easeofaccess",
            });
        }
        Some(Hit::Row(i)) => match page {
            Page::Wifi | Page::Bt => ST.with(|s| {
                let mut st = s.borrow_mut();
                st.sel = if st.sel == Some(i) { None } else { Some(i) };
            }),
            Page::Output if item.as_ref().is_some_and(|it| it.sub == "app") => {
                if let Some(it) = item {
                    let v = ((LAST_X.with(|x| x.get()) - MIX_X0) / (MIX_X1 - MIX_X0)).clamp(0.0, 1.0);
                    crate::qsx::set_app_volume(&it.name, v);
                    ST.with(|s| { if let Some(x) = s.borrow_mut().items.get_mut(i) { x.signal = (v * 100.0).round() as u32; } });
                }
            }
            Page::Output => {
                if let Some(it) = item {
                    crate::qsx::set_output(&it.key);
                    ST.with(|s| { for (j, x) in s.borrow_mut().items.iter_mut().enumerate() { x.on = j == i && x.sub != "app"; } });
                    ST.with(|s| refresh_local(&mut s.borrow_mut())); // volume of the new device
                }
            }
            Page::Access => {
                if let Some(it) = item {
                    crate::qsx::toggle_access(i, it.on);
                    ST.with(|s| { if let Some(x) = s.borrow_mut().items.get_mut(i) { x.on = !x.on; } });
                    load_page(Page::Access, 1200);
                }
            }
            Page::Temps if i == 2 => {
                let on = crate::qsx::fans_state() != Some(true);
                crate::qsx::fans_set(on);
                // the elevated task writes the confirmed state; repaint once it has
                std::thread::spawn(|| { std::thread::sleep(std::time::Duration::from_millis(2500)); post_qs(); });
            }
            Page::Perf | Page::Temps => {
                hide(hwnd);
                open("taskmgr.exe");
            }
            Page::Main => {}
        },
        Some(Hit::RowBtn(_)) => {
            if let Some(it) = item {
                if page == Page::Wifi {
                    crate::qsx::wifi_connect(&it);
                    load_page(Page::Wifi, 3000);
                } else {
                    // ponytail: Windows has no public connect/disconnect call for paired audio/input devices; Settings does it
                    hide(hwnd);
                    open("ms-settings:bluetooth");
                }
            }
        }
        _ => {}
    }
}

pub(crate) unsafe fn switch(rt: &ID2D1DCRenderTarget, x: f32, y: f32, on: bool) -> windows::core::Result<()> {
    let pill = rrect(rect(x + 0.5, y + 0.5, x + 39.5, y + 19.5), 9.5);
    let knob = |cx: f32, c: D2D1_COLOR_F| -> windows::core::Result<()> {
        rt.FillEllipse(&D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: cx, Y: y + 10.0 }, radiusX: 6.0, radiusY: 6.0 }, &rt.CreateSolidColorBrush(&c, None)?);
        Ok(())
    };
    if on {
        rt.FillRoundedRectangle(&pill, &rt.CreateSolidColorBrush(&C_ON, None)?);
        knob(x + 30.0, rgb(0x000000))
    } else {
        rt.DrawRoundedRectangle(&pill, &rt.CreateSolidColorBrush(&rgb(0xCFCFCF), None)?, 1.0, None);
        knob(x + 10.0, rgb(0xCFCFCF))
    }
}

unsafe fn draw_page(st: &State, g: &Gfx) -> windows::core::Result<()> {
    let rt = &g.rt;
    rt.Clear(Some(&crate::flyout::base(C_PANEL)));
    let brush = |c: D2D1_COLOR_F| rt.CreateSolidColorBrush(&c, None);
    let white = brush(rgb(0xFFFFFF))?;
    let black = brush(rgb(0x000000))?;
    let grey = brush(rgb(0xC5C5C5))?;
    let text = |s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush| {
        rt.DrawText(&utf16(s), f, &r, b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL)
    };
    let icon = |cp: u32, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush| {
        rt.DrawText(&glyph(cp), f, &r, b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL)
    };
    if st.hover == Some(Hit::Back) {
        rt.FillRoundedRectangle(&rrect(rect(12.0, 16.0, 44.0, 48.0), 4.0), &brush(C_TILE)?);
    }
    icon(0xE72B, &g.icon16, rect(12.0, 16.0, 44.0, 48.0), &white);
    let (title, link) = match st.page {
        Page::Wifi => ("Wi-Fi", "More Wi-Fi settings"),
        Page::Bt => ("Bluetooth", "More Bluetooth settings"),
        Page::Output => ("Sound output", "More volume settings"),
        Page::Perf => ("Performance", "Open Task Manager"),
        Page::Temps => ("Temperatures", "Open Task Manager"),
        _ => ("Accessibility", "More accessibility settings"),
    };
    text(title, &g.body_sb, rect(56.0, 22.0, 260.0, 42.0), &white);
    let radio = match st.page { Page::Wifi => Some(st.sys.wifi_on == Some(true)), Page::Bt => Some(st.sys.bt_on == Some(true)), _ => None };
    if let Some(on) = radio {
        switch(rt, 292.0, 22.0, on)?;
    }

    rt.PushAxisAlignedClip(&rect(0.0, HDR, W, FOOT_Y), D2D1_ANTIALIAS_MODE_ALIASED);
    if st.items.is_empty() {
        let msg = match (st.page, radio) {
            (Page::Wifi, Some(false)) => "Wi-Fi is turned off",
            (Page::Bt, Some(false)) => "Bluetooth is turned off",
            (Page::Wifi, _) => "Looking for networks\u{2026}",
            (Page::Bt, _) => "No paired devices",
            _ => "",
        };
        text(msg, &g.small_c, rect(0.0, HDR + 20.0, W, HDR + 40.0), &grey);
    }
    let rh = row_h(st.page);
    for (i, ((top, h), it)) in rows(st).into_iter().zip(&st.items).enumerate() {
        let y = HDR + top - st.pscroll;
        if y > FOOT_Y || y + h < HDR {
            continue;
        }
        let hov = matches!(st.hover, Some(Hit::Row(j) | Hit::RowBtn(j)) if j == i);
        let selected = st.sel == Some(i) || (st.page == Page::Output && it.on);
        if hov || selected {
            rt.FillRoundedRectangle(&rrect(rect(8.0, y + 2.0, 348.0, y + h - 2.0), 4.0), &brush(if hov { C_TILE_HOVER } else { C_TILE })?);
        }
        match st.page {
            Page::Wifi | Page::Bt => {
                let gl = if st.page == Page::Wifi { crate::qsx::wifi_glyph(it.signal) } else { 0xE702 };
                icon(gl, &g.icon16, rect(20.0, y + 16.0, 48.0, y + 40.0), &white);
                if it.secured {
                    icon(0xE72E, &g.icon12, rect(36.0, y + 30.0, 50.0, y + 44.0), &white);
                }
                text(&it.name, &g.body, rect(60.0, y + 9.0, 336.0, y + 28.0), &white);
                text(&it.sub, &g.small, rect(60.0, y + 28.0, 336.0, y + 46.0), &grey);
                if st.sel == Some(i) {
                    let b = rect(216.0, y + rh + 4.0, 336.0, y + rh + 36.0);
                    let label = if it.on { "Disconnect" } else { "Connect" };
                    if it.on {
                        rt.FillRoundedRectangle(&rrect(b, 4.0), &brush(if st.hover == Some(Hit::RowBtn(i)) { C_TILE_HOVER } else { rgb(0x3A3A3A) })?);
                        text(label, &g.small_c, b, &white);
                    } else {
                        rt.FillRoundedRectangle(&rrect(b, 4.0), &brush(if st.hover == Some(Hit::RowBtn(i)) { C_ON_HOVER } else { C_ON })?);
                        text(label, &g.small_c, b, &black);
                    }
                }
            }
            Page::Temps if i == 2 => {
                let on = crate::qsx::fans_state();
                icon(0xE9CA, &g.icon16, rect(20.0, y + 8.0, 48.0, y + 32.0), &white);
                text(&it.name, &g.body, rect(60.0, y + 8.0, 280.0, y + 28.0), &white);
                text(match on { Some(true) => "Fans at maximum", Some(false) => "Automatic", None => "Asks for admin once" }, &g.small, rect(60.0, y + 28.0, 280.0, y + 46.0), &grey);
                switch(rt, 292.0, y + 18.0, on == Some(true))?;
            }
            Page::Perf | Page::Temps => {
                let st_ = crate::perf::latest();
                let (gl, value, pct) = perf_row(st.page, i, &st_);
                icon(gl, &g.icon16, rect(20.0, y + 8.0, 48.0, y + 32.0), &white);
                text(&it.name, &g.body, rect(60.0, y + 8.0, 200.0, y + 28.0), &white);
                text(&value, &g.small_r, rect(150.0, y + 9.0, 336.0, y + 27.0), &grey);
                // Task-Manager-style bar
                let (x0, x1, by) = (60.0, 336.0, y + 38.0);
                rt.FillRoundedRectangle(&rrect(rect(x0, by, x1, by + 4.0), 2.0), &brush(C_TRACK_OFF)?);
                if let Some(p) = pct {
                    let col = if st.page == Page::Temps { crate::perf::heat(p) } else { 0x4CC2FF };
                    rt.FillRoundedRectangle(&rrect(rect(x0, by, x0 + (x1 - x0) * (p / 100.0).clamp(0.0, 1.0), by + 4.0), 2.0), &brush(rgb(col))?);
                }
            }
            Page::Output if it.sub == "app" => {
                // volume mixer row: app name, then a bar you click (or scroll) to set its volume
                icon(0xE767, &g.icon16, rect(20.0, y, 48.0, y + h), &white);
                text(&it.name, &g.body, rect(60.0, y, MIX_X0 - 8.0, y + h), &white);
                let by = y + h / 2.0;
                rt.FillRoundedRectangle(&rrect(rect(MIX_X0, by - 2.0, MIX_X1, by + 2.0), 2.0), &brush(C_TRACK_OFF)?);
                let x = MIX_X0 + (MIX_X1 - MIX_X0) * it.signal as f32 / 100.0;
                rt.FillRoundedRectangle(&rrect(rect(MIX_X0, by - 2.0, x, by + 2.0), 2.0), &brush(C_ON)?);
                rt.FillEllipse(&D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: x, Y: by }, radiusX: 6.0, radiusY: 6.0 }, &brush(C_ON)?);
            }
            Page::Output => {
                if it.on {
                    rt.FillRoundedRectangle(&rrect(rect(8.0, y + 16.0, 11.0, y + 32.0), 1.5), &brush(C_ON)?);
                }
                icon(0xE7F5, &g.icon16, rect(20.0, y, 48.0, y + h), &white);
                text(&it.name, &g.body, rect(60.0, y, 336.0, y + h), &white);
            }
            _ => {
                text(&it.name, &g.body, rect(24.0, y, 280.0, y + h), &white);
                switch(rt, 292.0, y + 14.0, it.on)?;
            }
        }
    }
    rt.PopAxisAlignedClip();

    rt.FillRectangle(&rect(0.0, FOOT_Y, W, H), &brush(C_FOOT)?);
    text(link, &g.body, rect(24.0, FOOT_Y + 14.0, 300.0, FOOT_Y + 34.0), &white);
    if st.hover == Some(Hit::Footer) {
        rt.FillRectangle(&rect(24.0, FOOT_Y + 32.0, 24.0 + 7.4 * link.len() as f32, FOOT_Y + 33.0), &white);
    }
    Ok(())
}


// ======================= performance / temperature tiles =======================

fn perf_label(s: &crate::perf::Stats) -> String {
    if s.ram_total_gb > 0.0 { format!("RAM {:.0}%", s.ram_pct()) } else { "Performance".into() }
}

fn temps_label(s: &crate::perf::Stats) -> String {
    match (s.cpu_temp, s.gpu_temp) {
        (Some(c), Some(g)) => format!("{c:.0}\u{B0} \u{B7} {g:.0}\u{B0}"),
        (Some(c), None) => format!("CPU {c:.0}\u{B0}C"),
        (None, Some(g)) => format!("GPU {g:.0}\u{B0}C"),
        _ => "Temps".into(),
    }
}

/// (glyph, value text, bar percent) for a row on the Performance / Temperatures page.
fn perf_row(page: Page, i: usize, s: &crate::perf::Stats) -> (u32, String, Option<f32>) {
    let pct = |v: Option<f32>| v.map(|v| format!("{v:.0}%")).unwrap_or_else(|| "\u{2026}".into());
    if page == Page::Temps {
        let t = if i == 0 { s.cpu_temp } else { s.gpu_temp };
        let label = t.map(|t| format!("{t:.0}\u{B0}C")).unwrap_or_else(|| "Not available".into());
        return (if i == 0 { 0xE950 } else { 0xE964 }, label, t);
    }
    match i {
        0 => (0xE950, pct(s.cpu), s.cpu),
        1 => (0xEEA0, if s.ram_total_gb > 0.0 { format!("{:.1}/{:.1} GB ({:.0}%)", s.ram_used_gb, s.ram_total_gb, s.ram_pct()) } else { "\u{2026}".into() }, (s.ram_total_gb > 0.0).then(|| s.ram_pct())),
        2 => (0xEDA2, match (s.disk_active, s.disk_free_gb) {
            (Some(a), Some(f)) => format!("{a:.0}% active \u{B7} {f:.0} GB free"),
            (a, _) => pct(a),
        }, s.disk_active),
        _ => (0xE964, pct(s.gpu), s.gpu),
    }
}

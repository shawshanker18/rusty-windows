//! System back ends for Quick Settings' toggles and sub-pages: Airplane mode, Energy saver,
#![allow(non_snake_case)] // COM interface methods keep their Windows names
//! accessibility features, Wi-Fi networks, Bluetooth devices and the default sound output.
//! Everything here talks to Windows directly; quick.rs only draws.

use std::ffi::c_void;

use windows::core::{w, GUID, HRESULT, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// One row on a sub-page.
#[derive(Clone, Default, Debug)]
pub struct Item {
    pub name: String,
    pub sub: String,
    pub on: bool,      // connected / default device / feature enabled
    pub key: String,   // Wi-Fi profile name, endpoint id, ...
    pub signal: u32,   // Wi-Fi 0-100
    pub secured: bool, // Wi-Fi
    pub known: bool,   // Wi-Fi: Windows has a saved profile
}

// ---------------------------------------------------------------- Airplane mode

#[windows_core::interface("db3afbfb-08e6-46c6-aa70-bf9a34c30ab7")]
unsafe trait IRadioManager: windows_core::IUnknown {
    fn IsRMSupported(&self, state: *mut u32) -> HRESULT;
    fn GetUIRadioInstances(&self, out: *mut *mut c_void) -> HRESULT;
    fn GetSystemRadioState(&self, enabled: *mut i32, hw: *mut i32, reason: *mut i32) -> HRESULT;
    fn SetSystemRadioState(&self, enabled: i32) -> HRESULT;
    fn Refresh(&self) -> HRESULT;
    fn OnHardwareSliderChange(&self, a: i32, b: i32) -> HRESULT;
}
const CLSID_RADIO_MANAGEMENT: GUID = GUID::from_u128(0x581333f6_28db_41be_bc7a_ff201f12f3f6);

fn radio_manager() -> Option<IRadioManager> {
    unsafe { CoCreateInstance(&CLSID_RADIO_MANAGEMENT, None, CLSCTX_ALL).ok() }
}

/// Some(true) = airplane mode is on.
pub fn airplane() -> Option<bool> {
    let rm = radio_manager()?;
    let (mut en, mut hw, mut why) = (0, 0, 0);
    unsafe { rm.GetSystemRadioState(&mut en, &mut hw, &mut why).ok().ok()? };
    Some(en == 0)
}

pub fn set_airplane(on: bool) {
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if let Some(rm) = radio_manager() {
            let _ = rm.SetSystemRadioState(!on as i32);
        }
    });
}

// ---------------------------------------------------------------- Energy saver

const SUB_ENERGYSAVER: GUID = GUID::from_u128(0xde830923_a562_41af_a086_e3a2c6bad2da);
const ES_THRESHOLD: GUID = GUID::from_u128(0xe69653ca_cf7f_4f05_aa73_cb833fa90ad4);

pub fn saver_on() -> bool {
    let mut s = windows::Win32::System::Power::SYSTEM_POWER_STATUS::default();
    unsafe { windows::Win32::System::Power::GetSystemPowerStatus(&mut s).is_ok() && s.SystemStatusFlag == 1 }
}

/// Energy saver "always on" = turn-on threshold at 100%; off = Windows' defaults (never on AC, 30% on battery).
/// Returns false if Windows didn't engage it (some PCs only allow it on battery): caller opens Settings.
pub fn set_saver(on: bool) -> bool {
    use windows::Win32::System::Power::*;
    unsafe {
        let mut scheme: *mut GUID = std::ptr::null_mut();
        if PowerGetActiveScheme(None, &mut scheme) != WIN32_ERROR(0) || scheme.is_null() {
            return false;
        }
        let (ac, dc) = if on { (100, 100) } else { (0, 30) };
        let _ = PowerWriteACValueIndex(None, scheme, Some(&SUB_ENERGYSAVER), Some(&ES_THRESHOLD), ac);
        let _ = PowerWriteDCValueIndex(None, scheme, Some(&SUB_ENERGYSAVER), Some(&ES_THRESHOLD), dc);
        let _ = PowerSetActiveScheme(None, Some(scheme));
        let _ = LocalFree(Some(HLOCAL(scheme as *mut c_void)));
    }
    for _ in 0..12 {
        if saver_on() == on {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

// ---------------------------------------------------------------- accessibility

pub const ACCESS: [&str; 6] = ["Magnifier", "Color filters", "Narrator", "Mono audio", "Live captions", "Sticky keys"];

fn running(exe: &str) -> bool {
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return false };
        let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = false;
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let name = String::from_utf16_lossy(&e.szExeFile[..e.szExeFile.iter().position(|&c| c == 0).unwrap_or(0)]);
            if name.eq_ignore_ascii_case(exe) {
                found = true;
                break;
            }
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
        found
    }
}

pub(crate) fn reg_dword(key: PCWSTR, name: PCWSTR) -> Option<u32> {
    let mut v = 0u32;
    let mut size = 4u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, key, name, RRF_RT_REG_DWORD, None, Some(&mut v as *mut _ as *mut c_void), Some(&mut size)).ok().ok()? };
    Some(v)
}

pub(crate) fn set_reg_dword(key: PCWSTR, name: PCWSTR, v: u32) {
    unsafe {
        let _ = RegSetKeyValueW(HKEY_CURRENT_USER, key, name, REG_DWORD.0, Some(&v as *const _ as *const c_void), 4);
    }
}

fn sticky() -> windows::Win32::UI::Accessibility::STICKYKEYS {
    let mut s = windows::Win32::UI::Accessibility::STICKYKEYS { cbSize: size_of::<windows::Win32::UI::Accessibility::STICKYKEYS>() as u32, dwFlags: Default::default() };
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETSTICKYKEYS, s.cbSize, Some(&mut s as *mut _ as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
    s
}

pub fn captions_on() -> bool {
    running("LiveCaptions.exe")
}

pub fn access_states() -> Vec<Item> {
    use windows::Win32::UI::Accessibility::SKF_STICKYKEYSON;
    let on = [
        running("Magnify.exe"),
        reg_dword(w!("Software\\Microsoft\\ColorFiltering"), w!("Active")) == Some(1),
        running("Narrator.exe"),
        reg_dword(w!("Software\\Microsoft\\Multimedia\\Audio"), w!("AccessibilityMonoMixState")) == Some(1),
        captions_on(),
        sticky().dwFlags.contains(SKF_STICKYKEYSON),
    ];
    ACCESS.iter().zip(on).map(|(n, on)| Item { name: n.to_string(), on, ..Default::default() }).collect()
}

/// Flip one accessibility feature the same way Windows' own shortcuts do.
pub fn toggle_access(i: usize, on_now: bool) {
    use crate::launcher::inject;
    const WIN: u16 = 0x5B;
    const CTRL: u16 = 0x11;
    let chord = |keys: &[u16]| unsafe {
        let mut v: Vec<(u16, bool)> = keys.iter().map(|&k| (k, false)).collect();
        v.extend(keys.iter().rev().map(|&k| (k, true)));
        inject(&v);
    };
    match i {
        0 => {
            if on_now { chord(&[WIN, 0x1B]) } else { crate::quick::open("C:\\Windows\\System32\\Magnify.exe") }
        }
        1 => {
            set_reg_dword(w!("Software\\Microsoft\\ColorFiltering"), w!("HotkeyEnabled"), 1);
            chord(&[WIN, CTRL, b'C' as u16]);
        }
        2 => chord(&[WIN, CTRL, 0x0D]),
        3 => set_reg_dword(w!("Software\\Microsoft\\Multimedia\\Audio"), w!("AccessibilityMonoMixState"), !on_now as u32),
        4 => chord(&[WIN, CTRL, b'L' as u16]),
        _ => unsafe {
            use windows::Win32::UI::Accessibility::SKF_STICKYKEYSON;
            let mut s = sticky();
            if on_now { s.dwFlags &= !SKF_STICKYKEYSON } else { s.dwFlags |= SKF_STICKYKEYSON }
            let _ = SystemParametersInfoW(SPI_SETSTICKYKEYS, s.cbSize, Some(&mut s as *mut _ as *mut c_void), SPIF_UPDATEINIFILE | SPIF_SENDCHANGE);
        },
    }
}

// ---------------------------------------------------------------- Wi-Fi

use windows::Win32::NetworkManagement::WiFi::*;

struct Wlan(HANDLE, GUID);
impl Drop for Wlan {
    fn drop(&mut self) {
        unsafe { WlanCloseHandle(self.0, None) };
    }
}

fn wlan() -> Option<Wlan> {
    unsafe {
        let (mut ver, mut h) = (0, HANDLE::default());
        if WlanOpenHandle(2, None, &mut ver, &mut h) != 0 {
            return None;
        }
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        if WlanEnumInterfaces(h, None, &mut list) != 0 || list.is_null() || (*list).dwNumberOfItems == 0 {
            WlanCloseHandle(h, None);
            return None;
        }
        let g = (*list).InterfaceInfo[0].InterfaceGuid;
        WlanFreeMemory(list as *const c_void);
        Some(Wlan(h, g))
    }
}

/// Ask the adapter for a fresh scan (results land a few seconds later).
pub fn wifi_scan() {
    if let Some(w) = wlan() {
        unsafe { WlanScan(w.0, &w.1, None, None, None) };
    }
}

pub fn wifi_networks() -> Vec<Item> {
    let Some(w) = wlan() else { return Vec::new() };
    let mut out: Vec<Item> = Vec::new();
    unsafe {
        let mut list: *mut WLAN_AVAILABLE_NETWORK_LIST = std::ptr::null_mut();
        if WlanGetAvailableNetworkList(w.0, &w.1, 0, None, &mut list) != 0 || list.is_null() {
            return out;
        }
        let l = &*list;
        for n in std::slice::from_raw_parts(l.Network.as_ptr(), l.dwNumberOfItems as usize) {
            let ssid = String::from_utf8_lossy(&n.dot11Ssid.ucSSID[..n.dot11Ssid.uSSIDLength as usize]).to_string();
            if ssid.is_empty() {
                continue; // hidden network
            }
            let connected = n.dwFlags & WLAN_AVAILABLE_NETWORK_CONNECTED != 0;
            let known = n.dwFlags & WLAN_AVAILABLE_NETWORK_HAS_PROFILE != 0;
            let profile = String::from_utf16_lossy(&n.strProfileName[..n.strProfileName.iter().position(|&c| c == 0).unwrap_or(0)]);
            let secured = n.bSecurityEnabled.as_bool();
            let item = Item {
                sub: match (connected, secured) {
                    (true, true) => "Connected, secured".into(),
                    (true, false) => "Connected, open".into(),
                    (false, true) => "Secured".into(),
                    (false, false) => "Open".into(),
                },
                name: ssid,
                on: connected,
                key: profile,
                signal: n.wlanSignalQuality,
                secured,
                known,
            };
            match out.iter_mut().find(|o| o.name == item.name) {
                Some(o) if (item.on, item.known, item.signal) > (o.on, o.known, o.signal) => *o = item,
                Some(_) => {}
                None => out.push(item),
            }
        }
        WlanFreeMemory(list as *const c_void);
    }
    sort_networks(&mut out);
    out
}

/// Connected first, then strongest.
pub fn sort_networks(v: &mut [Item]) {
    v.sort_by(|a, b| b.on.cmp(&a.on).then(b.signal.cmp(&a.signal)));
}

/// Connect with the saved profile, or disconnect. Networks without a saved profile need a
/// password: those go to Windows' own network list.
pub fn wifi_connect(item: &Item) {
    let item = item.clone();
    std::thread::spawn(move || unsafe {
        let Some(w) = wlan() else { return };
        if item.on {
            WlanDisconnect(w.0, &w.1, None);
        } else if item.known {
            let prof = HSTRING::from(item.key.as_str());
            let p = WLAN_CONNECTION_PARAMETERS {
                wlanConnectionMode: wlan_connection_mode_profile,
                strProfile: PCWSTR(prof.as_ptr()),
                dot11BssType: dot11_BSS_type_any,
                ..Default::default()
            };
            WlanConnect(w.0, &w.1, &p, None);
        } else {
            crate::quick::open("ms-availablenetworks:");
        }
    });
}

/// Signal-strength glyph like Windows' (1-4 bars).
pub fn wifi_glyph(signal: u32) -> u32 {
    match signal {
        0..=25 => 0xE872,
        26..=50 => 0xE873,
        51..=75 => 0xE874,
        _ => 0xE701,
    }
}

// ---------------------------------------------------------------- Bluetooth

/// Bluetooth devices this PC is paired with (classic and low-energy), connected ones first.
/// Read from Windows' device tree, which answers at once; the Bluetooth "association endpoint"
/// query was measured at 30 seconds here because it runs a live scan first.
pub fn bt_devices() -> Vec<Item> {
    use windows::core::Interface;
    use windows::Devices::Enumeration::{DeviceInformation, DeviceInformationKind};
    let k_id = HSTRING::from("System.Devices.DeviceInstanceId");
    let k_conn = HSTRING::from("{83DA6326-97A6-4088-9453-A1923F573B29} 15"); // the device's "is connected" flag
    let aqs = HSTRING::from("System.Devices.ClassGuid:=\"{e0cbf06c-cd8b-4647-bb8a-263b43f0f974}\"");
    (|| -> windows::core::Result<Vec<Item>> {
        let props: windows_collections::IIterable<HSTRING> = vec![k_id.clone(), k_conn.clone()].into();
        let devs = DeviceInformation::FindAllAsyncWithKindAqsFilterAndAdditionalProperties(&aqs, &props, DeviceInformationKind::Device)?.get()?;
        let mut v: Vec<Item> = Vec::new();
        for d in devs {
            let prop = |k: &HSTRING| d.Properties().ok().and_then(|m| m.Lookup(k).ok());
            let id = d.Id()?.to_string().to_uppercase(); // for device-tree entries the id is the instance path
            let name = d.Name()?.to_string();
            // the device itself, not its services or the radio
            if !(id.starts_with(r"BTHENUM\DEV_") || id.starts_with(r"BTHLE\DEV_")) || name.is_empty() {
                continue;
            }
            let on = prop(&k_conn).and_then(|x| x.cast::<windows::Foundation::IReference<bool>>().ok()).and_then(|r| r.Value().ok()).unwrap_or(false);
            match v.iter_mut().find(|i| i.name == name) {
                Some(i) => i.on |= on, // a dual-mode device shows up once per mode
                None => v.push(Item { sub: String::new(), name, on, key: id, ..Default::default() }),
            }
        }
        for i in &mut v {
            i.sub = if i.on { "Connected".into() } else { "Paired".into() };
        }
        v.sort_by(|a, b| b.on.cmp(&a.on).then(a.name.cmp(&b.name)));
        Ok(v)
    })()
    .unwrap_or_default()
}

// ---------------------------------------------------------------- sound output

#[windows_core::interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: windows_core::IUnknown {
    fn GetMixFormat(&self) -> HRESULT;
    fn GetDeviceFormat(&self) -> HRESULT;
    fn ResetDeviceFormat(&self) -> HRESULT;
    fn SetDeviceFormat(&self) -> HRESULT;
    fn GetProcessingPeriod(&self) -> HRESULT;
    fn SetProcessingPeriod(&self) -> HRESULT;
    fn GetShareMode(&self) -> HRESULT;
    fn SetShareMode(&self) -> HRESULT;
    fn GetPropertyValue(&self) -> HRESULT;
    fn SetPropertyValue(&self) -> HRESULT;
    fn SetDefaultEndpoint(&self, id: PCWSTR, role: u32) -> HRESULT;
    fn SetEndpointVisibility(&self) -> HRESULT;
}
const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

pub fn outputs() -> Vec<Item> {
    endpoints(false)
}
/// Microphones and other recording devices.
pub fn inputs() -> Vec<Item> {
    endpoints(true)
}
fn endpoints(capture: bool) -> Vec<Item> {
    use windows::Win32::Media::Audio::*;
    let flow = if capture { eCapture } else { eRender };
    use windows::Win32::Foundation::PROPERTYKEY;
    // PKEY_Device_FriendlyName
    const FRIENDLY: PROPERTYKEY = PROPERTYKEY { fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 14 };
    (|| -> windows::core::Result<Vec<Item>> {
        unsafe {
            let e: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let def = e.GetDefaultAudioEndpoint(flow, eMultimedia).and_then(|d| d.GetId()).map(|p| p.to_string().unwrap_or_default()).unwrap_or_default();
            let all = e.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)?;
            let mut v = Vec::new();
            for i in 0..all.GetCount()? {
                let d = all.Item(i)?;
                let id = d.GetId()?.to_string().unwrap_or_default();
                let store = d.OpenPropertyStore(STGM_READ)?;
                let name = store.GetValue(&FRIENDLY).map(|p| p.to_string()).unwrap_or_default();
                v.push(Item { on: id == def, name, key: id, ..Default::default() });
            }
            Ok(v)
        }
    })()
    .unwrap_or_default()
}

pub fn set_output(id: &str) {
    unsafe {
        if let Ok(pc) = CoCreateInstance::<_, IPolicyConfig>(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL) {
            let id = HSTRING::from(id);
            for role in 0..3 {
                let _ = pc.SetDefaultEndpoint(PCWSTR(id.as_ptr()), role);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exe_names_are_tidied() {
        assert_eq!(tidy_exe("audiodg.exe"), "Audiodg");
        assert_eq!(tidy_exe("chrome"), "Chrome");
    }
    #[test]
    fn networks_sort_connected_then_signal() {
        let mut v = vec![
            Item { name: "a".into(), signal: 40, ..Default::default() },
            Item { name: "b".into(), signal: 90, ..Default::default() },
            Item { name: "c".into(), signal: 10, on: true, ..Default::default() },
        ];
        sort_networks(&mut v);
        assert_eq!(v.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["c", "b", "a"]);
        assert_eq!(wifi_glyph(100), 0xE701);
        assert_eq!(wifi_glyph(10), 0xE872);
    }
}

// ---------------------------------------------------------------- fans (MSI Cooler Boost)

fn exe_dir() -> std::path::PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_default()
}

/// Last known Cooler Boost state, written by fans.ps1: Some(on/off), or None if never run / failed.
pub fn fans_state() -> Option<bool> {
    let s = std::fs::read_to_string(exe_dir().join("fans-state.txt")).ok()?;
    match s.trim() {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

fn quiet(cmd: &str, args: &[&str]) -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new(cmd).args(args).creation_flags(0x0800_0000).status().is_ok_and(|s| s.success())
}

/// Fans to max (or back to auto). Needs admin: the first time, Windows asks you once (UAC) to create
/// two small elevated tasks; after that the switch works silently.
pub fn fans_set(on: bool) {
    std::thread::spawn(move || {
        let task = if on { "shellbits-fans-on" } else { "shellbits-fans-off" };
        if quiet("schtasks.exe", &["/run", "/tn", task]) {
            return;
        }
        let script = exe_dir().join("fans.ps1");
        let mk = |mode: &str| {
            format!(
                "schtasks /create /f /rl highest /sc once /st 00:00 /tn shellbits-fans-{mode} /tr \"powershell -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File \\\"{}\\\" {mode}\"",
                script.display()
            )
        };
        let all = format!("/c {} & {} & schtasks /run /tn {task}", mk("on"), mk("off"));
        unsafe {
            windows::Win32::UI::Shell::ShellExecuteW(None, w!("runas"), w!("cmd.exe"), &HSTRING::from(all), PCWSTR::null(), windows::Win32::UI::WindowsAndMessaging::SW_HIDE);
        }
    });
}

// ---------------------------------------------------------------- volume mixer (per-app volume)

unsafe fn sessions() -> windows::core::Result<Vec<(u32, windows::Win32::Media::Audio::ISimpleAudioVolume)>> {
    use windows::core::Interface;
    use windows::Win32::Media::Audio::*;
    let e: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let mgr: IAudioSessionManager2 = e.GetDefaultAudioEndpoint(eRender, eMultimedia)?.Activate(CLSCTX_ALL, None)?;
    let list = mgr.GetSessionEnumerator()?;
    let mut out = Vec::new();
    for i in 0..list.GetCount()? {
        let c = list.GetSession(i)?;
        if c.GetState()? == AudioSessionStateExpired {
            continue;
        }
        let c2: IAudioSessionControl2 = c.cast()?;
        let pid = if c2.IsSystemSoundsSession() == S_OK { 0 } else { c2.GetProcessId().unwrap_or(0) };
        out.push((pid, c.cast()?));
    }
    Ok(out)
}

/// A process's file name from the system process list (works for protected processes too), tidied:
/// "audiodg.exe" -> "Audiodg".
fn exe_name(pid: u32) -> Option<String> {
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = None;
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            if e.th32ProcessID == pid {
                found = Some(String::from_utf16_lossy(&e.szExeFile[..e.szExeFile.iter().position(|&c| c == 0).unwrap_or(0)]));
                break;
            }
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
        found.map(|n| tidy_exe(&n))
    }
}

pub fn tidy_exe(file: &str) -> String {
    let stem = file.rsplit_once('.').map_or(file, |(a, _)| a);
    let mut c = stem.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

fn process_name(pid: u32) -> String {
    use windows::Win32::System::Threading::*;
    if pid == 0 {
        return "System sounds".into();
    }
    unsafe {
        let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return exe_name(pid).unwrap_or_else(|| format!("Process {pid}")) };
        let mut buf = [0u16; 1024];
        let mut n = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut n).is_ok();
        let _ = CloseHandle(p);
        let path = if ok { String::from_utf16_lossy(&buf[..n as usize]) } else { String::new() };
        let stem = std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        if stem.eq_ignore_ascii_case("shellbits") {
            return "Rust UI".into();
        }
        let mut c = stem.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_else(|| format!("Process {pid}"))
    }
}

/// Apps playing (or ready to play) sound on the current output: one row per app, volume in `signal`.
pub fn mixer() -> Vec<Item> {
    let mut out: Vec<Item> = Vec::new();
    for (pid, vol) in unsafe { sessions() }.unwrap_or_default() {
        let name = process_name(pid);
        if out.iter().any(|i| i.name == name) {
            continue; // one row per app, like Windows (browsers open several sessions)
        }
        let v = unsafe { vol.GetMasterVolume() }.unwrap_or(1.0);
        out.push(Item { name, sub: "app".into(), key: pid.to_string(), signal: (v * 100.0).round() as u32, ..Default::default() });
    }
    out.sort_by(|a, b| (a.key != "0").cmp(&(b.key != "0")).then(a.name.cmp(&b.name)));
    out
}

/// Set one app's volume (all of its sessions).
pub fn set_app_volume(name: &str, v: f32) {
    for (pid, vol) in unsafe { sessions() }.unwrap_or_default() {
        if process_name(pid) == name {
            unsafe {
                let _ = vol.SetMasterVolume(v.clamp(0.0, 1.0), std::ptr::null());
            }
        }
    }
}

/// The sound page: output devices, then the volume mixer.
pub fn outputs_and_apps() -> Vec<Item> {
    let mut v = outputs();
    v.extend(mixer());
    v
}

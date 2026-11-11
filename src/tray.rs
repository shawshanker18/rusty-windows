//! System tray host: apps' notification-area icons (Discord, Steam, ...) once shellbits is the shell.
//! Apps call Shell_NotifyIconW, which sends WM_COPYDATA (dwData = 1) to the window of class
//! "Shell_TrayWnd" with a TRAYNOTIFYDATA: signature 0x34753423, the NIM_* message, then a
//! NOTIFYICONDATA in its fixed 32-bit-handle layout. We keep the icons, draw them in the taskbar and
//! the ^ overflow flyout, and forward mouse events back exactly like Explorer.
//! The real "Shell_TrayWnd" is only created with `--shell`; `--tray-selftest` proves the protocol on a
//! private class name without touching Explorer.

use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};
use std::sync::Mutex;

use windows::core::{w, GUID, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::flyout::{rect, rrect};
use crate::quick::rgb;

pub static HOST: AtomicIsize = AtomicIsize::new(0);
pub static FLOW: AtomicIsize = AtomicIsize::new(0); // overflow flyout
/// Window to repaint when icons change (the shellbits taskbar).
pub static LISTENER: AtomicIsize = AtomicIsize::new(0);

const SIG: u32 = 0x3475_3423;
const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIM_SETVERSION: u32 = 4;
const NIF_MESSAGE: u32 = 1;
const NIF_ICON: u32 = 2;
const NIF_TIP: u32 = 4;
const NIF_STATE: u32 = 8;
const NIF_GUID: u32 = 0x20;
const NIS_HIDDEN: u32 = 1;
const NIN_SELECT: u32 = WM_USER;
const NIN_POPUPOPEN: u32 = WM_USER + 6;

/// What one Shell_NotifyIcon call carries (only the fields we use).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Nid {
    pub owner: isize,
    pub uid: u32,
    pub flags: u32,
    pub callback: u32,
    pub hicon: isize,
    pub tip: String,
    pub state: u32,
    pub state_mask: u32,
    pub version: u32,
    pub guid: Option<GUID>,
}

/// Parse the WM_COPYDATA payload. Layout (bytes): 0 signature, 4 message, then NOTIFYICONDATA32:
/// 8 cbSize, 12 hWnd(32-bit), 16 uID, 20 uFlags, 24 uCallbackMessage, 28 hIcon(32-bit),
/// 32 szTip[128], 288 dwState, 292 dwStateMask, 296 szInfo[256], 808 uVersion, 812 szInfoTitle[64],
/// 940 dwInfoFlags, 944 guidItem.
pub fn parse(b: &[u8]) -> Option<(u32, Nid)> {
    let u = |o: usize| b.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap()));
    if u(0)? != SIG || b.len() < 300 {
        return None;
    }
    let msg = u(4)?;
    let flags = u(20)?;
    let tip: Vec<u16> = (0..128).map(|i| u16::from_le_bytes([b[32 + 2 * i], b[33 + 2 * i]])).take_while(|&c| c != 0).collect();
    let guid = (flags & NIF_GUID != 0 && b.len() >= 960).then(|| {
        let g = &b[944..960];
        GUID::from_values(
            u32::from_le_bytes(g[0..4].try_into().unwrap()),
            u16::from_le_bytes(g[4..6].try_into().unwrap()),
            u16::from_le_bytes(g[6..8].try_into().unwrap()),
            g[8..16].try_into().unwrap(),
        )
    });
    Some((
        msg,
        Nid {
            owner: u(12)? as i32 as isize,
            uid: u(16)?,
            flags,
            callback: u(24)?,
            hicon: u(28)? as i32 as isize,
            tip: String::from_utf16_lossy(&tip),
            state: u(288)?,
            state_mask: u(292)?,
            version: u(808).unwrap_or(0),
            guid,
        },
    ))
}

/// Build the payload Shell_NotifyIconW would send (used by the self-test and unit tests).
pub fn build(msg: u32, n: &Nid) -> Vec<u8> {
    let mut b = vec![0u8; 964];
    let mut put = |o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    put(0, SIG);
    put(4, msg);
    put(8, 956);
    put(12, n.owner as u32);
    put(16, n.uid);
    put(20, n.flags);
    put(24, n.callback);
    put(28, n.hicon as u32);
    put(288, n.state);
    put(292, n.state_mask);
    put(808, n.version);
    for (i, c) in n.tip.encode_utf16().take(127).enumerate() {
        b[32 + 2 * i..34 + 2 * i].copy_from_slice(&c.to_le_bytes());
    }
    if let Some(g) = n.guid {
        b[944..948].copy_from_slice(&g.data1.to_le_bytes());
        b[948..950].copy_from_slice(&g.data2.to_le_bytes());
        b[950..952].copy_from_slice(&g.data3.to_le_bytes());
        b[952..960].copy_from_slice(&g.data4);
    }
    b
}

/// (wParam, lParam) for a callback message, the way Explorer packs it for each NOTIFYICON_VERSION.
pub fn pack(version: u32, event: u32, uid: u32, x: i32, y: i32) -> (usize, isize) {
    if version >= 4 {
        let wp = ((y as u32 & 0xFFFF) << 16 | (x as u32 & 0xFFFF)) as usize;
        let lp = ((uid & 0xFFFF) << 16 | (event & 0xFFFF)) as i32 as isize;
        (wp, lp)
    } else {
        (uid as usize, event as isize)
    }
}

#[derive(Clone)]
pub struct Icon {
    pub nid: Nid,
    pub px: Vec<u8>, // 16x16 premultiplied BGRA
    pub promoted: bool,
}

pub static ICONS: Mutex<Vec<Icon>> = Mutex::new(Vec::new());

fn same(a: &Nid, b: &Nid) -> bool {
    match (a.guid, b.guid) {
        (Some(x), Some(y)) => x == y,
        _ => a.owner == b.owner && a.uid == b.uid,
    }
}

/// Apply one NIM_* message to the icon list. Returns true if something visible changed.
pub fn apply(list: &mut Vec<Icon>, msg: u32, n: Nid, px: impl FnOnce(isize) -> Vec<u8>, exe: impl FnOnce(isize) -> String) -> bool {
    let pos = list.iter().position(|i| same(&i.nid, &n));
    match (msg, pos) {
        (NIM_ADD, None) => {
            let exe = exe(n.owner);
            let promoted = is_promoted(&exe);
            list.push(Icon { px: if n.flags & NIF_ICON != 0 { px(n.hicon) } else { Vec::new() }, nid: n, promoted });
            true
        }
        (NIM_ADD | NIM_MODIFY, Some(i)) => {
            let it = &mut list[i];
            if n.flags & NIF_MESSAGE != 0 {
                it.nid.callback = n.callback;
            }
            if n.flags & NIF_ICON != 0 {
                it.nid.hicon = n.hicon;
                it.px = px(n.hicon);
            }
            if n.flags & NIF_TIP != 0 {
                it.nid.tip = n.tip;
            }
            if n.flags & NIF_STATE != 0 {
                it.nid.state = (it.nid.state & !n.state_mask) | (n.state & n.state_mask);
            }
            true
        }
        (NIM_DELETE, Some(i)) => {
            list.remove(i);
            true
        }
        (NIM_SETVERSION, Some(i)) => {
            list[i].nid.version = n.version;
            false
        }
        _ => false,
    }
}

/// Windows' own "show this icon on the taskbar" choice: HKCU\Control Panel\NotifyIconSettings\<id>
/// has ExecutablePath + IsPromoted. Matched by file name (paths there use known-folder GUID prefixes).
pub fn is_promoted(exe: &str) -> bool {
    let name = exe.rsplit(['\\', '/']).next().unwrap_or("").to_lowercase();
    if name.is_empty() {
        return false;
    }
    promoted_names().iter().any(|n| *n == name)
}

fn promoted_names() -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        let mut root = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, w!("Control Panel\\NotifyIconSettings"), Some(0), KEY_READ, &mut root).is_err() {
            return out;
        }
        for i in 0.. {
            let mut name = [0u16; 64];
            let mut n = name.len() as u32;
            if RegEnumKeyExW(root, i, Some(windows::core::PWSTR(name.as_mut_ptr())), &mut n, None, None, None, None).is_err() {
                break;
            }
            let sub = HSTRING::from_wide(&name[..n as usize]);
            let mut v = 0u32;
            let mut sz = 4u32;
            let promoted = RegGetValueW(root, &sub, w!("IsPromoted"), RRF_RT_REG_DWORD, None, Some(&mut v as *mut _ as *mut c_void), Some(&mut sz)).is_ok() && v == 1;
            if !promoted {
                continue;
            }
            let mut buf = [0u16; 520];
            let mut sz = (buf.len() * 2) as u32;
            if RegGetValueW(root, &sub, w!("ExecutablePath"), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut c_void), Some(&mut sz)).is_ok() {
                let s = String::from_utf16_lossy(&buf[..(sz as usize / 2).saturating_sub(1)]);
                out.push(s.rsplit(['\\', '/']).next().unwrap_or("").to_lowercase());
            }
        }
        let _ = RegCloseKey(root);
    }
    out
}

fn owner_exe(owner: isize) -> String {
    use windows::Win32::System::Threading::*;
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(HWND(owner as *mut c_void), Some(&mut pid));
        let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 1024];
        let mut n = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut n).is_ok();
        let _ = CloseHandle(p);
        if ok { String::from_utf16_lossy(&buf[..n as usize]) } else { String::new() }
    }
}

/// 16x16 premultiplied BGRA from any HICON (alpha icons and old masked icons alike): draw it on
/// black and on white; alpha = 255 - (white - black), colour = the black render.
pub fn icon_px(hicon: isize) -> Vec<u8> {
    const S: i32 = 16;
    unsafe {
        let h = HICON(hicon as *mut c_void);
        let render = |bg: u8| -> Option<Vec<u8>> {
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: S, biHeight: -S, biPlanes: 1, biBitCount: 32, ..Default::default() },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let dib = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            let dc = CreateCompatibleDC(None);
            let old = SelectObject(dc, dib.into());
            std::ptr::write_bytes(bits as *mut u8, bg, (S * S * 4) as usize);
            let ok = DrawIconEx(dc, 0, 0, h, S, S, 0, None, DI_NORMAL).is_ok();
            let px = std::slice::from_raw_parts(bits as *const u8, (S * S * 4) as usize).to_vec();
            SelectObject(dc, old);
            let _ = DeleteDC(dc);
            let _ = DeleteObject(dib.into());
            ok.then_some(px)
        };
        let (Some(k), Some(wh)) = (render(0), render(255)) else { return Vec::new() };
        let mut out = vec![0u8; (S * S * 4) as usize];
        for i in 0..(S * S) as usize {
            let a = 255 - (0..3).map(|c| wh[i * 4 + c] as i32 - k[i * 4 + c] as i32).max().unwrap_or(0).clamp(0, 255);
            for c in 0..3 {
                out[i * 4 + c] = k[i * 4 + c].min(a as u8); // black render is already premultiplied
            }
            out[i * 4 + 3] = a as u8;
        }
        out
    }
}

fn changed() {
    // a readable list of what the tray holds right now (used by the shell dry-run checks)
    let list: Vec<String> = ICONS.lock().unwrap().iter().map(|i| format!("{} | owner={} uid={} v{} cb={:#x} promoted={} hidden={} icon={}", i.nid.tip.replace('\n', " "), i.nid.owner, i.nid.uid, i.nid.version, i.nid.callback, i.promoted, i.nid.state & NIS_HIDDEN != 0, !i.px.is_empty())).collect();
    let _ = std::fs::write(std::env::temp_dir().join("shellbits-tray.txt"), list.join("\n"));
    let l = LISTENER.load(Relaxed);
    if l != 0 {
        unsafe {
            let _ = InvalidateRect(Some(HWND(l as *mut c_void)), None, false);
        }
    }
    let f = HWND(FLOW.load(Relaxed) as *mut c_void);
    unsafe {
        if IsWindowVisible(f).as_bool() {
            let _ = InvalidateRect(Some(f), None, false);
        }
    }
}

/// Mouse on a tray icon -> the owning app, packed like Explorer does. `button`: 0 left, 1 right, 2 double.
pub fn click(index: usize, button: u8, x: i32, y: i32) {
    let Some(it) = ICONS.lock().unwrap().get(index).cloned() else { return };
    let n = &it.nid;
    if n.callback == 0 {
        return;
    }
    unsafe {
        let owner = HWND(n.owner as *mut c_void);
        let mut pid = 0;
        GetWindowThreadProcessId(owner, Some(&mut pid));
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(pid); // so its menu/window can come to the front
        // tray menus close unless their owner is the foreground window: hand it over first, like Explorer
        crate::flyout::force_foreground(owner);
        let events: &[u32] = match (button, n.version >= 4) {
            (0, true) => &[WM_LBUTTONDOWN, WM_LBUTTONUP, NIN_SELECT],
            (0, false) => &[WM_LBUTTONDOWN, WM_LBUTTONUP],
            (1, true) => &[WM_RBUTTONDOWN, WM_RBUTTONUP, WM_CONTEXTMENU],
            (1, false) => &[WM_RBUTTONDOWN, WM_RBUTTONUP],
            _ => &[WM_LBUTTONDBLCLK],
        };
        for &e in events {
            let (wp, lp) = pack(n.version, e, n.uid, x, y);
            let _ = PostMessageW(Some(owner), n.callback, WPARAM(wp), LPARAM(lp));
        }
    }
}

pub fn hover(index: usize, x: i32, y: i32) {
    let Some(it) = ICONS.lock().unwrap().get(index).cloned() else { return };
    let n = &it.nid;
    if n.callback == 0 {
        return;
    }
    let (wp, lp) = pack(n.version, if n.version >= 4 { NIN_POPUPOPEN } else { WM_MOUSEMOVE }, n.uid, x, y);
    unsafe {
        let _ = PostMessageW(Some(HWND(n.owner as *mut c_void)), n.callback, WPARAM(wp), LPARAM(lp));
    }
}

/// Visible icons split into (promoted on the taskbar, hidden in the overflow), as list indices.
pub fn split() -> (Vec<usize>, Vec<usize>) {
    let l = ICONS.lock().unwrap();
    let vis = l.iter().enumerate().filter(|(_, i)| i.nid.state & NIS_HIDDEN == 0 && !i.px.is_empty());
    let (mut a, mut b) = (Vec::new(), Vec::new());
    for (i, it) in vis {
        if it.promoted { a.push(i) } else { b.push(i) }
    }
    (a, b)
}

pub fn pixels(i: usize) -> Option<Vec<u8>> {
    ICONS.lock().unwrap().get(i).map(|i| i.px.clone())
}

// ======================= host window =======================

pub unsafe extern "system" fn host_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_COPYDATA {
        let cds = &*(lp.0 as *const COPYDATASTRUCT);
        if cds.dwData == 1 && !cds.lpData.is_null() {
            let b = std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize);
            if let Some((m, n)) = parse(b) {
                let vis = apply(&mut ICONS.lock().unwrap(), m, n, icon_px, owner_exe);
                if vis {
                    changed();
                }
                return LRESULT(1);
            }
            return LRESULT(0);
        }
        // Shell_NotifyIconGetRect (dwData 3): "where is my icon?" Many apps (tray-icon crate, Electron)
        // ask this on every click and ignore the click if we don't answer.
        // Payload: magic, message (1 = left/top, 2 = right/bottom), cbSize, pad, hWnd32, uID, guid.
        if cds.dwData == 3 && !cds.lpData.is_null() && cds.cbData >= 24 {
            let b = std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize);
            let u = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
            let (m, owner, uid) = (u(4), u(16) as i32 as isize, u(20));
            let known = ICONS.lock().unwrap().iter().any(|i| i.nid.owner == owner && (i.nid.uid == uid || i.nid.guid.is_some()));
            if known {
                // ponytail: the icon is under the cursor whenever an app asks mid-click; track real slots if an app asks at other times
                let mut p = POINT::default();
                let _ = GetCursorPos(&mut p);
                let (x, y) = if m == 1 { (p.x - 8, p.y - 8) } else { (p.x + 8, p.y + 8) };
                return LRESULT((((y as u32 & 0xFFFF) << 16) | (x as u32 & 0xFFFF)) as isize);
            }
            return LRESULT(0);
        }
        // ponytail: AppBar messages (dwData 0) are acknowledged but not implemented; add when an app needs them
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

/// Create the tray host. `class` is "Shell_TrayWnd" in shell mode (apps find it by that name).
pub unsafe fn create_host(inst: HINSTANCE, class: PCWSTR) -> windows::core::Result<HWND> {
    let wc = WNDCLASSW { lpfnWndProc: Some(host_proc), hInstance: inst, lpszClassName: class, ..Default::default() };
    RegisterClassW(&wc);
    let h = CreateWindowExW(WS_EX_TOOLWINDOW, class, w!(""), WS_POPUP, 0, 0, 0, 0, None, None, Some(inst), None)?;
    // some apps look for the notification area child
    let wc2 = WNDCLASSW { lpfnWndProc: Some(child_proc), hInstance: inst, lpszClassName: w!("TrayNotifyWnd"), ..Default::default() };
    RegisterClassW(&wc2);
    let _ = CreateWindowExW(WINDOW_EX_STYLE(0), w!("TrayNotifyWnd"), w!(""), WS_CHILD, 0, 0, 0, 0, Some(h), None, Some(inst), None);
    HOST.store(h.0 as isize, Relaxed);
    Ok(h)
}

/// Tell every running app the tray (re)appeared, so they re-add their icons.
pub unsafe fn announce() {
    let m = RegisterWindowMessageW(w!("TaskbarCreated"));
    let _ = PostMessageW(Some(HWND_BROADCAST), m, WPARAM(0), LPARAM(0));
}

// ======================= overflow flyout (^) =======================

const CELL: f32 = 40.0;
const PAD: f32 = 8.0;
const COLS: usize = 4;

thread_local! {
    static FLOW_HOVER: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static FLOW_RT: std::cell::RefCell<Option<ID2D1DCRenderTarget>> = const { std::cell::RefCell::new(None) };
}

fn flow_size(n: usize) -> (f32, f32) {
    let cols = n.clamp(1, COLS);
    let rows = n.div_ceil(COLS).max(1);
    (PAD * 2.0 + CELL * cols as f32, PAD * 2.0 + CELL * rows as f32)
}

fn flow_hit(n: usize, x: f32, y: f32) -> Option<usize> {
    let (c, r) = (((x - PAD) / CELL).floor(), ((y - PAD) / CELL).floor());
    if c < 0.0 || r < 0.0 || c >= COLS as f32 {
        return None;
    }
    let i = r as usize * COLS + c as usize;
    (i < n).then_some(i)
}

unsafe fn draw_flow(rt: &ID2D1DCRenderTarget, hidden: &[usize], hover: Option<usize>) -> windows::core::Result<()> {
    rt.Clear(Some(&rgb(0x2C2C2C)));
    let bp = D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
    };
    for (k, &i) in hidden.iter().enumerate() {
        let (cx, cy) = (PAD + CELL * (k % COLS) as f32, PAD + CELL * (k / COLS) as f32);
        if hover == Some(k) {
            rt.FillRoundedRectangle(&rrect(rect(cx + 2.0, cy + 2.0, cx + CELL - 2.0, cy + CELL - 2.0), 4.0), &rt.CreateSolidColorBrush(&rgb(0x3D3D3D), None)?);
        }
        if let Some(px) = pixels(i).filter(|p| p.len() == 16 * 16 * 4) {
            let b = rt.CreateBitmap(D2D_SIZE_U { width: 16, height: 16 }, Some(px.as_ptr() as *const c_void), 64, &bp)?;
            crate::flyout::draw_icon(rt, &b, cx + 12.0, cy + 12.0, 16.0);
        }
    }
    Ok(())
}

unsafe fn flow_rt() -> Option<ID2D1DCRenderTarget> {
    FLOW_RT.with(|r| {
        if r.borrow().is_none() {
            let f: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok()?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                ..Default::default()
            };
            *r.borrow_mut() = f.CreateDCRenderTarget(&props).ok();
        }
        r.borrow().clone()
    })
}

/// Open/close the overflow above the ^ chevron (screen rect of the chevron in pixels).
pub unsafe fn toggle_flow(anchor: RECT) {
    let f = HWND(FLOW.load(Relaxed) as *mut c_void);
    if IsWindowVisible(f).as_bool() {
        let _ = ShowWindow(f, SW_HIDE);
        return;
    }
    let (_, hidden) = split();
    let s = GetDpiForWindow(f) as f32 / 96.0;
    let (w, h) = flow_size(hidden.len());
    let (w, h) = ((w * s).round() as i32, (h * s).round() as i32);
    let x = (anchor.left + anchor.right) / 2 - w / 2;
    let y = anchor.top - h - (12.0 * s) as i32;
    let _ = SetWindowPos(f, Some(HWND_TOPMOST), x, y, w, h, SWP_SHOWWINDOW);
    crate::flyout::force_foreground(f);
}

pub unsafe extern "system" fn flow_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    let dips = |lp: LPARAM| ((lp.0 & 0xFFFF) as i16 as f32 / s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s);
    match msg {
        crate::flyout::WM_DISMISS => {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        WM_MOUSEMOVE => {
            let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::TrackMouseEvent(&mut tme);
            let (x, y) = dips(lp);
            let (_, hidden) = split();
            let h = flow_hit(hidden.len(), x, y);
            if FLOW_HOVER.with(|c| c.replace(h)) != h {
                if let Some(k) = h {
                    let mut p = POINT::default();
                    let _ = GetCursorPos(&mut p);
                    hover(hidden[k], p.x, p.y);
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
        WM_MOUSELEAVE => {
            FLOW_HOVER.with(|c| c.set(None));
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        WM_LBUTTONUP | WM_RBUTTONUP | WM_LBUTTONDBLCLK => {
            let (x, y) = dips(lp);
            let (_, hidden) = split();
            if let Some(k) = flow_hit(hidden.len(), x, y) {
                let mut p = POINT::default();
                let _ = GetCursorPos(&mut p);
                let button = match msg { WM_LBUTTONUP => 0, WM_RBUTTONUP => 1, _ => 2 };
                // like Explorer: close the overflow first, so the app's own window or menu can take focus
                let _ = ShowWindow(hwnd, SW_HIDE);
                click(hidden[k], button, p.x, p.y);
            }
        }
        WM_MOUSEACTIVATE => return LRESULT(MA_ACTIVATE as isize),
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let params = BP_PAINTPARAMS { cbSize: size_of::<BP_PAINTPARAMS>() as u32, dwFlags: BPPF_ERASE, ..Default::default() };
            let mut mem = HDC::default();
            let pb = BeginBufferedPaint(hdc, &rc, BPBF_TOPDOWNDIB, Some(&params), &mut mem);
            if !mem.is_invalid() {
                if let Some(rt) = flow_rt() {
                    if rt.BindDC(mem, &rc).is_ok() {
                        rt.SetDpi(96.0 * s, 96.0 * s);
                        rt.BeginDraw();
                        let (_, hidden) = split();
                        let _ = draw_flow(&rt, &hidden, FLOW_HOVER.with(|c| c.get()));
                        let _ = rt.EndDraw(None, None);
                    }
                }
                let _ = EndBufferedPaint(pb, true);
            }
            let _ = EndPaint(hwnd, &ps);
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

// ======================= tests without Explorer =======================

/// `shellbits --tray-selftest`: a private host + a client window exchanging real WM_COPYDATA messages.
pub unsafe fn selftest() -> windows::core::Result<()> {
    let inst: HINSTANCE = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?.into();
    let host = create_host(inst, w!("shellbits_tray_test"))?;
    let client = CreateWindowExW(WINDOW_EX_STYLE(0), w!("Static"), w!("client"), WS_POPUP, 0, 0, 0, 0, None, None, Some(inst), None)?;
    let icon = LoadIconW(None, IDI_APPLICATION)?;
    let send = |m: u32, n: &Nid| {
        let b = build(m, n);
        let cds = COPYDATASTRUCT { dwData: 1, cbData: b.len() as u32, lpData: b.as_ptr() as *mut c_void };
        SendMessageW(host, WM_COPYDATA, Some(WPARAM(client.0 as usize)), Some(LPARAM(&cds as *const _ as isize))).0
    };
    let base = Nid { owner: client.0 as isize, uid: 7, flags: NIF_MESSAGE | NIF_ICON | NIF_TIP, callback: WM_APP + 1, hicon: icon.0 as isize, tip: "Test icon".into(), ..Default::default() };
    let mut log = String::new();
    let mut check = |name: &str, ok: bool| {
        log.push_str(&format!("{} {name}\n", if ok { "PASS" } else { "FAIL" }));
        ok
    };
    let r = send(NIM_ADD, &base);
    let mut all = check("add returns TRUE", r == 1);
    all &= check("icon listed with tip and pixels", ICONS.lock().unwrap().first().is_some_and(|i| i.nid.tip == "Test icon" && i.px.len() == 1024 && i.px.chunks(4).any(|p| p[3] > 0)));
    send(NIM_MODIFY, &Nid { flags: NIF_TIP, tip: "Renamed".into(), ..base.clone() });
    all &= check("modify updates tip", ICONS.lock().unwrap().first().is_some_and(|i| i.nid.tip == "Renamed"));
    send(NIM_MODIFY, &Nid { flags: NIF_STATE, state: NIS_HIDDEN, state_mask: NIS_HIDDEN, ..base.clone() });
    all &= check("hidden state removes it from view", split().0.is_empty() && split().1.is_empty());
    send(NIM_DELETE, &base);
    all &= check("delete removes it", ICONS.lock().unwrap().is_empty());
    let _ = DestroyWindow(client);
    let _ = DestroyWindow(host);
    let _ = std::fs::write("tray-selftest.txt", &log);
    if all { Ok(()) } else { Err(windows::core::Error::from(E_FAIL)) }
}

/// `shellbits --snapshot-tray out.bmp`: the overflow flyout drawn off-screen with real app icons.
pub unsafe fn snapshot(path: &str) -> windows::core::Result<()> {
    use windows::Win32::UI::Shell::ExtractIconExW;
    let exes = [
        "C:\\Windows\\explorer.exe",
        "C:\\Windows\\System32\\notepad.exe",
        "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
        "C:\\Windows\\System32\\taskmgr.exe",
        "C:\\Windows\\System32\\mspaint.exe",
        "C:\\Windows\\regedit.exe",
    ];
    {
        let mut l = ICONS.lock().unwrap();
        for (k, e) in exes.iter().enumerate() {
            let mut small = HICON::default();
            if ExtractIconExW(&HSTRING::from(*e), 0, None, Some(&mut small), 1) == 0 || small.is_invalid() {
                continue;
            }
            l.push(Icon { nid: Nid { uid: k as u32, ..Default::default() }, px: icon_px(small.0 as isize), promoted: false });
            let _ = DestroyIcon(small);
        }
    }
    let (_, hidden) = split();
    let (w, h) = flow_size(hidden.len());
    let (w, h) = (w as i32, h as i32);
    let bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, ..Default::default() },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    let dib = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0)?;
    let dc = CreateCompatibleDC(None);
    let old = SelectObject(dc, dib.into());
    let rt = flow_rt().ok_or(windows::core::Error::from(E_FAIL))?;
    rt.BindDC(dc, &RECT { left: 0, top: 0, right: w, bottom: h })?;
    rt.BeginDraw();
    draw_flow(&rt, &hidden, Some(1))?;
    rt.EndDraw(None, None)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrip() {
        let n = Nid { owner: 0x1234, uid: 9, flags: NIF_MESSAGE | NIF_TIP | NIF_GUID, callback: 0x8001, hicon: 0x55, tip: "Discord".into(), version: 4, guid: Some(GUID::from_u128(0x01234567_89ab_cdef_0123_456789abcdef)), ..Default::default() };
        let (m, p) = parse(&build(NIM_MODIFY, &n)).unwrap();
        assert_eq!(m, NIM_MODIFY);
        assert_eq!(p, n);
        assert!(parse(&[0u8; 400]).is_none(), "wrong signature is rejected");
    }

    #[test]
    fn callback_packing() {
        // v4: wParam = x/y, lParam = event | uid << 16
        assert_eq!(pack(4, WM_CONTEXTMENU, 3, 100, 200), (200 << 16 | 100, (3 << 16 | WM_CONTEXTMENU) as isize));
        // old versions: wParam = uid, lParam = event
        assert_eq!(pack(0, WM_LBUTTONUP, 3, 100, 200), (3, WM_LBUTTONUP as isize));
    }

    #[test]
    fn apply_add_modify_delete() {
        let mut l = Vec::new();
        let n = Nid { owner: 1, uid: 1, flags: NIF_ICON | NIF_TIP, tip: "a".into(), ..Default::default() };
        assert!(apply(&mut l, NIM_ADD, n.clone(), |_| vec![1; 1024], |_| String::new()));
        assert!(apply(&mut l, NIM_MODIFY, Nid { flags: NIF_TIP, tip: "b".into(), ..n.clone() }, |_| vec![], |_| String::new()));
        assert_eq!(l[0].nid.tip, "b");
        assert_eq!(l[0].px.len(), 1024, "a tip-only change keeps the icon");
        assert!(apply(&mut l, NIM_DELETE, n, |_| vec![], |_| String::new()));
        assert!(l.is_empty());
    }

    #[test]
    fn flyout_grid() {
        assert_eq!(flow_size(6), (PAD * 2.0 + CELL * 4.0, PAD * 2.0 + CELL * 2.0));
        assert_eq!(flow_hit(6, PAD + 5.0, PAD + CELL + 5.0), Some(4));
        assert_eq!(flow_hit(6, PAD + CELL * 3.0, PAD + CELL + 5.0), None);
    }
}

unsafe extern "system" fn child_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}

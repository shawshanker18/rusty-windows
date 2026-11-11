//! Video wallpaper (replaces Lively + mpv): Media Foundation decodes on the GPU straight into a
//! Direct3D flip swap chain that sits behind the desktop icons. Pauses while a maximized or
//! fullscreen window covers the desktop, so it costs nothing while you work or game.
//! Video: `wallpaper.mp4` next to shellbits.exe. No file -> no wallpaper, nothing loaded.

use std::ffi::c_void;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use windows::core::{implement, w, Interface, BOOL, BSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::*;

/// The wallpaper video next to the exe. A copy already scaled to this screen's height
/// ("wallpaper-1080.mp4" on a 1080-line screen) is preferred: decoding a 4K file for a 1080p
/// screen keeps a pool of 4K frames in memory - measured ~230 MB against ~60 MB for the scaled copy.
pub fn video_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let screen_h = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    let (scaled, original) = (exe.with_file_name(format!("wallpaper-{screen_h}.mp4")), exe.with_file_name("wallpaper.mp4"));
    let changed = |p: &PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    // a scaled copy older than the original belongs to a previous wallpaper: ignore it
    match (changed(&scaled), changed(&original)) {
        (Some(s), Some(o)) if s >= o => Some(scaled),
        (Some(_), None) => Some(scaled),
        (_, Some(_)) => Some(original),
        _ => None,
    }
}

/// Media Foundation insists on an event sink; we don't need any events.
#[implement(IMFMediaEngineNotify)]
struct Notify;
impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, _event: u32, _p1: usize, _p2: u32) -> windows::core::Result<()> {
        Ok(())
    }
}

// ---------------- desktop layer (same recipe as Lively for Win11 24H2+) ----------------

pub static WALL: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// Shell mode (no Explorer, no Progman): our own bottom-most full-screen window.
pub unsafe fn attach_shell(inst: HINSTANCE) -> Option<HWND> {
    let (x, y, cx, cy) = (
        GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN),
        GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN),
    );
    let hwnd = CreateWindowExW(WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW, w!("shellbits_wall"), w!("Wallpaper"), WS_POPUP, x, y, cx, cy, None, None, Some(inst), None).ok()?;
    let _ = SetWindowPos(hwnd, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    WALL.store(hwnd.0 as isize, std::sync::atomic::Ordering::Relaxed);
    Some(hwnd)
}

/// Creates our window behind the desktop icons. Returns it, or None if Explorer isn't ready.
pub unsafe fn attach(inst: HINSTANCE) -> Option<HWND> {
    let progman = FindWindowW(w!("Progman"), None).ok()?;
    // ask Explorer to create the WorkerW layer behind the icons
    let _ = SendMessageTimeoutW(progman, 0x052C, WPARAM(0xD), LPARAM(1), SMTO_NORMAL, 1000, None);
    let (x, y, cx, cy) = (
        GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN),
        GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN),
    );
    let hwnd = CreateWindowExW(WS_EX_NOACTIVATE, w!("shellbits_wall"), w!(""), WS_POPUP, x, y, cx, cy, None, None, Some(inst), None).ok()?;
    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
    // style must be WS_CHILD *before* SetParent or DWM keeps treating it as a popup
    let _ = SetWindowLongPtrW(hwnd, GWL_STYLE, WS_CHILD.0 as isize);
    if let Ok(worker) = FindWindowExW(Some(progman), None, w!("WorkerW"), None) {
        // 24H2+: WorkerW lives inside Progman. Order: icons, us, WorkerW.
        let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (WS_EX_LAYERED | WS_EX_NOACTIVATE).0 as isize);
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
        let _ = SetParent(hwnd, Some(progman));
        let icons = FindWindowExW(Some(progman), None, w!("SHELLDLL_DefView"), None).ok();
        let _ = SetWindowPos(hwnd, icons, 0, 0, 0, 0, flags);
        let _ = SetWindowPos(worker, Some(HWND_BOTTOM), 0, 0, 0, 0, flags);
    } else {
        // older Windows: top-level WorkerW right after the one that holds the icons
        let mut worker = HWND::default();
        let _ = EnumWindows(Some(find_worker), LPARAM(&mut worker as *mut _ as isize));
        if worker.is_invalid() {
            let _ = DestroyWindow(hwnd);
            return None;
        }
        let _ = SetParent(hwnd, Some(worker));
    }
    let _ = SetWindowPos(hwnd, None, 0, 0, cx, cy, SWP_NOZORDER | SWP_NOACTIVATE);
    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    Some(hwnd)
}

unsafe extern "system" fn find_worker(h: HWND, out: LPARAM) -> BOOL {
    if FindWindowExW(Some(h), None, w!("SHELLDLL_DefView"), None).is_ok() {
        if let Ok(w) = FindWindowExW(None, Some(h), w!("WorkerW"), None) {
            *(out.0 as *mut HWND) = w;
            return FALSE;
        }
    }
    TRUE
}

pub unsafe extern "system" fn wall_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1), // the swap chain owns every pixel
        WM_WINDOWPOSCHANGING if GetParent(hwnd).map_or(true, |p| p.is_invalid()) => {
            // shell mode: always at the very back, except while Win+D has the desktop raised over the windows
            if !crate::session::DESKTOP_UP.load(std::sync::atomic::Ordering::Relaxed) {
                (*(lp.0 as *mut WINDOWPOS)).hwndInsertAfter = HWND_BOTTOM;
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

// ---------------- player ----------------

/// Is the desktop hidden behind a maximized / fullscreen window?
unsafe fn desktop_covered() -> bool {
    let fg = GetForegroundWindow();
    if fg.is_invalid() || IsIconic(fg).as_bool() {
        return false;
    }
    if IsZoomed(fg).as_bool() {
        return true;
    }
    let mut r = RECT::default();
    let _ = GetWindowRect(fg, &mut r);
    let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    let _ = GetMonitorInfoW(MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST), &mut mi);
    let m = mi.rcMonitor;
    r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
}

/// Runs forever on its own thread: decode -> swap chain -> present, paused while covered.
pub fn spawn(hwnd: HWND, video: PathBuf) {
    let h = hwnd.0 as isize;
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if let Err(e) = play(HWND(h as *mut c_void), &video) {
            let log = video.with_file_name("wallpaper-error.txt");
            let _ = std::fs::write(log, format!("{e:?}"));
        }
    });
}

unsafe fn play(hwnd: HWND, video: &std::path::Path) -> windows::core::Result<()> {
    MFStartup(MF_VERSION, MFSTARTUP_FULL)?;

    // ponytail: default GPU. Tried the power-saving one (2026-11-11): both drivers loaded and committed memory doubled (190 -> 373 MB)
    let mut device = None;
    D3D11CreateDevice(
        None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(),
        D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
        None, D3D11_SDK_VERSION, Some(&mut device), None, None,
    )?;
    let device = device.unwrap();
    let _ = device.cast::<ID3D11Multithread>()?.SetMultithreadProtected(true);

    let mut token = 0;
    let mut mgr = None;
    MFCreateDXGIDeviceManager(&mut token, &mut mgr)?;
    let mgr = mgr.unwrap();
    mgr.ResetDevice(&device, token)?;

    let mut attr = None;
    MFCreateAttributes(&mut attr, 3)?;
    let attr = attr.unwrap();
    let notify: IMFMediaEngineNotify = Notify.into();
    attr.SetUnknown(&MF_MEDIA_ENGINE_DXGI_MANAGER, &mgr)?;
    attr.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify)?;
    attr.SetUINT32(&MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32)?;
    let factory: IMFMediaEngineClassFactory = CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)?;
    let engine = factory.CreateInstance(0, &attr)?;
    engine.SetMuted(true)?;
    engine.SetLoop(true)?;
    engine.SetAutoPlay(true)?;
    engine.SetSource(&BSTR::from(video.to_string_lossy().as_ref()))?;

    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let (w, h) = ((rc.right - rc.left).max(1) as u32, (rc.bottom - rc.top).max(1) as u32);
    let dxgi: IDXGIFactory2 = device.cast::<IDXGIDevice>()?.GetAdapter()?.GetParent()?;
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: w,
        Height: h,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        Scaling: DXGI_SCALING_STRETCH,
        ..Default::default()
    };
    let chain = dxgi.CreateSwapChainForHwnd(&device, hwnd, &desc, None, None)?;
    let dst = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };

    let mut paused = false;
    let mut last_check = Instant::now() - Duration::from_secs(1);
    let mut src: Option<MFVideoNormalizedRect> = None;
    let mut last_pts = -1i64;
    loop {
        if last_check.elapsed() >= Duration::from_millis(500) {
            last_check = Instant::now();
            let covered = desktop_covered();
            if covered != paused {
                paused = covered;
                if paused {
                    engine.Pause()?;
                    // nothing to show: hand memory back to Windows until the desktop is visible again
                    let _ = windows::Win32::System::ProcessStatus::K32EmptyWorkingSet(windows::Win32::System::Threading::GetCurrentProcess());
                } else {
                    engine.Play()?;
                }
            }
        }
        if paused {
            std::thread::sleep(Duration::from_millis(250));
            continue;
        }
        // crop to fill (like "UniformToFill"), computed once the video size is known
        if src.is_none() {
            let (mut vw, mut vh) = (0, 0);
            if engine.GetNativeVideoSize(Some(&mut vw), Some(&mut vh)).is_ok() && vw > 0 && vh > 0 {
                let (va, sa) = (vw as f32 / vh as f32, w as f32 / h as f32);
                src = Some(if va > sa {
                    let k = sa / va;
                    MFVideoNormalizedRect { left: (1.0 - k) / 2.0, top: 0.0, right: (1.0 + k) / 2.0, bottom: 1.0 }
                } else {
                    let k = va / sa;
                    MFVideoNormalizedRect { left: 0.0, top: (1.0 - k) / 2.0, right: 1.0, bottom: (1.0 + k) / 2.0 }
                });
            }
        }
        // new frame = presentation time moved (S_FALSE "no frame yet" also maps to Ok)
        let fresh = matches!(engine.OnVideoStreamTick(), Ok(pts) if pts != last_pts && { last_pts = pts; true });
        if fresh {
            let back: ID3D11Texture2D = chain.GetBuffer(0)?;
            let s = src.unwrap_or(MFVideoNormalizedRect { left: 0.0, top: 0.0, right: 1.0, bottom: 1.0 });
            if engine.TransferVideoFrame(&back, Some(&s), &dst, None).is_ok() {
                let _ = chain.Present(1, DXGI_PRESENT(0));
                continue;
            }
        }
        std::thread::sleep(Duration::from_millis(4));
    }
}

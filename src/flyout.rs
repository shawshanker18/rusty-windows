//! One flyout window, four looks:
//!  - Volume: the Windows 11 volume pill, measured from the real one at 100% scale.
//!  - Media:  FluentFlyout's media card (album art, title, artist, prev / play / next).
//!  - Lock:   FluentFlyout's Caps / Num / Scroll Lock pill.
//!  - Text:   icon + one line (pin on top, errors).
//! All sizes are DIPs. Windows' own volume/media popup never shows: we own those keys.

use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, AtomicIsize, AtomicU32, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::Instant;

use windows::core::{w, Interface, IUnknown};
use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager as Gsmtc;
use windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eMultimedia, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, IStream, CLSCTX_ALL, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::System::WinRT::CreateStreamOverRandomAccessStream;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_TRACK: u32 = WM_APP + 1;
pub const WM_LOCK: u32 = WM_APP + 2;
pub const T_HIDE: usize = 2;
pub const T_ANIM: usize = 4;

// ---- timing (FluentFlyout defaults) ----
const ANIM_MS: f32 = 300.0; // slide + quadratic ease-out
const ANIM_DIST: f32 = 20.0;

// ---- Windows 11 volume pill ----
const VOL_W: f32 = 192.0;
const VOL_H: f32 = 48.0;
const VOL_GAP: f32 = 13.0; // above the taskbar
const VOL_ICON_X: f32 = 12.0;
const TRACK_X: f32 = 41.0;
const TRACK_W: f32 = 111.0;
const TRACK_Y: f32 = 22.0;
const TRACK_H: f32 = 4.0;
const NUM_X: f32 = 162.0;
const VOL_MS: u32 = 1800;

// ---- FluentFlyout media card ----
const MED_W: f32 = 310.0;
const MED_H: f32 = 102.0;
const MED_PAD: f32 = 10.0;
const ART: f32 = 82.0;
const ART_R: f32 = 4.0;
const COL_X: f32 = MED_PAD + ART + 8.0;
const ROW_Y: f32 = 58.0; // controls row top (35 tall)
const ROW_H: f32 = 35.0;
const BTN: f32 = 28.0;
const PLAY_BTN: f32 = 32.0;
const BTN_GAP: f32 = 8.0;
const FLY_GAP: f32 = 16.0;
const MED_MS: u32 = 3000;
const ART_PX: u32 = 164; // decoded at 2x for crisp HiDPI, tiny in RAM (105 KB)

// ---- FluentFlyout lock-keys pill ----
const LOCK_W: f32 = 160.0;
const LOCK_H: f32 = 50.0;
const LOCK_MS: u32 = 2000;

// ---- Windows dark-theme colours ----
pub(crate) const BG: (f32, f32, f32, f32) = (0.1725, 0.1725, 0.1725, 0.96); // #2C2C2C acrylic tint
pub(crate) const SECONDARY: f32 = 0.786; // TextFillColorSecondary
const TERTIARY: f32 = 0.5; // FluentFlyout artist / app name opacity
const TRACK_A: f32 = 0.5442; // ControlStrongFillColorDefault
pub(crate) const SUBTLE_A: f32 = 0.0605; // SubtleFillColorSecondary (placeholder art)

// Segoe Fluent Icons
const G_MUTE: u32 = 0xE74F;
const G_VOL: [u32; 4] = [0xE992, 0xE993, 0xE994, 0xE995];
const G_PLAY: u32 = 0xE768;
const G_PAUSE: u32 = 0xE769;
const G_NEXT: u32 = 0xE893;
const G_PREV: u32 = 0xE892;
const G_PIN: u32 = 0xE718;
const G_UNPIN: u32 = 0xE77A;
const G_NOTE: u32 = 0xEC4F;
const G_LOCK: u32 = 0xE72E;
const G_UNLOCK: u32 = 0xE785;

#[derive(Clone, Copy, PartialEq)]
#[repr(u32)]
enum Mode { Volume, Text, Media, Lock }

pub static FLYOUT: AtomicIsize = AtomicIsize::new(0);
static MODE: AtomicU32 = AtomicU32::new(0);
static GLYPH: AtomicU32 = AtomicU32::new(0xE995);
static LEVEL: AtomicI32 = AtomicI32::new(0);
static LOCK_VK: AtomicU32 = AtomicU32::new(0x14);
static TEXT: Mutex<String> = Mutex::new(String::new());

#[derive(Default)]
struct Track { title: String, artist: String, app: String, playing: bool, art: Option<Vec<u8>> }
static TRACK: Mutex<Option<Track>> = Mutex::new(None);

fn mode() -> Mode {
    match MODE.load(Relaxed) { 0 => Mode::Volume, 1 => Mode::Text, 2 => Mode::Media, _ => Mode::Lock }
}
fn set_mode(m: Mode) { MODE.store(m as u32, Relaxed); }

fn set_text(glyph: u32, text: &str) {
    GLYPH.store(glyph, Relaxed);
    *TEXT.lock().unwrap() = text.into();
    set_mode(Mode::Text);
}

// ======================= actions =======================

/// Talks to the audio endpoint directly, so the Windows volume popup never appears.
pub unsafe fn volume(id: i32) {
    let r = (|| -> windows::core::Result<(f32, bool)> {
        let e: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let v: IAudioEndpointVolume = e.GetDefaultAudioEndpoint(eRender, eMultimedia)?.Activate(CLSCTX_ALL, None)?;
        match id {
            4 => v.VolumeStepUp(std::ptr::null())?,
            5 => v.VolumeStepDown(std::ptr::null())?,
            _ => v.SetMute(!v.GetMute()?.as_bool(), std::ptr::null())?,
        }
        Ok((v.GetMasterVolumeLevelScalar()?, v.GetMute()?.as_bool()))
    })();
    match r {
        Ok((lvl, muted)) => {
            let n = (lvl * 100.0).round() as i32;
            // same icon steps Windows uses: 0 / 1-33 / 34-66 / 67-100
            let g = if muted { G_MUTE } else { G_VOL[match n { 0 => 0, 1..=33 => 1, 34..=66 => 2, _ => 3 }] };
            GLYPH.store(g, Relaxed);
            LEVEL.store(n, Relaxed);
            set_mode(Mode::Volume);
        }
        Err(_) => set_text(G_MUTE, "No audio device"),
    }
}

/// 1 = play/pause, 2 = next, 3 = previous, 0 = just refresh. Runs on a worker thread.
pub fn media(id: i32) {
    set_mode(Mode::Media); // show last-known track instantly, refresh when the worker is done
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED); // WIC needs COM on this thread
        let t = (|| -> windows::core::Result<Track> {
            let s = Gsmtc::RequestAsync()?.get()?.GetCurrentSession()?;
            match id {
                1 => { s.TryTogglePlayPauseAsync()?.get()?; }
                2 => { s.TrySkipNextAsync()?.get()?; }
                3 => { s.TrySkipPreviousAsync()?.get()?; }
                _ => {}
            }
            if id != 0 {
                std::thread::sleep(std::time::Duration::from_millis(350)); // let the player update
            }
            let p = s.TryGetMediaPropertiesAsync()?.get()?;
            let app = s.SourceAppUserModelId()?.to_string();
            let app = app.rsplit(['\\', '!']).next().unwrap_or(&app).trim_end_matches(".exe").to_string();
            Ok(Track {
                title: p.Title()?.to_string(),
                artist: p.Artist()?.to_string(),
                app,
                playing: s.GetPlaybackInfo()?.PlaybackStatus()? == Status::Playing,
                art: p.Thumbnail().ok().and_then(|t| decode_art(&t).ok()),
            })
        })();
        *TRACK.lock().unwrap() = t.ok();
        let h = HWND(FLYOUT.load(Relaxed) as *mut c_void);
        let _ = PostMessageW(Some(h), WM_TRACK, WPARAM(0), LPARAM(0));
    });
}

/// Album art -> 164x164 premultiplied BGRA, centre-cropped (UniformToFill).
unsafe fn decode_art(t: &windows::Storage::Streams::IRandomAccessStreamReference) -> windows::core::Result<Vec<u8>> {
    let stream = t.OpenReadAsync()?.get()?;
    let istream: IStream = CreateStreamOverRandomAccessStream(&stream.cast::<IUnknown>()?)?;
    let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
    let frame = wic.CreateDecoderFromStream(&istream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?.GetFrame(0)?;
    let (mut w, mut h) = (0, 0);
    frame.GetSize(&mut w, &mut h)?;
    let side = w.min(h);
    let clip = wic.CreateBitmapClipper()?;
    clip.Initialize(&frame, &WICRect { X: ((w - side) / 2) as i32, Y: ((h - side) / 2) as i32, Width: side as i32, Height: side as i32 })?;
    let scale = wic.CreateBitmapScaler()?;
    scale.Initialize(&clip, ART_PX, ART_PX, WICBitmapInterpolationModeHighQualityCubic)?;
    let conv = wic.CreateFormatConverter()?;
    conv.Initialize(&scale, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
    let mut px = vec![0u8; (ART_PX * ART_PX * 4) as usize];
    conv.CopyPixels(std::ptr::null(), ART_PX * 4, &mut px)?;
    Ok(px)
}

pub unsafe fn pin_foreground() {
    let h = GetForegroundWindow();
    if h.is_invalid() {
        return;
    }
    let on_top = GetWindowLongPtrW(h, GWL_EXSTYLE) & WS_EX_TOPMOST.0 as isize != 0;
    let after = if on_top { HWND_NOTOPMOST } else { HWND_TOPMOST };
    let _ = SetWindowPos(h, Some(after), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    // PowerToys-style accent frame on the pinned window (Windows 11 border colour)
    let border: u32 = if on_top {
        0xFFFF_FFFF // DWMWA_COLOR_DEFAULT
    } else {
        let c = windows::UI::ViewManagement::UISettings::new()
            .and_then(|u| u.GetColorValue(windows::UI::ViewManagement::UIColorType::Accent))
            .unwrap_or(windows::UI::Color { A: 255, R: 0x00, G: 0x99, B: 0xCC });
        c.R as u32 | (c.G as u32) << 8 | (c.B as u32) << 16 // COLORREF
    };
    let _ = windows::Win32::Graphics::Dwm::DwmSetWindowAttribute(
        h, windows::Win32::Graphics::Dwm::DWMWA_BORDER_COLOR, &border as *const _ as *const c_void, 4);
    let mut buf = [0u16; 256];
    let n = GetWindowTextW(h, &mut buf) as usize;
    set_text(if on_top { G_UNPIN } else { G_PIN }, &String::from_utf16_lossy(&buf[..n]));
}

pub fn lock_key(vk: u32) {
    LOCK_VK.store(vk, Relaxed);
    set_mode(Mode::Lock);
}

// ======================= low-level keyboard hook =======================
// Observes Caps/Num/Scroll Lock without swallowing them (like FluentFlyout),
// and lets the launcher take a lone Win-key tap.
pub unsafe extern "system" fn kb_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let k = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        let up = wp.0 as u32 == WM_KEYUP || wp.0 as u32 == WM_SYSKEYUP;
        let h = HWND(FLYOUT.load(Relaxed) as *mut c_void);
        if up && matches!(k.vkCode, 0x14 | 0x90 | 0x91) {
            let _ = PostMessageW(Some(h), WM_LOCK, WPARAM(k.vkCode as usize), LPARAM(0));
        }
        // Your PowerToys Keyboard Manager remaps, carried over:
        //   Numpad Enter -> Play/Pause, key 0xFC (keyboard knob) -> Volume Up.
        let numpad_enter = k.vkCode == 0x0D && k.flags.0 & LLKHF_EXTENDED.0 != 0;
        let remap = if numpad_enter { Some(11) } else if k.vkCode == 0xFC { Some(14) } else { None };
        if let Some(id) = remap {
            if !up && k.flags.0 & LLKHF_INJECTED.0 == 0 {
                let _ = PostMessageW(Some(h), WM_HOTKEY, WPARAM(id), LPARAM(0));
            }
            return LRESULT(1); // swallow down + up, like PowerToys
        }
        if crate::launcher::on_key(k, up) {
            return LRESULT(1);
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

// ======================= window placement + slide animation =======================
thread_local! {
    static ANIM: std::cell::Cell<Option<(Instant, i32)>> = const { std::cell::Cell::new(None) };
}

fn size_dips() -> (f32, f32, f32) {
    match mode() {
        Mode::Volume => (VOL_W, VOL_H, VOL_GAP),
        Mode::Text => (320.0, VOL_H, VOL_GAP),
        Mode::Media => (MED_W, MED_H, FLY_GAP),
        Mode::Lock => (LOCK_W, LOCK_H, FLY_GAP),
    }
}

pub unsafe fn show(hwnd: HWND) {
    let mut wa = RECT::default();
    let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut wa as *mut _ as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    let (wd, hd, gap) = size_dips();
    let (w, h) = ((wd * s).round() as i32, (hd * s).round() as i32);
    let x = (wa.left + wa.right - w) / 2;
    let rest = wa.bottom - h - (gap * s).round() as i32;
    let was_visible = IsWindowVisible(hwnd).as_bool();
    // slide up from 20px below on first show; already open -> snap to new size
    let y = if was_visible { rest } else { rest + (ANIM_DIST * s) as i32 };
    let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
    if !was_visible {
        ANIM.with(|a| a.set(Some((Instant::now(), rest))));
        SetTimer(Some(hwnd), T_ANIM, 10, None);
    }
    let _ = InvalidateRect(Some(hwnd), None, false);
    let ms = match mode() { Mode::Media => MED_MS, Mode::Lock => LOCK_MS, _ => VOL_MS };
    SetTimer(Some(hwnd), T_HIDE, ms, None);
}

/// T_ANIM tick: quadratic ease-out toward the resting position.
pub unsafe fn animate(hwnd: HWND) {
    let Some((t0, rest)) = ANIM.with(|a| a.get()) else {
        let _ = KillTimer(Some(hwnd), T_ANIM);
        return;
    };
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    let p = (t0.elapsed().as_secs_f32() * 1000.0 / ANIM_MS).min(1.0);
    let eased = 1.0 - (1.0 - p) * (1.0 - p);
    let y = rest + ((1.0 - eased) * ANIM_DIST * s).round() as i32;
    let mut r = RECT::default();
    let _ = GetWindowRect(hwnd, &mut r);
    let _ = SetWindowPos(hwnd, None, r.left, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    if p >= 1.0 {
        ANIM.with(|a| a.set(None));
        let _ = KillTimer(Some(hwnd), T_ANIM);
    }
}

/// T_HIDE: stay open while the mouse is over the flyout (FluentFlyout behaviour).
pub unsafe fn hide_tick(hwnd: HWND) {
    let mut pt = POINT::default();
    let mut r = RECT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = GetWindowRect(hwnd, &mut r);
    if PtInRect(&r, pt).as_bool() {
        return; // timer keeps ticking; check again next time
    }
    let _ = KillTimer(Some(hwnd), T_HIDE);
    let _ = ShowWindow(hwnd, SW_HIDE);
    // Idle most of the time: hand pages back to Windows (they fault back in on next show).
    let _ = windows::Win32::System::ProcessStatus::K32EmptyWorkingSet(windows::Win32::System::Threading::GetCurrentProcess());
}

/// Media card buttons: prev / play-pause / next.
pub unsafe fn click(hwnd: HWND, lp: LPARAM) {
    if mode() != Mode::Media {
        return;
    }
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    let (x, y) = ((lp.0 & 0xFFFF) as i16 as f32 / s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s);
    if !(ROW_Y..ROW_Y + ROW_H).contains(&y) {
        return;
    }
    let [prev, play, next] = button_xs();
    let id = if (prev..prev + BTN).contains(&x) { 3 } else if (play..play + PLAY_BTN).contains(&x) { 1 } else if (next..next + BTN).contains(&x) { 2 } else { return };
    media(id);
    show(hwnd);
}

fn button_xs() -> [f32; 3] {
    let prev = COL_X;
    let play = prev + BTN + BTN_GAP;
    [prev, play, play + PLAY_BTN + BTN_GAP]
}

// ======================= drawing =======================
struct Gfx {
    rt: ID2D1DCRenderTarget,
    icon16: IDWriteTextFormat,
    icon22: IDWriteTextFormat,
    icon40: IDWriteTextFormat,
    body: IDWriteTextFormat,
    medium: IDWriteTextFormat,
    small: IDWriteTextFormat,
    accent: D2D1_COLOR_F,
}

thread_local! {
    static GFX: std::cell::RefCell<Option<Gfx>> = const { std::cell::RefCell::new(None) };
}

pub fn reset_gfx() {
    GFX.with(|c| *c.borrow_mut() = None); // accent colour / DPI may have changed
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
    let fmt = |name, size, weight, center: bool| -> windows::core::Result<IDWriteTextFormat> {
        let t = dw.CreateTextFormat(name, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))?;
        t.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        t.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        if center {
            t.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
        } else {
            let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
            t.SetTrimming(&trim, &dw.CreateEllipsisTrimmingSign(&t)?)?;
        }
        Ok(t)
    };
    let text = w!("Segoe UI Variable Text");
    let icons = w!("Segoe Fluent Icons");
    let c = windows::UI::ViewManagement::UISettings::new()?
        .GetColorValue(windows::UI::ViewManagement::UIColorType::AccentLight2)
        .unwrap_or(windows::UI::Color { A: 255, R: 0x4C, G: 0xC2, B: 0xFF });
    Ok(Gfx {
        icon16: fmt(icons, 16.0, DWRITE_FONT_WEIGHT_NORMAL, true)?,
        icon22: fmt(icons, 22.0, DWRITE_FONT_WEIGHT_NORMAL, true)?,
        icon40: fmt(icons, 40.0, DWRITE_FONT_WEIGHT_NORMAL, true)?,
        body: fmt(text, 14.0, DWRITE_FONT_WEIGHT_NORMAL, false)?,
        medium: fmt(text, 14.0, DWRITE_FONT_WEIGHT_MEDIUM, false)?,
        small: fmt(text, 10.0, DWRITE_FONT_WEIGHT_NORMAL, false)?,
        accent: D2D1_COLOR_F { r: c.R as f32 / 255.0, g: c.G as f32 / 255.0, b: c.B as f32 / 255.0, a: 1.0 },
        rt,
    })
}

pub(crate) fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}
pub(crate) fn rrect(r: D2D_RECT_F, rad: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: r, radiusX: rad, radiusY: rad }
}
pub(crate) fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}
pub(crate) fn glyph(cp: u32) -> Vec<u16> {
    char::from_u32(cp).map(|c| utf16(&c.to_string())).unwrap_or_default()
}

unsafe fn draw(g: &Gfx, mem: HDC, rc: &RECT, dpi: f32) -> windows::core::Result<()> {
    let rt = &g.rt;
    rt.BindDC(mem, rc)?;
    rt.SetDpi(dpi, dpi);
    rt.BeginDraw();
    let (w, h) = ((rc.right - rc.left) as f32 * 96.0 / dpi, (rc.bottom - rc.top) as f32 * 96.0 / dpi);
    let white = |a: f32| rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a }, None);
    rt.Clear(Some(&base(D2D1_COLOR_F { r: BG.0, g: BG.1, b: BG.2, a: BG.3 })));
    let text = |s: &[u16], f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush| {
        rt.DrawText(s, f, &r, b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL)
    };
    let accent = rt.CreateSolidColorBrush(&g.accent, None)?;

    match mode() {
        Mode::Volume => {
            text(&glyph(GLYPH.load(Relaxed)), &g.icon16, rect(VOL_ICON_X, 0.0, VOL_ICON_X + 16.0, h), &white(SECONDARY)?);
            let level = LEVEL.load(Relaxed);
            let track = |x1| rrect(rect(TRACK_X, TRACK_Y, x1, TRACK_Y + TRACK_H), TRACK_H / 2.0);
            rt.FillRoundedRectangle(&track(TRACK_X + TRACK_W), &white(TRACK_A)?);
            if level > 0 {
                rt.FillRoundedRectangle(&track(TRACK_X + TRACK_W * level as f32 / 100.0), &accent);
            }
            text(&utf16(&level.to_string()), &g.body, rect(NUM_X, 0.0, w, h), &white(1.0)?);
        }
        Mode::Text => {
            text(&glyph(GLYPH.load(Relaxed)), &g.icon16, rect(VOL_ICON_X, 0.0, VOL_ICON_X + 16.0, h), &white(SECONDARY)?);
            text(&utf16(&TEXT.lock().unwrap()), &g.body, rect(TRACK_X, 0.0, w - VOL_ICON_X, h), &white(1.0)?);
        }
        Mode::Media => {
            let t = TRACK.lock().unwrap();
            let art = rect(MED_PAD, MED_PAD, MED_PAD + ART, MED_PAD + ART);
            match t.as_ref().and_then(|t| t.art.as_ref()) {
                Some(px) => {
                    let bp = D2D1_BITMAP_PROPERTIES {
                        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                        dpiX: 96.0,
                        dpiY: 96.0,
                    };
                    let bmp = rt.CreateBitmap(D2D_SIZE_U { width: ART_PX, height: ART_PX }, Some(px.as_ptr() as *const c_void), ART_PX * 4, &bp)?;
                    let brush = rt.CreateBitmapBrush(&bmp, None, None)?;
                    let k = ART / ART_PX as f32;
                    brush.SetTransform(&windows_numerics::Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: MED_PAD, M32: MED_PAD });
                    rt.FillRoundedRectangle(&rrect(art, ART_R), &brush);
                }
                None => {
                    rt.FillRoundedRectangle(&rrect(art, ART_R), &white(SUBTLE_A)?);
                    text(&glyph(G_NOTE), &g.icon40, art, &white(SECONDARY)?);
                }
            }
            let col_r = w - MED_PAD;
            let (title, artist, app, playing) = match t.as_ref() {
                Some(t) => (t.title.as_str(), t.artist.as_str(), t.app.as_str(), t.playing),
                None => ("Nothing playing", "", "", false),
            };
            text(&utf16(title), &g.medium, rect(COL_X, MED_PAD, col_r, MED_PAD + 20.0), &white(1.0)?);
            text(&utf16(artist), &g.medium, rect(COL_X, MED_PAD + 20.0, col_r, MED_PAD + 40.0), &white(TERTIARY)?);

            let [prev, play, next] = button_xs();
            let mid = ROW_Y + ROW_H / 2.0;
            text(&glyph(G_PREV), &g.icon16, rect(prev, mid - BTN / 2.0, prev + BTN, mid + BTN / 2.0), &white(1.0)?);
            let pr = rect(play, mid - PLAY_BTN / 2.0, play + PLAY_BTN, mid + PLAY_BTN / 2.0);
            rt.FillRoundedRectangle(&rrect(pr, 4.0), &accent);
            let on_accent = rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }, None)?; // TextOnAccentFillColorPrimary (dark)
            text(&glyph(if playing { G_PAUSE } else { G_PLAY }), &g.icon16, pr, &on_accent);
            text(&glyph(G_NEXT), &g.icon16, rect(next, mid - BTN / 2.0, next + BTN, mid + BTN / 2.0), &white(1.0)?);
            g.small.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
            text(&utf16(app), &g.small, rect(next + BTN + BTN_GAP, ROW_Y, col_r, ROW_Y + ROW_H), &white(TERTIARY)?);
        }
        Mode::Lock => {
            let vk = LOCK_VK.load(Relaxed);
            let on = GetKeyState(vk as i32) & 1 == 1;
            let name = match vk { 0x90 => "Num Lock", 0x91 => "Scroll Lock", _ => "Caps Lock" };
            text(&glyph(if on { G_LOCK } else { G_UNLOCK }), &g.icon22, rect(16.0, 6.0, 38.0, h - 10.0), &white(1.0)?);
            text(&utf16(&format!("{name} {}", if on { "on" } else { "off" })), &g.body, rect(56.0, 6.0, w - 12.0, 30.0), &white(1.0)?);
            let (bw, ba) = if on { (60.0, 1.0) } else { (36.0, 0.2) };
            let bar = rt.CreateSolidColorBrush(&D2D1_COLOR_F { a: ba, ..g.accent }, None)?;
            rt.FillRoundedRectangle(&rrect(rect(56.0, 33.0, 56.0 + bw, 36.0), 2.0), &bar);
        }
    }
    rt.EndDraw(None, None)
}

/// WM_PAINT: buffered 32-bit DIB with real alpha over the DWM acrylic backdrop.
pub unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let params = BP_PAINTPARAMS { cbSize: size_of::<BP_PAINTPARAMS>() as u32, dwFlags: BPPF_ERASE, ..Default::default() };
    let mut mem = HDC::default();
    let pb = BeginBufferedPaint(hdc, &rc, BPBF_TOPDOWNDIB, Some(&params), &mut mem);
    if !mem.is_invalid() {
        GFX.with(|cell| {
            let mut g = cell.borrow_mut();
            if g.is_none() {
                *g = make_gfx().ok();
            }
            if let Some(gfx) = g.as_ref() {
                if draw(gfx, mem, &rc, GetDpiForWindow(hwnd) as f32).is_err() {
                    *g = None; // rebuild on next paint
                }
            }
        });
        let _ = EndBufferedPaint(pb, true);
    }
    let _ = EndPaint(hwnd, &ps);
}

/// Display name for an app id ("Chrome" -> "Google Chrome"), the way Windows shows it.
pub fn app_display_name(aumid: &str) -> String {
    unsafe {
        use windows::Win32::UI::Shell::*;
        let p = windows::core::HSTRING::from(format!("shell:AppsFolder\\{aumid}"));
        let item: windows::core::Result<IShellItem> = SHCreateItemFromParsingName(&p, None);
        if let Ok(n) = item.and_then(|i| i.GetDisplayName(SIGDN_NORMALDISPLAY)) {
            let s = n.to_string().unwrap_or_default();
            windows::Win32::System::Com::CoTaskMemFree(Some(n.0 as *const c_void));
            if !s.is_empty() {
                return s;
            }
        }
    }
    aumid.rsplit(['\\', '!']).next().unwrap_or(aumid).trim_end_matches(".exe").to_string()
}

/// Media thumbnail kept at its own aspect ratio (video = 16:9), 116px wide (2x for sharpness).
pub unsafe fn decode_thumb(t: &windows::Storage::Streams::IRandomAccessStreamReference) -> windows::core::Result<(u32, u32, Vec<u8>)> {
    let stream = t.OpenReadAsync()?.get()?;
    let istream: IStream = CreateStreamOverRandomAccessStream(&stream.cast::<IUnknown>()?)?;
    let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
    let frame = wic.CreateDecoderFromStream(&istream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?.GetFrame(0)?;
    let (mut w, mut h) = (0, 0);
    frame.GetSize(&mut w, &mut h)?;
    let tw = 116u32;
    let th = ((h as f32 * tw as f32 / w.max(1) as f32).round() as u32).clamp(1, 116);
    let scale = wic.CreateBitmapScaler()?;
    scale.Initialize(&frame, tw, th, WICBitmapInterpolationModeHighQualityCubic)?;
    let conv = wic.CreateFormatConverter()?;
    conv.Initialize(&scale, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
    let mut px = vec![0u8; (tw * th * 4) as usize];
    conv.CopyPixels(std::ptr::null(), tw * 4, &mut px)?;
    Ok((tw, th, px))
}

static PREV_FG: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

fn is_ours(h: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    pid == std::process::id()
}

/// Take keyboard focus even when Windows' foreground lock says no (we're opened from a hook or
/// another window's click). Remembers who had it, so give_back() can return it.
pub(crate) unsafe fn force_foreground(h: windows::Win32::Foundation::HWND) {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows::Win32::UI::WindowsAndMessaging::*;
    let fg = GetForegroundWindow();
    if fg == h {
        return;
    }
    if !fg.is_invalid() && !is_ours(fg) {
        PREV_FG.store(fg.0 as isize, std::sync::atomic::Ordering::Relaxed);
    }
    let _ = SetForegroundWindow(h);
    if GetForegroundWindow() == h {
        return;
    }
    if !is_ours(h) {
        // the call Windows' own Alt+Tab uses: allowed past the foreground lock
        SwitchToThisWindow(h, true);
        for _ in 0..4 {
            if GetForegroundWindow() == h {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // switching to another app (Alt+Tab, giving focus back): Windows hands the foreground to
        // the process that sent the last input, so send a tagged Alt tap (with a dummy key in
        // between so the app doesn't open its menu bar) and give it a moment to land
        crate::launcher::inject(&[(0x12, false), (0xE8, false), (0xE8, true), (0x12, true)]);
        for _ in 0..6 {
            std::thread::sleep(std::time::Duration::from_millis(15));
            let _ = SetForegroundWindow(h);
            if GetForegroundWindow() == h {
                return;
            }
        }
    }
    let fg = GetForegroundWindow();
    let ft = GetWindowThreadProcessId(fg, None);
    let me = GetCurrentThreadId();
    let attached = ft != 0 && ft != me && AttachThreadInput(me, ft, true).as_bool();
    let _ = BringWindowToTop(h);
    let _ = SetForegroundWindow(h);
    let _ = SetFocus(Some(h));
    if attached {
        let _ = AttachThreadInput(me, ft, false);
    }
}

/// After hiding a panel: if keyboard focus is still on one of our (now hidden) windows, hand it
/// back to the app you were in, like Windows does when Start or Quick Settings closes.
pub(crate) unsafe fn give_back() {
    use windows::Win32::UI::WindowsAndMessaging::*;
    let fg = GetForegroundWindow();
    if !fg.is_invalid() && (!is_ours(fg) || IsWindowVisible(fg).as_bool()) {
        return;
    }
    let prev = windows::Win32::Foundation::HWND(PREV_FG.load(std::sync::atomic::Ordering::Relaxed) as *mut core::ffi::c_void);
    if IsWindow(Some(prev)).as_bool() && IsWindowVisible(prev).as_bool() {
        force_foreground(prev);
    }
}

/// Sent by the mouse hook when you click outside an open panel (Windows' light dismiss).
pub const WM_DISMISS: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 40;

/// Draw an app icon pixel-exact: snapped to whole pixels, and unsmoothed when the bitmap already has
/// the exact size (smoothing a same-size bitmap at a half-pixel offset is what makes icons look blurry).
pub(crate) unsafe fn draw_icon(rt: &windows::Win32::Graphics::Direct2D::ID2D1DCRenderTarget, b: &windows::Win32::Graphics::Direct2D::ID2D1Bitmap, x: f32, y: f32, size: f32) {
    use windows::Win32::Graphics::Direct2D::*;
    let (mut dx, mut dy) = (96.0, 96.0);
    rt.GetDpi(&mut dx, &mut dy);
    let s = dx / 96.0;
    let (x, y) = ((x * s).round() / s, (y * s).round() / s);
    let exact = (b.GetPixelSize().width as f32 - size * s).abs() < 0.5;
    let mode = if exact { D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR } else { D2D1_BITMAP_INTERPOLATION_MODE_LINEAR };
    rt.DrawBitmap(b, Some(&rect(x, y, x + size, y + size)), 1.0, mode, None);
}

/// Area-average resize of a square premultiplied BGRA image (e.g. 32px icon -> 24px), so small
/// icons are made from real pixels instead of being squeezed by the GPU at draw time.
pub(crate) fn shrink(px: &[u8], from: u32, to: u32) -> Vec<u8> {
    let (f, t) = (from as usize, to as usize);
    let mut out = vec![0u8; t * t * 4];
    let r = f as f32 / t as f32;
    for oy in 0..t {
        for ox in 0..t {
            let (x0, x1, y0, y1) = (ox as f32 * r, (ox + 1) as f32 * r, oy as f32 * r, (oy + 1) as f32 * r);
            let mut acc = [0f32; 4];
            let mut wsum = 0f32;
            for iy in y0.floor() as usize..(y1.ceil() as usize).min(f) {
                let wy = (y1.min(iy as f32 + 1.0) - y0.max(iy as f32)).max(0.0);
                for ix in x0.floor() as usize..(x1.ceil() as usize).min(f) {
                    let w = wy * (x1.min(ix as f32 + 1.0) - x0.max(ix as f32)).max(0.0);
                    let i = (iy * f + ix) * 4;
                    for c in 0..4 {
                        acc[c] += px[i + c] as f32 * w;
                    }
                    wsum += w;
                }
            }
            let o = (oy * t + ox) * 4;
            for c in 0..4 {
                out[o + c] = (acc[c] / wsum.max(1e-6)).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod icon_tests {
    #[test]
    fn shrink_keeps_flat_colour_and_size() {
        let px: Vec<u8> = std::iter::repeat([10u8, 20, 30, 255]).take(32 * 32).flatten().collect();
        let s = super::shrink(&px, 32, 24);
        assert_eq!(s.len(), 24 * 24 * 4);
        assert!(s.chunks(4).all(|p| p == [10, 20, 30, 255]));
    }
}

/// Ask Windows to draw this process's popup menus dark (right-click menus on the taskbar, desktop,
/// Calculator, Notepad...). Explorer and Notepad use the same two uxtheme entry points, which are
/// exported by number only: 135 SetPreferredAppMode(ForceDark = 2), 136 FlushMenuThemes.
// ponytail: always dark, as the whole UI is; follow the system light/dark setting when a light theme exists
pub unsafe fn dark_menus() {
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    let Ok(lib) = LoadLibraryW(windows::core::w!("uxtheme.dll")) else { return };
    let by_number = |n: usize| GetProcAddress(lib, windows::core::PCSTR(n as *const u8));
    if let (Some(set), Some(flush)) = (by_number(135), by_number(136)) {
        let set: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(set);
        let flush: unsafe extern "system" fn() = std::mem::transmute(flush);
        set(2);
        flush();
    }
}

// ======================= Windows' panel material and motion =======================

/// True in the running shell; never set in the off-screen snapshot modes.
pub static LIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What a panel is cleared to. Live: a see-through tint, so the blurred backdrop Windows gives a
/// `fluent_window` shows through (a flat opaque fill hid it; no tint at all came out light grey).
/// Off screen (snapshots) there is no backdrop, so the flat colour stands in.
pub fn base(flat: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F) -> windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
    // 86% of the panel colour over the blur: the film shows Windows' panels within about 6 levels of
    // #2C2C2C whatever is behind them, i.e. a strong tint with a little of the background showing
    if LIVE.load(std::sync::atomic::Ordering::Relaxed) { windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F { a: 0.86, ..flat } } else { flat }
}

// Measured from 30 fps film of Windows' Quick Settings (snap/study/quick-settings):
// opening it is 200 px low and arrives in 280 ms, fast first (quartic ease-out);
// closing it drops 200 px in 126 ms, slow first (cubic ease-in), then is gone.
const SLIDE: f32 = 200.0;
const OPEN_MS: f32 = 280.0;
const CLOSE_MS: f32 = 126.0;

/// How far below its resting place (px at 100%) a panel is `ms` after it starts opening.
pub fn open_offset(ms: f32) -> f32 {
    SLIDE * (1.0 - (ms / OPEN_MS).clamp(0.0, 1.0)).powi(4)
}

/// ...and `ms` after it starts closing.
pub fn close_offset(ms: f32) -> f32 {
    SLIDE * (ms / CLOSE_MS).clamp(0.0, 1.0).powi(3)
}

/// Slide panels that are already shown at their resting place: up into it (`open`) or down out of it.
/// The part that would cover the taskbar is cut off, as in Windows (panels rise from behind it).
// ponytail: runs on the caller's thread for 126-280 ms; move to a timer if input ever feels held up
pub unsafe fn slide(wins: &[windows::Win32::Foundation::HWND], open: bool) {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{CreateRectRgn, SetWindowRgn};
    use windows::Win32::UI::WindowsAndMessaging::*;
    let mut wa = RECT::default();
    let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut wa as *mut _ as *mut std::ffi::c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    let rest: Vec<(windows::Win32::Foundation::HWND, RECT)> = wins.iter().filter(|h| IsWindowVisible(**h).as_bool()).map(|h| { let mut r = RECT::default(); let _ = GetWindowRect(*h, &mut r); (*h, r) }).collect();
    let Some(first) = rest.first() else { return };
    let scale = windows::Win32::UI::HiDpi::GetDpiForWindow(first.0) as f32 / 96.0;
    let total = if open { OPEN_MS } else { CLOSE_MS };
    let t0 = std::time::Instant::now();
    loop {
        let ms = t0.elapsed().as_secs_f32() * 1000.0;
        let off = ((if open { open_offset(ms) } else { close_offset(ms) }) * scale).round() as i32;
        for (h, r) in &rest {
            let (w, hgt) = (r.right - r.left, r.bottom - r.top);
            let _ = SetWindowRgn(*h, Some(CreateRectRgn(0, 0, w, (wa.bottom - (r.top + off)).clamp(0, hgt))), false);
            let _ = SetWindowPos(*h, None, r.left, r.top + off, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
        let _ = windows::Win32::Graphics::Dwm::DwmFlush();
        if ms >= total {
            break;
        }
    }
    for (h, r) in &rest {
        if open {
            let _ = SetWindowPos(*h, None, r.left, r.top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            let _ = SetWindowRgn(*h, None, true);
        } else {
            // leave it hidden at its resting place, whole again, for the next time it opens
            let _ = ShowWindow(*h, SW_HIDE);
            let _ = SetWindowRgn(*h, None, false);
            let _ = SetWindowPos(*h, None, r.left, r.top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
}

#[cfg(test)]
mod motion_tests {
    use super::*;

    #[test]
    fn slide_matches_the_film() {
        // opening: film showed 164, 104, 64, 37, 19, 3 px left at 15, 41, 73, 101, 131, 181 ms
        for (ms, px) in [(15.0, 164.0), (41.0, 104.0), (73.0, 64.0), (101.0, 37.0), (131.0, 19.0), (181.0, 3.0)] {
            assert!((open_offset(ms) - px).abs() < 8.0, "open {ms} ms: {} vs {px}", open_offset(ms));
        }
        assert_eq!(open_offset(280.0), 0.0);
        // closing: 17, 60, 130 px down at 43, 78, 107 ms
        for (ms, px) in [(43.0, 17.0), (78.0, 60.0), (107.0, 130.0)] {
            assert!((close_offset(ms) - px).abs() < 14.0, "close {ms} ms: {} vs {px}", close_offset(ms));
        }
        assert_eq!(close_offset(500.0), 200.0);
    }
}

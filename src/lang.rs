//! Input language switcher: hold Win and tap Space to step through your keyboard layouts in a small
//! list by the taskbar corner; let go of Win and the app you are in switches to the chosen one.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering::Relaxed};

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Globalization::{GetLocaleInfoEx, LCIDToLocaleName, LOCALE_SLOCALIZEDDISPLAYNAME};
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub static LANG: AtomicIsize = AtomicIsize::new(0);
static OPEN: AtomicBool = AtomicBool::new(false);
static SEL: AtomicUsize = AtomicUsize::new(0);
static TARGET: AtomicIsize = AtomicIsize::new(0);
const WM_LANG_STEP: u32 = WM_APP + 67;
const WM_LANG_COMMIT: u32 = WM_APP + 68;

const W: f32 = 320.0;
const ROW: f32 = 56.0;
const PAD: f32 = 8.0;

/// The keyboard layouts installed for this user, as Windows hands them out.
unsafe fn layouts() -> Vec<isize> {
    let n = GetKeyboardLayoutList(None);
    let mut v = vec![HKL::default(); n.max(0) as usize];
    let got = GetKeyboardLayoutList(Some(&mut v));
    v.truncate(got.max(0) as usize);
    v.iter().map(|h| h.0 as isize).collect()
}

/// The language id (low word) and layout id (high word) packed in a layout handle.
pub fn parts(hkl: isize) -> (u16, u16) {
    ((hkl as usize & 0xFFFF) as u16, ((hkl as usize >> 16) & 0xFFFF) as u16)
}

/// The registry name of a layout, when its handle carries the layout id directly ("00000409").
/// Layouts with a device-specific handle (high word 0xFxxx) have no simple name here.
pub fn klid(hkl: isize) -> Option<String> {
    let (_, hi) = parts(hkl);
    (hi & 0xF000 != 0xF000).then(|| format!("{hi:08X}"))
}

/// (language name, layout name) to show for a layout, both from Windows.
unsafe fn names(hkl: isize) -> (String, String) {
    let (lang, _) = parts(hkl);
    let mut loc = [0u16; 85];
    let mut buf = [0u16; 128];
    let mut language = String::new();
    if LCIDToLocaleName(lang as u32, Some(&mut loc), 0) > 0 {
        let n = GetLocaleInfoEx(PCWSTR(loc.as_ptr()), LOCALE_SLOCALIZEDDISPLAYNAME, Some(&mut buf));
        language = String::from_utf16_lossy(&buf[..(n.max(1) - 1) as usize]);
    }
    let mut layout = String::new();
    if let Some(id) = klid(hkl) {
        let mut text = [0u16; 128];
        let mut size = 256u32;
        let key = HSTRING::from(format!("SYSTEM\\CurrentControlSet\\Control\\Keyboard Layouts\\{id}"));
        if RegGetValueW(HKEY_LOCAL_MACHINE, &key, w!("Layout Text"), RRF_RT_REG_SZ, None, Some(text.as_mut_ptr() as *mut c_void), Some(&mut size)).is_ok() {
            layout = String::from_utf16_lossy(&text[..text.iter().position(|c| *c == 0).unwrap_or(0)]);
        }
    }
    (language, layout)
}

/// Next layout after `current` (wrapping), or the first when `current` isn't in the list.
pub fn next(list: &[isize], current: isize) -> usize {
    if list.is_empty() {
        return 0;
    }
    list.iter().position(|h| *h == current).map_or(0, |i| (i + 1) % list.len())
}

/// Which row of the list a point (panel units, from its top) is on, for a list of `n` layouts.
pub fn row_at(y: f32, n: usize) -> Option<usize> {
    let i = ((y - PAD) / ROW).floor();
    (y >= PAD && (i as usize) < n).then_some(i as usize)
}

pub unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    match msg {
        WM_LANG_STEP => {
            let list = layouts();
            if list.len() < 2 {
                return LRESULT(0); // one layout: nothing to switch between
            }
            if OPEN.load(Relaxed) {
                SEL.store((SEL.load(Relaxed) + 1) % list.len(), Relaxed);
            } else {
                // first tap: start from the layout after the one the app is using now
                let fg = GetForegroundWindow();
                TARGET.store(fg.0 as isize, Relaxed);
                let current = GetKeyboardLayout(GetWindowThreadProcessId(fg, None)).0 as isize;
                SEL.store(next(&list, current), Relaxed);
                let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
                let _ = GetMonitorInfoW(MonitorFromWindow(fg, MONITOR_DEFAULTTOPRIMARY), &mut mi);
                let (w_, h) = ((W * s) as i32, ((PAD * 2.0 + ROW * list.len() as f32) * s) as i32);
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), mi.rcWork.right - w_ - (12.0 * s) as i32, mi.rcWork.bottom - h - (12.0 * s) as i32, w_, h, SWP_SHOWWINDOW | SWP_NOACTIVATE);
                OPEN.store(true, Relaxed);
            }
            let _ = InvalidateRect(Some(hwnd), None, true);
        }
        WM_LANG_COMMIT => {
            if OPEN.swap(false, Relaxed) {
                let _ = ShowWindow(hwnd, SW_HIDE);
                let list = layouts();
                let target = HWND(TARGET.load(Relaxed) as *mut c_void);
                if let Some(hkl) = list.get(SEL.load(Relaxed)) {
                    if IsWindow(Some(target)).as_bool() {
                        // the documented way to ask an app to switch its input language
                        let _ = PostMessageW(Some(target), WM_INPUTLANGCHANGEREQUEST, WPARAM(0), LPARAM(*hkl));
                    }
                }
            }
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            // Direct2D into a buffer that keeps alpha: Windows' blurred panel material shows through
            use windows::Win32::UI::Controls::{BeginBufferedPaint, EndBufferedPaint, BPBF_TOPDOWNDIB, BPPF_ERASE, BP_PAINTPARAMS};
            let params = BP_PAINTPARAMS { cbSize: size_of::<BP_PAINTPARAMS>() as u32, dwFlags: BPPF_ERASE, ..Default::default() };
            let mut mem = HDC::default();
            let pb = BeginBufferedPaint(dc, &rc, BPBF_TOPDOWNDIB, Some(&params), &mut mem);
            if !mem.is_invalid() {
                let _ = draw(mem, &rc, s);
                let _ = EndBufferedPaint(pb, true);
            }
            let _ = EndPaint(hwnd, &ps);
        }
        WM_LBUTTONUP => {
            // clicking a layout picks it at once (no need to let go of Win)
            if let Some(i) = row_at(((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s, layouts().len()) {
                SEL.store(i, Relaxed);
                let _ = PostMessageW(Some(hwnd), WM_LANG_COMMIT, WPARAM(0), LPARAM(0));
            }
        }
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

/// Win+Space: show the list / step to the next layout. Called from the keyboard hook.
pub unsafe fn step() {
    let _ = PostMessageW(Some(HWND(LANG.load(Relaxed) as *mut c_void)), WM_LANG_STEP, WPARAM(0), LPARAM(0));
}

/// The Win key was released: switch to the chosen layout. Returns true if the list was open.
pub unsafe fn commit() -> bool {
    if !OPEN.load(Relaxed) {
        return false;
    }
    let _ = PostMessageW(Some(HWND(LANG.load(Relaxed) as *mut c_void)), WM_LANG_COMMIT, WPARAM(0), LPARAM(0));
    true
}

/// `--lang-selftest`: the layouts and their names, and which one this process uses.
pub unsafe fn report() -> String {
    let list = layouts();
    let current = GetKeyboardLayout(0).0 as isize;
    list.iter().map(|h| { let (a, b) = names(*h); format!("{:08X} {a} / {b}{}", *h as usize & 0xFFFF_FFFF, if *h == current { " (current)" } else { "" }) }).collect::<Vec<_>>().join("\n")
}

/// The list: one row per keyboard layout, the chosen one highlighted with an accent bar.
unsafe fn draw(dc: HDC, rc: &RECT, s: f32) -> windows::core::Result<()> {
    use windows::Win32::Graphics::Direct2D::Common::*;
    use windows::Win32::Graphics::Direct2D::*;
    use windows::Win32::Graphics::DirectWrite::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use crate::flyout::{rect, rrect, utf16};
    use crate::quick::rgb;
    let f: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
    let props = D2D1_RENDER_TARGET_PROPERTIES { pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }, ..Default::default() };
    let rt = f.CreateDCRenderTarget(&props)?;
    rt.BindDC(dc, rc)?;
    rt.SetDpi(96.0 * s, 96.0 * s);
    let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
    let font = |px: f32, weight: DWRITE_FONT_WEIGHT| dw.CreateTextFormat(w!("Segoe UI Variable Text"), None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, px, w!(""));
    let (big, small) = (font(14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?, font(12.0, DWRITE_FONT_WEIGHT_NORMAL)?);
    let w = rc.right as f32 / s;
    let b = |c: u32| rt.CreateSolidColorBrush(&rgb(c), None);
    rt.BeginDraw();
    rt.Clear(Some(&crate::flyout::base(rgb(0x2C2C2C))));
    let sel = SEL.load(Relaxed);
    for (i, hkl) in layouts().iter().enumerate() {
        let y = PAD + i as f32 * ROW;
        if i == sel {
            rt.FillRoundedRectangle(&rrect(rect(PAD, y + 2.0, w - PAD, y + ROW - 2.0), 4.0), &b(0x3D3D3D)?);
            rt.FillRoundedRectangle(&rrect(rect(PAD, y + 18.0, PAD + 3.0, y + ROW - 18.0), 1.5), &b(0x4CC2FF)?);
        }
        let (language, layout) = names(*hkl);
        let layout = if layout.is_empty() { "Keyboard".to_string() } else { format!("{layout} keyboard") };
        rt.DrawText(&utf16(&language), &big, &rect(PAD + 16.0, y + 9.0, w - PAD, y + 29.0), &b(0xFFFFFF)?, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        rt.DrawText(&utf16(&layout), &small, &rect(PAD + 16.0, y + 29.0, w - PAD, y + 48.0), &b(0xC5C5C5)?, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
    }
    rt.EndDraw(None, None)
}

pub unsafe fn create(inst: HINSTANCE) -> windows::core::Result<()> {
    let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: inst, lpszClassName: w!("shellbits_lang"), ..Default::default() };
    RegisterClassW(&wc);
    let h = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE, w!("shellbits_lang"), w!("Input language"), WS_POPUP, 0, 0, 10, 10, None, None, Some(inst), None)?;
    let (dark, round) = (1u32, 2u32);
    let _ = DwmSetWindowAttribute(h, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const c_void, 4);
    let _ = DwmSetWindowAttribute(h, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as *const c_void, 4);
    let _ = crate::fluent_window(h); // Windows' blurred panel material
    LANG.store(h.0 as isize, Relaxed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rows_under_the_mouse() {
        assert_eq!(row_at(PAD + 1.0, 2), Some(0));
        assert_eq!(row_at(PAD + ROW + 1.0, 2), Some(1));
        assert_eq!(row_at(PAD + 2.0 * ROW + 1.0, 2), None); // below the last row
        assert_eq!(row_at(2.0, 2), None); // the padding above the first
    }
    #[test]
    fn layout_handles() {
        assert_eq!(parts(0x0409_0409), (0x0409, 0x0409));
        assert_eq!(klid(0x0409_0409).as_deref(), Some("00000409"));
        assert_eq!(klid(0x4009_0409).as_deref(), Some("00004009"));
        assert_eq!(klid(0xF002_0409u32 as isize), None); // device-specific handle
        let list = [10, 20, 30];
        assert_eq!(next(&list, 10), 1);
        assert_eq!(next(&list, 30), 0); // wraps
        assert_eq!(next(&list, 99), 0); // unknown current
        assert_eq!(next(&[], 1), 0);
    }
}

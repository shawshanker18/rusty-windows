//! Win+Shift+S while we are the shell: Windows' own screen-clip overlay does not come up without
//! Explorer, so this is ours. Drag a rectangle on a frozen, dimmed picture of the screen; the piece
//! goes to the clipboard and to Pictures\Screenshots as a PNG. Esc, right-click or clicking away cancels.
// ponytail: primary monitor, rectangle only (no window / freeform modes, no editor, no delay);
// add modes when someone asks for them.
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;

static BUSY: AtomicBool = AtomicBool::new(false);

struct St {
    shot: HBITMAP, // the screen as it was
    dim: HBITMAP,  // the same, darkened
    w: i32,
    h: i32,
    from: Option<POINT>,
    to: POINT,
}

thread_local! {
    static ST: RefCell<Option<St>> = const { RefCell::new(None) };
}

/// The rectangle between two corners, whichever way it was dragged.
pub fn norm(a: (i32, i32), b: (i32, i32)) -> (i32, i32, i32, i32) {
    (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
}

/// "Screenshot 2026-11-11 163455.png", the way Windows names them.
pub fn file_name(y: u16, mo: u16, d: u16, h: u16, mi: u16, s: u16) -> String {
    format!("Screenshot {y:04}-{mo:02}-{d:02} {h:02}{mi:02}{s:02}.png")
}

/// Open the overlay on its own thread (the caller is the keyboard hook: it must not wait).
pub fn start() {
    if BUSY.swap(true, Relaxed) {
        return;
    }
    std::thread::spawn(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let _ = run();
        BUSY.store(false, Relaxed);
    });
}

/// `shellbits --snip`: the same overlay in a process of its own.
pub unsafe fn run_now() -> Result<()> {
    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    run()
}

unsafe fn run() -> Result<()> {
    let (w, h) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
    let screen = GetDC(None);
    let mem = CreateCompatibleDC(Some(screen));
    let shot = CreateCompatibleBitmap(screen, w, h);
    let old = SelectObject(mem, shot.into());
    let _ = BitBlt(mem, 0, 0, w, h, Some(screen), 0, 0, SRCCOPY | CAPTUREBLT);
    // darkened copy: the picture with half-transparent black over it
    let dim = CreateCompatibleBitmap(screen, w, h);
    let mem2 = CreateCompatibleDC(Some(screen));
    let old2 = SelectObject(mem2, dim.into());
    let _ = BitBlt(mem2, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
    let black = CreateCompatibleDC(Some(screen));
    let dot = CreateCompatibleBitmap(screen, 1, 1);
    let old3 = SelectObject(black, dot.into());
    let _ = PatBlt(black, 0, 0, 1, 1, BLACKNESS);
    let _ = AlphaBlend(mem2, 0, 0, w, h, black, 0, 0, 1, 1, BLENDFUNCTION { BlendOp: 0, BlendFlags: 0, SourceConstantAlpha: 120, AlphaFormat: 0 });
    SelectObject(black, old3);
    let _ = DeleteObject(dot.into());
    let _ = DeleteDC(black);
    SelectObject(mem2, old2);
    let _ = DeleteDC(mem2);
    SelectObject(mem, old);
    let _ = DeleteDC(mem);
    ReleaseDC(None, screen);
    ST.with(|s| *s.borrow_mut() = Some(St { shot, dim, w, h, from: None, to: POINT::default() }));

    let inst: HINSTANCE = GetModuleHandleW(None)?.into();
    let class = w!("shellbits_snip");
    RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(proc), hInstance: inst, lpszClassName: class, hCursor: LoadCursorW(None, IDC_CROSS)?, ..Default::default() });
    let hwnd = CreateWindowExW(WS_EX_TOPMOST | WS_EX_TOOLWINDOW, class, w!(""), WS_POPUP | WS_VISIBLE, 0, 0, w, h, None, None, Some(inst), None)?;
    crate::flyout::force_foreground(hwnd);
    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    Ok(())
}

/// Copy the chosen piece to the clipboard and save it as a PNG.
unsafe fn finish(hwnd: HWND, shot: HBITMAP, r: (i32, i32, i32, i32)) {
    let (w, h) = (r.2 - r.0, r.3 - r.1);
    let screen = GetDC(None);
    let (src, dst) = (CreateCompatibleDC(Some(screen)), CreateCompatibleDC(Some(screen)));
    let piece = CreateCompatibleBitmap(screen, w, h);
    let (o1, o2) = (SelectObject(src, shot.into()), SelectObject(dst, piece.into()));
    let _ = BitBlt(dst, 0, 0, w, h, Some(src), r.0, r.1, SRCCOPY);
    SelectObject(src, o1);
    SelectObject(dst, o2);
    let _ = DeleteDC(src);
    let _ = DeleteDC(dst);
    ReleaseDC(None, screen);
    let _ = save_png(piece);
    // the clipboard takes the bitmap over: it is not deleted here
    // (tests set SHELLBITS_SNIP_NO_CLIP so they never overwrite the user's clipboard)
    if std::env::var_os("SHELLBITS_SNIP_NO_CLIP").is_none() && OpenClipboard(Some(hwnd)).is_ok() {
        let _ = EmptyClipboard();
        if SetClipboardData(2 /* CF_BITMAP */, Some(HANDLE(piece.0))).is_err() {
            let _ = DeleteObject(piece.into());
        }
        let _ = CloseClipboard();
    } else {
        let _ = DeleteObject(piece.into());
    }
}

unsafe fn save_png(piece: HBITMAP) -> Result<()> {
    let dir = std::path::PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join("Pictures").join("Screenshots");
    let _ = std::fs::create_dir_all(&dir);
    let t = windows::Win32::System::SystemInformation::GetLocalTime();
    let path = dir.join(file_name(t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond));
    let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
    let bmp = wic.CreateBitmapFromHBITMAP(piece, HPALETTE::default(), WICBitmapIgnoreAlpha)?;
    let stream = wic.CreateStream()?;
    stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), GENERIC_WRITE.0)?;
    let enc = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
    enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
    let mut frame = None;
    enc.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
    let frame = frame.ok_or_else(Error::empty)?;
    frame.Initialize(None)?;
    frame.WriteSource(&bmp, std::ptr::null())?;
    frame.Commit()?;
    enc.Commit()
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let pt = || POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            ST.with(|s| {
                if let Some(st) = s.borrow().as_ref() {
                    // compose off screen, then show in one go (no flicker while dragging)
                    let (back, src) = (CreateCompatibleDC(Some(dc)), CreateCompatibleDC(Some(dc)));
                    let buf = CreateCompatibleBitmap(dc, st.w, st.h);
                    let ob = SelectObject(back, buf.into());
                    let os = SelectObject(src, st.dim.into());
                    let _ = BitBlt(back, 0, 0, st.w, st.h, Some(src), 0, 0, SRCCOPY);
                    if let Some(a) = st.from {
                        let r = norm((a.x, a.y), (st.to.x, st.to.y));
                        SelectObject(src, st.shot.into());
                        let _ = BitBlt(back, r.0, r.1, r.2 - r.0, r.3 - r.1, Some(src), r.0, r.1, SRCCOPY);
                        FrameRect(back, &RECT { left: r.0 - 1, top: r.1 - 1, right: r.2 + 1, bottom: r.3 + 1 }, HBRUSH(GetStockObject(WHITE_BRUSH).0));
                    }
                    let _ = BitBlt(dc, 0, 0, st.w, st.h, Some(back), 0, 0, SRCCOPY);
                    SelectObject(src, os);
                    SelectObject(back, ob);
                    let _ = DeleteObject(buf.into());
                    let _ = DeleteDC(src);
                    let _ = DeleteDC(back);
                }
            });
            let _ = EndPaint(hwnd, &ps);
        }
        WM_LBUTTONDOWN => {
            let p = pt();
            ST.with(|s| if let Some(st) = s.borrow_mut().as_mut() { st.from = Some(p); st.to = p; });
            SetCapture(hwnd);
        }
        WM_MOUSEMOVE => {
            let p = pt();
            if ST.with(|s| s.borrow_mut().as_mut().is_some_and(|st| { st.to = p; st.from.is_some() })) {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            // copy what is needed out before anything that sends messages
            let p = pt(); // where the button came up, not the last move seen
            let pick = ST.with(|s| s.borrow().as_ref().and_then(|st| st.from.map(|a| (st.shot, norm((a.x, a.y), (p.x, p.y))))));
            let _ = ShowWindow(hwnd, SW_HIDE);
            if let Some((shot, r)) = pick {
                if r.2 - r.0 > 2 && r.3 - r.1 > 2 {
                    finish(hwnd, shot, r);
                }
            }
            let _ = DestroyWindow(hwnd);
        }
        WM_RBUTTONUP => { let _ = DestroyWindow(hwnd); }
        WM_KEYDOWN if wp.0 as u16 == VK_ESCAPE.0 => { let _ = DestroyWindow(hwnd); }
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => { let _ = DestroyWindow(hwnd); }
        WM_DESTROY => {
            if let Some(st) = ST.with(|s| s.borrow_mut().take()) {
                let _ = DeleteObject(st.shot.into());
                let _ = DeleteObject(st.dim.into());
            }
            PostQuitMessage(0);
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangle_and_name() {
        assert_eq!(norm((50, 80), (10, 20)), (10, 20, 50, 80)); // dragged up and left
        assert_eq!(norm((10, 80), (50, 20)), (10, 20, 50, 80));
        assert_eq!(file_name(2026, 11, 1, 9, 5, 7), "Screenshot 2026-11-01 090507.png");
    }
}

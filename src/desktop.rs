//! Desktop icons for when shellbits is the shell (Explorer draws them today). Same items Explorer
//! shows (the desktop namespace: your files + public desktop + Recycle Bin and any icons enabled in
//! "Desktop icon settings"), medium 48px icons on Explorer's 100% grid (76 x 86 cells, column-major),
//! white 12px labels with a soft shadow, Windows' own context menus, Delete to the Recycle Bin.
//! Drawn into a per-pixel-alpha layered window that sits above the wallpaper.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};

use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Shell::Common::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::flyout::{rect, utf16};

pub static DESK: AtomicIsize = AtomicIsize::new(0);
const WM_DESK_REFRESH: u32 = WM_APP + 90;

pub const CELL_W: f32 = 76.0;
pub const CELL_H: f32 = 86.0;
const ICON: f32 = 48.0;
const MARGIN: f32 = 2.0;
const RECYCLE: &str = "::{645FF040-5081-101B-9F08-08002B27D9AA}";

#[derive(Clone)]
struct Item {
    name: String,
    parse: String, // full path, or ::{GUID} for shell items like Recycle Bin
}

#[derive(Default)]
struct State {
    items: Vec<Item>,
    icons: Vec<Option<Vec<u8>>>,
    sel: Vec<usize>,
    hover: Option<usize>,
    size: (i32, i32),
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
}

/// Grid cell of the i-th item: column-major, top to bottom then left to right (Explorer's auto-arrange).
pub fn cell(i: usize, screen_h: f32) -> (f32, f32) {
    let rows = ((screen_h - MARGIN) / CELL_H).floor().max(1.0) as usize;
    (MARGIN + (i / rows) as f32 * CELL_W, MARGIN + (i % rows) as f32 * CELL_H)
}

pub fn hit(n: usize, screen_h: f32, x: f32, y: f32) -> Option<usize> {
    (0..n).find(|&i| {
        let (cx, cy) = cell(i, screen_h);
        x >= cx && x < cx + CELL_W && y >= cy && y < cy + CELL_H
    })
}

/// Explorer's "Desktop icon settings": a ::{GUID} item shows when its HideDesktopIcons value is 0.
/// Recycle Bin is on unless hidden; everything else (This PC, Network...) is off unless enabled.
fn shown(parse: &str) -> bool {
    let Some(guid) = parse.strip_prefix("::") else { return true };
    let mut v = 0u32;
    let mut size = 4u32;
    let set = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\HideDesktopIcons\\NewStartPanel"),
            &HSTRING::from(guid),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut _ as *mut c_void),
            Some(&mut size),
        )
        .is_ok()
    };
    if set { v == 0 } else { parse.eq_ignore_ascii_case(RECYCLE) }
}

fn hide_all() -> bool {
    let mut v = 0u32;
    let mut size = 4u32;
    unsafe {
        RegGetValueW(HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced"), w!("HideIcons"), RRF_RT_REG_DWORD, None, Some(&mut v as *mut _ as *mut c_void), Some(&mut size)).is_ok() && v == 1
    }
}

/// The desktop namespace, the way Explorer lists it (shell items first, then by name).
unsafe fn enumerate() -> Vec<Item> {
    let mut out = Vec::new();
    if hide_all() && std::env::var_os("SHELLBITS_SHOW_ICONS").is_none() {
        return out;
    }
    let Ok(desk) = SHGetDesktopFolder() else { return out };
    let mut en = None;
    if desk.EnumObjects(HWND::default(), (SHCONTF_FOLDERS.0 | SHCONTF_NONFOLDERS.0) as u32, &mut en).is_err() {
        return out;
    }
    let Some(en) = en else { return out };
    loop {
        let mut pidl = [std::ptr::null_mut(); 1];
        let mut got = 0;
        if en.Next(&mut pidl, Some(&mut got)).is_err() || got == 0 {
            break;
        }
        let name = |flags: SHGDNF| -> String {
            let mut sr = STRRET::default();
            if desk.GetDisplayNameOf(pidl[0], flags, &mut sr).is_ok() {
                let mut p = PWSTR::null();
                if StrRetToStrW(&mut sr, Some(pidl[0]), &mut p).is_ok() {
                    let s = p.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(p.0 as *const c_void));
                    return s;
                }
            }
            String::new()
        };
        let it = Item { name: name(SHGDN_NORMAL), parse: name(SHGDN_FORPARSING) };
        CoTaskMemFree(Some(pidl[0] as *const c_void));
        if !it.name.is_empty() && shown(&it.parse) {
            out.push(it);
        }
    }
    sort(&mut out);
    out
}

fn sort(v: &mut [Item]) {
    v.sort_by(|a, b| b.parse.starts_with("::").cmp(&a.parse.starts_with("::")).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
}

unsafe fn load_icons(items: &[Item]) -> Vec<Option<Vec<u8>>> {
    let wic: Option<IWICImagingFactory> = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok();
    items.iter().map(|it| wic.as_ref().and_then(|w| crate::launcher::icon_pixels(w, &it.parse, ICON as u32).ok())).collect()
}

// ======================= drawing =======================

unsafe fn render(st: &State, w: i32, h: i32, opaque_bg: bool) -> windows::core::Result<(HBITMAP, *mut c_void)> {
    let bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, ..Default::default() },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    let dib = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0)?;
    let dc = CreateCompatibleDC(None);
    let old = SelectObject(dc, dib.into());
    let f: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
    let props = D2D1_RENDER_TARGET_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        ..Default::default()
    };
    let rt = f.CreateDCRenderTarget(&props)?;
    rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
    let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
    let fmt = dw.CreateTextFormat(w!("Segoe UI"), None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 12.0, w!("en-us"))?;
    fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
    fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_EMERGENCY_BREAK)?;
    let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
    fmt.SetTrimming(&trim, &dw.CreateEllipsisTrimmingSign(&fmt)?)?;
    rt.BindDC(dc, &RECT { left: 0, top: 0, right: w, bottom: h })?;
    rt.BeginDraw();
    rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: if opaque_bg { 1.0 } else { 0.0 } }));
    if opaque_bg {
        // stand-in for the wallpaper in snapshots
        let stops = [D2D1_GRADIENT_STOP { position: 0.0, color: crate::quick::rgb(0x1E3A5F) }, D2D1_GRADIENT_STOP { position: 1.0, color: crate::quick::rgb(0x5B2A4E) }];
        let coll = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
        let g = rt.CreateLinearGradientBrush(&D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: windows_numerics::Vector2 { X: 0.0, Y: 0.0 }, endPoint: windows_numerics::Vector2 { X: w as f32, Y: h as f32 } }, None, &coll)?;
        rt.FillRectangle(&rect(0.0, 0.0, w as f32, h as f32), &g);
    }
    let white = rt.CreateSolidColorBrush(&crate::quick::rgb(0xFFFFFF), None)?;
    let shadow = rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.75 }, None)?;
    let selb = rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 0.22 }, None)?;
    let hovb = rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 0.12 }, None)?;
    let edge = rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 0.35 }, None)?;
    let bp = D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
    };
    for (i, it) in st.items.iter().enumerate() {
        let (cx, cy) = cell(i, h as f32);
        let sel = st.sel.contains(&i);
        let r = rect(cx + 1.0, cy + 1.0, cx + CELL_W - 1.0, cy + CELL_H - 1.0);
        if sel || st.hover == Some(i) {
            rt.FillRectangle(&r, if sel { &selb } else { &hovb });
            if sel {
                rt.DrawRectangle(&rect(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5), &edge, 1.0, None);
            }
        }
        if let Some(Some(px)) = st.icons.get(i) {
            if let Ok(b) = rt.CreateBitmap(D2D_SIZE_U { width: ICON as u32, height: ICON as u32 }, Some(px.as_ptr() as *const c_void), ICON as u32 * 4, &bp) {
                crate::flyout::draw_icon(&rt, &b, cx + (CELL_W - ICON) / 2.0, cy + 2.0, ICON);
            }
        }
        // two lines max unless selected, white with a soft dark shadow like Explorer's desktop
        let lines = if sel { 5.0 } else { 2.0 };
        let tr = rect(cx + 2.0, cy + ICON + 5.0, cx + CELL_W - 2.0, cy + ICON + 5.0 + 16.0 * lines);
        let text = utf16(&it.name);
        for (dx, dy) in [(1.0, 1.0), (0.0, 1.0), (1.0, 0.0)] {
            rt.DrawText(&text, &fmt, &rect(tr.left + dx, tr.top + dy, tr.right + dx, tr.bottom + dy), &shadow, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        }
        rt.DrawText(&text, &fmt, &tr, &white, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
    }
    rt.EndDraw(None, None)?;
    let _ = GdiFlush();
    SelectObject(dc, old);
    let _ = DeleteDC(dc);
    Ok((dib, bits))
}

/// Push the rendered icons to the layered window (per-pixel alpha over the wallpaper).
unsafe fn present(hwnd: HWND) {
    ST.with(|s| {
        let st = s.borrow();
        let (w, h) = st.size;
        if w == 0 {
            return;
        }
        if let Ok((dib, _)) = render(&st, w, h, false) {
            let src = CreateCompatibleDC(None);
            let old = SelectObject(src, dib.into());
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8, ..Default::default() };
            let size = SIZE { cx: w, cy: h };
            let zero = POINT::default();
            let _ = UpdateLayeredWindow(hwnd, None, None, Some(&size), Some(src), Some(&zero), COLORREF(0), Some(&blend), ULW_ALPHA);
            SelectObject(src, old);
            let _ = DeleteDC(src);
            let _ = DeleteObject(dib.into());
        }
    });
}

unsafe fn reload(hwnd: HWND) {
    let items = enumerate();
    let icons = load_icons(&items);
    ST.with(|s| {
        let mut st = s.borrow_mut();
        st.items = items;
        st.icons = icons;
        st.sel.clear();
    });
    present(hwnd);
}

// ======================= actions =======================

unsafe fn open(it: &Item) {
    // folders open in the Rust File Explorer
    if !it.parse.starts_with("::") && std::path::Path::new(&it.parse).is_dir() {
        crate::files::request(&it.parse);
        return;
    }
    let (file, args) = if it.parse.starts_with("::") { (String::from("explorer.exe"), format!("shell:{}", it.parse)) } else { (it.parse.clone(), String::new()) };
    ShellExecuteW(None, w!("open"), &HSTRING::from(file), &HSTRING::from(args), PCWSTR::null(), SW_SHOWNORMAL);
}

/// Windows' own context menu for the selected items, or for the desktop background when none.
unsafe fn context_menu(hwnd: HWND, items: &[Item], pt: POINT) {
    let Ok(desk) = SHGetDesktopFolder() else { return };
    let menu: Option<IContextMenu> = if items.is_empty() {
        desk.CreateViewObject(hwnd).ok()
    } else {
        let pidls: Vec<*mut windows::Win32::UI::Shell::Common::ITEMIDLIST> = items
            .iter()
            .filter_map(|it| {
                let mut p = std::ptr::null_mut();
                desk.ParseDisplayName(hwnd, None, &HSTRING::from(it.parse.as_str()), None, &mut p, std::ptr::null_mut()).ok().map(|_| p)
            })
            .collect();
        let consts: Vec<*const windows::Win32::UI::Shell::Common::ITEMIDLIST> = pidls.iter().map(|p| *p as *const _).collect();
        let r = desk.GetUIObjectOf(hwnd, &consts, None).ok();
        for p in pidls {
            CoTaskMemFree(Some(p as *const c_void));
        }
        r
    };
    let Some(cm) = menu else { return };
    let Ok(m) = CreatePopupMenu() else { return };
    if cm.QueryContextMenu(m, 0, 1, 0x7FFF, CMF_NORMAL | if items.is_empty() { CMF_NODEFAULT } else { CMF_EXPLORE }).is_ok() {
        crate::flyout::force_foreground(hwnd);
        let cmd = TrackPopupMenuEx(m, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, pt.x, pt.y, hwnd, None).0;
        if cmd > 0 {
            let ici = CMINVOKECOMMANDINFOEX {
                cbSize: size_of::<CMINVOKECOMMANDINFOEX>() as u32,
                fMask: 0x4000 | CMIC_MASK_PTINVOKE, // CMIC_MASK_UNICODE
                hwnd,
                lpVerb: windows::core::PCSTR((cmd - 1) as usize as *const u8),
                lpVerbW: PCWSTR((cmd - 1) as usize as *const u16),
                nShow: SW_SHOWNORMAL.0,
                ptInvoke: pt,
                ..Default::default()
            };
            let _ = cm.InvokeCommand(&ici as *const _ as *const CMINVOKECOMMANDINFO);
        }
    }
    let _ = DestroyMenu(m);
    let _ = PostMessageW(Some(hwnd), WM_DESK_REFRESH, WPARAM(0), LPARAM(0));
}

/// Delete key: send the selection to the Recycle Bin (undoable, with Windows' own progress UI).
unsafe fn recycle(hwnd: HWND, items: &[Item]) {
    let Ok(op) = CoCreateInstance::<_, IFileOperation>(&FileOperation, None, CLSCTX_ALL) else { return };
    let _ = op.SetOwnerWindow(hwnd);
    let _ = op.SetOperationFlags(FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE);
    for it in items.iter().filter(|i| !i.parse.starts_with("::")) {
        if let Ok(si) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(it.parse.as_str()), None) {
            let _ = op.DeleteItem(&si, None);
        }
    }
    let _ = op.PerformOperations();
}

/// Refresh when anything changes on either desktop folder.
fn watch(hwnd: HWND) {
    let h = hwnd.0 as isize;
    std::thread::spawn(move || unsafe {
        use windows::Win32::Storage::FileSystem::*;
        use windows::Win32::System::Threading::WaitForMultipleObjects;
        let dirs: Vec<String> = [&FOLDERID_Desktop, &FOLDERID_PublicDesktop]
            .iter()
            .filter_map(|id| SHGetKnownFolderPath(*id, KF_FLAG_DEFAULT, None).ok().map(|p| { let s = p.to_string().unwrap_or_default(); CoTaskMemFree(Some(p.0 as *const c_void)); s }))
            .collect();
        let flags = FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME | FILE_NOTIFY_CHANGE_ATTRIBUTES;
        let handles: Vec<HANDLE> = dirs.iter().filter_map(|d| FindFirstChangeNotificationW(&HSTRING::from(d.as_str()), false, flags).ok()).collect();
        if handles.is_empty() {
            return;
        }
        loop {
            let r = WaitForMultipleObjects(&handles, false, u32::MAX);
            let i = r.0 as usize;
            if i >= handles.len() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(250)); // a burst of changes -> one refresh
            let _ = PostMessageW(Some(HWND(h as *mut c_void)), WM_DESK_REFRESH, WPARAM(0), LPARAM(0));
            let _ = FindNextChangeNotification(handles[i]);
        }
    });
}

// ======================= window =======================

pub unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let xy = |lp: LPARAM| ((lp.0 & 0xFFFF) as i16 as f32, ((lp.0 >> 16) & 0xFFFF) as i16 as f32);
    let at = |lp: LPARAM| ST.with(|s| { let st = s.borrow(); let (x, y) = xy(lp); hit(st.items.len(), st.size.1 as f32, x, y) });
    match msg {
        WM_DESK_REFRESH => reload(hwnd),
        WM_LBUTTONDOWN => {
            let i = at(lp);
            let ctrl = (wp.0 & 0x8) != 0; // MK_CONTROL
            ST.with(|s| {
                let mut st = s.borrow_mut();
                match (i, ctrl) {
                    (Some(i), true) => {
                        if let Some(p) = st.sel.iter().position(|&x| x == i) { st.sel.remove(p); } else { st.sel.push(i); }
                    }
                    (Some(i), false) => st.sel = vec![i],
                    (None, _) => st.sel.clear(),
                }
            });
            let _ = SetFocus(Some(hwnd));
            present(hwnd);
        }
        WM_LBUTTONDBLCLK => {
            if let Some(i) = at(lp) {
                if let Some(it) = ST.with(|s| s.borrow().items.get(i).cloned()) {
                    open(&it);
                }
            }
        }
        WM_RBUTTONUP => {
            let i = at(lp);
            let items = ST.with(|s| {
                let mut st = s.borrow_mut();
                match i {
                    Some(i) if !st.sel.contains(&i) => st.sel = vec![i],
                    None => st.sel.clear(),
                    _ => {}
                }
                st.sel.iter().filter_map(|&k| st.items.get(k).cloned()).collect::<Vec<_>>()
            });
            present(hwnd);
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            context_menu(hwnd, &items, pt);
        }
        WM_KEYDOWN => {
            let items = ST.with(|s| { let st = s.borrow(); st.sel.iter().filter_map(|&k| st.items.get(k).cloned()).collect::<Vec<_>>() });
            match wp.0 as u16 {
                0x2E => recycle(hwnd, &items), // Delete
                0x0D => items.iter().for_each(|i| open(i)), // Enter
                0x74 => reload(hwnd), // F5
                _ => return DefWindowProcW(hwnd, msg, wp, lp),
            }
        }
        WM_MOUSEMOVE => {
            let i = at(lp);
            if ST.with(|s| std::mem::replace(&mut s.borrow_mut().hover, i) != i) {
                let mut tme = windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT { cbSize: 16 + size_of::<HWND>() as u32, dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::TrackMouseEvent(&mut tme);
                present(hwnd);
            }
        }
        WM_MOUSELEAVE => {
            ST.with(|s| s.borrow_mut().hover = None);
            present(hwnd);
        }
        WM_WINDOWPOSCHANGING => {
            // always the bottom-most window (just above the wallpaper)
            let p = &mut *(lp.0 as *mut WINDOWPOS);
            if !crate::session::DESKTOP_UP.load(std::sync::atomic::Ordering::Relaxed) {
                p.hwndInsertAfter = HWND_BOTTOM; // (Win+D raises the desktop over the windows for a while)
            }
            return LRESULT(0);
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;

/// Shell mode: full-screen desktop icon layer at the bottom of the z-order.
pub unsafe fn create(inst: HINSTANCE) -> windows::core::Result<HWND> {
    let wc = WNDCLASSW { style: CS_DBLCLKS, lpfnWndProc: Some(proc), hInstance: inst, lpszClassName: w!("shellbits_desktop"), hCursor: LoadCursorW(None, IDC_ARROW)?, ..Default::default() };
    RegisterClassW(&wc);
    let (w, h) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
    // owned by the wallpaper window: owned windows always stay just above their owner
    let wall = HWND(crate::wallpaper::WALL.load(Relaxed) as *mut c_void);
    let owner = (!wall.is_invalid()).then_some(wall);
    let hwnd = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_LAYERED, w!("shellbits_desktop"), w!("Desktop"), WS_POPUP | WS_VISIBLE, 0, 0, w, h, owner, None, Some(inst), None)?;
    ST.with(|s| s.borrow_mut().size = (w, h));
    DESK.store(hwnd.0 as isize, Relaxed);
    let _ = SetWindowPos(hwnd, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    reload(hwnd);
    watch(hwnd);
    Ok(hwnd)
}

/// `shellbits --snapshot-desktop out.bmp`: the real desktop icons over a stand-in wallpaper.
pub unsafe fn snapshot(path: &str) -> windows::core::Result<()> {
    let items = enumerate();
    let icons = load_icons(&items);
    let (w, h) = (1920, 1032);
    let st = State { sel: if items.len() > 1 { vec![1] } else { vec![] }, hover: (items.len() > 2).then_some(2), items, icons, size: (w, h) };
    let (dib, bits) = render(&st, w, h, true)?;
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
    let _ = DeleteObject(dib.into());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grid_is_column_major() {
        // 1032px tall work area at 86px rows -> 11 rows
        assert_eq!(cell(0, 1032.0), (MARGIN, MARGIN));
        assert_eq!(cell(1, 1032.0), (MARGIN, MARGIN + CELL_H));
        assert_eq!(cell(11, 1032.0), (MARGIN + CELL_W, MARGIN));
        assert_eq!(hit(12, 1032.0, MARGIN + CELL_W + 5.0, MARGIN + 5.0), Some(11));
        assert_eq!(hit(5, 1032.0, 1000.0, 5.0), None);
    }
    #[test]
    fn shell_items_first_then_names() {
        let mut v = vec![
            Item { name: "b".into(), parse: "C:\\b".into() },
            Item { name: "Recycle Bin".into(), parse: RECYCLE.into() },
            Item { name: "A".into(), parse: "C:\\A".into() },
        ];
        sort(&mut v);
        assert_eq!(v.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["Recycle Bin", "A", "b"]);
    }
}

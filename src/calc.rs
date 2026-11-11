//! Calculator (Standard and Scientific modes), laid out like Windows' own: title bar, mode header,
//! display, memory row, keypad, and the History / Memory side panel when the window is wide enough.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::flyout::{glyph, rect, rrect, utf16};
use crate::quick::rgb;

pub static WIN: AtomicIsize = AtomicIsize::new(0);

// ======================= the arithmetic =======================

/// Keys, both modes: 0-9 . + - * / = ( ) and C clear, E clear entry, B backspace, N negate, % percent,
/// R 1/x, S square, Q square root; memory: m clear, r recall, p add, s subtract, t store.
/// Scientific: P pi, X e, A |x|, F n!, T 10^x, L log, l ln, c cube, b cube root, w 2^x, z e^x,
/// f floor, k ceiling, i/o/a sin/cos/tan, I/O/U their inverses, D next angle unit, and the two-operand
/// keys ^ power, y y-th root, g log base y, d mod, x "times ten to the".
#[derive(Default, Clone)]
pub struct Calc {
    entry: String,             // the number being typed or shown ("" = 0)
    stack: Vec<(f64, char)>,   // left operands waiting with their operator; '(' marks an open bracket
    after_op: bool,            // the last key was an operator (another one replaces it)
    fresh: bool,               // the entry is a result: the next digit starts a new number
    last: Option<(char, f64)>, // for pressing = again
    pub sci: bool,             // Scientific: × ÷ before + −, powers first; Standard works left to right
    pub angle: u8,             // 0 degrees, 1 radians, 2 gradians
    pub expr: String,          // the small line above the number
    pub error: Option<&'static str>,
    pub mem: Option<f64>,
    pub history: Vec<(String, String)>, // newest last: expression, result
}

/// A number as the display shows it before grouping: 15 significant digits, no trailing zeros.
pub fn fmt(v: f64) -> String {
    if v == 0.0 {
        return "0".into();
    }
    let r: f64 = format!("{v:.14e}").parse().unwrap_or(v); // drops binary noise such as 0.1 + 0.2
    if r.abs() >= 1e16 || r.abs() < 1e-9 { format!("{r:e}") } else { format!("{r}") }
}

/// Thousands separators for the whole part.
pub fn grouped(s: &str) -> String {
    if s.contains('e') {
        return s.to_string();
    }
    let (neg, rest) = s.strip_prefix('-').map_or((false, s), |r| (true, r));
    let (whole, frac) = rest.split_once('.').map_or((rest, None), |(a, b)| (a, Some(b)));
    let mut out = String::new();
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    format!("{}{out}{}", if neg { "-" } else { "" }, frac.map(|f| format!(".{f}")).unwrap_or_default())
}

/// How an operator reads on the expression line.
fn sign(op: char) -> &'static str {
    match op {
        '+' => "+",
        '-' => "−",
        '*' => "×",
        '/' => "÷",
        '^' => "^",
        'y' => "yroot",
        'g' => "log base",
        'd' => "mod",
        _ => "e+",
    }
}

impl Calc {
    fn value(&self) -> f64 {
        self.entry.parse().unwrap_or(0.0)
    }
    /// A number is being typed (so "C" offers to clear just that entry first).
    pub fn typing(&self) -> bool {
        !self.entry.is_empty() && !self.fresh
    }
    /// What the big line shows.
    pub fn display(&self) -> String {
        match self.error {
            Some(e) => e.to_string(),
            None => grouped(if self.entry.is_empty() { "0" } else { &self.entry }),
        }
    }
    fn prec(&self, op: char) -> u8 {
        match (self.sci, op) {
            (false, _) | (_, '+' | '-') => 1,
            (_, '*' | '/' | 'd') => 2,
            _ => 3,
        }
    }
    fn apply(&mut self, a: f64, op: char, b: f64) -> Option<f64> {
        Some(match op {
            '+' => a + b,
            '-' => a - b,
            '*' => a * b,
            '/' | 'd' | 'y' if b == 0.0 => {
                self.error = Some("Cannot divide by zero");
                return None;
            }
            '/' => a / b,
            'd' => a % b,
            '^' => a.powf(b),
            'y' => a.powf(1.0 / b),
            'g' if a <= 0.0 || b <= 0.0 || b == 1.0 => {
                self.error = Some("Invalid input");
                return None;
            }
            'g' => a.ln() / b.ln(),
            _ => a * 10f64.powf(b),
        })
    }
    fn result(&mut self, r: Option<f64>) {
        match r {
            Some(r) if r.is_finite() => self.entry = fmt(r),
            Some(_) => self.error = Some("Overflow"),
            None => {}
        }
        self.fresh = true;
    }
    /// The operands and operators still waiting, as the expression line shows them.
    fn pending(&self) -> String {
        self.stack.iter().map(|&(v, op)| if op == '(' { "(".to_string() } else { format!("{} {}", fmt(v), sign(op)) }).collect::<Vec<_>>().join(" ")
    }
    /// Fold waiting operators into `v` while `keep_going` says so (top of the stack first).
    fn fold(&mut self, v: f64, keep_going: impl Fn(&Calc, char) -> bool) -> Option<f64> {
        let mut v = Some(v);
        while let Some(&(a, op)) = self.stack.last() {
            if !keep_going(self, op) {
                break;
            }
            self.stack.pop();
            if op != '(' {
                v = v.and_then(|b| self.apply(a, op, b));
            }
        }
        v
    }
    fn start_over(&mut self) {
        *self = Calc { mem: self.mem, history: std::mem::take(&mut self.history), sci: self.sci, angle: self.angle, ..Default::default() };
    }
    /// Put a past result (history row `k`, newest = 0) back on the display.
    pub fn recall(&mut self, k: usize) {
        if let Some((_, r)) = self.history.iter().rev().nth(k) {
            self.entry = r.replace(',', "");
            (self.fresh, self.error, self.after_op) = (true, None, false);
        }
    }
    fn radians(&self, v: f64) -> f64 {
        match self.angle {
            0 => v.to_radians(),
            1 => v,
            _ => v * std::f64::consts::PI / 200.0,
        }
    }
    fn from_radians(&self, v: f64) -> f64 {
        match self.angle {
            0 => v.to_degrees(),
            1 => v,
            _ => v * 200.0 / std::f64::consts::PI,
        }
    }
    /// One-operand keys. None = an error was set.
    fn unary(&mut self, k: char, v: f64) -> Option<f64> {
        let bad = |c: &mut Calc, why| {
            c.error = Some(why);
            None
        };
        let tidy = |r: f64| if r.abs() < 1e-15 { 0.0 } else { r }; // sin 180° is 0, not 1.2e-16
        match k {
            'R' if v == 0.0 => bad(self, "Cannot divide by zero"),
            'R' => Some(1.0 / v),
            'S' => Some(v * v),
            'c' => Some(v * v * v),
            'Q' if v < 0.0 => bad(self, "Invalid input"),
            'Q' => Some(v.sqrt()),
            'b' => Some(v.cbrt()),
            'A' => Some(v.abs()),
            'F' if v < 0.0 || v.fract() != 0.0 || v > 170.0 => bad(self, if v > 170.0 { "Overflow" } else { "Invalid input" }),
            'F' => Some((1..=v as u64).fold(1.0, |p, i| p * i as f64)),
            'T' => Some(10f64.powf(v)),
            'w' => Some(2f64.powf(v)),
            'z' => Some(v.exp()),
            'L' | 'l' if v <= 0.0 => bad(self, "Invalid input"),
            'L' => Some(v.log10()),
            'l' => Some(v.ln()),
            'f' => Some(v.floor()),
            'k' => Some(v.ceil()),
            'i' => Some(tidy(self.radians(v).sin())),
            'o' => Some(tidy(self.radians(v).cos())),
            'a' if tidy(self.radians(v).cos()) == 0.0 => bad(self, "Invalid input"),
            'a' => Some(tidy(self.radians(v).tan())),
            'I' | 'O' if v.abs() > 1.0 => bad(self, "Invalid input"),
            'I' => Some(self.from_radians(v.asin())),
            'O' => Some(self.from_radians(v.acos())),
            _ => Some(self.from_radians(v.atan())),
        }
    }
    pub fn key(&mut self, k: char) {
        if self.error.is_some() {
            // after an error the next key starts over (memory and history stay)
            self.start_over();
            if !k.is_ascii_digit() && k != '.' {
                return;
            }
        }
        let after_op = std::mem::take(&mut self.after_op);
        let v = self.value();
        match k {
            '0'..='9' | '.' => {
                if self.fresh {
                    self.entry.clear();
                    self.fresh = false;
                    if self.stack.is_empty() {
                        self.expr.clear();
                    }
                }
                let digits = self.entry.chars().filter(char::is_ascii_digit).count();
                match k {
                    '.' if self.entry.contains('.') => {}
                    '.' if self.entry.is_empty() => self.entry.push_str("0."),
                    '.' => self.entry.push('.'),
                    _ if digits >= 16 => {}
                    _ if self.entry == "0" => self.entry = k.to_string(),
                    _ => self.entry.push(k),
                }
            }
            '+' | '-' | '*' | '/' | '^' | 'y' | 'g' | 'd' | 'x' => {
                match self.stack.last_mut() {
                    // operator pressed twice: just change it
                    Some(top) if after_op && top.1 != '(' => top.1 = k,
                    _ => {
                        let Some(a) = self.fold(v, |c, op| op != '(' && c.prec(op) >= c.prec(k)) else { return };
                        self.stack.push((a, k));
                        self.result(Some(a));
                    }
                }
                self.last = None;
                self.expr = self.pending();
                self.after_op = true;
            }
            '(' => {
                self.stack.push((0.0, '('));
                self.expr = self.pending();
                self.fresh = true;
            }
            ')' if self.stack.iter().any(|s| s.1 == '(') => {
                let r = self.fold(v, |_, op| op != '(');
                self.stack.pop();
                self.expr = self.pending();
                self.result(r);
            }
            '=' => {
                if self.stack.is_empty() {
                    let Some((op, b)) = self.last else {
                        self.expr = format!("{} =", fmt(v));
                        self.fresh = true;
                        return;
                    };
                    self.stack.push((v, op));
                    return self.equals(b);
                }
                self.equals(v);
            }
            'C' => self.start_over(),
            'E' => self.entry.clear(),
            'B' if !self.fresh => {
                self.entry.pop();
                if self.entry == "-" {
                    self.entry.clear();
                }
            }
            'N' if v != 0.0 => {
                self.entry = match self.entry.strip_prefix('-') {
                    Some(p) => p.to_string(),
                    None => format!("-{}", self.entry),
                }
            }
            '%' => {
                let r = match self.stack.last() {
                    Some(&(a, op)) if op != '(' => a * v / 100.0,
                    _ => 0.0,
                };
                self.result(Some(r));
            }
            'P' => self.result(Some(std::f64::consts::PI)),
            'X' => self.result(Some(std::f64::consts::E)),
            'D' => self.angle = (self.angle + 1) % 3,
            'R' | 'S' | 'Q' | 'c' | 'b' | 'A' | 'F' | 'T' | 'w' | 'z' | 'L' | 'l' | 'f' | 'k' | 'i' | 'o' | 'a' | 'I' | 'O' | 'U' => {
                let r = self.unary(k, v);
                self.result(r);
            }
            'm' => self.mem = None,
            'r' => {
                if let Some(m) = self.mem {
                    self.result(Some(m));
                }
            }
            'p' => self.mem = Some(self.mem.unwrap_or(0.0) + v),
            's' => self.mem = Some(self.mem.unwrap_or(0.0) - v),
            't' => self.mem = Some(v),
            _ => self.after_op = after_op, // not a calculator key: nothing changes
        }
    }
    /// Finish the calculation with `b` as the last operand (brackets still open are closed).
    fn equals(&mut self, b: f64) {
        self.expr = format!("{} {} =", self.pending(), fmt(b));
        let repeat = self.stack.iter().rev().find(|s| s.1 != '(').map(|s| (s.1, b));
        let r = self.fold(b, |_, _| true);
        self.result(r);
        if self.error.is_none() {
            self.history.push((self.expr.clone(), self.display()));
        }
        self.stack.clear();
        self.last = repeat;
    }
}

// ======================= layout (measured on Windows' Calculator at 1202x934) =======================

const CAPTION: f32 = 32.0;
const ICON: u32 = 32; // decoded size of the title-bar icon (drawn at 15)
const SIDE: f32 = 322.0; // History / Memory panel
const SIDE_MIN: f32 = 560.0; // narrower windows drop the panel, as Windows' does
const ROW_H: f32 = 72.0; // one History / Memory entry
const LIST_TOP: f32 = 92.0;

/// Standard keypad, row by row: (label, key). Labels in the private-use range are icon glyphs.
const STANDARD: [(&str, char); 24] = [
    ("\u{E94C}", '%'), ("CE", 'E'), ("C", 'C'), ("\u{E94F}", 'B'),
    ("¹⁄x", 'R'), ("x²", 'S'), ("²√x", 'Q'), ("\u{E94A}", '/'),
    ("7", '7'), ("8", '8'), ("9", '9'), ("\u{E947}", '*'),
    ("4", '4'), ("5", '5'), ("6", '6'), ("\u{E949}", '-'),
    ("1", '1'), ("2", '2'), ("3", '3'), ("\u{E948}", '+'),
    ("⁺∕₋", 'N'), ("0", '0'), (".", '.'), ("\u{E94E}", '='),
];
/// Scientific keypad: 7 rows of 5. '\u{2}' is the 2nd switch (it swaps the first column, see SECOND).
const SCIENTIFIC: [(&str, char); 35] = [
    ("2ⁿᵈ", '\u{2}'), ("π", 'P'), ("e", 'X'), ("C", 'C'), ("\u{E94F}", 'B'),
    ("x²", 'S'), ("¹⁄x", 'R'), ("|x|", 'A'), ("exp", 'x'), ("mod", 'd'),
    ("²√x", 'Q'), ("(", '('), (")", ')'), ("n!", 'F'), ("\u{E94A}", '/'),
    ("xʸ", '^'), ("7", '7'), ("8", '8'), ("9", '9'), ("\u{E947}", '*'),
    ("10ˣ", 'T'), ("4", '4'), ("5", '5'), ("6", '6'), ("\u{E949}", '-'),
    ("log", 'L'), ("1", '1'), ("2", '2'), ("3", '3'), ("\u{E948}", '+'),
    ("ln", 'l'), ("⁺∕₋", 'N'), ("0", '0'), (".", '.'), ("\u{E94E}", '='),
];
/// What rows 1-6 of the first Scientific column become while 2nd is on.
const SECOND: [(&str, char); 6] = [("x³", 'c'), ("³√x", 'b'), ("ʸ√x", 'y'), ("2ˣ", 'w'), ("logᵧx", 'g'), ("eˣ", 'z')];
const MEM: [(&str, char); 5] = [("MC", 'm'), ("MR", 'r'), ("M+", 'p'), ("M−", 's'), ("MS", 't')];
const TRIG: [(&str, char); 6] = [("sin", 'i'), ("cos", 'o'), ("tan", 'a'), ("sin⁻¹", 'I'), ("cos⁻¹", 'O'), ("tan⁻¹", 'U')];
const FUNC: [(&str, char); 3] = [("|x|", 'A'), ("⌊x⌋", 'f'), ("⌈x⌉", 'k')];
const ANGLES: [&str; 3] = ["DEG", "RAD", "GRAD"];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hit {
    Key(usize),
    Mem(usize),
    Min,
    Max,
    Close,
    Menu,       // the mode list
    Pin,        // keep on top (Standard)
    Angle,      // DEG / RAD / GRAD (Scientific)
    Trig,       // the Trigonometry and Function drop-downs (Scientific)
    Func,
    Tab(usize), // History / Memory
    Row(usize), // a row of the side panel's list, top one = 0
    Clear,      // the bin at the foot of the side panel: clear history / memory
}

/// The bin button at the bottom-right of the side panel.
fn clear_rect(w: f32, h: f32) -> D2D_RECT_F {
    rect(w - 44.0, h - 44.0, w - 12.0, h - 12.0)
}

/// What is remembered between runs: the mode and the angle unit ("sci=1 angle=0").
pub fn parse_prefs(s: &str) -> (bool, u8) {
    let get = |key: &str| s.split_whitespace().find_map(|t| t.strip_prefix(key)).and_then(|v| v.parse::<u8>().ok()).unwrap_or(0);
    (get("sci=") == 1, get("angle=").min(2))
}
fn prefs_file() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join("shellbits").join("calc.txt")
}
fn save_prefs(sci: bool, angle: u8) {
    let f = prefs_file();
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(f, format!("sci={} angle={angle}", sci as u8));
}

/// Width of the keypad side (everything left of the side panel).
fn main_w(w: f32) -> f32 {
    if w >= SIDE_MIN + SIDE { w - SIDE } else { w }
}

/// (columns, rows, keypad top) for a mode in a window `h` tall.
/// ponytail: the keypad's top is a fixed share of the height (329 or 462 of 934, as measured at one
/// size); Windows' own keeps a minimum display height - revisit if small windows look off.
fn grid(sci: bool, h: f32) -> (usize, usize, f32) {
    if sci { (5, 7, (h * 462.0 / 934.0).round()) } else { (4, 6, (h * 329.0 / 934.0).round()) }
}

/// Box of keypad button `i` in a window of w x h.
pub fn key_rect(i: usize, sci: bool, w: f32, h: f32) -> D2D_RECT_F {
    let (cols, rows, t) = grid(sci, h);
    let (l, r, b) = (5.0, main_w(w) - 0.4, h - 2.6);
    let (cw, rh) = ((r - l) / cols as f32, (b - t) / rows as f32);
    // columns land on rounded pixels, rows on floored ones - that is what the measured boxes show
    let (x, y) = ((l + (i % cols) as f32 * cw).round(), (t + (i / cols) as f32 * rh).floor());
    rect(x, y, (l + (i % cols + 1) as f32 * cw).round() - 2.0, (t + (i / cols + 1) as f32 * rh).floor() - 2.0)
}

/// Centre line of the memory row (the angle row sits 61 above it in Scientific).
fn mem_y(sci: bool, h: f32) -> f32 {
    grid(sci, h).2 - if sci { 103.0 } else { 33.0 }
}
fn mem_rect(i: usize, sci: bool, h: f32) -> D2D_RECT_F {
    let y = mem_y(sci, h);
    rect(4.0 + i as f32 * 80.0, y - 16.0, 84.0 + i as f32 * 80.0, y + 16.0)
}
const PIN: D2D_RECT_F = D2D_RECT_F { left: 141.0, top: 41.0, right: 173.0, bottom: 73.0 };
const MENU: D2D_RECT_F = D2D_RECT_F { left: 5.0, top: 37.0, right: 45.0, bottom: 77.0 };

pub fn hit(x: f32, y: f32, sci: bool, w: f32, h: f32) -> Option<Hit> {
    let inside = |r: D2D_RECT_F| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
    if y < CAPTION && x >= w - 138.0 {
        return Some([Hit::Min, Hit::Max, Hit::Close][(((x - (w - 138.0)) / 46.0) as usize).min(2)]);
    }
    let mw = main_w(w);
    if x >= mw {
        let c = clear_rect(w, h);
        if x >= c.left && x < c.right && y >= c.top && y < c.bottom {
            return Some(Hit::Clear);
        }
        return match y {
            y if (40.0..80.0).contains(&y) && x < mw + 150.0 => Some(Hit::Tab((x >= mw + 74.0) as usize)),
            y if y >= LIST_TOP => Some(Hit::Row(((y - LIST_TOP) / ROW_H) as usize)),
            _ => None,
        };
    }
    if inside(MENU) {
        return Some(Hit::Menu);
    }
    if !sci && inside(PIN) {
        return Some(Hit::Pin);
    }
    if sci {
        let (my, top) = (mem_y(true, h), grid(true, h).2);
        if inside(rect(4.0, my - 77.0, 84.0, my - 45.0)) {
            return Some(Hit::Angle);
        }
        if inside(rect(5.0, top - 46.0, 163.0, top - 16.0)) {
            return Some(Hit::Trig);
        }
        if inside(rect(167.0, top - 46.0, 289.0, top - 16.0)) {
            return Some(Hit::Func);
        }
    }
    let n = if sci { 35 } else { 24 };
    (0..n).find(|i| inside(key_rect(*i, sci, w, h))).map(Hit::Key).or_else(|| (0..5).find(|i| inside(mem_rect(*i, sci, h))).map(Hit::Mem))
}

// ======================= drawing =======================

struct Gfx {
    rt: ID2D1DCRenderTarget,
    dw: IDWriteFactory,
    accent: D2D1_COLOR_F,
    icon: Option<ID2D1Bitmap>, // the app's own icon, for the title bar
}

#[derive(Default)]
struct State {
    calc: Calc,
    hover: Option<Hit>,
    down: Option<Hit>,
    tab: usize,   // side panel: 0 History, 1 Memory
    on_top: bool, // keep on top
    second: bool, // Scientific: the 2nd switch
    gfx: Option<Gfx>,
}

impl State {
    /// Label and key of keypad button `i` in the current mode.
    fn key(&self, i: usize) -> (&'static str, char) {
        match (self.calc.sci, self.second, i) {
            (false, ..) => STANDARD[i],
            (true, true, i) if i % 5 == 0 && i >= 5 => SECOND[i / 5 - 1],
            (true, _, 3) if self.calc.typing() => ("CE", 'E'), // "C" clears the entry first, then everything
            _ => SCIENTIFIC[i],
        }
    }
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
}

unsafe fn make_gfx() -> windows::core::Result<Gfx> {
    let f: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
    let props = D2D1_RENDER_TARGET_PROPERTIES { pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }, ..Default::default() };
    let rt = f.CreateDCRenderTarget(&props)?;
    rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
    let c = windows::UI::ViewManagement::UISettings::new()
        .and_then(|u| u.GetColorValue(windows::UI::ViewManagement::UIColorType::AccentLight2))
        .unwrap_or(windows::UI::Color { A: 255, R: 0x4C, G: 0xC2, B: 0xFF });
    // the icon Windows has for Calculator (none if the app is not installed)
    let icon = (|| -> windows::core::Result<ID2D1Bitmap> {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let px = crate::launcher::icon_pixels(&wic, r"shell:AppsFolder\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", ICON)?;
        let props = D2D1_BITMAP_PROPERTIES { pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }, dpiX: 96.0, dpiY: 96.0 };
        rt.CreateBitmap(D2D_SIZE_U { width: ICON, height: ICON }, Some(px.as_ptr() as *const c_void), ICON * 4, &props)
    })()
    .ok();
    Ok(Gfx { icon, rt, dw: DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?, accent: D2D1_COLOR_F { r: c.R as f32 / 255.0, g: c.G as f32 / 255.0, b: c.B as f32 / 255.0, a: 1.0 } })
}

unsafe fn draw(st: &State, g: &Gfx, w: f32, h: f32, maximized: bool) -> windows::core::Result<()> {
    let rt = &g.rt;
    rt.Clear(Some(&rgb(0x202020)));
    let brush = |c: D2D1_COLOR_F| rt.CreateSolidColorBrush(&c, None);
    let b = |v: u32| brush(rgb(v));
    let font = |name: PCWSTR, size: f32, weight, style, align| -> windows::core::Result<IDWriteTextFormat> {
        let t = g.dw.CreateTextFormat(name, None, weight, style, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))?;
        t.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        t.SetTextAlignment(align)?;
        t.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        Ok(t)
    };
    let (ui, disp, icons) = (w!("Segoe UI Variable Text"), w!("Segoe UI Variable Display"), w!("Segoe Fluent Icons"));
    let (n, sb, lead, mid, trail) = (DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_TRAILING);
    let (up, italic) = (DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STYLE_ITALIC);
    let text = |s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, br: &ID2D1SolidColorBrush| rt.DrawText(&utf16(s), f, &r, br, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
    let white = b(0xFFFFFF)?;
    let hot = |r: D2D_RECT_F, which: Hit| -> windows::core::Result<()> {
        if st.hover == Some(which) {
            rt.FillRoundedRectangle(&rrect(r, 4.0), &b(0x2D2D2D)?);
        }
        Ok(())
    };
    let (mw, sci) = (main_w(w), st.calc.sci);

    // title bar: icon, name, then minimise / maximise / close
    if let Some(i) = &g.icon {
        rt.DrawBitmap(i, Some(&rect(19.0, 9.0, 34.0, 24.0)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
    }
    text("Calculator", &font(w!("Segoe UI Variable Small"), 12.0, n, up, lead)?, rect(49.0, 0.0, 300.0, 35.0), &white);
    let icon10 = font(icons, 10.0, n, up, mid)?;
    for (k, (cp, which)) in [(0xE921u32, Hit::Min), (if maximized { 0xE923 } else { 0xE922 }, Hit::Max), (0xE8BB, Hit::Close)].into_iter().enumerate() {
        let r = rect(w - 138.0 + k as f32 * 46.0, 0.0, w - 92.0 + k as f32 * 46.0, CAPTION);
        if st.hover == Some(which) {
            rt.FillRectangle(&r, &b(if which == Hit::Close { 0xC42B1C } else { 0x2D2D2D })?);
        }
        rt.DrawText(&glyph(cp), &icon10, &rect(r.left - 2.0, r.top, r.right, r.bottom), &white, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
    }

    // header: mode list, mode name, keep on top (Standard only)
    let icon16 = font(icons, 16.0, n, up, mid)?;
    hot(MENU, Hit::Menu)?;
    rt.DrawText(&glyph(0xE700), &icon16, &rect(9.0, 41.0, 41.0, 73.0), &white, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
    text(if sci { "Scientific" } else { "Standard" }, &font(disp, 20.0, sb, up, lead)?, rect(48.0, 40.0, 180.0, 72.0), &white);
    if !sci {
        hot(PIN, Hit::Pin)?;
        let pin = if st.on_top { brush(g.accent)? } else { white.clone() };
        rt.DrawText(&glyph(0xEE49), &font(icons, 14.0, n, up, mid)?, &PIN, &pin, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
    }

    // display: the expression so far, then the number (shrunk to fit)
    text(&st.calc.expr, &font(ui, 14.0, n, up, trail)?, rect(12.0, 96.0, mw - 12.0, 120.0), &b(0xA5A5A5)?);
    let shown = st.calc.display();
    let size = (72.0f32).min((mw - 24.0) / (shown.chars().count() as f32 * 0.56));
    text(&shown, &font(disp, size, sb, up, trail)?, rect(12.0, 122.0, mw - 11.0, 222.0), &white);

    // Scientific: angle unit and notation switch, then (below the memory row) the two drop-downs
    let (_, _, top) = grid(sci, h);
    let my = mem_y(sci, h);
    if sci {
        let semi12 = font(ui, 12.0, n, up, mid)?;
        hot(rect(4.0, my - 77.0, 84.0, my - 45.0), Hit::Angle)?;
        text(ANGLES[st.calc.angle as usize], &semi12, rect(3.0, my - 79.0, 83.0, my - 45.0), &white);
        text("F-E", &semi12, rect(83.0, my - 79.0, 163.0, my - 45.0), &white);
        rt.FillRectangle(&rect(1.0, top - 67.0, mw, top - 66.0), &b(0x323232)?);
        let (body14, chev) = (font(ui, 14.0, n, up, lead)?, font(icons, 8.0, n, up, mid)?);
        hot(rect(5.0, top - 46.0, 163.0, top - 16.0), Hit::Trig)?;
        hot(rect(167.0, top - 46.0, 289.0, top - 16.0), Hit::Func)?;
        text("⊿", &font(w!("Segoe UI Symbol"), 24.0, n, up, mid)?, rect(10.0, top - 46.0, 36.0, top - 16.0), &white);
        text("Trigonometry", &body14, rect(41.0, top - 46.0, 145.0, top - 18.0), &white);
        rt.DrawText(&glyph(0xE70D), &chev, &rect(141.0, top - 46.0, 157.0, top - 16.0), &white, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        text("f", &font(w!("Cambria Math"), 22.0, n, italic, mid)?, rect(172.0, top - 48.0, 192.0, top - 16.0), &white);
        text("Function", &body14, rect(201.0, top - 46.0, 270.0, top - 18.0), &white);
        rt.DrawText(&glyph(0xE70D), &chev, &rect(267.0, top - 46.0, 283.0, top - 16.0), &white, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
    }

    // memory row: clear and recall are dim until something is stored
    let mem12 = font(ui, 12.0, n, up, mid)?;
    for (i, (label, _)) in MEM.iter().enumerate() {
        let r = mem_rect(i, sci, h);
        let live = i >= 2 || st.calc.mem.is_some();
        if live {
            hot(r, Hit::Mem(i))?;
        }
        text(label, &mem12, rect(r.left - 1.0, r.top - 2.0, r.right - 1.0, r.bottom), &b(if live { 0xFFFFFF } else { 0x717171 })?);
    }

    // keypad
    let (digit, word, math, icon20) = (font(disp, 24.0, n, up, mid)?, font(ui, 20.0, n, up, mid)?, font(w!("Cambria Math"), 20.0, n, italic, mid)?, font(icons, 20.0, n, up, mid)?);
    for i in 0..if sci { 35 } else { 24 } {
        let (label, key) = st.key(i);
        let r = key_rect(i, sci, w, h);
        let (over, pressed) = (st.hover == Some(Hit::Key(i)), st.down == Some(Hit::Key(i)));
        let number = key.is_ascii_digit() || matches!(key, '.' | 'N');
        let lit = key == '\u{2}' && st.second; // the 2nd switch stays lit while on
        let fill = match (key, number, over, pressed) {
            ('=', _, false, _) => g.accent,
            ('=', _, true, false) => D2D1_COLOR_F { a: 0.9, ..g.accent },
            ('=', ..) => D2D1_COLOR_F { a: 0.8, ..g.accent },
            _ if lit => g.accent,
            (_, _, _, true) => rgb(0x282828),
            (_, true, false, _) | (_, false, true, _) => rgb(0x3B3B3B),
            _ => rgb(0x323232),
        };
        rt.FillRoundedRectangle(&rrect(r, 4.0), &brush(fill)?);
        if key != '=' && !lit {
            // the lighter line Windows' buttons carry along their top edge
            rt.FillRectangle(&rect(r.left + 4.0, r.top, r.right - 4.0, r.top + 1.0), &b(0x353535)?);
        }
        let ink = if key == '=' || lit { b(0x000000)? } else { white.clone() };
        let f = if label.starts_with(|c| ('\u{E000}'..'\u{F8FF}').contains(&c)) { &icon20 } else if number { &digit } else if label.contains(['x', 'n', 'ʸ']) && !label.contains("exp") && label != "ln" { &math } else { &word };
        // "CE", "C" and the other words sit a pixel lower than the centred digits in Windows' own
        text(label, f, if std::ptr::eq(f, &word) { rect(r.left, r.top + 2.0, r.right, r.bottom) } else { r }, &ink);
    }

    // side panel: History (the calculations so far, newest first) and Memory
    if mw < w {
        let side14 = font(ui, 14.0, n, up, lead)?;
        text("History", &side14, rect(mw + 13.5, 40.0, mw + 80.0, 68.0), &white);
        text("Memory", &side14, rect(mw + 81.5, 40.0, mw + 160.0, 68.0), &white);
        let ux = if st.tab == 0 { mw + 27.0 } else { mw + 99.5 }; // the underline sits under the open tab
        rt.FillRoundedRectangle(&rrect(rect(ux, 73.0, ux + 16.0, 76.0), 1.5), &brush(g.accent)?);
        let (small, big) = (font(ui, 14.0, n, up, trail)?, font(disp, 24.0, sb, up, trail)?);
        // rows: (small line, big line)
        let rows: Vec<(String, String)> = match (st.tab, st.calc.mem) {
            (0, _) => st.calc.history.iter().rev().cloned().collect(),
            (_, Some(m)) => vec![(String::new(), grouped(&fmt(m)))],
            _ => Vec::new(),
        };
        if rows.is_empty() {
            text(if st.tab == 0 { "There’s no history yet." } else { "There’s nothing saved in memory." }, &side14, rect(mw + 13.5, 90.0, w - 8.0, 120.0), &white);
        }
        for (k, (expr, result)) in rows.iter().enumerate() {
            let y = LIST_TOP + k as f32 * ROW_H;
            if y + ROW_H > h {
                break;
            }
            if y + ROW_H > h - 48.0 && k > 0 {
                break; // keep clear of the bin
            }
            hot(rect(mw + 4.0, y, w - 6.0, y + ROW_H - 4.0), Hit::Row(k))?;
            text(expr, &small, rect(mw + 8.0, y + 4.0, w - 16.0, y + 28.0), &b(0xA5A5A5)?);
            text(result, &big, rect(mw + 8.0, y + 26.0, w - 16.0, y + 62.0), &white);
        }
        if !rows.is_empty() {
            let c = clear_rect(w, h);
            hot(c, Hit::Clear)?;
            rt.DrawText(&glyph(0xE74D), &icon16, &c, &white, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        }
    }
    Ok(())
}

// ======================= window =======================

/// A drop-down under the point (x, y) of the window (DIPs). Returns the chosen item's index.
unsafe fn menu(hwnd: HWND, s: f32, x: f32, y: f32, items: &[&str], checked: Option<usize>) -> Option<usize> {
    let m = CreatePopupMenu().ok()?;
    for (i, label) in items.iter().enumerate() {
        let flags = if checked == Some(i) { MF_STRING | MF_CHECKED } else { MF_STRING };
        let wide = utf16(&format!("{label}\0"));
        let _ = AppendMenuW(m, flags, i + 1, PCWSTR(wide.as_ptr()));
    }
    let mut pt = POINT { x: (x * s) as i32, y: (y * s) as i32 };
    let _ = ClientToScreen(hwnd, &mut pt);
    let pick = TrackPopupMenu(m, TPM_RETURNCMD | TPM_NONOTIFY, pt.x, pt.y, None, hwnd, None).0;
    let _ = DestroyMenu(m);
    (pick > 0).then(|| pick as usize - 1)
}

/// Switch between Standard (false) and Scientific (true); memory and history stay.
fn set_mode(st: &mut State, sci: bool) {
    if st.calc.sci != sci {
        st.calc.key('C');
        st.calc.sci = sci;
        (st.second, st.hover, st.down) = (false, None, None);
    }
}

unsafe fn press(hwnd: HWND, h: Hit) {
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let top = ST.with(|st| grid(st.borrow().calc.sci, rc.bottom as f32 / s).2);
    match h {
        Hit::Key(i) => ST.with(|st| {
            let st = &mut *st.borrow_mut();
            match st.key(i).1 {
                '\u{2}' => st.second = !st.second,
                k => st.calc.key(k),
            }
        }),
        Hit::Mem(i) => ST.with(|st| st.borrow_mut().calc.key(MEM[i].1)),
        Hit::Min => _ = ShowWindow(hwnd, SW_MINIMIZE),
        Hit::Max => _ = ShowWindow(hwnd, if IsZoomed(hwnd).as_bool() { SW_RESTORE } else { SW_MAXIMIZE }),
        Hit::Close => _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)),
        Hit::Angle => {
            let c = ST.with(|st| {
                let st = &mut *st.borrow_mut();
                st.calc.key('D');
                (st.calc.sci, st.calc.angle)
            });
            save_prefs(c.0, c.1);
        }
        Hit::Clear => ST.with(|st| {
            let st = &mut *st.borrow_mut();
            match st.tab {
                0 => st.calc.history.clear(),
                _ => st.calc.key('m'),
            }
        }),
        // the menus run their own message loop, so nothing of ours is borrowed while they are open
        Hit::Menu => {
            let now = ST.with(|st| st.borrow().calc.sci) as usize;
            if let Some(i) = menu(hwnd, s, MENU.left, MENU.bottom, &["Standard", "Scientific"], Some(now)) {
                let angle = ST.with(|st| {
                    let st = &mut *st.borrow_mut();
                    set_mode(st, i == 1);
                    st.calc.angle
                });
                save_prefs(i == 1, angle);
            }
        }
        Hit::Trig | Hit::Func => {
            let (table, x): (&[(&str, char)], f32) = if h == Hit::Trig { (&TRIG, 5.0) } else { (&FUNC, 167.0) };
            let labels: Vec<&str> = table.iter().map(|t| t.0).collect();
            if let Some(i) = menu(hwnd, s, x, top - 14.0, &labels, None) {
                ST.with(|st| st.borrow_mut().calc.key(table[i].1));
            }
        }
        Hit::Tab(t) => ST.with(|st| st.borrow_mut().tab = t),
        // a History row brings its result back; the Memory row is memory recall
        Hit::Row(k) => ST.with(|st| {
            let st = &mut *st.borrow_mut();
            match (st.tab, k) {
                (0, k) => st.calc.recall(k),
                (_, 0) => st.calc.key('r'),
                _ => {}
            }
        }),
        Hit::Pin => {
            let on = ST.with(|st| {
                let st = &mut *st.borrow_mut();
                st.on_top = !st.on_top;
                st.on_top
            });
            let _ = SetWindowPos(hwnd, Some(if on { HWND_TOPMOST } else { HWND_NOTOPMOST }), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }
    let _ = InvalidateRect(Some(hwnd), None, false);
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // Re-entered while the window's state is in use: a handler called into Windows (a shell call, a
    // menu, a dialog) and that ran a message loop. Touching the state again would panic and close the
    // window, so this message gets default handling instead.
    if ST.with(|u| u.try_borrow_mut().is_err()) && !matches!(msg, WM_NCCREATE | WM_CREATE | WM_NCDESTROY | WM_DESTROY) {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    let at = |lp: LPARAM| {
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let sci = ST.with(|st| st.borrow().calc.sci);
        hit((lp.0 & 0xFFFF) as i16 as f32 / s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s, sci, rc.right as f32 / s, rc.bottom as f32 / s)
    };
    let key = |k: char| {
        ST.with(|st| st.borrow_mut().calc.key(k));
        let _ = InvalidateRect(Some(hwnd), None, false);
    };
    match msg {
        // no system title bar: the client area is the whole window (as in the File Explorer window)
        WM_NCCALCSIZE if wp.0 == 1 => {
            if IsZoomed(hwnd).as_bool() {
                let p = &mut *(lp.0 as *mut NCCALCSIZE_PARAMS);
                let f = GetSystemMetrics(SM_CXSIZEFRAME) + GetSystemMetrics(SM_CXPADDEDBORDER);
                p.rgrc[0].left += f;
                p.rgrc[0].top += f;
                p.rgrc[0].right -= f;
                p.rgrc[0].bottom -= f;
            }
        }
        WM_NCHITTEST => {
            let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
            let _ = ScreenToClient(hwnd, &mut pt);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let e = (6.0 * s) as i32;
            let (l, t, r, bt) = (pt.x < e, pt.y < e, pt.x >= rc.right - e, pt.y >= rc.bottom - e);
            let code = match (IsZoomed(hwnd).as_bool(), l, t, r, bt) {
                (false, true, true, _, _) => HTTOPLEFT,
                (false, _, true, true, _) => HTTOPRIGHT,
                (false, true, _, _, true) => HTBOTTOMLEFT,
                (false, _, _, true, true) => HTBOTTOMRIGHT,
                (false, true, ..) => HTLEFT,
                (false, _, true, ..) => HTTOP,
                (false, _, _, true, _) => HTRIGHT,
                (false, .., true) => HTBOTTOM,
                _ if pt.y < (CAPTION * s) as i32 && pt.x < rc.right - (138.0 * s) as i32 => HTCAPTION,
                _ => HTCLIENT,
            };
            return LRESULT(code as isize);
        }
        WM_GETMINMAXINFO => {
            let m = &mut *(lp.0 as *mut MINMAXINFO);
            m.ptMinTrackSize = POINT { x: (320.0 * s) as i32, y: (500.0 * s) as i32 };
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_SIZE => _ = InvalidateRect(Some(hwnd), None, false),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            ST.with(|st| {
                let Ok(mut st) = st.try_borrow_mut() else { return };
                let st = &mut *st;
                if st.gfx.is_none() {
                    st.gfx = make_gfx().ok();
                }
                if let Some(g) = &st.gfx {
                    if g.rt.BindDC(dc, &rc).is_ok() {
                        g.rt.SetDpi(96.0 * s, 96.0 * s);
                        g.rt.BeginDraw();
                        let _ = draw(st, g, rc.right as f32 / s, rc.bottom as f32 / s, IsZoomed(hwnd).as_bool());
                        if g.rt.EndDraw(None, None).is_err() {
                            st.gfx = None; // device lost: rebuilt on the next paint
                        }
                    }
                }
            });
            let _ = EndPaint(hwnd, &ps);
        }
        WM_MOUSEMOVE => {
            let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            let h = at(lp);
            if ST.with(|st| std::mem::replace(&mut st.borrow_mut().hover, h) != h) {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
        WM_MOUSELEAVE => {
            ST.with(|st| {
                let st = &mut *st.borrow_mut();
                (st.hover, st.down) = (None, None);
            });
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        WM_LBUTTONDOWN => {
            let h = at(lp);
            ST.with(|st| st.borrow_mut().down = h);
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        WM_LBUTTONUP => {
            let h = at(lp);
            // a click counts when it is released on the button it went down on
            if let Some(h) = ST.with(|st| st.borrow_mut().down.take()).filter(|d| Some(*d) == h) {
                press(hwnd, h);
            }
        }
        WM_CHAR => {
            let sci = ST.with(|st| st.borrow().calc.sci);
            let k = match (char::from_u32(wp.0 as u32).unwrap_or(' '), sci) {
                (c @ ('0'..='9' | '.' | '+' | '-' | '*' | '/' | '='), _) => c,
                ('%', false) => '%',
                ('\r', _) => '=',
                ('\u{8}', _) => 'B',
                ('\u{1B}', _) => 'C',
                (',', _) => '.',
                ('r' | 'R', _) => 'R',
                ('q' | 'Q', _) => 'S',
                ('@', _) => 'Q',
                // Scientific's own shortcuts, as in Windows' Calculator
                (c @ ('(' | ')' | '^'), true) => c,
                ('%', true) => 'd',
                ('!', true) => 'F',
                ('p', true) => 'P',
                ('e', true) => 'X',
                ('l', true) => 'L',
                ('n', true) => 'l',
                ('s', true) => 'i',
                ('o', true) => 'o',
                ('t', true) => 'a',
                _ => return LRESULT(0),
            };
            key(k);
        }
        WM_KEYDOWN => match VIRTUAL_KEY(wp.0 as u16) {
            VK_DELETE => key('E'),
            VK_F9 => key('N'),
            _ => {}
        },
        // Alt+1 Standard, Alt+2 Scientific
        WM_SYSKEYDOWN if matches!(wp.0, 0x31 | 0x32) => {
            let angle = ST.with(|st| {
                let st = &mut *st.borrow_mut();
                set_mode(st, wp.0 == 0x32);
                st.calc.angle
            });
            save_prefs(wp.0 == 0x32, angle);
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        WM_SYSCHAR if matches!(wp.0, 0x31 | 0x32) => {} // no beep for the two shortcuts above
        WM_DESTROY => {
            WIN.store(0, Relaxed);
            ST.with(|st| *st.borrow_mut() = State::default()); // a closed calculator keeps nothing in memory
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

/// Open the calculator (or bring the open one to the front). The window is built on demand and freed on close.
pub unsafe fn show(inst: HINSTANCE) {
    let mut h = HWND(WIN.load(Relaxed) as *mut c_void);
    if h.is_invalid() {
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: inst, lpszClassName: w!("shellbits_calc"), hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
        RegisterClassW(&wc);
        let Ok(w_) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("shellbits_calc"), w!("Calculator"), WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT, 1202, 934, None, None, Some(inst), None) else { return };
        h = w_;
        // open in the mode it was last used in
        let (sci, angle) = parse_prefs(&std::fs::read_to_string(prefs_file()).unwrap_or_default());
        ST.with(|st| {
            let st = &mut *st.borrow_mut();
            (st.calc.sci, st.calc.angle) = (sci, angle);
        });
        WIN.store(h.0 as isize, Relaxed);
        let dark = 1u32;
        let _ = DwmSetWindowAttribute(h, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const c_void, 4);
        let _ = DwmExtendFrameIntoClientArea(h, &MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 1, cyBottomHeight: 0 });
        let _ = SetWindowPos(h, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER);
    }
    let _ = ShowWindow(h, if IsIconic(h).as_bool() { SW_RESTORE } else { SW_SHOW });
    crate::flyout::force_foreground(h);
}

/// `shellbits --snapshot-calc out.bmp [keys]`: the window at Windows' own size, after typing `keys`.
/// Snapshot-only keys: 'M' opens the Memory tab, 'H' recalls the newest history row, 'Z' switches to
/// Scientific, 'V' turns 2nd on, 'K' clears the history.
pub unsafe fn snapshot(path: &str, keys: &str) -> windows::core::Result<()> {
    let (w, h) = (1202, 934);
    let mut st = State { gfx: Some(make_gfx()?), ..Default::default() };
    for k in keys.chars() {
        match k {
            'M' => st.tab = 1,
            'H' => st.calc.recall(0),
            'Z' => set_mode(&mut st, true),
            'V' => st.second = true,
            'K' => st.calc.history.clear(),
            k => st.calc.key(k),
        }
    }
    let bi = BITMAPINFO { bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, ..Default::default() }, ..Default::default() };
    let mut bits = std::ptr::null_mut();
    let dib = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0)?;
    let dc = CreateCompatibleDC(None);
    let old = SelectObject(dc, dib.into());
    let g = st.gfx.as_ref().unwrap();
    g.rt.BindDC(dc, &RECT { left: 0, top: 0, right: w, bottom: h })?;
    g.rt.BeginDraw();
    draw(&st, g, w as f32, h as f32, false)?;
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
    px.chunks(4).for_each(|p| out.extend_from_slice(&[p[0], p[1], p[2], 255]));
    let _ = std::fs::write(path, out);
    SelectObject(dc, old);
    let _ = DeleteDC(dc);
    let _ = DeleteObject(dib.into());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run_in(sci: bool, keys: &str) -> String {
        let mut c = Calc { sci, ..Default::default() };
        keys.chars().for_each(|k| c.key(k));
        c.display()
    }
    fn run(keys: &str) -> String {
        run_in(false, keys)
    }
    #[test]
    fn arithmetic() {
        assert_eq!(run(""), "0");
        assert_eq!(run("12+3="), "15");
        assert_eq!(run("2+3*4="), "20"); // standard mode works left to right
        assert_eq!(run("0.1+0.2="), "0.3");
        assert_eq!(run("1234567*1000="), "1,234,567,000");
        assert_eq!(run("5-8="), "-3");
        assert_eq!(run("2+3=="), "8"); // = again repeats the last step
        assert_eq!(run("9+*2="), "18"); // a second operator replaces the first
        assert_eq!(run("5+9Q+"), "8"); // ...but not after a function key
        assert_eq!(run("7/0="), "Cannot divide by zero");
        assert_eq!(run("7/0=5"), "5"); // the next digit starts over
        assert_eq!(run("9Q"), "3");
        assert_eq!(run("4N Q"), "Invalid input");
        assert_eq!(run("4R"), "0.25");
        assert_eq!(run("12S"), "144");
        assert_eq!(run("200+10%="), "220");
        assert_eq!(run("123B"), "12");
        assert_eq!(run("5N"), "-5");
        assert_eq!(run("5+3E2="), "7");
        assert_eq!(run("5+3C"), "0");
        assert_eq!(run("..5.1"), "0.51");
        assert_eq!(run("7tC3pCr"), "10"); // store 7, add 3, recall
        assert_eq!(run("12345678901234567890"), "1,234,567,890,123,456"); // 16 digits at most
    }
    #[test]
    fn scientific() {
        let sci = |k| run_in(true, k);
        assert_eq!(sci("2+3*4="), "14"); // × before +
        assert_eq!(sci("2*3+4="), "10");
        assert_eq!(sci("2+3*4^2="), "50"); // powers first
        assert_eq!(sci("(2+3)*4="), "20");
        assert_eq!(sci("2*(3+4"), "4"); // still typing inside the bracket
        assert_eq!(sci("2*(3+4)"), "7");
        assert_eq!(sci("2*(3+4="), "14"); // = closes what is open
        assert_eq!(sci("10-2-3="), "5"); // left to right among equals
        assert_eq!(sci("5F"), "120");
        assert_eq!(sci("171F"), "Overflow");
        assert_eq!(sci("2.5F"), "Invalid input");
        assert_eq!(sci("3T"), "1,000");
        assert_eq!(sci("1000L"), "3");
        assert_eq!(sci("Xl"), "1");
        assert_eq!(sci("0L"), "Invalid input");
        assert_eq!(sci("P"), "3.14159265358979");
        assert_eq!(sci("17d5="), "2");
        assert_eq!(sci("27y3="), "3");
        assert_eq!(sci("8g2="), "3");
        assert_eq!(sci("3x2="), "300");
        assert_eq!(sci("2c"), "8");
        assert_eq!(sci("27b"), "3");
        assert_eq!(sci("5NA"), "5");
        assert_eq!(sci("2.5f"), "2");
        assert_eq!(sci("2.5k"), "3");
        // angles: degrees first, D steps to radians, then gradians
        assert_eq!(sci("30i"), "0.5");
        assert_eq!(sci("180i"), "0");
        assert_eq!(sci("60o"), "0.5");
        assert_eq!(sci("45a"), "1");
        assert_eq!(sci("90a"), "Invalid input");
        assert_eq!(sci("1I"), "90");
        assert_eq!(sci("DPo"), "-1");
        assert_eq!(sci("DD100i"), "1");
    }
    #[test]
    fn expression_line_and_history() {
        let mut c = Calc::default();
        "12*3".chars().for_each(|k| c.key(k));
        assert_eq!(c.expr, "12 ×");
        c.key('=');
        assert_eq!(c.expr, "12 × 3 =");
        assert_eq!(c.history, vec![("12 × 3 =".to_string(), "36".to_string())]);
        // a history row comes back as a number you can keep calculating with
        "1000*5=C".chars().for_each(|k| c.key(k));
        c.recall(0);
        assert_eq!(c.display(), "5,000");
        "+1=".chars().for_each(|k| c.key(k));
        assert_eq!(c.display(), "5,001");
        c.recall(2);
        assert_eq!(c.display(), "36");
        let mut c = Calc { sci: true, ..Default::default() };
        "2+(3*4".chars().for_each(|k| c.key(k));
        assert_eq!(c.expr, "2 + ( 3 ×");
    }
    #[test]
    fn layout_matches_the_measured_window() {
        // button boxes measured on Windows' Calculator at 1202x934
        let r = key_rect(0, false, 1202.0, 934.0);
        assert_eq!((r.left, r.top, r.right, r.bottom), (5.0, 329.0, 222.0, 427.0));
        let r = key_rect(23, false, 1202.0, 934.0);
        assert_eq!((r.left, r.top), (661.0, 831.0));
        assert_eq!([4, 8, 12, 16].map(|i| key_rect(i, false, 1202.0, 934.0).top), [429.0, 529.0, 630.0, 730.0]);
        // Scientific: five columns from 5, seven rows from 462
        assert_eq!([0, 1, 2, 3, 4].map(|i| key_rect(i, true, 1202.0, 934.0).left), [5.0, 180.0, 355.0, 530.0, 705.0]);
        assert_eq!([0, 5, 10, 15, 20, 25, 30].map(|i| key_rect(i, true, 1202.0, 934.0).top), [462.0, 529.0, 596.0, 663.0, 730.0, 797.0, 864.0]);
        assert_eq!(hit(100.0, 580.0, false, 1202.0, 934.0), Some(Hit::Key(8))); // "7"
        assert_eq!(hit(265.0, 700.0, true, 1202.0, 934.0), Some(Hit::Key(16))); // Scientific's "7"
        assert_eq!(hit(44.0, 298.0, true, 1202.0, 934.0), Some(Hit::Angle));
        assert_eq!(hit(80.0, 430.0, true, 1202.0, 934.0), Some(Hit::Trig));
        assert_eq!(hit(230.0, 430.0, true, 1202.0, 934.0), Some(Hit::Func));
        assert_eq!(hit(1180.0, 10.0, false, 1202.0, 934.0), Some(Hit::Close));
        assert_eq!(hit(25.0, 57.0, false, 1202.0, 934.0), Some(Hit::Menu));
        assert_eq!(hit(1000.0, 500.0, false, 1202.0, 934.0), Some(Hit::Row(5))); // the side panel's list
        assert_eq!(hit(915.0, 56.0, false, 1202.0, 934.0), Some(Hit::Tab(0)));
        assert_eq!(hit(985.0, 56.0, false, 1202.0, 934.0), Some(Hit::Tab(1)));
        assert_eq!(hit(157.0, 57.0, false, 1202.0, 934.0), Some(Hit::Pin));
        assert_eq!(main_w(600.0), 600.0); // too narrow for the panel
    }
    #[test]
    fn second_switch_swaps_the_first_column() {
        let mut st = State::default();
        st.calc.sci = true;
        assert_eq!(st.key(5), ("x²", 'S'));
        st.second = true;
        assert_eq!(st.key(5), ("x³", 'c'));
        assert_eq!(st.key(30), ("eˣ", 'z'));
        assert_eq!(st.key(0).1, '\u{2}'); // the switch itself stays
        assert_eq!(st.key(6), ("¹⁄x", 'R'));
        // "C" reads "CE" while a number is being typed, and "C" again once it is a result
        assert_eq!(st.key(3), ("C", 'C'));
        st.calc.key('5');
        assert_eq!(st.key(3), ("CE", 'E'));
        st.calc.key('S');
        assert_eq!(st.key(3), ("C", 'C'));
    }
    #[test]
    fn preferences_and_bin() {
        assert_eq!(parse_prefs("sci=1 angle=2"), (true, 2));
        assert_eq!(parse_prefs("sci=0 angle=1"), (false, 1));
        assert_eq!(parse_prefs(""), (false, 0));
        assert_eq!(parse_prefs("sci=1 angle=9"), (true, 2));
        assert_eq!(hit(1202.0 - 28.0, 934.0 - 28.0, false, 1202.0, 934.0), Some(Hit::Clear));
        assert_eq!(hit(1202.0 - 60.0, 934.0 - 28.0, false, 1202.0, 934.0), Some(Hit::Row(11)));
    }
}

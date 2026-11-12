//! Claude Code as a native Rust window (Ctrl+Alt+C / `shellbits --claude`), laid out like the
//! Claude desktop app's Code tab (measured from it at 100%): custom 36px title bar, 266px session
//! sidebar (#1F1F1F) with your real sessions grouped by folder, centered 733px conversation column on
//! #191919, #252525 user bubbles, collapsed grey tool summaries, the #303030 composer with model /
//! effort / permission pickers under it, and the Environment / Context panel on the right.
//! It drives the real Claude Code CLI (`claude -p` stream-json, kept alive per session), including
//! permission prompts (Allow / Deny), stop, resume, and switching model, effort or permission mode.

use std::cell::RefCell;
use std::ffi::c_void;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering::Relaxed};
use std::sync::Mutex;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::flyout::{glyph, rect, rrect, utf16};
use crate::quick::rgb;

pub static WIN: AtomicIsize = AtomicIsize::new(0);
static EDIT: AtomicIsize = AtomicIsize::new(0);
const WM_CC_DATA: u32 = WM_APP + 100;
const T_ANIM: usize = 7;

// measured from the desktop app (DIPs)
const TOP: f32 = 36.0;
const SIDE: f32 = 266.0;
const RIGHT: f32 = 320.0;
const COL: f32 = 733.0;
const COMPOSER_H: f32 = 44.0;
const BOTTOM: f32 = 84.0; // composer + model row
const C_MAIN: u32 = 0x191919;
const C_SIDE: u32 = 0x1F1F1F;
const C_LINE: u32 = 0x303030;
const C_TOPLINE: u32 = 0x353535;
const C_BUBBLE: u32 = 0x252525;
const C_SEL: u32 = 0x404040;
const C_HOVER: u32 = 0x2A2A2A;
const C_DIM: u32 = 0x9B9B9B;
const C_ACCENT: u32 = 0xD97757;
const C_BLUE: u32 = 0x257DE0;

// ======================= model =======================

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    User,
    Assistant,
    Tool,
    ToolResult,
    Info,
    Permission, // waiting for Allow / Deny
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub kind: Kind,
    pub text: String,
    pub tool: String,       // tool name for Tool entries
    pub request_id: String, // Permission
    pub input: String,      // Permission: original tool input (JSON)
    pub images: Vec<String>, // User: pictures sent with the message (base64), shown as thumbnails
}

impl Entry {
    fn new(kind: Kind, text: impl Into<String>) -> Self {
        Entry { kind, text: text.into(), tool: String::new(), request_id: String::new(), input: String::new(), images: Vec::new() }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub modified: u64,
}

struct Proc {
    stdin: std::process::ChildStdin,
    child: std::process::Child,
}

#[derive(Clone)]
struct Settings {
    folder: String,
    session: Option<String>, // resume this id on the next start
    model: String,           // "" = the CLI's default
    effort: String,
    mode: String,
}

static LOG: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
static LIVE: Mutex<Option<usize>> = Mutex::new(None);
static PROC: Mutex<Option<Proc>> = Mutex::new(None);
static SET: Mutex<Option<Settings>> = Mutex::new(None);
static SESSIONS: Mutex<Vec<SessionInfo>> = Mutex::new(Vec::new());
static MODEL_SHOWN: Mutex<String> = Mutex::new(String::new());
static CONTEXT: Mutex<Option<f32>> = Mutex::new(None);
static CONTEXT_TOKENS: Mutex<Option<(f64, f64)>> = Mutex::new(None); // tokens in use, window size
static WINDOW: Mutex<Option<f64>> = Mutex::new(None); // the model's context window, once the CLI has reported it
static BUSY: AtomicBool = AtomicBool::new(false);
static BUSY_SINCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0); // tick count when the reply was asked for

/// The status line under a reply being written: "Working... 12s", minutes once past a minute.
pub fn status_line(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 { format!("Working… {s}s") } else { format!("Working… {}m {}s", s / 60, s % 60) }
}
static IDLE_SINCE: Mutex<Option<std::time::Instant>> = Mutex::new(None); // when the session last did anything

/// Environment variables that belong to a Claude Code session we were started from: all CLAUDE* and
/// ANTHROPIC* ones, but only when the CLAUDECODE marker says we are inside one (your own settings,
/// such as an API key set for the whole PC, are otherwise left alone).
pub fn inherited_session_vars(names: &[String]) -> Vec<&str> {
    if !names.iter().any(|n| n.eq_ignore_ascii_case("CLAUDECODE")) {
        return Vec::new();
    }
    names.iter().map(String::as_str).filter(|n| { let u = n.to_ascii_uppercase(); u.starts_with("CLAUDE") || u.starts_with("ANTHROPIC") }).collect()
}

/// Is the `claude` tool itself signed in to a Claude account (its own stored sign-in, the one
/// `claude auth login` makes)? Only then can a session run on the user's Claude plan.
pub fn plan_signed_in(credentials_json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(credentials_json).ok().is_some_and(|v| v["claudeAiOauth"]["accessToken"].as_str().is_some_and(|t| !t.is_empty()))
}

/// Whether to stop the CLI helper: only one that exists, is not working, and has been idle past the limit.
pub fn release_helper(busy: bool, has_helper: bool, idle_secs: u64, limit_secs: u64) -> bool {
    has_helper && !busy && idle_secs >= limit_secs
}

/// Idle time after which the helper is let go: 10 minutes (SHELLBITS_CLAUDE_IDLE_SECS overrides, for tests).
fn idle_limit() -> u64 {
    std::env::var("SHELLBITS_CLAUDE_IDLE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(600)
}
static TODOS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
/// Messages typed with Ctrl+Enter while a reply was being written: sent, in order, once it ends.
static QUEUE: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Height of the task card pinned above the message box: none once every task is done.
pub fn task_card_h(todos: &[(String, String)]) -> f32 {
    if todos.is_empty() || todos.iter().all(|t| t.1 == "completed") { 0.0 } else { 16.0 + todos.len().min(8) as f32 * 28.0 }
}

/// Tokens left before Claude Code compacts on its own (it keeps a buffer of ~3.3% of the window).
pub fn until_compact(used: f64, window: f64) -> f64 {
    (window * (1.0 - 0.033) - used).max(0.0)
}

fn settings() -> Settings {
    SET.lock().unwrap().clone().unwrap_or_else(|| Settings {
        folder: std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".into()),
        session: None,
        model: String::new(),
        effort: "medium".into(),
        mode: "default".into(),
    })
}

fn set_settings(f: impl FnOnce(&mut Settings)) {
    let mut s = settings();
    f(&mut s);
    *SET.lock().unwrap() = Some(s);
}

fn claude_exe() -> String {
    let local = format!("{}\\.local\\bin\\claude.exe", std::env::var("USERPROFILE").unwrap_or_default());
    if std::path::Path::new(&local).exists() { local } else { "claude.exe".into() }
}

fn notify() {
    unsafe {
        let _ = PostMessageW(Some(HWND(WIN.load(Relaxed) as *mut c_void)), WM_CC_DATA, WPARAM(0), LPARAM(0));
    }
}

/// One tool call as the app summarises it ("ran a command", "read a file", ...).
/// Percent of the context window one request filled, from its usage block.
/// ponytail: until the CLI has reported the real window, it is guessed - 1M for "[1m]" models or
/// when more than 200k is already in use, else 200k; a 1M session under 200k reads too high until then.
#[cfg(test)]
pub fn context_percent(usage: &serde_json::Value, model: &str, known_window: Option<f64>) -> Option<f32> {
    context_tokens(usage, model, known_window).map(|(used, window)| (used / window * 100.0).min(100.0) as f32)
}

/// (tokens in use, window size) for one request's usage block - see `context_percent` for the window guess.
pub fn context_tokens(usage: &serde_json::Value, model: &str, known_window: Option<f64>) -> Option<(f64, f64)> {
    let used: f64 = ["input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens", "output_tokens"].iter().filter_map(|k| usage[*k].as_f64()).sum();
    if used <= 0.0 {
        return None;
    }
    Some((used, known_window.unwrap_or(if model.contains("[1m]") || used > 200_000.0 { 1_000_000.0 } else { 200_000.0 })))
}

/// Which docked pane is open in place of the right-hand panel.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Pane {
    Files,
    Diff,
    Terminal,
    Preview,
}
const PANE_W: f32 = 520.0;

/// Terminal pane: a real cmd.exe on a pseudo console (crate::term), started in the session folder
/// the first time the pane is used, and the folder it was started in.
static CONSOLE: Mutex<Option<crate::term::Console>> = Mutex::new(None);
static TERM_DIR: Mutex<String> = Mutex::new(String::new());
static TEDIT: AtomicIsize = AtomicIsize::new(0);
/// Preview pane: local servers that are listening, as (port, program).
static SERVERS: Mutex<Vec<(u16, String)>> = Mutex::new(Vec::new());

/// Send keys to the console, starting it first if it is not running.
fn term_send(keys: &str) {
    let mut c = CONSOLE.lock().unwrap();
    if !c.as_ref().is_some_and(|c| c.alive()) {
        let dir = TERM_DIR.lock().unwrap().clone();
        // as wide as the pane holds at 13 px Cascadia Mono (~7.9 px a character)
        // ponytail: set once at start; ResizePseudoConsole on window resize if it matters
        *c = crate::term::start(&dir, ((PANE_W - 28.0) / 7.9) as usize, 30, notify).ok();
    }
    if let Some(c) = c.as_ref() {
        c.write(keys);
    }
}

fn term_lines() -> Vec<String> {
    CONSOLE.lock().unwrap().as_ref().map(|c| c.screen.lock().unwrap().lines()).unwrap_or_default()
}

unsafe extern "system" fn term_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_KEYDOWN && wp.0 == VK_RETURN.0 as usize {
        let cmd = edit_text(hwnd);
        let _ = SetWindowTextW(hwnd, w!(""));
        {
            UI.with(|u| if let Ok(mut u) = u.try_borrow_mut() { u.pane_scroll = f32::MAX; });
        }
        term_send(&format!("{cmd}\r")); // an empty line too, like pressing Enter in a console
        return LRESULT(0);
    }
    if msg == WM_KEYDOWN && wp.0 == 'C' as usize && GetKeyState(VK_CONTROL.0 as i32) < 0 && SendMessageW(hwnd, EM_GETSEL, None, None).0 & 0xFFFF == (SendMessageW(hwnd, EM_GETSEL, None, None).0 >> 16) & 0xFFFF {
        term_send("\x03"); // Ctrl+C with nothing selected stops the running program
        return LRESULT(0);
    }
    if msg == WM_CHAR && wp.0 == 13 {
        return LRESULT(0);
    }
    let prev: WNDPROC = std::mem::transmute(TERM_PROC.load(Relaxed));
    CallWindowProcW(prev, hwnd, msg, wp, lp)
}
static TERM_PROC: AtomicIsize = AtomicIsize::new(0);

/// Listening TCP ports from `netstat -ano -p tcp` that a browser could open (1024 and up), with the
/// process id, each port once.
pub fn listening(netstat: &str) -> Vec<(u16, u32)> {
    let mut v: Vec<(u16, u32)> = Vec::new();
    for l in netstat.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() == 5 && f[0] == "TCP" && f[3] == "LISTENING" {
            let (addr, pid) = (f[1], f[4].parse().unwrap_or(0));
            let local = addr.starts_with("127.0.0.1:") || addr.starts_with("0.0.0.0:") || addr.starts_with("[::1]:") || addr.starts_with("[::]:");
            if let Some(port) = addr.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) {
                if local && port >= 1024 && !v.iter().any(|x| x.0 == port) {
                    v.push((port, pid));
                }
            }
        }
    }
    v.sort();
    v
}

fn load_servers() {
    *SERVERS.lock().unwrap() = Vec::new();
    std::thread::spawn(|| {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("netstat").args(["-ano", "-p", "tcp"]).creation_flags(0x0800_0000).output();
        let ports = listening(&out.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default());
        let names = std::process::Command::new("tasklist").args(["/fo", "csv", "/nh"]).creation_flags(0x0800_0000).output().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
        let name_of = |pid: u32| names.lines().find_map(|l| {
            let f: Vec<&str> = l.split("\",\"").collect();
            (f.get(1).map(|x| x.trim_matches('"')) == Some(&pid.to_string())).then(|| f[0].trim_matches('"').to_string())
        }).unwrap_or_default();
        // Windows' own listeners (RPC, printing, sign-in) are not something to preview
        const SYSTEM: [&str; 7] = ["", "System", "svchost.exe", "lsass.exe", "wininit.exe", "services.exe", "spoolsv.exe"];
        *SERVERS.lock().unwrap() = ports.into_iter().filter(|(p, _)| *p < 49152).map(|(p, pid)| (p, name_of(pid))).filter(|(_, n)| !SYSTEM.contains(&n.as_str())).collect();
        notify();
    });
}

/// The lines of the Diff pane (git's output), or why there are none.
static DIFF: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Colour of one line of a diff: additions green, removals red, hunk and file headers dimmed.
pub fn diff_colour(line: &str) -> u32 {
    if line.starts_with("+++") || line.starts_with("---") || line.starts_with("diff ") || line.starts_with("index ") {
        0x9B9B9B
    } else if line.starts_with("@@") {
        0x6CB4EE
    } else if line.starts_with('+') {
        0x6CCB8E
    } else if line.starts_with('-') {
        0xF0806F
    } else {
        0xD8D8D8
    }
}

/// A folder's entries for the Files pane: folders first, then files, each group by name without
/// regard to case; names starting with a dot stay (they are part of a project).
pub fn sorted_entries(mut entries: Vec<(String, bool)>) -> Vec<(String, bool)> {
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    entries
}

fn list_dir(dir: &str) -> Vec<(String, bool)> {
    sorted_entries(std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| (e.file_name().to_string_lossy().into_owned(), e.file_type().is_ok_and(|t| t.is_dir()))).collect())
}

/// Run `git diff` for the session's folder in the background and hand the lines to the Diff pane.
fn load_diff(folder: String) {
    *DIFF.lock().unwrap() = vec!["Reading changes\u{2026}".to_string()];
    std::thread::spawn(move || {
        use std::os::windows::process::CommandExt;
        let run = |args: &[&str]| std::process::Command::new("git").arg("-C").arg(&folder).args(args).creation_flags(0x0800_0000).output();
        let lines = match (run(&["diff", "--stat"]), run(&["diff"])) {
            (Ok(stat), Ok(diff)) if diff.status.success() => {
                let text = format!("{}\n{}", String::from_utf8_lossy(&stat.stdout), String::from_utf8_lossy(&diff.stdout));
                // ponytail: the first 2,000 lines; a bigger diff wants a real viewer
                let v: Vec<String> = text.lines().take(2000).map(|l| l.replace('\t', "    ")).collect();
                if v.iter().all(|l| l.trim().is_empty()) { vec!["No changes.".to_string()] } else { v }
            }
            (_, Ok(diff)) => {
                let err = String::from_utf8_lossy(&diff.stderr).to_string();
                // git's own wording here is a usage hint, not something to show
                if err.to_lowercase().contains("not a git repository") { vec!["Not a git repository: no changes to show.".to_string()] }
                else { vec![err.lines().next().unwrap_or("git could not read the changes.").to_string()] }
            }
            _ => vec!["git was not found on this PC.".to_string()],
        };
        *DIFF.lock().unwrap() = lines;
        notify();
    });
}

/// A token count as the app writes it: 273, 1.7k, 10k, 918.3k, 1M.
/// The Context panel's rows: memory files and skill listings (estimated from their text, about
/// 4 characters a token), the rest of what is in use (messages, tools, system prompt), free space.
pub fn breakdown(used: f64, window: f64, memory: f64, skills: f64) -> Vec<(u32, &'static str, f64)> {
    let memory = memory.min(used);
    let skills = skills.min(used - memory);
    vec![
        (0xC98A5A, "Memory files", memory),
        (0x8C6CD9, "Skills", skills),
        (C_BLUE, "Messages and system", used - memory - skills),
        (0x4A4A4A, "Free space", (window - used).max(0.0)),
    ]
}

/// (memory-file tokens, skill-listing tokens) for a session folder, cached per folder.
fn context_parts(folder: &str) -> (f64, f64) {
    static CACHE: Mutex<Option<(String, f64, f64)>> = Mutex::new(None);
    if let Some((f, m, k)) = CACHE.lock().unwrap().as_ref() {
        if f == folder {
            return (*m, *k);
        }
    }
    let home = std::path::PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join(".claude");
    let len = |p: std::path::PathBuf| std::fs::metadata(p).map_or(0, |m| m.len()) as f64;
    // CLAUDE.md in ~/.claude, the folder and each folder above it (Claude Code reads them all)
    let mut memory = len(home.join("CLAUDE.md"));
    let mut dir = Some(std::path::Path::new(folder));
    while let Some(d) = dir {
        memory += len(d.join("CLAUDE.md"));
        dir = d.parent();
    }
    // each skill is listed by its name and description line
    let skills: f64 = std::fs::read_dir(home.join("skills")).into_iter().flatten().flatten()
        .filter_map(|e| std::fs::read_to_string(e.path().join("SKILL.md")).ok())
        .map(|t| t.lines().filter(|l| l.starts_with("name:") || l.starts_with("description:")).map(|l| l.len() as f64).sum::<f64>())
        .sum();
    let r = (memory / 4.0, skills / 4.0);
    *CACHE.lock().unwrap() = Some((folder.to_string(), r.0, r.1));
    r
}

/// MCP servers Claude Code would start for a folder, from ~/.claude.json: the global ones and the
/// folder's own (by path, either slash), as (name, kind). Only names and kinds are read.
pub fn connectors(config: &serde_json::Value, folder: &str) -> Vec<(String, String)> {
    let kind = |v: &serde_json::Value| v.get("type").and_then(|t| t.as_str()).unwrap_or("stdio").to_string();
    let mut v: Vec<(String, String)> = Vec::new();
    let norm = |p: &str| p.replace('\\', "/").trim_end_matches('/').to_lowercase();
    let want = norm(folder);
    let project = config.get("projects").and_then(|p| p.as_object()).and_then(|p| p.iter().find(|(k, _)| norm(k) == want)).map(|(_, v)| v);
    for m in [config.get("mcpServers"), project.and_then(|p| p.get("mcpServers"))].into_iter().flatten() {
        if let Some(m) = m.as_object() {
            for (name, s) in m {
                if !v.iter().any(|x| &x.0 == name) {
                    v.push((name.clone(), kind(s)));
                }
            }
        }
    }
    v
}

fn folder_connectors(folder: &str) -> Vec<(String, String)> {
    let f = std::path::PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join(".claude.json");
    let cfg: serde_json::Value = std::fs::read_to_string(f).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    connectors(&cfg, folder)
}

pub fn tokens(n: f64) -> String {
    let short = |v: f64, unit: &str| {
        let t = format!("{v:.1}");
        format!("{}{unit}", t.strip_suffix(".0").unwrap_or(&t))
    };
    if n >= 999_950.0 {
        short(n / 1e6, "M")
    } else if n >= 1000.0 {
        short(n / 1e3, "k")
    } else {
        format!("{n:.0}")
    }
}

/// The files this session created (its Write calls; edits to existing files do not count - that is
/// what the real app lists), newest first, each once.
pub fn outputs(log: &[Entry]) -> Vec<String> {
    let mut seen = Vec::new();
    for e in log.iter().rev().filter(|e| e.kind == Kind::Tool && !e.input.is_empty() && e.tool == "Write") {
        if !seen.contains(&e.input) {
            seen.push(e.input.clone());
        }
    }
    seen
}

/// A model id as the app shows it: "claude-opus-5-5" -> "Opus 5.5", "claude-sonnet-4-5-20250929" ->
/// "Sonnet 4.5", a "[1m]" suffix -> " (1M)". None for ids that are not a Claude model.
pub fn model_name(id: &str) -> Option<String> {
    let (base, big) = id.strip_suffix("[1m]").map_or((id, false), |b| (b, true));
    let mut parts = base.strip_prefix("claude-")?.split('-');
    let family = parts.next().filter(|f| !f.is_empty() && f.chars().all(|c| c.is_ascii_alphabetic()))?;
    // version numbers follow; a trailing 8-digit release date is not part of the name
    let version: Vec<&str> = parts.filter(|p| p.len() < 8 && p.chars().all(|c| c.is_ascii_digit())).take(2).collect();
    let mut name = cap(family);
    if !version.is_empty() {
        name = format!("{name} {}", version.join("."));
    }
    Some(if big { format!("{name} (1M)") } else { name })
}

pub fn verb(tool: &str) -> &'static str {
    match tool {
        "Bash" | "PowerShell" => "ran a command",
        "Read" => "read a file",
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => "edited a file",
        "Grep" | "Glob" => "searched",
        "WebFetch" => "fetched a page",
        "WebSearch" => "searched the web",
        "Agent" | "Task" => "ran an agent",
        "TodoWrite" => "updated the plan",
        "Thinking" => "thought",
        _ => "used a tool",
    }
}

/// "Ran a command, read 2 files, edited a file": consecutive tool calls folded like the app does.
pub fn summary(tools: &[&str]) -> String {
    let mut parts: Vec<(&str, usize)> = Vec::new();
    for t in tools {
        let v = verb(t);
        match parts.iter_mut().find(|p| p.0 == v) {
            Some(p) => p.1 += 1,
            None => parts.push((v, 1)),
        }
    }
    let s = parts
        .iter()
        .map(|(v, n)| {
            if *n == 1 {
                return v.to_string();
            }
            // "read a file" x3 -> "read 3 files"
            let words: Vec<&str> = v.split(' ').collect();
            match words.iter().position(|x| *x == "a" || *x == "an") {
                Some(i) => {
                    let mut out: Vec<String> = words.iter().map(|x| x.to_string()).collect();
                    out[i] = n.to_string();
                    let last = out.len() - 1;
                    out[last].push('s');
                    out.join(" ")
                }
                None => format!("{v} ({n}x)"),
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    cap(&s)
}

/// One-line detail of a tool call ("Bash  git status").
pub fn tool_line(name: &str, input: &serde_json::Value) -> String {
    let key = ["command", "file_path", "pattern", "path", "url", "description", "query", "prompt"]
        .iter()
        .find_map(|k| input.get(*k).and_then(|v| v.as_str()))
        .unwrap_or("");
    let first: String = key.lines().next().unwrap_or("").chars().take(140).collect();
    format!("{name}  {first}")
}

fn text_of(c: &serde_json::Value) -> String {
    match c {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(a) => a.iter().filter_map(|x| x.get("text").and_then(|t| t.as_str())).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// Apply one stream-json line (live) or one session-file line (history) to the conversation.
pub fn apply(v: &serde_json::Value, log: &mut Vec<Entry>, live: &mut Option<usize>, history: bool) -> bool {
    match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        "system" if v["subtype"] == "init" => {
            if let Some(m) = v["model"].as_str().and_then(model_name) {
                *MODEL_SHOWN.lock().unwrap() = m;
            }
            if let Some(id) = v["session_id"].as_str() {
                let id = id.to_string();
                set_settings(|s| s.session = Some(id));
            }
            false
        }
        "stream_event" => {
            let ev = &v["event"];
            if ev["type"] == "content_block_delta" && ev["delta"]["type"] == "text_delta" {
                let t = ev["delta"]["text"].as_str().unwrap_or("");
                match *live {
                    Some(i) if i < log.len() => log[i].text.push_str(t),
                    _ => {
                        log.push(Entry::new(Kind::Assistant, t));
                        *live = Some(log.len() - 1);
                    }
                }
                return true;
            }
            false
        }
        "assistant" => {
            // how full the context is: what the latest request sent in (its prompt, cached or not) plus its reply
            if let Some((used, window)) = context_tokens(&v["message"]["usage"], v["message"]["model"].as_str().unwrap_or(""), *WINDOW.lock().unwrap()) {
                *CONTEXT.lock().unwrap() = Some((used / window * 100.0).min(100.0) as f32);
                *CONTEXT_TOKENS.lock().unwrap() = Some((used, window));
            }
            // the model that wrote this reply (so a session opened from history shows its model too)
            if let Some(m) = v["message"]["model"].as_str().and_then(model_name) {
                *MODEL_SHOWN.lock().unwrap() = m;
            }
            let mut changed = false;
            for b in v["message"]["content"].as_array().into_iter().flatten() {
                match b["type"].as_str() {
                    Some("text") => {
                        let t = b["text"].as_str().unwrap_or("").to_string();
                        if t.trim().is_empty() {
                            continue;
                        }
                        match live.take() {
                            Some(i) if i < log.len() => log[i].text = t,
                            _ => log.push(Entry::new(Kind::Assistant, t)),
                        }
                        changed = true;
                    }
                    Some("thinking") => {
                        // shown folded like the app's "Thought" step; the text when the model gives it
                        *live = None;
                        let t = b["thinking"].as_str().unwrap_or("").trim();
                        let mut e = Entry::new(Kind::Tool, if t.is_empty() { "Thought".to_string() } else { t.to_string() });
                        e.tool = "Thinking".into();
                        log.push(e);
                        changed = true;
                    }
                    Some("tool_use") => {
                        *live = None;
                        let name = b["name"].as_str().unwrap_or("Tool");
                        if name == "TodoWrite" {
                            // the plan Claude keeps: shown as a checklist in the right panel, like the app
                            *TODOS.lock().unwrap() = b["input"]["todos"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|t| (t["content"].as_str().unwrap_or("").to_string(), t["status"].as_str().unwrap_or("pending").to_string()))
                                .collect();
                        }
                        let mut e = Entry::new(Kind::Tool, tool_line(name, &b["input"]));
                        e.tool = name.to_string();
                        if matches!(name, "Write" | "Edit" | "MultiEdit" | "NotebookEdit") {
                            // the file it wrote, for the Outputs list
                            e.input = b["input"]["file_path"].as_str().or(b["input"]["notebook_path"].as_str()).unwrap_or("").to_string();
                        }
                        log.push(e);
                        changed = true;
                    }
                    _ => {}
                }
            }
            changed
        }
        "user" => {
            if v["isMeta"] == true {
                return false;
            }
            let content = &v["message"]["content"];
            if history {
                if let Some(s) = content.as_str() {
                    // the prompts you typed (skip command echoes / caveats)
                    if !s.starts_with('<') && !s.trim().is_empty() {
                        log.push(Entry::new(Kind::User, s));
                        return true;
                    }
                    return false;
                }
            }
            let mut changed = false;
            let pics: Vec<String> = if history { content.as_array().into_iter().flatten().filter(|b| b["type"] == "image").filter_map(|b| b["source"]["data"].as_str().map(String::from)).collect() } else { Vec::new() };
            let before = log.len();
            for b in content.as_array().into_iter().flatten() {
                if b["type"] == "tool_result" {
                    let t = text_of(&b["content"]);
                    let lines: Vec<&str> = t.lines().collect();
                    let mut s = lines.iter().take(4).copied().collect::<Vec<_>>().join("\n");
                    if lines.len() > 4 {
                        s.push_str(&format!("\n\u{2026} {} more lines", lines.len() - 4));
                    }
                    log.push(Entry::new(Kind::ToolResult, s));
                    changed = true;
                } else if history && b["type"] == "text" {
                    let t = b["text"].as_str().unwrap_or("");
                    if !t.starts_with('<') && !t.trim().is_empty() {
                        log.push(Entry::new(Kind::User, t));
                        changed = true;
                    }
                }
            }
            if !pics.is_empty() {
                // the pictures belong to the message they came with (or stand alone)
                match log[before..].iter_mut().rev().find(|e| e.kind == Kind::User) {
                    Some(e) => e.images = pics,
                    None => {
                        let mut e = Entry::new(Kind::User, "");
                        e.images = pics;
                        log.push(e);
                    }
                }
                changed = true;
            }
            changed
        }
        "control_request" if v["request"]["subtype"] == "can_use_tool" => {
            let tool = v["request"]["tool_name"].as_str().unwrap_or("Tool");
            let input = &v["request"]["input"];
            let mut e = Entry::new(Kind::Permission, format!("Allow Claude to use {}?\n{}", tool, tool_line(tool, input)));
            e.tool = tool.to_string();
            e.request_id = v["request_id"].as_str().unwrap_or("").to_string();
            e.input = input.to_string();
            log.push(e);
            true
        }
        "result" => {
            *live = None;
            // the CLI reports the model's real context window here: remember it for the percentage
            // (the turn's "usage" is a total over all its requests, so it is not used for that)
            if let Some(w) = v["modelUsage"].as_object().and_then(|m| m.values().filter_map(|x| x["contextWindow"].as_f64()).fold(None, |a: Option<f64>, b| Some(a.map_or(b, |a| a.max(b))))) {
                *WINDOW.lock().unwrap() = Some(w);
            }
            if v["is_error"] == true {
                log.push(Entry::new(Kind::Info, v["result"].as_str().unwrap_or("Something went wrong.")));
            }
            true
        }
        _ => false,
    }
}

// ======================= sessions on disk =======================

fn projects_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join(".claude").join("projects")
}

/// Every session file: title (custom or first prompt), folder, last change. Newest first.
/// A fallback name for a session without a title: the first line of what you typed (60 characters at
/// most). The message may be plain text or a list of blocks; text that is not yours - tool results,
/// system notes in <tags> - is skipped.
pub fn prompt_title(content: &serde_json::Value) -> Option<String> {
    let texts: Vec<&str> = match content {
        serde_json::Value::String(s) => vec![s.as_str()],
        serde_json::Value::Array(a) => a.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect(),
        _ => Vec::new(),
    };
    texts
        .into_iter()
        .flat_map(str::lines)
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('<'))
        .map(|l| l.chars().take(60).collect())
}

/// The newest title among session-file lines: a name you gave it beats an automatic one.
pub fn last_title(lines: &str) -> Option<String> {
    let find = |kind: &str, field: &str| {
        lines.lines().rev().filter(|l| l.contains(kind)).find_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()?[field].as_str().map(str::to_string))
    };
    find("\"custom-title\"", "customTitle").or_else(|| find("\"ai-title\"", "aiTitle"))
}

/// `last_title` over the last 64 KB of a session file.
fn tail_title(path: &std::path::Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(65_536))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    last_title(&String::from_utf8_lossy(&buf))
}

/// The folder named by the newest line in `text` that names one. A session started without a folder
/// and later moved into a project says so only in its later lines.
pub fn last_cwd(text: &str) -> Option<String> {
    text.lines().rev().filter(|l| l.contains("\"cwd\"")).find_map(|l| {
        let v: serde_json::Value = serde_json::from_str(l).ok()?;
        v["cwd"].as_str().filter(|c| !c.is_empty()).map(str::to_string)
    })
}

/// `last_cwd` over the last 64 KB of a session file.
fn tail_cwd(path: &std::path::Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(65_536))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    last_cwd(&String::from_utf8_lossy(&buf))
}

/// The line Claude Code itself writes when a session is renamed.
pub fn title_line(id: &str, title: &str) -> String {
    serde_json::json!({ "type": "custom-title", "customTitle": title, "sessionId": id }).to_string()
}

/// Rename a session by appending that line to its history file (nothing already there is touched).
pub fn rename_in(path: &std::path::Path, id: &str, title: &str) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut f = std::fs::OpenOptions::new().read(true).append(true).open(path)?;
    // the new line must start on its own line
    let mut last = [b'\n'];
    if f.metadata()?.len() > 0 {
        f.seek(SeekFrom::End(-1))?;
        f.read_exact(&mut last)?;
    }
    let lead = if last[0] == b'\n' { "" } else { "\n" };
    f.write_all(format!("{lead}{}\n", title_line(id, title)).as_bytes())
}

/// One session file read from disk: its folder, and its title (a name given later wins, else the
/// automatic one, else the first thing typed). None if the file names no folder.
fn read_session(p: &std::path::Path, id: &str, modified: u64) -> Option<SessionInfo> {
    let file = std::fs::File::open(p).ok()?;
    let mut info = SessionInfo { id: id.to_string(), modified, ..Default::default() };
    let mut first_prompt = String::new();
    for line in std::io::BufReader::new(file).lines().map_while(Result::ok).take(400) {
        if info.cwd.is_empty() && line.contains("\"cwd\"") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                info.cwd = v["cwd"].as_str().unwrap_or("").to_string();
            }
        }
        if line.contains("\"custom-title\"") || line.contains("\"ai-title\"") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                if let Some(t) = v["customTitle"].as_str().or(v["aiTitle"].as_str()) {
                    info.title = t.to_string();
                }
            }
        }
        if first_prompt.is_empty() && line.contains("\"type\":\"user\"") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                if v["isMeta"] != true {
                    first_prompt = prompt_title(&v["message"]["content"]).unwrap_or_default();
                }
            }
        }
    }
    // a later rename sits near the end of the file: the newest title there wins
    if let Some(t) = tail_title(p) {
        info.title = t;
    }
    // where the session is NOW: one that was moved into a project belongs in that project's group
    // (the first folder is often the app's own scratch folder, which put everything under "This computer")
    if let Some(c) = tail_cwd(p) {
        info.cwd = c;
    }
    if info.title.is_empty() {
        info.title = if first_prompt.is_empty() { "New session".into() } else { first_prompt };
    }
    (!info.cwd.is_empty()).then_some(info)
}

/// The session list as kept between runs: one line per session - modified time, id, folder, title.
pub fn sessions_to_text(list: &[SessionInfo]) -> String {
    let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    list.iter().map(|s| format!("{}\t{}\t{}\t{}", s.modified, s.id, clean(&s.cwd), clean(&s.title))).collect::<Vec<_>>().join("\n")
}

pub fn sessions_from_text(text: &str) -> Vec<SessionInfo> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(4, '\t').collect();
            let [modified, id, cwd, title] = f[..] else { return None };
            Some(SessionInfo { modified: modified.parse().ok()?, id: id.to_string(), cwd: cwd.to_string(), title: title.to_string() })
        })
        .collect()
}

fn sessions_cache() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join("shellbits").join("claude-sessions-2.txt")
}

/// Every session, newest first. Reading 250 session files takes half a second or more, so what was
/// learned last time is kept: a file whose modified time has not changed is not opened again.
/// What the real Claude desktop app records about one of its sessions (its own files, only ever read).
#[derive(Debug, Default, PartialEq)]
pub struct AppInfo {
    pub title: String,
    pub folder: String, // the project folder it is filed under ("" when it has none)
    pub starred: bool,  // pinned in the app's sidebar
    pub archived: bool,
    pub status: String, // completed / review_ready / blocked ... from the app's last summary
    pub last: u64,      // last activity, seconds
}

/// One of the app's `local_<id>.json` session records -> (the CLI session id it belongs to, its details).
pub fn parse_app_session(json: &str) -> Option<(String, AppInfo)> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let id = v["cliSessionId"].as_str().filter(|s| !s.is_empty())?.to_string();
    let text = |x: &serde_json::Value| x.as_str().unwrap_or("").to_string();
    // a project folder is one the user chose; the app's own scratch folders are "This computer"
    let folder = [&v["folderPlace"]["cwd"], &v["gitAnchorsFolderRealpath"]].into_iter().map(text).find(|f| !f.is_empty()).unwrap_or_default();
    Some((id, AppInfo {
        title: text(&v["title"]),
        folder,
        starred: v["isStarred"] == true,
        archived: v["isArchived"] == true,
        status: text(&v["postTurnSummary"]["status_category"]),
        last: v["lastActivityAt"].as_u64().unwrap_or(0) / 1000,
    }))
}

/// Every session the desktop app knows, by CLI session id. Empty when the app is not installed.
fn app_sessions() -> std::collections::HashMap<String, AppInfo> {
    let root = std::path::PathBuf::from(std::env::var("APPDATA").unwrap_or_default()).join("Claude").join("claude-code-sessions");
    let mut out = std::collections::HashMap::new();
    for a in std::fs::read_dir(root).into_iter().flatten().flatten() {
        for b in std::fs::read_dir(a.path()).into_iter().flatten().flatten() {
            for f in std::fs::read_dir(b.path()).into_iter().flatten().flatten() {
                let name = f.file_name().to_string_lossy().to_string();
                if name.starts_with("local_") && name.ends_with(".json") {
                    out.extend(std::fs::read_to_string(f.path()).ok().and_then(|t| parse_app_session(&t)));
                }
            }
        }
    }
    out
}

/// Each session's state as the app last summed it up (completed / review_ready / blocked), by id.
static STATUS: Mutex<Option<std::collections::HashMap<String, String>>> = Mutex::new(None);

/// The dot at the right of a sidebar row: waiting for you (blue), stuck (amber), or none.
pub fn status_dot(status: &str) -> Option<u32> {
    match status {
        "review_ready" => Some(0x4CC2FF),
        "blocked" => Some(0xF2B04D),
        _ => None,
    }
}

/// Sessions the app has starred: they show under Pinned, with the ones pinned in this window.
static STARRED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Make the list say what the real app's sidebar says: its titles, its project folders, its pins,
/// newest activity first, archived ones left out. Sessions the app does not know keep their own details
/// and come after.
pub fn overlay(mut list: Vec<SessionInfo>, app: &std::collections::HashMap<String, AppInfo>) -> (Vec<SessionInfo>, Vec<String>) {
    list.retain(|s| !app.get(&s.id).is_some_and(|a| a.archived));
    for s in &mut list {
        if let Some(a) = app.get(&s.id) {
            if !a.title.is_empty() {
                s.title = a.title.clone();
            }
            if !a.folder.is_empty() {
                s.cwd = a.folder.clone();
            }
            s.modified = s.modified.max(a.last);
        }
    }
    list.sort_by(|x, y| (app.contains_key(&y.id), y.modified).cmp(&(app.contains_key(&x.id), x.modified)));
    let starred = list.iter().filter(|s| app.get(&s.id).is_some_and(|a| a.starred)).map(|s| s.id.clone()).collect();
    (list, starred)
}

pub fn scan_sessions() -> Vec<SessionInfo> {
    let known: std::collections::HashMap<String, SessionInfo> = sessions_from_text(&std::fs::read_to_string(sessions_cache()).unwrap_or_default()).into_iter().map(|s| (s.id.clone(), s)).collect();
    let mut out = Vec::new();
    for d in std::fs::read_dir(projects_dir()).into_iter().flatten().flatten() {
        for f in std::fs::read_dir(d.path()).into_iter().flatten().flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let modified = f.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
            let id = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
            match known.get(&id).filter(|k| k.modified == modified) {
                Some(k) => out.push(k.clone()),
                None => out.extend(read_session(&p, &id, modified)),
            }
        }
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    let cache = sessions_cache();
    if let Some(dir) = cache.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(cache, sessions_to_text(&out));
    let app = app_sessions();
    *STATUS.lock().unwrap() = Some(app.iter().map(|(id, a)| (id.clone(), a.status.clone())).collect());
    let (out, starred) = overlay(out, &app);
    *STARRED.lock().unwrap() = starred;
    out
}

/// Where a session's history is kept, if it still exists.
fn session_file(id: &str) -> Option<std::path::PathBuf> {
    std::fs::read_dir(projects_dir()).into_iter().flatten().flatten().map(|d| d.path().join(format!("{id}.jsonl"))).find(|p| p.exists())
}

fn load_history(info: &SessionInfo) -> Vec<Entry> {
    let p = session_file(&info.id);
    let mut log = Vec::new();
    let mut live = None;
    if let Some(p) = p {
        if let Ok(f) = std::fs::File::open(p) {
            for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                    if v["isSidechain"] == true || v["type"] == "system" {
                        continue;
                    }
                    apply(&v, &mut log, &mut live, true);
                }
            }
        }
    }
    log
}

// ======================= the CLI process =======================

fn stop_proc() {
    if let Some(mut p) = PROC.lock().unwrap().take() {
        let _ = p.child.kill();
    }
    BUSY.store(false, Relaxed);
}

fn ensure_proc() -> bool {
    let mut g = PROC.lock().unwrap();
    if let Some(p) = g.as_mut() {
        if p.child.try_wait().ok().flatten().is_none() {
            return true;
        }
    }
    use std::os::windows::process::CommandExt;
    let s = settings();
    let mut args: Vec<String> = ["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--include-partial-messages", "--permission-prompts", "host"]
        .iter()
        .map(|x| x.to_string())
        .collect();
    args.extend(["--permission-mode".into(), s.mode.clone(), "--effort".into(), s.effort.clone()]);
    if !s.model.is_empty() {
        args.extend(["--model".into(), s.model.clone()]);
    }
    // resume the session - unless its history has been deleted meanwhile: then start a new one
    // (the CLI would just exit, and the message would vanish without a word)
    match s.session.as_ref().filter(|id| session_file(id).is_some()) {
        Some(id) => args.extend(["--resume".into(), id.clone()]),
        None if s.session.is_some() => set_settings(|s| s.session = None),
        None => {}
    }
    // Use the Claude plan when the tool is signed in to one: leave out the user settings file (on this
    // PC it redirects the tool to another provider with an API key) and any ANTHROPIC_* variables, so
    // the tool falls back to its own Claude sign-in. No key or token is read or copied by this window.
    // ponytail: this also drops the user file's permission rules and hooks for these sessions; if that
    // matters, pass a --settings file with everything but its env block instead
    let creds = std::path::PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join(".claude").join(".credentials.json");
    let plan = plan_signed_in(&std::fs::read_to_string(creds).unwrap_or_default());
    if plan {
        args.extend(["--setting-sources".into(), "project,local".into()]);
    }
    let mut cmd = std::process::Command::new(claude_exe());
    if plan {
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().to_ascii_uppercase().starts_with("ANTHROPIC_") {
                cmd.env_remove(k);
            }
        }
    }
    // started from inside another Claude Code session (a terminal it opened, a test): do not hand
    // that session's own variables to our helper - with them it exits without answering
    let names: Vec<String> = std::env::vars_os().map(|(k, _)| k.to_string_lossy().into_owned()).collect();
    for k in inherited_session_vars(&names) {
        cmd.env_remove(k);
    }
    let child = cmd
        .args(&args)
        .current_dir(&s.folder)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .creation_flags(0x0800_0000)
        .spawn();
    let Ok(mut child) = child else {
        LOG.lock().unwrap().push(Entry::new(Kind::Info, "Couldn't start Claude Code (claude.exe)."));
        return false;
    };
    let out = child.stdout.take().unwrap();
    let stdin = child.stdin.take().unwrap();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                let is_result = v["type"] == "result";
                let changed = apply(&v, &mut LOG.lock().unwrap(), &mut LIVE.lock().unwrap(), false);
                if is_result {
                    BUSY.store(false, Relaxed);
                    // the next queued message (Ctrl+Enter) goes now
                    let next = { let mut q = QUEUE.lock().unwrap(); (!q.is_empty()).then(|| q.remove(0)) };
                    if let Some(t) = next {
                        send(t);
                    }
                }
                if changed || is_result {
                    notify();
                }
            }
        }
        // the CLI went away while a reply was still expected: say so instead of waiting forever
        if BUSY.swap(false, Relaxed) {
            LOG.lock().unwrap().push(Entry::new(Kind::Info, "Claude Code stopped before it replied. Send the message again to restart it."));
        }
        notify();
    });
    *g = Some(Proc { stdin, child });
    true
}

fn write_line(v: serde_json::Value) -> bool {
    let mut g = PROC.lock().unwrap();
    match g.as_mut() {
        Some(p) => writeln!(p.stdin, "{v}").and_then(|_| p.stdin.flush()).is_ok(),
        None => false,
    }
}

/// Standard base64 (for pictures sent inside a message).
/// Standard base64 back to bytes (padding and line breaks allowed). None if it is not base64.
pub fn unbase64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c { b'A'..=b'Z' => c - b'A', b'a'..=b'z' => c - b'a' + 26, b'0'..=b'9' => c - b'0' + 52, b'+' => 62, b'/' => 63, _ => return None } as u32)
    };
    let bytes: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for c in bytes.chunks(4) {
        let n = c.iter().try_fold(0u32, |acc, b| Some(acc << 6 | val(*b)?))? << (6 * (4 - c.len()));
        let k = match c.len() { 4 => 3, 3 => 2, 2 => 1, _ => return None };
        out.extend_from_slice(&n.to_be_bytes()[1..1 + k]);
    }
    Some(out)
}

/// A picture's bytes as a thumbnail that fits max_w x max_h: (w, h, premultiplied BGRA).
fn picture_thumb(bytes: &[u8], max_w: u32, max_h: u32) -> Option<(u32, u32, Vec<u8>)> {
    use windows::Win32::Graphics::Imaging::*;
    use windows::Win32::System::Com::*;
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let stream = wic.CreateStream().ok()?;
        stream.InitializeFromMemory(bytes).ok()?;
        let dec = wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = dec.GetFrame(0).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        let (tw, th) = crate::clip::fit(w, h, max_w, max_h);
        let sc = wic.CreateBitmapScaler().ok()?;
        sc.Initialize(&frame, tw, th, WICBitmapInterpolationModeFant).ok()?;
        let conv = wic.CreateFormatConverter().ok()?;
        conv.Initialize(&sc, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        let mut px = vec![0u8; (tw * th * 4) as usize];
        conv.CopyPixels(std::ptr::null(), tw * 4, &mut px).ok()?;
        Some((tw, th, px))
    }
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            out.push(if k <= c.len() { T[(n >> (18 - 6 * k) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// What a typed message becomes: the text to show in the bubble, and the content blocks to send.
/// Every `@path` (or `@"path with spaces"`) that names a picture `read` can load is sent as a real
/// image block and shown as "[image: name]"; other @mentions stay in the text for the CLI to resolve.
pub fn message_content(text: &str, read: impl Fn(&str) -> Option<Vec<u8>>) -> (String, serde_json::Value) {
    let mut shown = String::new();
    let mut blocks = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find('@') {
        let after = &rest[at + 1..];
        let (path, used) = match after.strip_prefix('"').and_then(|q| q.find('"').map(|end| (&q[..end], end + 2))) {
            Some(quoted) => quoted,
            None => {
                let end = after.find(char::is_whitespace).unwrap_or(after.len());
                (&after[..end], end)
            }
        };
        let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        let media = match ext.as_str() {
            "png" => Some("image/png"),
            "jpg" | "jpeg" => Some("image/jpeg"),
            "gif" => Some("image/gif"),
            "webp" => Some("image/webp"),
            _ => None,
        };
        shown.push_str(&rest[..at]);
        match media.and_then(|m| read(path).map(|bytes| (m, bytes))) {
            Some((m, bytes)) => {
                blocks.push(serde_json::json!({ "type": "image", "source": { "type": "base64", "media_type": m, "data": base64(&bytes) } }));
                shown.push_str(&format!("[image: {}]", path.rsplit(['\\', '/']).next().unwrap_or(path)));
            }
            None => shown.push_str(&rest[at..at + 1 + used]),
        }
        rest = &rest[at + 1 + used..];
    }
    shown.push_str(rest);
    blocks.push(serde_json::json!({ "type": "text", "text": shown }));
    (shown, serde_json::Value::Array(blocks))
}

fn send(text: String) {
    if text.trim().is_empty() {
        return;
    }
    // pictures up to 5 MB go along as images
    let (text, content) = message_content(&text, |p| std::fs::read(p).ok().filter(|b| b.len() <= 5_000_000));
    let mut e = Entry::new(Kind::User, text.clone());
    e.images = content.as_array().into_iter().flatten().filter(|b| b["type"] == "image").filter_map(|b| b["source"]["data"].as_str().map(String::from)).collect();
    LOG.lock().unwrap().push(e);
    *IDLE_SINCE.lock().unwrap() = Some(std::time::Instant::now());
    notify();
    std::thread::spawn(move || {
        if !ensure_proc() {
            notify();
            return;
        }
        BUSY.store(true, Relaxed);
        BUSY_SINCE.store(unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }, Relaxed);
        let ok = write_line(serde_json::json!({ "type": "user", "message": { "role": "user", "content": content } }));
        if !ok {
            stop_proc();
            LOG.lock().unwrap().push(Entry::new(Kind::Info, "Lost the Claude Code process; send again to restart it."));
        }
        notify();
    });
}

/// Answer a permission prompt.
fn answer(i: usize, allow: bool) {
    let e = {
        let mut log = LOG.lock().unwrap();
        let Some(e) = log.get_mut(i) else { return };
        if e.kind != Kind::Permission {
            return;
        }
        let snapshot = e.clone();
        e.kind = Kind::Info;
        e.text = format!("{} {}", if allow { "Allowed" } else { "Denied" }, e.tool);
        snapshot
    };
    let input: serde_json::Value = serde_json::from_str(&e.input).unwrap_or(serde_json::json!({}));
    let response = if allow {
        serde_json::json!({ "behavior": "allow", "updatedInput": input })
    } else {
        serde_json::json!({ "behavior": "deny", "message": "The user denied this." })
    };
    write_line(serde_json::json!({ "type": "control_response", "response": { "subtype": "success", "request_id": e.request_id, "response": response } }));
    notify();
}

fn interrupt() {
    write_line(serde_json::json!({ "type": "control_request", "request_id": format!("stop-{}", std::process::id()), "request": { "subtype": "interrupt" } }));
}

// ======================= UI state =======================

#[derive(Clone, Copy, PartialEq, Debug)]
enum Act {
    New,
    Search,
    Session(usize),
    Group(usize, bool), // tool group start index, expanded?
    Allow(usize),
    Deny(usize),
    Send,
    Stop,
    Attach,
    Mic,
    Compact,
    CopyMsg(usize),
    Model,
    Effort,
    Mode,
    Folder,
    Panel,
    Link(usize), // a link in a reply (index into Ui::links)
    Fold(usize), // a sidebar group heading (index into Ui::group_names): fold or unfold it
    Output(usize), // a file in the Outputs list (index into Ui::outputs): open it
    PaneClose,     // the x of a docked pane
    PaneUp,        // Files pane: the parent folder
    PaneRow(usize), // Files pane: an entry (index into Ui::pane_files)
    MoreOutputs,   // "Show more" / "Show less" under the Outputs list
    Web(u8),  // claude.ai pages the CLI doesn't cover
    Tool(u8), // top-right tools: terminal, diff, browser, folder, more
    Gear, // the settings button in the account row
    Min,
    Max,
    Close,
}

#[derive(Default)]
struct Ui {
    scroll: f32,
    stick: bool,
    hits: Vec<(D2D_RECT_F, Act)>,
    links: Vec<String>, // addresses of the links on screen, for Act::Link
    hover_msg: Option<usize>, // the message under the mouse: it shows a copy button
    msgs: Vec<(D2D_RECT_F, String)>, // the messages on screen and their text, for right-click > Copy
    side_scroll: f32, // how far the session list is scrolled
    items: Vec<(Item, f32)>,                             // the conversation, laid out
    items_key: Option<(usize, usize, u32, usize, usize)>, // what `items` was laid out for
    folded: std::collections::HashSet<String>, // sidebar groups folded shut, by name
    outputs: Vec<String>, // the files in the Outputs list, for Act::Output
    pane: Option<Pane>,              // a docked pane open in place of the right-hand panel
    pane_dir: String,                // Files pane: the folder shown
    pane_files: Vec<(String, bool)>, // ...and its entries (name, is a folder)
    pane_scroll: f32,
    more_outputs: bool,   // the Outputs list is showing everything
    group_names: Vec<String>,                  // the groups on screen, for Act::Fold
    hover: Option<Act>,
    expanded: std::collections::HashSet<usize>,
    selected: Option<usize>,
    filter: String,
    panel: bool,
    anim: u32,
    gfx: Option<Gfx>,
}

thread_local! {
    static UI: RefCell<Ui> = RefCell::new(Ui { stick: true, panel: true, ..Default::default() });
}

struct Gfx {
    rt: ID2D1DCRenderTarget,
    dw: IDWriteFactory,
    body: IDWriteTextFormat,
    mono: IDWriteTextFormat,
    small: IDWriteTextFormat,
    side: IDWriteTextFormat,
    head: IDWriteTextFormat,
    #[allow(dead_code)]
    icon: IDWriteTextFormat,
    icon10: IDWriteTextFormat,
}

unsafe fn make_gfx() -> windows::core::Result<Gfx> {
    let f: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
    let props = D2D1_RENDER_TARGET_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
        ..Default::default()
    };
    let rt = f.CreateDCRenderTarget(&props)?;
    // an opaque window, so ClearType works here: the same crisp text Chromium draws in the real app
    rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE);
    let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
    let fmt = |name: PCWSTR, size: f32, weight: DWRITE_FONT_WEIGHT, wrap: bool| -> windows::core::Result<IDWriteTextFormat> {
        let t = dw.CreateTextFormat(name, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))?;
        t.SetWordWrapping(if wrap { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP })?;
        if !wrap {
            let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
            t.SetTrimming(&trim, &dw.CreateEllipsisTrimmingSign(&t)?)?;
            t.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        }
        Ok(t)
    };
    let ui = w!("Segoe UI");
    let n = DWRITE_FONT_WEIGHT_NORMAL;
    let icon = fmt(w!("Segoe Fluent Icons"), 16.0, n, false)?;
    icon.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
    let icon10 = fmt(w!("Segoe Fluent Icons"), 10.0, n, false)?;
    icon10.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
    Ok(Gfx {
        body: fmt(ui, 15.0, n, true)?,
        mono: fmt(w!("Cascadia Mono"), 13.0, n, true)?,
        small: fmt(ui, 13.0, n, false)?,
        side: fmt(ui, 14.0, n, false)?,
        head: fmt(ui, 14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, false)?,
        icon,
        icon10,
        dw,
        rt,
    })
}

// ----- markdown (the subset the app renders most): **bold**, `code`, ``` blocks, # headings, lists -----

pub fn blocks(md: &str) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let mut code = false;
    let mut cur = String::new();
    for line in md.lines() {
        if line.trim_start().starts_with("```") {
            if !cur.trim().is_empty() {
                out.push((code, cur.trim_end_matches('\n').to_string()));
            }
            cur.clear();
            code = !code;
            continue;
        }
        cur.push_str(line);
        cur.push('\n');
    }
    if !cur.trim().is_empty() {
        out.push((code, cur.trim_end_matches('\n').to_string()));
    }
    out
}

/// A stretch of prose: ordinary text, or a markdown table (rows of cells, the first row is the header).
#[derive(Debug, PartialEq)]
pub enum Seg {
    Text(String),
    Table(Vec<Vec<String>>),
}

/// Split prose into text and tables. A table is a line starting with `|`, then a `|---|---|` line, then more `|` lines.
pub fn tables(text: &str) -> Vec<Seg> {
    let cells = |l: &str| -> Vec<String> { l.trim().trim_start_matches('|').trim_end_matches('|').split('|').map(|c| c.trim().to_string()).collect() };
    let is_row = |l: &str| l.trim_start().starts_with('|');
    let is_rule = |l: &str| is_row(l) && l.contains('-') && l.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '));
    let lines: Vec<&str> = text.lines().collect();
    let (mut out, mut prose, mut i) = (Vec::new(), String::new(), 0);
    while i < lines.len() {
        if is_row(lines[i]) && lines.get(i + 1).is_some_and(|l| is_rule(l)) {
            if !prose.trim().is_empty() {
                out.push(Seg::Text(prose.trim_end_matches('\n').to_string()));
            }
            prose.clear();
            let mut rows = vec![cells(lines[i])];
            i += 2;
            while i < lines.len() && is_row(lines[i]) {
                rows.push(cells(lines[i]));
                i += 1;
            }
            out.push(Seg::Table(rows));
        } else {
            prose.push_str(lines[i]);
            prose.push('\n');
            i += 1;
        }
    }
    if !prose.trim().is_empty() {
        out.push(Seg::Text(prose.trim_end_matches('\n').to_string()));
    }
    out
}

/// Column widths for a table whose columns would naturally be `natural` wide, inside `width`:
/// as they are if they fit, otherwise shrunk in proportion (never under 60).
pub fn column_widths(natural: &[f32], width: f32) -> Vec<f32> {
    let total: f32 = natural.iter().sum();
    if total <= width {
        return natural.to_vec();
    }
    natural.iter().map(|n| (n * width / total).max(60.0)).collect()
}

/// How a stretch of reply text is styled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Style {
    Code,
    Bold,
    Italic,
    Link,
    H1,
    H2,
    H3,
    Quote,
}

// private one-character markers that `lines` wraps headings and quotes in, so `inline` can style them
const M_H1: char = '\u{11}';
const M_H2: char = '\u{12}';
const M_H3: char = '\u{13}';
const M_QUOTE: char = '\u{14}';

/// Line-level markdown -> the text `inline` takes: headings and quotes get their markers, list
/// items their bullets (nested ones indented), a rule becomes a line.
pub fn lines(md: &str) -> String {
    md.lines()
        .map(|l| {
            let body = l.trim_start();
            let depth = (l.len() - body.len()) / 2;
            let pad = "\u{a0}\u{a0}\u{a0}\u{a0}".repeat(depth);
            if let Some(h) = body.strip_prefix('#') {
                let level = 1 + h.chars().take_while(|c| *c == '#').count();
                let m = [M_H1, M_H2, M_H3][level.min(3) - 1];
                format!("{m}{}{m}", h.trim_start_matches('#').trim().replace("**", ""))
            } else if let Some(x) = body.strip_prefix("- ").or(body.strip_prefix("* ")) {
                format!("{pad}{} {x}", if depth == 0 { '\u{2022}' } else { '\u{25e6}' })
            } else if let Some(q) = body.strip_prefix('>') {
                format!("{M_QUOTE}{}{M_QUOTE}", q.trim_start())
            } else if body.len() >= 3 && (body.chars().all(|c| c == '-') || body.chars().all(|c| c == '*')) {
                "\u{2500}".repeat(24)
            } else if depth > 0 && body.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                format!("{pad}{body}") // a nested numbered item keeps its indent
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Strip inline markers; return the plain text and (start, length, style) runs in UTF-16 units.
/// Handles **bold**, `code`, *italic*, [text](link) and the heading / quote markers from `lines`.
#[cfg(test)]
pub fn inline(s: &str) -> (String, Vec<(u32, u32, Style)>) {
    let (text, runs, _) = inline_links(s);
    (text, runs)
}

/// `inline`, plus where each link goes (in the order of its Link runs).
pub fn inline_links(s: &str) -> (String, Vec<(u32, u32, Style)>, Vec<String>) {
    let mut out = String::new();
    let mut runs = Vec::new();
    let mut urls = Vec::new();
    let mut rest = s;
    let u16len = |x: &str| x.encode_utf16().count() as u32;
    while !rest.is_empty() {
        let c = rest.chars().next().unwrap();
        // [text](url): show the text
        if c == '[' {
            if let Some((text, after)) = rest[1..].split_once("](") {
                if let (false, Some(close)) = (text.contains('\n') || text.contains('['), after.find(')')) {
                    runs.push((u16len(&out), u16len(text), Style::Link));
                    urls.push(after[..close].trim().to_string());
                    out.push_str(text);
                    rest = &after[close + 1..];
                    continue;
                }
            }
        }
        let (marker, style) = match c {
            '*' if rest.starts_with("**") => ("**", Style::Bold),
            '*' => ("*", Style::Italic),
            '`' => ("`", Style::Code),
            M_H1 => ("\u{11}", Style::H1),
            M_H2 => ("\u{12}", Style::H2),
            M_H3 => ("\u{13}", Style::H3),
            M_QUOTE => ("\u{14}", Style::Quote),
            _ => ("", Style::Bold),
        };
        if !marker.is_empty() {
            if let Some(end) = rest[marker.len()..].find(marker) {
                let inner = &rest[marker.len()..marker.len() + end];
                // a lone * is only italic when it hugs the words ("2 * 3 * 4" stays as typed)
                let hugging = !inner.is_empty() && !inner.starts_with(' ') && !inner.ends_with(' ') && !inner.contains('\n');
                if style != Style::Italic || hugging {
                    runs.push((u16len(&out), u16len(inner), style));
                    out.push_str(inner);
                    rest = &rest[marker.len() * 2 + end..];
                    continue;
                }
            }
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    (out, runs, urls)
}

/// Only web addresses are opened from a reply: never a file path or another kind of link.
pub fn safe_link(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    (u.starts_with("https://") || u.starts_with("http://")) && !u.contains(char::is_whitespace)
}

unsafe fn text_layout(g: &Gfx, md: &str, width: f32) -> Option<IDWriteTextLayout> {
    text_layout_links(g, md, width).map(|(l, _)| l)
}

/// The layout, and its links as (start, length, address) in UTF-16 units.
unsafe fn text_layout_links(g: &Gfx, md: &str, width: f32) -> Option<(IDWriteTextLayout, Vec<(u32, u32, String)>)> {
    let (plain, runs, urls) = inline_links(&lines(md));
    let links: Vec<(u32, u32, String)> = runs.iter().filter(|r| r.2 == Style::Link).zip(urls).map(|(r, u)| (r.0, r.1, u)).collect();
    let l = g.dw.CreateTextLayout(&utf16(&plain), &g.body, width, 1e6).ok()?;
    let _ = l.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, 22.0, 17.0);
    for (start, len, style) in runs {
        let r = DWRITE_TEXT_RANGE { startPosition: start, length: len };
        match style {
            Style::Bold => _ = l.SetFontWeight(DWRITE_FONT_WEIGHT_SEMI_BOLD, r),
            Style::Italic => _ = l.SetFontStyle(DWRITE_FONT_STYLE_ITALIC, r),
            Style::Link => _ = l.SetUnderline(true, r),
            Style::Code => {
                let _ = l.SetFontFamilyName(w!("Cascadia Mono"), r);
                let _ = l.SetFontSize(13.5, r);
            }
            Style::H1 | Style::H2 | Style::H3 => {
                let _ = l.SetFontWeight(DWRITE_FONT_WEIGHT_SEMI_BOLD, r);
                let _ = l.SetFontSize(match style { Style::H1 => 20.0, Style::H2 => 17.0, _ => 15.0 }, r);
            }
            Style::Quote => {
                if let Ok(dim) = g.rt.CreateSolidColorBrush(&rgb(C_DIM), None) {
                    let _ = l.SetDrawingEffect(&dim, r);
                }
            }
        }
    }
    Some((l, links))
}

// ----- conversation layout -----

/// One piece of a reply.
enum Part {
    Text(IDWriteTextLayout, Vec<(u32, u32, String)>), // layout, its links
    Code(IDWriteTextLayout),
    Table { widths: Vec<f32>, rows: Vec<(f32, Vec<IDWriteTextLayout>)> }, // row height, its cells
}
const CELL_PAD: f32 = 12.0; // around a table cell's text

enum Item {
    User(IDWriteTextLayout, String, Vec<(ID2D1Bitmap, f32, f32)>), // layout, the message as typed (for Copy), picture thumbnails

    Text(Vec<Part>, String),         // parts, the reply's markdown (for Copy)
    Group { start: usize, label: String, lines: Vec<IDWriteTextLayout>, open: bool },
    Permission(usize, IDWriteTextLayout),
    Info(IDWriteTextLayout),
}

unsafe fn metrics(l: &IDWriteTextLayout) -> DWRITE_TEXT_METRICS {
    let mut m = DWRITE_TEXT_METRICS::default();
    let _ = l.GetMetrics(&mut m);
    m
}

unsafe fn build(g: &Gfx, log: &[Entry], width: f32, ui: &Ui) -> Vec<(Item, f32)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < log.len() {
        let e = &log[i];
        match e.kind {
            Kind::User => {
                if let Ok(l) = g.dw.CreateTextLayout(&utf16(&e.text), &g.body, width * 0.75, 1e6) {
                    let _ = l.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, 21.0, 16.0);
                    // pictures sent with it: thumbnails under the bubble, like the app
                    let props = D2D1_BITMAP_PROPERTIES { pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }, dpiX: 96.0, dpiY: 96.0 };
                    let thumbs: Vec<(ID2D1Bitmap, f32, f32)> = e.images.iter().filter_map(|d| unbase64(d)).filter_map(|b| picture_thumb(&b, 240, 160))
                        .filter_map(|(w, h, px)| g.rt.CreateBitmap(D2D_SIZE_U { width: w, height: h }, Some(px.as_ptr() as *const c_void), w * 4, &props).ok().map(|bm| (bm, w as f32, h as f32))).collect();
                    let empty = e.text.trim().is_empty();
                    let text_h = if empty { 0.0 } else { metrics(&l).height + 22.0 };
                    let pics_h = thumbs.iter().map(|t| t.2).fold(0.0, f32::max);
                    let h = text_h + if pics_h > 0.0 { pics_h + if empty { 0.0 } else { 8.0 } } else { 0.0 };
                    if h > 0.0 {
                        out.push((Item::User(l, e.text.clone(), thumbs), h));
                    }
                }
                i += 1;
            }
            Kind::Assistant => {
                let mut parts = Vec::new();
                let mut h = 0.0;
                for (code, s) in blocks(&e.text) {
                    if code {
                        if let Ok(l) = g.dw.CreateTextLayout(&utf16(&s), &g.mono, width - 28.0, 1e6) {
                            h += metrics(&l).height + 38.0;
                            parts.push(Part::Code(l));
                        }
                        continue;
                    }
                    for seg in tables(&s) {
                        match seg {
                            Seg::Text(t) => {
                                if let Some((l, links)) = text_layout_links(g, &t, width) {
                                    h += metrics(&l).height + 10.0;
                                    parts.push(Part::Text(l, links));
                                }
                            }
                            Seg::Table(cells) => {
                                // lay every cell out unconstrained to learn the columns' natural widths, then fit them
                                let ncols = cells.iter().map(Vec::len).max().unwrap_or(0);
                                let mut rows: Vec<Vec<IDWriteTextLayout>> = Vec::new();
                                let mut natural = vec![0f32; ncols];
                                for (r, row) in cells.iter().enumerate() {
                                    let mut ls = Vec::new();
                                    for (c, cell) in row.iter().enumerate() {
                                        let src = if r == 0 { format!("**{}**", cell.replace("**", "")) } else { cell.clone() };
                                        if let Some(l) = text_layout(g, &src, 4000.0) {
                                            natural[c] = natural[c].max(metrics(&l).widthIncludingTrailingWhitespace + CELL_PAD * 2.0);
                                            ls.push(l);
                                        }
                                    }
                                    rows.push(ls);
                                }
                                let widths = column_widths(&natural, width);
                                let mut laid = Vec::new();
                                for row in rows {
                                    let mut rh = 0f32;
                                    for (c, l) in row.iter().enumerate() {
                                        let _ = l.SetMaxWidth((widths[c] - CELL_PAD * 2.0).max(20.0));
                                        rh = rh.max(metrics(l).height);
                                    }
                                    h += rh + 16.0;
                                    laid.push((rh + 16.0, row));
                                }
                                h += 10.0;
                                parts.push(Part::Table { widths, rows: laid });
                            }
                        }
                    }
                }
                out.push((Item::Text(parts, e.text.clone()), (h - 10.0).max(0.0)));
                i += 1;
            }
            Kind::Tool | Kind::ToolResult => {
                let start = i;
                let mut names = Vec::new();
                let mut lines = Vec::new();
                while i < log.len() && matches!(log[i].kind, Kind::Tool | Kind::ToolResult) {
                    if log[i].kind == Kind::Tool {
                        names.push(log[i].tool.as_str());
                    }
                    lines.push(log[i].text.clone());
                    i += 1;
                }
                let open = ui.expanded.contains(&start);
                let mut ls = Vec::new();
                let mut h = 22.0;
                if open {
                    for t in &lines {
                        if let Ok(l) = g.dw.CreateTextLayout(&utf16(t), &g.mono, width - 24.0, 1e6) {
                            h += metrics(&l).height + 6.0;
                            ls.push(l);
                        }
                    }
                    h += 4.0;
                }
                out.push((Item::Group { start, label: summary(&names), lines: ls, open }, h));
            }
            Kind::Permission => {
                if let Ok(l) = g.dw.CreateTextLayout(&utf16(&e.text), &g.body, width - 32.0, 1e6) {
                    let h = metrics(&l).height + 72.0;
                    out.push((Item::Permission(i, l), h));
                }
                i += 1;
            }
            Kind::Info => {
                if let Ok(l) = g.dw.CreateTextLayout(&utf16(&e.text), &g.body, width, 1e6) {
                    let h = metrics(&l).height;
                    out.push((Item::Info(l), h));
                }
                i += 1;
            }
        }
    }
    out
}

// ======================= drawing =======================

unsafe fn draw(ui: &mut Ui, cw: f32, ch: f32, maximized: bool) -> windows::core::Result<()> {
    let Some(g) = ui.gfx.as_ref() else { return Ok(()) };
    let rt = &g.rt;
    let mut hits: Vec<(D2D_RECT_F, Act)> = Vec::new();
    let mut links: Vec<String> = Vec::new();
    let mut msgs: Vec<(D2D_RECT_F, String)> = Vec::new();
    let mut outputs_list: Vec<String> = Vec::new();
    rt.Clear(Some(&rgb(C_MAIN)));
    let b = |c: u32| rt.CreateSolidColorBrush(&rgb(c), None);
    let white = b(0xFFFFFF)?;
    let dim = b(C_DIM)?;
    let soft = b(0xCFCFCF)?;
    let txt = |s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, br: &ID2D1SolidColorBrush| rt.DrawText(&utf16(s), f, &r, br, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
    let ico = |cp: u32, f: &IDWriteTextFormat, r: D2D_RECT_F, br: &ID2D1SolidColorBrush| {
        rt.DrawText(&glyph(cp), f, &r, br, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        rt.DrawText(&glyph(cp), f, &rect(r.left + 0.5, r.top, r.right + 0.5, r.bottom), br, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
    };
    let hover = ui.hover;
    let hov = |a: Act| hover == Some(a);
    let set = settings();

    // ---- sidebar ----
    rt.FillRectangle(&rect(0.0, 0.0, SIDE, ch), &b(C_SIDE)?);
    rt.FillRectangle(&rect(SIDE, 0.0, SIDE + 1.0, ch), &b(C_LINE)?);
    for (k, name) in ["align-left", "panel-left", "chevron-left", "chevron-right"].iter().enumerate() {
        lucide(rt, name, rect(12.0 + k as f32 * 29.0, 4.0, 40.0 + k as f32 * 29.0, 32.0), 18.0, if k == 3 { &dim } else { &soft });
    }
    rt.FillRoundedRectangle(&rrect(rect(196.0, 5.0, 256.0, 31.0), 6.0), &b(0x2A2A2A)?);
    rt.FillRoundedRectangle(&rrect(rect(227.0, 7.0, 254.0, 29.0), 5.0), &b(C_SEL)?);
    lucide(rt, "message-circle", rect(198.0, 5.0, 226.0, 31.0), 18.0, &soft);
    lucide(rt, "code", rect(227.0, 5.0, 254.0, 31.0), 18.0, &white);
    let mut y = TOP + 5.0;
    for (icon_name, label, act) in [("plus", "New", Act::New), ("search", "Search", Act::Search), ("copy", "Artifacts", Act::Web(0)), ("clock", "Scheduled", Act::Web(1)), ("briefcase", "Customize", Act::Web(2))] {
        let r = rect(8.0, y, SIDE - 8.0, y + 32.0);
        if hov(act) {
            rt.FillRoundedRectangle(&rrect(r, 6.0), &b(C_HOVER)?);
        }
        lucide(rt, icon_name, rect(16.0, y, 36.0, y + 32.0), 18.0, &white);
        txt(label, &g.side, rect(44.0, y, SIDE - 16.0, y + 32.0), &white);
        hits.push((r, act));
        y += 34.0;
    }
    y += 18.0;
    if !ui.filter.is_empty() {
        txt(&format!("Filter: {}", ui.filter), &g.small, rect(16.0, y, SIDE - 16.0, y + 24.0), &dim);
        y += 26.0;
    }
    let sessions = SESSIONS.lock().unwrap().clone();
    let groups = grouped(&sessions, &pins(), &ui.filter);
    // every session is listed; the list scrolls with the mouse wheel when it is taller than the sidebar
    let (list_top, list_bot) = (y, ch - 60.0);
    // a group whose heading was clicked is folded: just its heading shows
    let open_rows = |g: &(String, Vec<usize>)| if ui.folded.contains(&g.0) { 0 } else { g.1.len() };
    let content: f32 = groups.iter().map(|g| 40.0 + open_rows(g) as f32 * 34.0).sum();
    ui.side_scroll = ui.side_scroll.clamp(0.0, (content - (list_bot - list_top)).max(0.0));
    y -= ui.side_scroll;
    rt.PushAxisAlignedClip(&rect(0.0, list_top, SIDE, list_bot), D2D1_ANTIALIAS_MODE_ALIASED);
    let mut group_names = Vec::new();
    for (name, items) in groups.iter() {
        let folded = ui.folded.contains(name);
        if y + 30.0 > list_top && y < list_bot {
            txt(&format!("{name}  {}", if folded { '\u{203a}' } else { '\u{2304}' }), &g.small, rect(18.0, y, SIDE - 16.0, y + 28.0), &dim);
            hits.push((rect(8.0, y.max(list_top), SIDE - 8.0, (y + 28.0).min(list_bot)), Act::Fold(group_names.len())));
        }
        group_names.push(name.clone());
        y += 30.0;
        for &i in items.iter().take(if folded { 0 } else { usize::MAX }) {
            if y + 34.0 <= list_top || y >= list_bot {
                y += 34.0;
                continue; // scrolled out of view
            }
            let r = rect(8.0, y.max(list_top), SIDE - 8.0, (y + 32.0).min(list_bot));
            let sel = ui.selected == Some(i);
            if sel || hov(Act::Session(i)) {
                rt.FillRoundedRectangle(&rrect(r, 6.0), &b(if sel { C_SEL } else { C_HOVER })?);
            }
            let dot = STATUS.lock().unwrap().as_ref().and_then(|m| m.get(&sessions[i].id).and_then(|st| status_dot(st)));
            txt(&sessions[i].title, &g.side, rect(18.0, y, SIDE - if dot.is_some() { 34.0 } else { 20.0 }, y + 32.0), &white);
            if let Some(c) = dot {
                rt.FillEllipse(&D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: SIDE - 22.0, Y: y + 16.0 }, radiusX: 3.5, radiusY: 3.5 }, &b(c)?);
            }
            hits.push((r, Act::Session(i)));
            y += 34.0;
        }
        y += 10.0;
    }
    rt.PopAxisAlignedClip();
    rt.FillEllipse(&D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: 28.0, Y: ch - 30.0 }, radiusX: 11.0, radiusY: 11.0 }, &b(0x3A3A3A)?);
    let name = crate::launcher::display_name();
    let initial = name.chars().next().unwrap_or('?').to_uppercase().to_string();
    txt(&initial, &g.small, rect(23.0, ch - 41.0, 40.0, ch - 19.0), &white);
    txt(&name, &g.side, rect(48.0, ch - 44.0, 220.0, ch - 16.0), &white);
    let gear = rect(222.0, ch - 46.0, 258.0, ch - 14.0);
    if ui.hover == Some(Act::Gear) {
        rt.FillRoundedRectangle(&rrect(gear, 4.0), &b(0x2A2A2A)?);
    }
    lucide(rt, "settings", rect(226.0, ch - 44.0, 254.0, ch - 16.0), 18.0, &soft);
    hits.push((gear, Act::Gear));

    // ---- title bar ----
    rt.FillRectangle(&rect(SIDE + 1.0, TOP - 1.0, cw, TOP), &b(C_TOPLINE)?);
    let title = ui.selected.and_then(|i| sessions.get(i).map(|s| s.title.clone())).unwrap_or_else(|| "New session".into());
    let tw = measure_side(g, &title);
    txt(&title, &g.side, rect(SIDE + 24.0, 0.0, cw - 420.0, TOP), &white);
    lucide(rt, "chevron-down", rect(SIDE + 30.0 + tw, 0.0, SIDE + 46.0 + tw, TOP), 18.0, &white);
    let wb = [(Act::Min, 0xE921u32), (Act::Max, if maximized { 0xE923 } else { 0xE922 }), (Act::Close, 0xE8BB)];
    for (k, (act, cp)) in wb.iter().enumerate() {
        let r = rect(cw - 46.0 * (3 - k) as f32, 0.0, cw - 46.0 * (2 - k) as f32, TOP - 1.0);
        if hov(*act) {
            rt.FillRectangle(&r, &b(if *act == Act::Close { 0xC42B1C } else { 0x2D2D2D })?);
        }
        ico(*cp, &g.icon10, r, &white);
        hits.push((r, *act));
    }
    let tools = [(Act::Panel, "list"), (Act::Tool(0), "terminal"), (Act::Tool(1), "diff"), (Act::Tool(2), "globe"), (Act::Tool(3), "folder"), (Act::Tool(4), "ellipsis")];
    for (k, (act, name)) in tools.iter().enumerate() {
        let x = cw - 46.0 * 3.0 - 196.0 + k as f32 * 30.0;
        let r = rect(x, 4.0, x + 28.0, 32.0);
        if (k == 0 && ui.panel) || hov(*act) {
            rt.FillRoundedRectangle(&rrect(r, 5.0), &b(0x2D2D2D)?);
        }
        lucide(rt, name, r, 18.0, &white);
        hits.push((r, *act));
    }

    // ---- right panel ----
    let right = match ui.pane {
        Some(_) => PANE_W.min(cw * 0.45),
        None if ui.panel => RIGHT,
        None => 0.0,
    };
    if let Some(pane) = ui.pane {
        let x = cw - right;
        rt.FillRectangle(&rect(x, TOP, x + 1.0, ch), &b(C_LINE)?);
        // header: what it shows, and a close button
        let title = match pane {
            Pane::Files => ui.pane_dir.rsplit(['\\', '/']).find(|p| !p.is_empty()).unwrap_or("Files").to_string(),
            Pane::Diff => "Changes".to_string(),
            Pane::Terminal => "Terminal".to_string(),
            Pane::Preview => "Preview".to_string(),
        };
        txt(&title, &g.side, rect(x + 17.0, TOP + 8.0, cw - 48.0, TOP + 36.0), &white);
        let close = rect(cw - 40.0, TOP + 8.0, cw - 12.0, TOP + 36.0);
        if hov(Act::PaneClose) {
            rt.FillRoundedRectangle(&rrect(close, 5.0), &b(C_HOVER)?);
        }
        lucide(rt, "x", close, 16.0, &soft);
        hits.push((close, Act::PaneClose));
        rt.FillRectangle(&rect(x + 1.0, TOP + 44.0, cw, TOP + 45.0), &b(C_LINE)?);
        let (top, bot) = (TOP + 52.0, ch - 8.0);
        rt.PushAxisAlignedClip(&rect(x + 1.0, top, cw, bot), D2D1_ANTIALIAS_MODE_ALIASED);
        match pane {
            Pane::Files => {
                // ".." first, then folders, then files; 28px rows
                let rows = ui.pane_files.len() + 1;
                ui.pane_scroll = ui.pane_scroll.clamp(0.0, (rows as f32 * 28.0 - (bot - top)).max(0.0));
                for k in 0..rows {
                    let y = top + k as f32 * 28.0 - ui.pane_scroll;
                    if y + 28.0 <= top || y >= bot {
                        continue;
                    }
                    let r = rect(x + 8.0, y.max(top), cw - 8.0, (y + 26.0).min(bot));
                    let act = if k == 0 { Act::PaneUp } else { Act::PaneRow(k - 1) };
                    if hov(act) {
                        rt.FillRoundedRectangle(&rrect(r, 5.0), &b(C_HOVER)?);
                    }
                    let (name, is_dir) = if k == 0 { ("..".to_string(), true) } else { ui.pane_files[k - 1].clone() };
                    let code = ["rs", "py", "js", "ts", "tsx", "jsx", "c", "cpp", "h", "go", "java", "cs", "html", "css", "json", "toml"].contains(&name.rsplit('.').next().unwrap_or(""));
                    lucide(rt, if is_dir { "folder" } else if code { "code" } else { "file" }, rect(x + 16.0, y, x + 38.0, y + 26.0), 17.0, &soft);
                    txt(&name, &g.side, rect(x + 47.0, y, cw - 16.0, y + 26.0), &white);
                    hits.push((r, act));
                }
            }
            Pane::Diff => {
                let lines = DIFF.lock().unwrap().clone();
                ui.pane_scroll = ui.pane_scroll.clamp(0.0, (lines.len() as f32 * 18.0 - (bot - top)).max(0.0));
                let first = (ui.pane_scroll / 18.0) as usize;
                for (k, line) in lines.iter().enumerate().skip(first).take(((bot - top) / 18.0) as usize + 2) {
                    let y = top + k as f32 * 18.0 - ui.pane_scroll;
                    txt(line, &g.mono, rect(x + 14.0, y, cw + 2000.0, y + 18.0), &b(diff_colour(line))?);
                }
            }
            Pane::Terminal => {
                // output above, the command box (a child edit) in the last 40 px
                let lines = term_lines();
                let view = bot - top - 44.0;
                ui.pane_scroll = ui.pane_scroll.clamp(0.0, (lines.len() as f32 * 18.0 - view).max(0.0));
                let first = (ui.pane_scroll / 18.0) as usize;
                for (k, line) in lines.iter().enumerate().skip(first).take((view / 18.0) as usize + 2) {
                    let y = top + k as f32 * 18.0 - ui.pane_scroll;
                    if y + 18.0 > top + view { break; }
                    let c = if line.split_once("> ").is_some_and(|(d, _)| !d.contains(' ') && !d.is_empty()) { 0x6CB4EE } else { 0xD8D8D8 };
                    txt(line, &g.mono, rect(x + 14.0, y, cw + 2000.0, y + 18.0), &b(c)?);
                }
                let boxr = rect(x + 10.0, bot - 38.0, cw - 10.0, bot - 4.0);
                rt.FillRoundedRectangle(&rrect(boxr, 8.0), &b(0x1A1A1A)?);
                rt.DrawRoundedRectangle(&rrect(boxr, 8.0), &b(C_SEL)?, 1.0, None);
                txt(">", &g.mono, rect(x + 20.0, bot - 38.0, x + 34.0, bot - 4.0), &b(0x6CB4EE)?);
            }
            Pane::Preview => {
                // the app previews a running dev server; here: the local servers, click opens one
                let servers = SERVERS.lock().unwrap().clone();
                if servers.is_empty() {
                    txt("No local server is running. Start one (npm run dev, cargo run ...) and it shows here.", &g.small, rect(x + 17.0, top, cw - 16.0, top + 26.0), &soft);
                }
                for (k, (port, name)) in servers.iter().enumerate() {
                    let y = top + k as f32 * 44.0;
                    let r = rect(x + 8.0, y, cw - 8.0, y + 40.0);
                    if hov(Act::PaneRow(k)) {
                        rt.FillRoundedRectangle(&rrect(r, 5.0), &b(C_HOVER)?);
                    }
                    lucide(rt, "globe", rect(x + 16.0, y, x + 38.0, y + 40.0), 17.0, &soft);
                    txt(&format!("http://localhost:{port}"), &g.side, rect(x + 47.0, y + 2.0, cw - 16.0, y + 22.0), &white);
                    txt(name, &g.small, rect(x + 47.0, y + 20.0, cw - 16.0, y + 38.0), &soft);
                    hits.push((r, Act::PaneRow(k)));
                }
            }
        }
        rt.PopAxisAlignedClip();
    } else if ui.panel {
        let x = cw - RIGHT;
        rt.FillRectangle(&rect(x, TOP, x + 1.0, ch), &b(C_LINE)?);
        txt("Environment", &g.small, rect(x + 17.0, TOP + 13.0, cw - 16.0, TOP + 35.0), &white);
        // three rows, 32 apart: where it runs, its folder, Remote Control
        let row = |k: usize| rect(x + 8.0, TOP + 41.0 + k as f32 * 32.0, cw - 8.0, TOP + 69.0 + k as f32 * 32.0);
        let label = |r: D2D_RECT_F| rect(x + 47.0, r.top, cw - 56.0, r.bottom);
        let icon = |r: D2D_RECT_F| rect(x + 16.0, r.top, x + 38.0, r.bottom);
        lucide(rt, "laptop", icon(row(0)), 18.0, &soft);
        txt("This computer", &g.side, label(row(0)), &white);
        if hov(Act::Folder) {
            rt.FillRoundedRectangle(&rrect(row(1), 5.0), &b(C_HOVER)?);
        }
        lucide(rt, "folder", icon(row(1)), 18.0, &soft);
        // a session in the app's own scratch area has no folder of its own yet
        let folder = if set.folder.contains("scratch-workspaces") { "Choose folder\u{2026}".to_string() } else { set.folder.rsplit(['\\', '/']).find(|p| !p.is_empty()).unwrap_or(&set.folder).to_string() };
        txt(&folder, &g.side, label(row(1)), &white);
        hits.push((row(1), Act::Folder));
        lucide(rt, "monitor", icon(row(2)), 18.0, &soft);
        txt("Remote Control", &g.side, label(row(2)), &white);
        // the switch is drawn off: this window has no Remote Control
        let sw = rect(cw - 45.0, TOP + 112.0, cw - 17.0, TOP + 128.0);
        rt.FillRoundedRectangle(&rrect(sw, 8.0), &b(0x4A4A4A)?);
        rt.FillEllipse(&D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: sw.left + 8.0, Y: sw.top + 8.0 }, radiusX: 5.0, radiusY: 5.0 }, &white);

        rt.FillRectangle(&rect(x + 16.0, TOP + 144.0, cw - 16.0, TOP + 145.0), &b(C_LINE)?);
        // Context: how full, tokens in use against the window, a bar, then what it is made of
        txt("Context", &g.small, rect(x + 17.0, TOP + 157.0, cw - 16.0, TOP + 179.0), &white);
        let use_ = *CONTEXT_TOKENS.lock().unwrap();
        let pct = *CONTEXT.lock().unwrap();
        txt(&pct.map(|p| format!("{p:.0}% used")).unwrap_or_else(|| "\u{2013}".into()), &g.small, rect(x + 17.0, TOP + 187.0, cw - 130.0, TOP + 209.0), &white);
        if let Some((used, window)) = use_ {
            let t = format!("{} / {}", tokens(used), tokens(window));
            txt(&t, &g.small, rect(cw - 16.0 - measure(g, &t) - 2.0, TOP + 187.0, cw - 8.0, TOP + 209.0), &soft);
        }
        let bar = rect(x + 17.0, TOP + 212.0, cw - 16.0, TOP + 215.0);
        rt.FillRoundedRectangle(&rrect(bar, 1.5), &b(0x3A3A3A)?);
        if let Some(p) = pct {
            rt.FillRoundedRectangle(&rrect(rect(bar.left, bar.top, bar.left + (bar.right - bar.left) * p / 100.0, bar.bottom), 1.5), &b(C_BLUE)?);
        }
        let mut py = TOP + 223.0;
        if let Some((used, window)) = use_ {
            // ponytail: memory and skills are estimated from their text; tools / system prompt / MCP
            // stay inside "Messages and system" until the CLI's own context report is read
            let (memory, skills) = context_parts(&set.folder);
            for (colour, name, n) in breakdown(used, window, memory, skills) {
                rt.FillRoundedRectangle(&rrect(rect(x + 18.0, py + 7.0, x + 27.0, py + 16.0), 2.0), &b(colour)?);
                txt(name, &g.small, rect(x + 41.0, py, cw - 130.0, py + 24.0), &soft);
                let (t, share) = (tokens(n), format!("{:.1}%", n / window * 100.0));
                txt(&t, &g.small, rect(cw - 72.0 - measure(g, &t) - 2.0, py, cw - 64.0, py + 24.0), &soft);
                txt(&share, &g.small, rect(cw - 16.0 - measure(g, &share) - 2.0, py, cw - 8.0, py + 24.0), &white);
                py += 24.0;
            }
            let left = until_compact(used, window);
            txt(&format!("{} until auto-compact", tokens(left)), &g.small, rect(x + 17.0, py + 6.0, cw - 130.0, py + 30.0), &soft);
            let br = rect(cw - 124.0, py + 4.0, cw - 16.0, py + 30.0);
            rt.FillRoundedRectangle(&rrect(br, 6.0), &b(if hov(Act::Compact) { 0x3A3A3A } else { 0x2C2C2C })?);
            let label = "Compact session";
            txt(label, &g.small, rect(br.left + (br.right - br.left - measure(g, label)) / 2.0, br.top, br.right, br.bottom), &white);
            hits.push((br, Act::Compact));
            py += 34.0;
        }
        py += 14.0;

        // Connectors: the MCP servers this folder's sessions get (read when the folder changes)
        {
            static CONN: Mutex<Option<(String, Vec<(String, String)>)>> = Mutex::new(None);
            let mut c = CONN.lock().unwrap();
            if c.as_ref().map(|x| &x.0) != Some(&set.folder) {
                *c = Some((set.folder.clone(), folder_connectors(&set.folder)));
            }
            let list = c.as_ref().map(|x| x.1.clone()).unwrap_or_default();
            drop(c);
            rt.FillRectangle(&rect(x + 16.0, py, cw - 16.0, py + 1.0), &b(C_LINE)?);
            txt("Connectors", &g.small, rect(x + 17.0, py + 12.0, cw - 16.0, py + 34.0), &white);
            py += 40.0;
            if list.is_empty() {
                txt("None set up for this folder", &g.small, rect(x + 17.0, py, cw - 16.0, py + 24.0), &soft);
                py += 24.0;
            }
            for (name, kind) in &list {
                lucide(rt, "server", rect(x + 14.0, py, x + 32.0, py + 24.0), 14.0, &soft);
                txt(name, &g.small, rect(x + 41.0, py, cw - 90.0, py + 24.0), &white);
                txt(kind, &g.small, rect(cw - 16.0 - measure(g, kind) - 2.0, py, cw - 8.0, py + 24.0), &soft);
                py += 24.0;
            }
            py += 14.0;
        }

        // Outputs: the files this session wrote, newest first
        // ...those inside the session's own folder (scratch files elsewhere are not its outputs)
        let home = set.folder.replace('/', "\\").to_lowercase();
        let files: Vec<String> = outputs(&LOG.lock().unwrap()).into_iter().filter(|f| f.replace('/', "\\").to_lowercase().starts_with(&home)).collect();
        if !files.is_empty() {
            rt.FillRectangle(&rect(x + 16.0, py, cw - 16.0, py + 1.0), &b(C_LINE)?);
            txt("Outputs", &g.small, rect(x + 17.0, py + 12.0, cw - 16.0, py + 34.0), &white);
            py += 42.0;
            let shown = if ui.more_outputs { files.len() } else { files.len().min(5) };
            for (k, f) in files.iter().take(shown).enumerate() {
                if py + 32.0 > ch - 8.0 {
                    break;
                }
                let r = rect(x + 8.0, py, cw - 8.0, py + 28.0);
                if hov(Act::Output(k)) {
                    rt.FillRoundedRectangle(&rrect(r, 5.0), &b(C_HOVER)?);
                }
                let name = f.rsplit(['\\', '/']).next().unwrap_or(f);
                let code = ["rs", "py", "js", "ts", "tsx", "jsx", "c", "cpp", "h", "go", "java", "cs", "html", "css", "json", "toml"].contains(&name.rsplit('.').next().unwrap_or(""));
                lucide(rt, if code { "code" } else { "file" }, icon(r), 18.0, &soft);
                txt(name, &g.side, label(r), &white);
                hits.push((r, Act::Output(k)));
                py += 32.0;
            }
            if files.len() > 5 {
                let r = rect(x + 8.0, py, x + 120.0, py + 24.0);
                txt(if ui.more_outputs { "Show less" } else { "Show more" }, &g.small, rect(x + 17.0, py, cw - 16.0, py + 24.0), &dim);
                hits.push((r, Act::MoreOutputs));
                py += 32.0;
            }
        }
        outputs_list = files;
        let tasks_top = py;
        let todos = TODOS.lock().unwrap().clone();
        if !todos.is_empty() {
            rt.FillRectangle(&rect(x + 16.0, tasks_top, cw - 16.0, tasks_top + 1.0), &b(C_LINE)?);
            let done = todos.iter().filter(|t| t.1 == "completed").count();
            txt(&format!("Tasks  {done}/{}", todos.len()), &g.small, rect(x + 17.0, tasks_top + 10.0, cw - 16.0, tasks_top + 32.0), &white);
            for (k, (t, status)) in todos.iter().enumerate() {
                let yy = tasks_top + 40.0 + k as f32 * 26.0;
                if yy + 24.0 > ch {
                    break;
                }
                let (gl, col) = match status.as_str() {
                    "completed" => ("\u{2713}", C_DIM),
                    "in_progress" => ("\u{25CF}", C_ACCENT),
                    _ => ("\u{25CB}", C_DIM),
                };
                txt(gl, &g.small, rect(x + 16.0, yy, x + 34.0, yy + 22.0), &b(col)?);
                txt(t, &g.small, rect(x + 38.0, yy, cw - 16.0, yy + 22.0), &b(if status == "completed" { C_DIM } else { 0xFFFFFF })?);
            }
        }
    }

    // ---- conversation ----
    let main_l = SIDE + 1.0;
    let main_r = cw - right;
    let width = COL.min(main_r - main_l - 48.0);
    let x0 = col_x(main_l, cw, main_r, width);
    let view_top = TOP;
    let card_h = task_card_h(&TODOS.lock().unwrap());
    let status_h = if BUSY.load(Relaxed) || card_h > 0.0 { 30.0 } else { 0.0 };
    let view_bot = ch - BOTTOM - 8.0 - card_h - status_h - if card_h > 0.0 { 8.0 } else { 0.0 };
    // Laying the whole conversation out again on every repaint made a long session crawl (85 ms a
    // frame at 4,000 entries): the laid-out items are kept and only rebuilt when something that
    // shapes them changes - the entries, the newest text (streaming), the width, what is expanded.
    let key = {
        let l = LOG.lock().unwrap();
        let tail: usize = l.iter().rev().take(3).map(|e| e.text.len() + e.kind.clone() as usize).sum();
        (l.len(), tail, width.to_bits(), ui.expanded.len(), ui.expanded.iter().sum::<usize>())
    };
    if ui.items_key != Some(key) {
        let log = LOG.lock().unwrap().clone();
        ui.items = build(g, &log, width, ui);
    }
    let log_is_empty = key.0 == 0;
    // moved out while drawing (so the rest of `ui` stays free to change); the key goes back with them
    let items = std::mem::take(&mut ui.items);
    ui.items_key = None;
    let total: f32 = items.iter().map(|(_, h)| h + 20.0).sum::<f32>() + 40.0;
    let max = (total - (view_bot - view_top)).max(0.0);
    if ui.stick {
        ui.scroll = max;
        // snapshots can look further up: SHELLBITS_SCROLL_UP=<pixels above the end>
        if let Some(up) = std::env::var("SHELLBITS_SCROLL_UP").ok().and_then(|v| v.parse::<f32>().ok()) {
            ui.scroll = max - up;
        }
    }
    ui.scroll = ui.scroll.clamp(0.0, max);
    rt.PushAxisAlignedClip(&rect(main_l, view_top, main_r, view_bot), D2D1_ANTIALIAS_MODE_ALIASED);
    if log_is_empty {
        txt("\u{2731}", &g.head, rect(x0, view_top + 160.0, x0 + width, view_top + 190.0), &b(C_ACCENT)?);
        txt("What should we work on?", &g.side, rect(x0, view_top + 192.0, x0 + width, view_top + 220.0), &white);
    }
    let mut y = view_top + 24.0 - ui.scroll;
    for (item, h) in &items {
        if y + h >= view_top && y <= view_bot {
            match item {
                Item::User(l, raw, thumbs) => {
                    let m = metrics(l);
                    let bw = m.widthIncludingTrailingWhitespace + 30.0;
                    let text_h = if raw.trim().is_empty() { 0.0 } else { m.height + 22.0 };
                    let r = rect(x0 + width - bw, y, x0 + width, y + text_h);
                    msgs.push((rect(r.left, y, r.right, y + h), raw.clone()));
                    if text_h > 0.0 {
                        rt.FillRoundedRectangle(&rrect(r, 12.0), &b(C_BUBBLE)?);
                        rt.DrawTextLayout(windows_numerics::Vector2 { X: r.left + 15.0, Y: y + 11.0 }, l, &white, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    }
                    // pictures: right-aligned in a row under the bubble, rounded corners
                    let mut px = x0 + width;
                    let py = y + if text_h > 0.0 { text_h + 8.0 } else { 0.0 };
                    for (bm, tw, th) in thumbs.iter().rev() {
                        let pr = rect(px - tw, py, px, py + th);
                        let layer = rt.CreateLayer(None)?;
                        let geo: ID2D1Factory = rt.GetFactory()?;
                        let rr = geo.CreateRoundedRectangleGeometry(&rrect(pr, 8.0))?;
                        let params = D2D1_LAYER_PARAMETERS { contentBounds: pr, geometricMask: std::mem::ManuallyDrop::new(Some(rr.into())), maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, maskTransform: windows_numerics::Matrix3x2::identity(), opacity: 1.0, opacityBrush: std::mem::ManuallyDrop::new(None), layerOptions: D2D1_LAYER_OPTIONS_NONE };
                        rt.PushLayer(&params, &layer);
                        rt.DrawBitmap(bm, Some(&pr), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                        rt.PopLayer();
                        px -= tw + 8.0;
                    }
                }
                Item::Text(parts, raw) => {
                    msgs.push((rect(x0, y, x0 + width, y + h), raw.clone()));
                    let mut yy = y;
                    for part in parts {
                        match part {
                            Part::Code(l) => {
                                let m = metrics(l);
                                let r = rect(x0, yy, x0 + width, yy + m.height + 28.0);
                                rt.FillRoundedRectangle(&rrect(r, 8.0), &b(0x141414)?);
                                rt.DrawRoundedRectangle(&rrect(rect(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5), 8.0), &b(C_LINE)?, 1.0, None);
                                rt.DrawTextLayout(windows_numerics::Vector2 { X: x0 + 14.0, Y: yy + 14.0 }, l, &b(0xE6E6E6)?, D2D1_DRAW_TEXT_OPTIONS_NONE);
                                yy += m.height + 38.0;
                            }
                            Part::Text(l, part_links) => {
                                // each link becomes a click zone over the boxes its text occupies
                                for (start, len, url) in part_links {
                                    let mut boxes = [DWRITE_HIT_TEST_METRICS::default(); 8];
                                    let mut n = 0u32;
                                    if safe_link(url) && l.HitTestTextRange(*start, *len, x0, yy, Some(&mut boxes), &mut n).is_ok() {
                                        for m in &boxes[..(n as usize).min(8)] {
                                            hits.push((rect(m.left, m.top, m.left + m.width, m.top + m.height), Act::Link(links.len())));
                                        }
                                        links.push(url.clone());
                                    }
                                }
                                rt.DrawTextLayout(windows_numerics::Vector2 { X: x0, Y: yy }, l, &white, D2D1_DRAW_TEXT_OPTIONS_NONE);
                                yy += metrics(l).height + 10.0;
                            }
                            Part::Table { widths, rows } => {
                                // header row, then rows parted by hairlines; no vertical rules
                                let total: f32 = widths.iter().sum();
                                for (r, (rh, cells)) in rows.iter().enumerate() {
                                    let mut cx = x0;
                                    for (c, l) in cells.iter().enumerate() {
                                        rt.DrawTextLayout(windows_numerics::Vector2 { X: cx + CELL_PAD, Y: yy + 8.0 }, l, &white, D2D1_DRAW_TEXT_OPTIONS_CLIP);
                                        cx += widths[c];
                                    }
                                    yy += rh;
                                    rt.FillRectangle(&rect(x0, yy - 1.0, x0 + total, yy), &b(if r == 0 { 0x4A4A4A } else { C_LINE })?);
                                }
                                yy += 10.0;
                            }
                        }
                    }
                }
                Item::Group { start, label, lines, open } => {
                    let r = rect(x0, y, x0 + width, y + 22.0);
                    let col = if hov(Act::Group(*start, *open)) { 0xD0D0D0 } else { C_DIM };
                    txt(&format!("{label}  {}", if *open { "\u{02C5}" } else { "\u{203A}" }), &g.side, r, &b(col)?);
                    hits.push((r, Act::Group(*start, *open)));
                    let mut yy = y + 26.0;
                    for l in lines {
                        let lh = metrics(l).height;
                        rt.FillRectangle(&rect(x0 + 6.0, yy, x0 + 7.0, yy + lh), &b(0x3A3A3A)?);
                        rt.DrawTextLayout(windows_numerics::Vector2 { X: x0 + 18.0, Y: yy }, l, &b(0xA8A8A8)?, D2D1_DRAW_TEXT_OPTIONS_NONE);
                        yy += lh + 6.0;
                    }
                }
                Item::Permission(i, l) => {
                    let r = rect(x0, y, x0 + width, y + h);
                    rt.FillRoundedRectangle(&rrect(r, 10.0), &b(0x232323)?);
                    rt.DrawRoundedRectangle(&rrect(rect(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5), 10.0), &b(0x4A4A4A)?, 1.0, None);
                    rt.DrawTextLayout(windows_numerics::Vector2 { X: x0 + 16.0, Y: y + 14.0 }, l, &white, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    let by = r.bottom - 44.0;
                    let ar = rect(x0 + 16.0, by, x0 + 106.0, by + 30.0);
                    let dr = rect(x0 + 114.0, by, x0 + 204.0, by + 30.0);
                    rt.FillRoundedRectangle(&rrect(ar, 6.0), &b(if hov(Act::Allow(*i)) { 0xF0F0F0 } else { 0xE6E6E6 })?);
                    txt("Allow", &g.head, rect(ar.left + 26.0, ar.top, ar.right, ar.bottom), &b(0x111111)?);
                    rt.FillRoundedRectangle(&rrect(dr, 6.0), &b(if hov(Act::Deny(*i)) { 0x3A3A3A } else { 0x2E2E2E })?);
                    txt("Deny", &g.head, rect(dr.left + 28.0, dr.top, dr.right, dr.bottom), &white);
                    hits.push((ar, Act::Allow(*i)));
                    hits.push((dr, Act::Deny(*i)));
                }
                Item::Info(l) => rt.DrawTextLayout(windows_numerics::Vector2 { X: x0, Y: y }, l, &dim, D2D1_DRAW_TEXT_OPTIONS_NONE),
            }
        }
        y += h + 20.0;
    }
    // copy button under the message the mouse is on (a reply: left, your message: right)
    if let Some((r, _)) = ui.hover_msg.and_then(|i| msgs.get(i)).cloned() {
        let i = ui.hover_msg.unwrap_or(0);
        let mine = r.right >= x0 + width - 1.0 && r.left > x0 + 1.0;
        let bx = if mine { r.right - 28.0 } else { r.left };
        let br = rect(bx, r.bottom + 2.0, bx + 28.0, r.bottom + 26.0);
        if hov(Act::CopyMsg(i)) {
            rt.FillRoundedRectangle(&rrect(br, 5.0), &b(C_HOVER)?);
        }
        lucide(rt, "copy", br, 14.0, &dim);
        hits.push((br, Act::CopyMsg(i)));
    }
    rt.PopAxisAlignedClip();
    ui.items = items;
    ui.items_key = Some(key);

    // ---- task card + status line, pinned above the message box (as in the app) ----
    let todos = TODOS.lock().unwrap().clone();
    let mut sy = ch - BOTTOM - 8.0 - status_h;
    if card_h > 0.0 {
        let top = sy - card_h - 8.0;
        let card = rect(x0 - 9.0, top, x0 + width + 15.0, top + card_h);
        rt.FillRoundedRectangle(&rrect(card, 10.0), &b(0x1F1F1F)?);
        rt.DrawRoundedRectangle(&rrect(rect(card.left + 0.5, card.top + 0.5, card.right - 0.5, card.bottom - 0.5), 10.0), &b(C_LINE)?, 1.0, None);
        for (k, (t, status)) in todos.iter().take(8).enumerate() {
            let yy = top + 8.0 + k as f32 * 28.0;
            let c = windows_numerics::Vector2 { X: x0 + 9.0, Y: yy + 14.0 };
            match status.as_str() {
                "completed" => {
                    rt.FillEllipse(&D2D1_ELLIPSE { point: c, radiusX: 6.0, radiusY: 6.0 }, &b(0x5A5A5A)?);
                    txt("\u{2713}", &g.small, rect(c.X - 6.0, yy, c.X + 7.0, yy + 28.0), &white);
                }
                "in_progress" => {
                    rt.DrawEllipse(&D2D1_ELLIPSE { point: c, radiusX: 6.0, radiusY: 6.0 }, &b(C_ACCENT)?, 1.5, None);
                    rt.FillEllipse(&D2D1_ELLIPSE { point: c, radiusX: 3.0, radiusY: 3.0 }, &b(C_ACCENT)?);
                }
                _ => rt.DrawEllipse(&D2D1_ELLIPSE { point: c, radiusX: 6.0, radiusY: 6.0 }, &b(0x8C8C8C)?, 1.0, None),
            }
            txt(t, &g.small, rect(x0 + 24.0, yy, x0 + width, yy + 28.0), &b(if status == "completed" { C_DIM } else { 0xE8E8E8 })?);
        }
    }
    if status_h > 0.0 {
        // spinner (a turning asterisk), what is happening, how long, and a chevron
        let busy = BUSY.load(Relaxed);
        let spin = ["\u{2722}", "\u{2733}", "\u{2734}", "\u{2735}"][ui.anim as usize % 4];
        txt(if busy { spin } else { "\u{2713}" }, &g.small, rect(x0 + 2.0, sy, x0 + 20.0, sy + 26.0), &b(C_ACCENT)?);
        let what = todos.iter().find(|t| t.1 == "in_progress").map(|t| t.0.clone());
        let ms = windows::Win32::System::SystemInformation::GetTickCount64().saturating_sub(BUSY_SINCE.load(Relaxed));
        let mut line = match (&what, busy) {
            (Some(w), true) => format!("{w}  {}", status_line(ms).trim_start_matches("Working\u{2026} ")),
            (None, true) => status_line(ms),
            (Some(w), false) => w.clone(),
            (None, false) => String::new(),
        };
        let queued = QUEUE.lock().unwrap().len();
        if queued > 0 {
            line += &format!("  \u{00B7}  {queued} queued");
        }
        txt(&format!("{line}  \u{203A}"), &g.small, rect(x0 + 24.0, sy, x0 + width, sy + 26.0), &dim);
        sy += status_h;
    }
    let _ = sy;

    // ---- composer ----
    let cr = rect(x0 - 9.0, ch - BOTTOM, x0 + width + 15.0, ch - BOTTOM + COMPOSER_H);
    rt.FillRoundedRectangle(&rrect(cr, 12.0), &b(C_LINE)?);
    rt.DrawRoundedRectangle(&rrect(rect(cr.left + 0.5, cr.top + 0.5, cr.right - 0.5, cr.bottom - 0.5), 12.0), &b(C_SEL)?, 1.0, None);
    let plus = rect(cr.left + 6.0, cr.top + 6.0, cr.left + 38.0, cr.bottom - 6.0);
    if hov(Act::Attach) {
        rt.FillRoundedRectangle(&rrect(plus, 6.0), &b(0x3A3A3A)?);
    }
    lucide(rt, "plus", plus, 18.0, &white);
    hits.push((plus, Act::Attach));
    let busy = BUSY.load(Relaxed);
    let sr = rect(cr.right - 40.0, cr.top + 6.0, cr.right - 8.0, cr.bottom - 6.0);
    let mr = rect(sr.left - 34.0, sr.top, sr.left - 2.0, sr.bottom);
    if hov(Act::Mic) {
        rt.FillRoundedRectangle(&rrect(mr, 6.0), &b(0x3A3A3A)?);
    }
    lucide(rt, "mic", mr, 17.0, &soft);
    hits.push((mr, Act::Mic));
    if hov(Act::Stop) || hov(Act::Send) {
        rt.FillRoundedRectangle(&rrect(sr, 6.0), &b(0x3A3A3A)?);
    }
    if busy {
        rt.FillRoundedRectangle(&rrect(rect(sr.left + 10.0, sr.top + 10.0, sr.right - 10.0, sr.bottom - 10.0), 2.0), &white);
        hits.push((sr, Act::Stop));
    } else {
        lucide(rt, "arrow-up", sr, 18.0, &white);
        hits.push((sr, Act::Send));
    }
    if ui.gfx.is_some() && std::env::var_os("SHELLBITS_SNAPSHOT").is_some() {
        txt("Enter to send, Ctrl+Enter to queue for later", &g.side, rect(cr.left + 43.0, cr.top, cr.right - 48.0, cr.bottom), &b(0x8C8C8C)?);
    }
    // model / effort / permissions row
    let model = if set.model.is_empty() { MODEL_SHOWN.lock().unwrap().clone() } else { cap(&set.model) };
    let model = if model.is_empty() { "Default model".to_string() } else { model };
    let effort = cap(&set.effort);
    let mode = match set.mode.as_str() {
        "acceptEdits" => "Accept edits",
        "plan" => "Plan mode",
        "bypassPermissions" => "Bypass permissions",
        _ => "Ask permissions",
    };
    let row_y = ch - BOTTOM + COMPOSER_H + 8.0;
    let (mw, ew, pw) = (measure(g, &model) + 6.0, measure(g, &effort) + 6.0, measure(g, mode) + 6.0);
    let total_w = mw + ew + pw + 24.0;
    let mut rx = (x0 + width / 2.0 - total_w / 2.0).round();
    for (label, w_, act, col) in [(model.as_str(), mw, Act::Model, 0xFFFFFF), (effort.as_str(), ew, Act::Effort, C_DIM), (mode, pw, Act::Mode, 0xFFFFFF)] {
        let r = rect(rx - 4.0, row_y, rx + w_ + 4.0, row_y + 24.0);
        if hov(act) {
            rt.FillRoundedRectangle(&rrect(r, 5.0), &b(C_HOVER)?);
        }
        txt(label, &g.small, rect(rx, row_y, rx + w_ + 20.0, row_y + 24.0), &b(col)?);
        hits.push((r, act));
        rx += w_ + 12.0;
    }
    // the round context meter after the row
    if let Some(p) = *CONTEXT.lock().unwrap() {
        let c = windows_numerics::Vector2 { X: rx + 6.0, Y: row_y + 12.0 };
        rt.DrawEllipse(&D2D1_ELLIPSE { point: c, radiusX: 6.0, radiusY: 6.0 }, &b(0x3A3A3A)?, 2.0, None);
        let f: ID2D1Factory = rt.GetFactory()?;
        let geo = f.CreatePathGeometry()?;
        let sink = geo.Open()?;
        let a = (p.clamp(0.0, 99.9) / 100.0) * std::f32::consts::TAU;
        sink.BeginFigure(windows_numerics::Vector2 { X: c.X, Y: c.Y - 6.0 }, D2D1_FIGURE_BEGIN_HOLLOW);
        sink.AddArc(&D2D1_ARC_SEGMENT { point: windows_numerics::Vector2 { X: c.X + 6.0 * a.sin(), Y: c.Y - 6.0 * a.cos() }, size: D2D_SIZE_F { width: 6.0, height: 6.0 }, rotationAngle: 0.0, sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE, arcSize: if a > std::f32::consts::PI { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL } });
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
        sink.Close()?;
        rt.DrawGeometry(&geo, &b(if p > 90.0 { 0xF2B04D } else { C_ACCENT })?, 2.0, None);
    }
    ui.hits = hits;
    ui.links = links;
    ui.group_names = group_names;
    ui.outputs = outputs_list;
    ui.msgs = msgs;
    Ok(())
}

/// The app centres the column over the whole area right of the sidebar, unless that would run
/// under the right panel.
fn col_x(left: f32, full_right: f32, visible_right: f32, width: f32) -> f32 {
    let x = ((left + full_right - width) / 2.0).round();
    x.min(visible_right - 24.0 - width).max(left + 24.0)
}

/// Stroke a Lucide icon (24x24 grid, 2px strokes, round caps) into `r` at `size` DIPs, like the app.
unsafe fn lucide(rt: &ID2D1DCRenderTarget, name: &str, r: D2D_RECT_F, size: f32, br: &ID2D1SolidColorBrush) {
    let Some((_, ops)) = crate::lucide::ICONS.iter().find(|(n, _)| *n == name) else { return };
    let Ok(f) = rt.GetFactory() else { return };
    let Ok(path) = f.CreatePathGeometry() else { return };
    let Ok(sink) = path.Open() else { return };
    let s = size / 24.0;
    let (ox, oy) = (((r.left + r.right) / 2.0 - size / 2.0).round(), ((r.top + r.bottom) / 2.0 - size / 2.0).round());
    let pt = |x: f32, y: f32| windows_numerics::Vector2 { X: ox + x * s, Y: oy + y * s };
    let (mut i, mut open) = (0, false);
    while i < ops.len() {
        match ops[i] as u32 {
            0 => {
                if open {
                    sink.EndFigure(D2D1_FIGURE_END_OPEN);
                }
                sink.BeginFigure(pt(ops[i + 1], ops[i + 2]), D2D1_FIGURE_BEGIN_HOLLOW);
                open = true;
                i += 3;
            }
            1 => {
                sink.AddLine(pt(ops[i + 1], ops[i + 2]));
                i += 3;
            }
            2 => {
                sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: pt(ops[i + 1], ops[i + 2]), point2: pt(ops[i + 3], ops[i + 4]), point3: pt(ops[i + 5], ops[i + 6]) });
                i += 7;
            }
            _ => {
                if open {
                    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                    open = false;
                }
                i += 1;
            }
        }
    }
    if open {
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
    }
    let _ = sink.Close();
    let props = D2D1_STROKE_STYLE_PROPERTIES { startCap: D2D1_CAP_STYLE_ROUND, endCap: D2D1_CAP_STYLE_ROUND, dashCap: D2D1_CAP_STYLE_ROUND, lineJoin: D2D1_LINE_JOIN_ROUND, ..Default::default() };
    let style = f.CreateStrokeStyle(&props, None).ok();
    rt.DrawGeometry(&path, br, 2.0 * s, style.as_ref());
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

unsafe fn measure_side(g: &Gfx, s: &str) -> f32 {
    g.dw.CreateTextLayout(&utf16(s), &g.side, 2000.0, 30.0).map(|l| metrics(&l).widthIncludingTrailingWhitespace).unwrap_or(100.0)
}

unsafe fn measure(g: &Gfx, s: &str) -> f32 {
    g.dw.CreateTextLayout(&utf16(s), &g.small, 1000.0, 30.0).map(|l| metrics(&l).widthIncludingTrailingWhitespace).unwrap_or(60.0)
}

/// The sidebar's groups, in order: Pinned first (if any), then one per folder; each lists session
/// indexes. Pinned sessions are not repeated under their folder. `filter` keeps titles that contain it.
pub fn grouped(sessions: &[SessionInfo], pins: &[String], filter: &str) -> Vec<(String, Vec<usize>)> {
    let f = filter.to_lowercase();
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    let mut pinned = Vec::new();
    for (i, s) in sessions.iter().enumerate() {
        if !f.is_empty() && !s.title.to_lowercase().contains(&f) {
            continue;
        }
        if pins.contains(&s.id) {
            pinned.push(i);
            continue;
        }
        let name = folder_label(&s.cwd);
        match groups.iter_mut().find(|g| g.0 == name) {
            Some(g) => g.1.push(i),
            None => groups.push((name, vec![i])),
        }
    }
    if !pinned.is_empty() {
        groups.insert(0, ("Pinned".into(), pinned));
    }
    groups
}

/// Session ids pinned to the top of the sidebar (kept by this app; the real app's pins live in its own store).
static PINS: Mutex<Option<Vec<String>>> = Mutex::new(None);
fn pins_file() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join("shellbits").join("claude-pins.txt")
}
fn pins() -> Vec<String> {
    let mut all = own_pins();
    all.extend(STARRED.lock().unwrap().iter().filter(|s| !all.contains(s)).cloned().collect::<Vec<_>>());
    all
}
fn own_pins() -> Vec<String> {
    PINS.lock().unwrap().get_or_insert_with(|| std::fs::read_to_string(pins_file()).unwrap_or_default().lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()).clone()
}
fn toggle_pin(id: &str) {
    let mut list = own_pins();
    match list.iter().position(|p| p == id) {
        Some(i) => _ = list.remove(i),
        None => list.push(id.to_string()),
    }
    let f = pins_file();
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(f, list.join("
"));
    *PINS.lock().unwrap() = Some(list);
}

fn folder_label(cwd: &str) -> String {
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    if cwd.contains("scratch-workspaces") {
        return "This computer".into();
    }
    if cwd.eq_ignore_ascii_case(&home) {
        return "Home".into();
    }
    cwd.rsplit(['\\', '/']).next().unwrap_or(cwd).to_string()
}

// ======================= window =======================

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let params = BP_PAINTPARAMS { cbSize: size_of::<BP_PAINTPARAMS>() as u32, dwFlags: BPPF_ERASE, ..Default::default() };
    let mut mem = HDC::default();
    let pb = BeginBufferedPaint(hdc, &rc, BPBF_TOPDOWNDIB, Some(&params), &mut mem);
    if !mem.is_invalid() {
        let dpi = GetDpiForWindow(hwnd) as f32;
        let maximized = IsZoomed(hwnd).as_bool();
        UI.with(|u| {
            let mut ui = u.borrow_mut();
            if ui.gfx.is_none() {
                ui.gfx = make_gfx().ok();
            }
            let Some(rt) = ui.gfx.as_ref().map(|g| g.rt.clone()) else { return };
            let _ = rt.BindDC(mem, &rc);
            rt.SetDpi(dpi, dpi);
            rt.BeginDraw();
            let _ = draw(&mut ui, rc.right as f32 * 96.0 / dpi, rc.bottom as f32 * 96.0 / dpi, maximized);
            let _ = rt.EndDraw(None, None);
        });
        let _ = BufferedPaintSetAlpha(pb, None, 255);
        let _ = EndBufferedPaint(pb, true);
    }
    let _ = EndPaint(hwnd, &ps);
}

unsafe fn place_edit(hwnd: HWND) {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    let (cw, ch) = (rc.right as f32 / s, rc.bottom as f32 / s);
    let (right, term) = UI.with(|u| {
        let u = u.borrow();
        (match u.pane { Some(_) => PANE_W.min(cw * 0.45), None if u.panel => RIGHT, None => 0.0 }, u.pane == Some(Pane::Terminal))
    });
    let t = HWND(TEDIT.load(Relaxed) as *mut c_void);
    if term {
        let x = cw - right;
        let _ = SetWindowPos(t, None, ((x + 36.0) * s) as i32, ((ch - 8.0 - 30.0) * s) as i32, ((right - 56.0) * s) as i32, (18.0 * s) as i32, SWP_NOZORDER | SWP_SHOWWINDOW);
    } else {
        let _ = ShowWindow(t, SW_HIDE);
    }
    let width = COL.min(cw - right - SIDE - 49.0);
    let x0 = col_x(SIDE + 1.0, cw, cw - right, width);
    let e = HWND(EDIT.load(Relaxed) as *mut c_void);
    let top = ch - BOTTOM + 12.0;
    let _ = SetWindowPos(e, None, ((x0 + 34.0) * s) as i32, (top * s) as i32, ((width - 70.0) * s) as i32, ((COMPOSER_H - 22.0) * s) as i32, SWP_NOZORDER);
}

static EDIT_PROC: AtomicIsize = AtomicIsize::new(0);

/// The commands the slash menu offers: Claude Code's own, then the user's skills and commands.
pub fn slash_commands(skills: &[String], commands: &[String]) -> Vec<String> {
    let mut v: Vec<String> = ["compact", "clear", "context", "cost", "init", "review", "security-review", "memory", "model"].iter().map(|s| s.to_string()).collect();
    for n in skills.iter().chain(commands) {
        if !v.contains(n) {
            v.push(n.clone());
        }
    }
    v
}

/// Names in ~/.claude/skills (folders) and ~/.claude/commands (*.md files).
fn user_commands() -> (Vec<String>, Vec<String>) {
    let home = std::path::PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join(".claude");
    let names = |dir: &str, md: bool| -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(home.join(dir)).into_iter().flatten().flatten()
            .filter_map(|e| {
                let p = e.path();
                if md { if p.extension()? == "md" { Some(p.file_stem()?.to_string_lossy().to_string()) } else { None } } else { p.is_dir().then(|| e.file_name().to_string_lossy().to_string()) }
            })
            .collect();
        v.sort();
        v
    };
    (names("skills", false), names("commands", true))
}

/// Typing '/' into an empty box opens the command list, like the app; the pick goes into the box.
unsafe fn slash_menu(edit: HWND) -> bool {
    let (skills, cmds) = user_commands();
    let list = slash_commands(&skills, &cmds);
    let Ok(m) = CreatePopupMenu() else { return false };
    for (k, n) in list.iter().enumerate() {
        let _ = AppendMenuW(m, MF_STRING, k + 1, &windows::core::HSTRING::from(format!("/{n}")));
    }
    let mut r = RECT::default();
    let _ = GetWindowRect(edit, &mut r);
    let parent = GetParent(edit).unwrap_or(edit);
    let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_BOTTOMALIGN, r.left, r.top - 4, None, parent, None).0;
    let _ = DestroyMenu(m);
    let text = if cmd > 0 { format!("/{} ", list[cmd as usize - 1]) } else { "/".to_string() };
    let _ = SetWindowTextW(edit, &windows::core::HSTRING::from(text.as_str()));
    let n = text.encode_utf16().count();
    SendMessageW(edit, EM_SETSEL, Some(WPARAM(n)), Some(LPARAM(n as isize)));
    true
}

unsafe fn picture_only() -> bool {
    use windows::Win32::System::DataExchange::IsClipboardFormatAvailable;
    IsClipboardFormatAvailable(2).is_ok() && IsClipboardFormatAvailable(13).is_err()
}

/// Files and folders under `dir` for the @ menu: two levels deep, build/dependency folders skipped,
/// paths relative with forward slashes, folders first, at most `max`.
pub fn mention_list(dir: &std::path::Path, max: usize) -> Vec<String> {
    let mut v = Vec::new();
    fn rec(d: &std::path::Path, prefix: &str, depth: usize, v: &mut Vec<String>) {
        const SKIP: [&str; 6] = ["target", "node_modules", ".git", "dist", "build", "__pycache__"];
        let mut es: Vec<(String, bool)> = std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| (e.file_name().to_string_lossy().to_string(), e.path().is_dir())).filter(|(n, _)| !SKIP.contains(&n.as_str())).collect();
        es.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.to_lowercase().cmp(&b.0.to_lowercase())));
        for (n, is_dir) in es {
            let rel = format!("{prefix}{n}{}", if is_dir { "/" } else { "" });
            v.push(rel.clone());
            if is_dir && depth > 0 {
                rec(&d.join(&n), &rel, depth - 1, v);
            }
        }
    }
    rec(dir, "", 1, &mut v);
    v.truncate(max);
    v
}

/// Typing '@' at the start or after a space lists the session folder's files; the pick is inserted.
unsafe fn mention_menu(edit: HWND) -> bool {
    let list = mention_list(std::path::Path::new(&settings().folder), 60);
    if list.is_empty() {
        return false;
    }
    let Ok(m) = CreatePopupMenu() else { return false };
    for (k, n) in list.iter().enumerate() {
        let _ = AppendMenuW(m, MF_STRING, k + 1, &windows::core::HSTRING::from(n.as_str()));
    }
    let mut r = RECT::default();
    let _ = GetWindowRect(edit, &mut r);
    let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_BOTTOMALIGN, r.left, r.top - 4, None, GetParent(edit).unwrap_or(edit), None).0;
    let _ = DestroyMenu(m);
    let ins = if cmd > 0 {
        let p = &list[cmd as usize - 1];
        if p.contains(' ') { format!("@\"{p}\" ") } else { format!("@{p} ") }
    } else {
        "@".to_string()
    };
    SendMessageW(edit, EM_REPLACESEL, Some(WPARAM(1)), Some(LPARAM(windows::core::HSTRING::from(ins.as_str()).as_ptr() as isize)));
    true
}

/// Ctrl+V with a picture on the clipboard: save it as a PNG and mention it, so it is sent as an image.
unsafe fn paste_picture(edit: HWND) -> bool {
    use windows::Win32::System::DataExchange::*;
    if IsClipboardFormatAvailable(2).is_err() || IsClipboardFormatAvailable(13).is_ok() {
        return false; // no picture, or text is there too: normal paste
    }
    if OpenClipboard(Some(edit)).is_err() {
        return false;
    }
    let bmp = GetClipboardData(2).map(|h| HBITMAP(h.0));
    let t = windows::Win32::System::SystemInformation::GetLocalTime();
    let path = std::env::temp_dir().join(format!("claude-paste-{:02}{:02}{:02}{:03}.png", t.wHour, t.wMinute, t.wSecond, t.wMilliseconds));
    let ok = bmp.is_ok_and(|b| crate::snip::save_png_at(b, &path).is_ok());
    let _ = CloseClipboard();
    if ok {
        let mention = format!("@\"{}\" ", path.display());
        SendMessageW(edit, EM_REPLACESEL, Some(WPARAM(1)), Some(LPARAM(windows::core::HSTRING::from(mention.as_str()).as_ptr() as isize)));
    }
    ok
}

unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_CHAR && wp.0 == '/' as usize && GetWindowTextLengthW(hwnd) == 0 && slash_menu(hwnd) {
        return LRESULT(0);
    }
    if msg == WM_CHAR && wp.0 == '@' as usize {
        // only at a word start: not inside an e-mail address
        let t = edit_text(hwnd);
        let mut caret = 0u32;
        SendMessageW(hwnd, EM_GETSEL, Some(WPARAM(&mut caret as *mut _ as usize)), None);
        let before: Vec<u16> = t.encode_utf16().take(caret as usize).collect();
        if before.last().is_none_or(|c| *c == b' ' as u16 || *c == b'\n' as u16) && mention_menu(hwnd) {
            return LRESULT(0);
        }
    }
    if (msg == WM_PASTE || (msg == WM_KEYDOWN && wp.0 == 'V' as usize && GetKeyState(VK_CONTROL.0 as i32) < 0)) && paste_picture(hwnd) {
        return LRESULT(0);
    }
    if msg == WM_CHAR && wp.0 == 0x16 && picture_only() {
        return LRESULT(0); // the Ctrl+V character after a picture paste
    }
    if msg == WM_KEYDOWN && wp.0 == VK_RETURN.0 as usize && GetKeyState(VK_SHIFT.0 as i32) >= 0 {
        submit(hwnd);
        return LRESULT(0);
    }
    if msg == WM_CHAR && wp.0 == 13 && GetKeyState(VK_SHIFT.0 as i32) >= 0 {
        return LRESULT(0);
    }
    let prev: WNDPROC = std::mem::transmute(EDIT_PROC.load(Relaxed));
    let r = CallWindowProcW(prev, hwnd, msg, wp, lp);
    if msg == WM_PAINT && GetWindowTextLengthW(hwnd) == 0 {
        // the app's placeholder text
        let dc = GetDC(Some(hwnd));
        let font = HFONT(SendMessageW(hwnd, WM_GETFONT, None, None).0 as *mut c_void);
        let old = SelectObject(dc, font.into());
        SetTextColor(dc, COLORREF(0x8C8C8C));
        SetBkMode(dc, TRANSPARENT);
        let mut t: Vec<u16> = "Enter to send, Ctrl+Enter to queue for later".encode_utf16().collect();
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        DrawTextW(dc, &mut t, &mut rc, DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
        SelectObject(dc, old);
        ReleaseDC(Some(hwnd), dc);
    }
    if msg == WM_SETFOCUS || msg == WM_KILLFOCUS || msg == WM_CHAR {
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
    r
}

unsafe fn edit_text(e: HWND) -> String {
    let n = GetWindowTextLengthW(e);
    let mut buf = vec![0u16; n as usize + 1];
    GetWindowTextW(e, &mut buf);
    String::from_utf16_lossy(&buf[..n as usize])
}

unsafe fn submit(edit: HWND) {
    let text = edit_text(edit);
    // Ctrl+Enter while a reply is being written: queue it for later, like the app
    if BUSY.load(Relaxed) && GetKeyState(VK_CONTROL.0 as i32) < 0 {
        if !text.trim().is_empty() {
            QUEUE.lock().unwrap().push(text);
            let _ = SetWindowTextW(edit, w!(""));
            notify();
        }
        return;
    }
    let _ = SetWindowTextW(edit, w!(""));
    UI.with(|u| u.borrow_mut().stick = true);
    send(text);
}

/// The account row's settings menu: what the real app offers there that this window can do.
unsafe fn gear_menu(hwnd: HWND) {
    let creds = std::path::PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join(".claude").join(".credentials.json");
    let signed_in = plan_signed_in(&std::fs::read_to_string(&creds).unwrap_or_default());
    let items: [(&str, &str); 7] = [
        ("Settings", "settings"),
        (if signed_in { "Signed in to your Claude plan" } else { "Sign in with your Claude plan..." }, "login"),
        ("Open Claude Code settings file", "file"),
        ("Keyboard shortcuts", "keys"),
        ("Get help", "help"),
        ("Learn more", "learn"),
        ("About this window", "about"),
    ];
    let Some(pick) = menu(hwnd, &items, "") else { return };
    let open = |what: &str, args: &str| {
        windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), &windows::core::HSTRING::from(what), &windows::core::HSTRING::from(args), PCWSTR::null(), SW_SHOWNORMAL);
    };
    let say = |title: &str, text: &str| {
        MessageBoxW(Some(hwnd), &windows::core::HSTRING::from(text), &windows::core::HSTRING::from(title), MB_OK | MB_ICONINFORMATION);
    };
    match pick.as_str() {
        "settings" => open("https://claude.ai/settings", ""),
        // signing in is the user's own step: it opens a terminal with Claude Code's own sign-in, which asks in the browser
        "login" if !signed_in => open("cmd.exe", &format!("/k \"{}\" auth login", claude_exe())),
        "login" => say("Claude plan", "The claude tool is signed in to a Claude account, so this window uses your plan."),
        "file" => {
            let f = std::path::PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join(".claude").join("settings.json");
            crate::notepad::request(&f.display().to_string());
            if let Ok(fly) = FindWindowW(w!("shellbits_flyout"), None) {
                let _ = PostMessageW(Some(fly), WM_HOTKEY, WPARAM(crate::NOTEPAD_ID as usize), LPARAM(0));
            }
        }
        "keys" => say("Keyboard shortcuts", "Enter  send\nShift+Enter  new line\nCtrl+Alt+C  open this window\nWheel over the list  scroll sessions\nRight-click a session  pin, rename, open folder\nRight-click a message  copy"),
        "help" => open("https://support.claude.com", ""),
        "learn" => open("https://docs.claude.com/en/docs/claude-code/overview", ""),
        _ => say("About", "Claude Code window from Rusty Windows.\nIt runs the official claude tool; your sessions are shared with the Claude app."),
    }
}

unsafe fn menu(hwnd: HWND, items: &[(&str, &str)], current: &str) -> Option<String> {
    let m = CreatePopupMenu().ok()?;
    for (k, (label, value)) in items.iter().enumerate() {
        let flags = if *value == current { MF_STRING | MF_CHECKED } else { MF_STRING };
        let _ = AppendMenuW(m, flags, k + 1, &windows::core::HSTRING::from(*label));
    }
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_BOTTOMALIGN, pt.x, pt.y, None, hwnd, None).0;
    let _ = DestroyMenu(m);
    (cmd > 0).then(|| items[cmd as usize - 1].1.to_string())
}

unsafe fn pick(hwnd: HWND, folder: bool) -> Option<String> {
    use windows::Win32::System::Com::*;
    use windows::Win32::UI::Shell::*;
    let d = CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_ALL).ok()?;
    if folder {
        let _ = d.SetOptions(FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM);
    }
    d.Show(Some(hwnd)).ok()?;
    let p = d.GetResult().and_then(|i| i.GetDisplayName(SIGDN_FILESYSPATH)).ok()?;
    let s = p.to_string().unwrap_or_default();
    CoTaskMemFree(Some(p.0 as *const c_void));
    Some(s)
}

fn new_session(folder: Option<String>) {
    stop_proc();
    LOG.lock().unwrap().clear();
    *LIVE.lock().unwrap() = None;
    *CONTEXT.lock().unwrap() = None;
    *WINDOW.lock().unwrap() = None;
    *CONTEXT_TOKENS.lock().unwrap() = None;
    MODEL_SHOWN.lock().unwrap().clear();
    set_settings(|s| {
        s.session = None;
        if let Some(f) = folder {
            s.folder = f;
        }
    });
}

unsafe fn click(hwnd: HWND, act: Act) {
    match act {
        Act::New => {
            if let Some(f) = pick(hwnd, true) {
                new_session(Some(f));
                UI.with(|u| u.borrow_mut().selected = None);
            }
        }
        Act::Search => {
            // type in the box, then click Search to filter the sessions (empty box clears it)
            let e = HWND(EDIT.load(Relaxed) as *mut c_void);
            let q = edit_text(e);
            let _ = SetWindowTextW(e, w!(""));
            UI.with(|u| u.borrow_mut().filter = q);
        }
        Act::Session(i) => {
            let Some(info) = SESSIONS.lock().unwrap().get(i).cloned() else { return };
            stop_proc();
            *LOG.lock().unwrap() = load_history(&info);
            *LIVE.lock().unwrap() = None;
            set_settings(|s| {
                s.folder = info.cwd.clone();
                s.session = Some(info.id.clone());
            });
            UI.with(|u| {
                let mut u = u.borrow_mut();
                u.selected = Some(i);
                u.stick = true;
                u.expanded.clear();
            });
        }
        Act::Group(start, open) => UI.with(|u| {
            let mut u = u.borrow_mut();
            if open {
                u.expanded.remove(&start);
            } else {
                u.expanded.insert(start);
            }
            u.stick = false;
        }),
        Act::Allow(i) => answer(i, true),
        Act::Deny(i) => answer(i, false),
        Act::Send => submit(HWND(EDIT.load(Relaxed) as *mut c_void)),
        Act::Stop => interrupt(),
        Act::Mic => {
            // Windows' own voice typing (Win+H) types into the message box
            let e = HWND(EDIT.load(Relaxed) as *mut c_void);
            let _ = SetFocus(Some(e));
            let k = |vk: VIRTUAL_KEY, up: bool| INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, ..Default::default() } } };
            SendInput(&[k(VK_LWIN, false), k(VK_H, false), k(VK_H, true), k(VK_LWIN, true)], size_of::<INPUT>() as i32);
        }
        Act::Compact => send("/compact".into()),
        Act::CopyMsg(i) => {
            if let Some(t) = UI.with(|u| u.borrow().msgs.get(i).map(|m| m.1.clone())) {
                crate::settings::copy_text(&t);
            }
        }
        Act::Attach => {
            if let Some(f) = pick(hwnd, false) {
                let e = HWND(EDIT.load(Relaxed) as *mut c_void);
                // quoted when the path has spaces, so the whole of it is taken as one mention
                let t = utf16(&if f.contains(' ') { format!("@\"{f}\" \0") } else { format!("@{f} \0") });
                SendMessageW(e, EM_REPLACESEL, Some(WPARAM(1)), Some(LPARAM(t.as_ptr() as isize)));
            }
        }
        Act::Model => {
            let cur = settings().model;
            if let Some(v) = menu(hwnd, &[("Default", ""), ("Opus", "opus"), ("Sonnet", "sonnet"), ("Haiku", "haiku")], &cur) {
                set_settings(|s| s.model = v);
                stop_proc(); // the next message resumes this session with the new model
            }
        }
        Act::Effort => {
            let cur = settings().effort;
            if let Some(v) = menu(hwnd, &[("Low", "low"), ("Medium", "medium"), ("High", "high"), ("Max", "max")], &cur) {
                set_settings(|s| s.effort = v);
                stop_proc();
            }
        }
        Act::Mode => {
            let cur = settings().mode;
            if let Some(v) = menu(hwnd, &[("Ask permissions", "default"), ("Accept edits", "acceptEdits"), ("Plan mode", "plan"), ("Bypass permissions", "bypassPermissions")], &cur) {
                set_settings(|s| s.mode = v);
                stop_proc();
            }
        }
        Act::Folder => {
            if let Some(f) = pick(hwnd, true) {
                new_session(Some(f));
            }
        }
        Act::Panel => {
            UI.with(|u| {
                let mut u = u.borrow_mut();
                u.panel = !u.panel;
            });
            place_edit(hwnd);
        }
        Act::Output(i) => {
            // open the file with whatever Windows opens it with (only a file that still exists)
            if let Some(f) = UI.with(|u| u.borrow().outputs.get(i).cloned()).filter(|f| std::path::Path::new(f).is_file()) {
                windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), &windows::core::HSTRING::from(f), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
            }
        }
        Act::PaneClose => {
            UI.with(|u| u.borrow_mut().pane = None);
            place_edit(hwnd);
        }
        Act::PaneUp => UI.with(|u| {
            let u = &mut *u.borrow_mut();
            if let Some(parent) = std::path::Path::new(&u.pane_dir).parent().map(|p| p.to_string_lossy().into_owned()).filter(|p| !p.is_empty()) {
                u.pane_files = list_dir(&parent);
                u.pane_dir = parent;
                u.pane_scroll = 0.0;
            }
        }),
        Act::PaneRow(i) if UI.with(|u| u.borrow().pane == Some(Pane::Preview)) => {
            if let Some((port, _)) = SERVERS.lock().unwrap().get(i).cloned() {
                windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), &windows::core::HSTRING::from(format!("http://localhost:{port}")), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
            }
        }
        Act::PaneRow(i) => {
            // a folder opens in the pane; a file opens with whatever Windows opens it with
            let file = UI.with(|u| {
                let u = &mut *u.borrow_mut();
                let (name, is_dir) = u.pane_files.get(i)?.clone();
                let full = std::path::Path::new(&u.pane_dir).join(name).to_string_lossy().into_owned();
                if is_dir {
                    u.pane_files = list_dir(&full);
                    u.pane_dir = full;
                    u.pane_scroll = 0.0;
                    return None;
                }
                Some(full)
            });
            if let Some(f) = file {
                windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), &windows::core::HSTRING::from(f), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
            }
        }
        Act::MoreOutputs => UI.with(|u| {
            let u = &mut *u.borrow_mut();
            u.more_outputs = !u.more_outputs;
        }),
        Act::Fold(i) => UI.with(|u| {
            let u = &mut *u.borrow_mut();
            if let Some(name) = u.group_names.get(i).cloned() {
                if !u.folded.remove(&name) {
                    u.folded.insert(name);
                }
            }
        }),
        Act::Link(i) => {
            if let Some(url) = UI.with(|u| u.borrow().links.get(i).cloned()).filter(|u| safe_link(u)) {
                windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), &windows::core::HSTRING::from(url), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
            }
        }
        Act::Gear => gear_menu(hwnd),
        Act::Web(k) => {
            let url = ["https://claude.ai/artifacts", "https://claude.ai/code/scheduled", "https://claude.ai/settings"][k as usize % 3];
            windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), &windows::core::HSTRING::from(url), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
        }
        Act::Tool(k) => {
            let folder = settings().folder;
            let run = |file: &str, args: &str| {
                windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), &windows::core::HSTRING::from(file), &windows::core::HSTRING::from(args), &windows::core::HSTRING::from(folder.as_str()), SW_SHOWNORMAL);
            };
            match k {
                0 | 2 => {
                    // Terminal / Preview pane, docked on the right (click again to close)
                    let want = if k == 0 { Pane::Terminal } else { Pane::Preview };
                    let open = UI.with(|u| {
                        let u = &mut *u.borrow_mut();
                        u.pane = if u.pane == Some(want) { None } else { Some(want) };
                        u.pane_scroll = f32::MAX;
                        u.pane.is_some()
                    });
                    if open && k == 0 {
                        if CONSOLE.lock().unwrap().is_none() {
                            *TERM_DIR.lock().unwrap() = folder.clone();
                            term_send(""); // start it so the prompt shows
                        }
                        place_edit(hwnd);
                        let _ = SetFocus(Some(HWND(TEDIT.load(Relaxed) as *mut c_void)));
                    } else {
                        place_edit(hwnd);
                    }
                    if open && k == 2 {
                        load_servers();
                    }
                }
                1 => {
                    // the Diff pane, docked on the right (click again to close)
                    let open = UI.with(|u| {
                        let u = &mut *u.borrow_mut();
                        u.pane = if u.pane == Some(Pane::Diff) { None } else { Some(Pane::Diff) };
                        u.pane_scroll = 0.0;
                        u.pane.is_some()
                    });
                    if open {
                        load_diff(folder.clone());
                    }
                }
                3 => UI.with(|u| {
                    // the Files pane, docked on the right (click again to close)
                    let u = &mut *u.borrow_mut();
                    u.pane = if u.pane == Some(Pane::Files) { None } else { Some(Pane::Files) };
                    u.pane_scroll = 0.0;
                    u.pane_files = list_dir(&folder);
                    u.pane_dir = folder.clone();
                }),
                _ => {
                    if let Some(v) = menu(hwnd, &[("Reload sessions", "reload"), ("Open session file folder", "files")], "") {
                        if v == "reload" {
                            std::thread::spawn(|| { *SESSIONS.lock().unwrap() = scan_sessions(); notify(); });
                        } else {
                            run("explorer.exe", &format!("\"{}\"", projects_dir().display()));
                        }
                    }
                }
            }
        }
        Act::Min => {
            let _ = ShowWindow(hwnd, SW_MINIMIZE);
        }
        Act::Max => {
            let _ = ShowWindow(hwnd, if IsZoomed(hwnd).as_bool() { SW_RESTORE } else { SW_MAXIMIZE });
        }
        Act::Close => {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
    let _ = InvalidateRect(Some(hwnd), None, false);
}

fn hit(x: f32, y: f32) -> Option<Act> {
    UI.with(|u| u.try_borrow().ok().and_then(|ui| ui.hits.iter().rev().find(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom).map(|(_, a)| *a)))
}

pub unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // Re-entered while the window's state is in use: a handler called into Windows (a shell call, a
    // menu, a dialog) and that ran a message loop. Touching the state again would panic and close the
    // window, so this message gets default handling instead.
    if UI.with(|u| u.try_borrow_mut().is_err()) && !matches!(msg, WM_NCCREATE | WM_CREATE | WM_NCDESTROY | WM_DESTROY) {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let s = GetDpiForWindow(hwnd) as f32 / 96.0;
    let dips = |lp: LPARAM| ((lp.0 & 0xFFFF) as i16 as f32 / s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / s);
    match msg {
        WM_CREATE => {
            let inst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).map(|m| HINSTANCE(m.0)).ok();
            let e = CreateWindowExW(WINDOW_EX_STYLE(0), w!("EDIT"), w!(""), WS_CHILD | WS_VISIBLE | WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32), 0, 0, 10, 10, Some(hwnd), None, inst, None).unwrap_or_default();
            let font = CreateFontW(-(15.0 * s) as i32, 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI Variable Text"));
            SendMessageW(e, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            EDIT_PROC.store(SetWindowLongPtrW(e, GWLP_WNDPROC, edit_proc as *const () as usize as isize), Relaxed);
            EDIT.store(e.0 as isize, Relaxed);
            let t = CreateWindowExW(WINDOW_EX_STYLE(0), w!("EDIT"), w!(""), WS_CHILD | WINDOW_STYLE(ES_AUTOHSCROLL as u32), 0, 0, 10, 10, Some(hwnd), None, inst, None).unwrap_or_default();
            let mono = CreateFontW(-(13.0 * s) as i32, 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Cascadia Mono"));
            SendMessageW(t, WM_SETFONT, Some(WPARAM(mono.0 as usize)), Some(LPARAM(1)));
            TERM_PROC.store(SetWindowLongPtrW(t, GWLP_WNDPROC, term_proc as *const () as usize as isize), Relaxed);
            TEDIT.store(t.0 as isize, Relaxed);
            SetTimer(Some(hwnd), T_ANIM, 380, None);
            std::thread::spawn(|| {
                *SESSIONS.lock().unwrap() = scan_sessions();
                notify();
            });
        }
        // our own title bar: the whole window is client area, with resize edges and a drag strip
        WM_NCCALCSIZE if wp.0 == 1 => {
            if IsZoomed(hwnd).as_bool() {
                let p = &mut *(lp.0 as *mut NCCALCSIZE_PARAMS);
                let f = GetSystemMetrics(SM_CXSIZEFRAME) + GetSystemMetrics(SM_CXPADDEDBORDER);
                p.rgrc[0].left += f;
                p.rgrc[0].top += f;
                p.rgrc[0].right -= f;
                p.rgrc[0].bottom -= f;
            }
            return LRESULT(0);
        }
        WM_NCHITTEST => {
            let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
            let _ = ScreenToClient(hwnd, &mut pt);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let e = (6.0 * s) as i32;
            if !IsZoomed(hwnd).as_bool() {
                let (l, t, r, b) = (pt.x < e, pt.y < e, pt.x >= rc.right - e, pt.y >= rc.bottom - e);
                let code = match (l, t, r, b) {
                    (true, true, _, _) => HTTOPLEFT,
                    (_, true, true, _) => HTTOPRIGHT,
                    (true, _, _, true) => HTBOTTOMLEFT,
                    (_, _, true, true) => HTBOTTOMRIGHT,
                    (true, ..) => HTLEFT,
                    (_, true, ..) => HTTOP,
                    (_, _, true, _) => HTRIGHT,
                    (.., true) => HTBOTTOM,
                    _ => HTCLIENT,
                };
                if code != HTCLIENT {
                    return LRESULT(code as isize);
                }
            }
            let (x, y) = (pt.x as f32 / s, pt.y as f32 / s);
            if y < TOP && x > SIDE && hit(x, y).is_none() {
                return LRESULT(HTCAPTION as isize);
            }
            return LRESULT(HTCLIENT as isize);
        }
        WM_SIZE => {
            place_edit(hwnd);
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        WM_TIMER if wp.0 == T_ANIM => {
            let busy = BUSY.load(Relaxed);
            if busy {
                UI.with(|u| u.borrow_mut().anim += 1);
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            // the CLI helper is ~540 MB: let it go once the session has sat idle long enough
            let mut since = IDLE_SINCE.lock().unwrap();
            let idle = since.get_or_insert_with(std::time::Instant::now).elapsed().as_secs();
            if busy {
                *since = Some(std::time::Instant::now());
            } else if release_helper(busy, PROC.lock().unwrap().is_some(), idle, idle_limit()) {
                drop(since);
                stop_proc();
            }
        }
        WM_CTLCOLOREDIT => {
            let dc = HDC(wp.0 as *mut c_void);
            SetTextColor(dc, COLORREF(0xFFFFFF));
            SetBkColor(dc, COLORREF(C_LINE));
            thread_local! { static BR: HBRUSH = unsafe { CreateSolidBrush(COLORREF(0x303030)) }; }
            return LRESULT(BR.with(|b| b.0 as isize));
        }
        WM_CC_DATA => {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        WM_MOUSEWHEEL => {
            let d = ((wp.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
            // the wheel scrolls whatever is under the mouse: the session list or the conversation
            let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
            let _ = ScreenToClient(hwnd, &mut pt);
            let scale = GetDpiForWindow(hwnd) as f32 / 96.0;
            let over_list = (pt.x as f32) < SIDE * scale;
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let over_pane = UI.with(|u| u.borrow().pane.is_some()) && pt.x as f32 >= rc.right as f32 - PANE_W.min(rc.right as f32 / scale * 0.45) * scale;
            UI.with(|u| {
                let mut u = u.borrow_mut();
                if over_pane {
                    u.pane_scroll -= d * 84.0; // clamped when drawn
                } else if over_list {
                    u.side_scroll -= d * 68.0; // two rows a notch; clamped when drawn
                } else {
                    u.scroll -= d * 72.0;
                    u.stick = d < 0.0 && u.stick;
                }
            });
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        WM_MOUSEMOVE => {
            let (x, y) = dips(lp);
            let h = hit(x, y);
            let m = UI.with(|u| u.borrow().msgs.iter().position(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom + 28.0));
            if UI.with(|u| std::mem::replace(&mut u.borrow_mut().hover_msg, m) != m) {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            if UI.with(|u| std::mem::replace(&mut u.borrow_mut().hover, h) != h) {
                let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                let _ = TrackMouseEvent(&mut tme);
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
        WM_MOUSELEAVE => {
            UI.with(|u| u.borrow_mut().hover = None);
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        WM_LBUTTONUP => {
            let (x, y) = dips(lp);
            match hit(x, y) {
                Some(a) => click(hwnd, a),
                None => {
                    let _ = SetFocus(Some(HWND(EDIT.load(Relaxed) as *mut c_void)));
                }
            }
        }
        WM_RBUTTONUP => {
            // right-click on a session: pin it to the top, or open its folder
            let (x, y) = dips(lp);
            if let Some(Act::Session(i)) = hit(x, y) {
                let Some(s) = SESSIONS.lock().unwrap().get(i).cloned() else { return LRESULT(0) };
                let pinned = pins().contains(&s.id);
                // "Rename" takes the new name from what is typed in the message box (as Search does)
                let typed = edit_text(HWND(EDIT.load(Relaxed) as *mut c_void)).trim().to_string();
                let rename = format!("Rename to \u{201c}{}\u{201d}", typed.chars().take(40).collect::<String>());
                let mut items = vec![(if pinned { "Unpin" } else { "Pin" }, "pin"), ("Open folder", "folder")];
                if !typed.is_empty() && !typed.contains('\n') {
                    items.push((rename.as_str(), "rename"));
                }
                match menu(hwnd, &items, "").as_deref() {
                    Some("rename") => {
                        if session_file(&s.id).is_some_and(|p| rename_in(&p, &s.id, &typed).is_ok()) {
                            let _ = SetWindowTextW(HWND(EDIT.load(Relaxed) as *mut c_void), w!(""));
                            if let Some(x) = SESSIONS.lock().unwrap().get_mut(i) {
                                x.title = typed.clone();
                            }
                        }
                    }
                    Some("pin") => toggle_pin(&s.id),
                    Some("folder") => {
                        windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), &windows::core::HSTRING::from(s.cwd.as_str()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
                    }
                    _ => {}
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
            } else if let Some(text) = UI.with(|u| u.borrow().msgs.iter().find(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom).map(|(_, t)| t.clone())) {
                // right-click on a message: copy its text
                if menu(hwnd, &[("Copy", "copy")], "").is_some() {
                    crate::settings::copy_text(&text);
                }
            }
        }
        WM_SETFOCUS => {
            let _ = SetFocus(Some(HWND(EDIT.load(Relaxed) as *mut c_void)));
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => paint(hwnd),
        WM_CLOSE => {
            let _ = ShowWindow(hwnd, SW_HIDE);
            // an idle session's CLI process is let go with the window (it is hundreds of MB); the session
            // is resumed by id on the next message. A reply still being written keeps running.
            if !BUSY.load(Relaxed) {
                stop_proc();
            }
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

pub unsafe fn show(inst: HINSTANCE) {
    let mut h = HWND(WIN.load(Relaxed) as *mut c_void);
    if h.is_invalid() {
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: inst, lpszClassName: w!("shellbits_claude"), hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
        RegisterClassW(&wc);
        let Ok(w_) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("shellbits_claude"), w!("Claude Code"), WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT, 1400, 900, None, None, Some(inst), None) else { return };
        h = w_;
        WIN.store(h.0 as isize, Relaxed);
        let dark = 1u32;
        let _ = DwmSetWindowAttribute(h, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const c_void, 4);
        let _ = DwmExtendFrameIntoClientArea(h, &MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 1, cyBottomHeight: 0 });
        let _ = SetWindowPos(h, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER);
        place_edit(h);
    }
    let _ = ShowWindow(h, SW_SHOW);
    crate::flyout::force_foreground(h);
    let _ = SetFocus(Some(HWND(EDIT.load(Relaxed) as *mut c_void)));
}

/// `shellbits --snapshot-claude out.bmp`: the window drawn off-screen at 1920x1032 with your newest
/// session open, for side-by-side comparison with the desktop app.
pub unsafe fn snapshot(path: &str) -> windows::core::Result<()> {
    std::env::set_var("SHELLBITS_SNAPSHOT", "1");
    let (w, h) = (1920, 1032);
    let t0 = std::time::Instant::now();
    let sessions = scan_sessions();
    let t_scan = t0.elapsed();
    // SHELLBITS_SESSION picks a session by title (to compare against a capture of the real app)
    let want = std::env::var("SHELLBITS_SESSION").unwrap_or_default().to_lowercase();
    let pick = sessions.iter().position(|s| !want.is_empty() && s.title.to_lowercase().contains(&want)).unwrap_or(0);
    let t1 = std::time::Instant::now();
    if let Some(first) = sessions.get(pick).cloned() {
        *LOG.lock().unwrap() = load_history(&first);
        set_settings(|s| {
            s.folder = first.cwd.clone();
            s.session = Some(first.id.clone());
        });
        UI.with(|u| u.borrow_mut().selected = Some(pick));
        // SHELLBITS_PANE=files|diff: show that docked pane in the snapshot
        // SHELLBITS_TODOS="text|status;...": a task list to show in the snapshot
        if let Ok(t) = std::env::var("SHELLBITS_TODOS") {
            *TODOS.lock().unwrap() = t.split(';').filter_map(|x| x.split_once('|')).map(|(a, b)| (a.to_string(), b.to_string())).collect();
        }
        if std::env::var_os("SHELLBITS_BUSY").is_some() {
            BUSY.store(true, Relaxed);
            BUSY_SINCE.store(windows::Win32::System::SystemInformation::GetTickCount64() - 11_000, Relaxed);
        }
        match std::env::var("SHELLBITS_PANE").unwrap_or_default().as_str() {
            "files" => UI.with(|u| {
                let u = &mut *u.borrow_mut();
                (u.pane, u.pane_files, u.pane_dir) = (Some(Pane::Files), list_dir(&first.cwd), first.cwd.clone());
            }),
            "terminal" => {
                *TERM_DIR.lock().unwrap() = first.cwd.clone();
                term_send("echo hello from the terminal pane\r");
                term_send("set /p NAME=Your name? \r");
                std::thread::sleep(std::time::Duration::from_millis(800));
                term_send("Rusty\r");
                term_send("echo hi %NAME%\r");
                std::thread::sleep(std::time::Duration::from_millis(1200));
                UI.with(|u| { let u = &mut *u.borrow_mut(); u.pane = Some(Pane::Terminal); u.pane_scroll = f32::MAX; });
            }
            "preview" => {
                load_servers();
                std::thread::sleep(std::time::Duration::from_millis(2500));
                UI.with(|u| u.borrow_mut().pane = Some(Pane::Preview));
            }
            "diff" => {
                UI.with(|u| u.borrow_mut().pane = Some(Pane::Diff));
                load_diff(first.cwd.clone());
                std::thread::sleep(std::time::Duration::from_millis(1500)); // let git answer
            }
            _ => {}
        }
    }
    *SESSIONS.lock().unwrap() = sessions;
    UI.with(|u| -> windows::core::Result<()> {
        let mut ui = u.borrow_mut();
        ui.gfx = Some(make_gfx()?);
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, ..Default::default() },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let dib = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let dc = CreateCompatibleDC(None);
        let old = SelectObject(dc, dib.into());
        let rt = ui.gfx.as_ref().unwrap().rt.clone();
        rt.BindDC(dc, &RECT { left: 0, top: 0, right: w, bottom: h })?;
        rt.SetDpi(96.0, 96.0);
        let t_load = t1.elapsed();
        rt.BeginDraw();
        let t2 = std::time::Instant::now();
        draw(&mut ui, w as f32, h as f32, true)?;
        let t_draw = t2.elapsed();
        // a second frame: what every repaint (scrolling, hovering) costs once things are warm
        let t3 = std::time::Instant::now();
        draw(&mut ui, w as f32, h as f32, true)?;
        let t_again = t3.elapsed();
        rt.EndDraw(None, None)?;
        let _ = GdiFlush();
        // where the time goes, for tuning: %TEMP%\shellbits-claude-timing.txt
        let _ = std::fs::write(
            std::env::temp_dir().join("shellbits-claude-timing.txt"),
            format!("sessions listed: {} in {} ms\nhistory loaded: {} entries in {} ms\nfirst frame: {} ms\nnext frame: {} ms", SESSIONS.lock().unwrap().len(), t_scan.as_millis(), LOG.lock().unwrap().len(), t_load.as_millis(), t_draw.as_millis(), t_again.as_millis()),
        );
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

#[cfg(test)]
mod tests {
    #[test]
    fn status_line_counts_up() {
        assert_eq!(super::status_line(12_400), "Working… 12s");
        assert_eq!(super::status_line(75_000), "Working… 1m 15s");
    }

    #[test]
    fn listening_ports() {
        let ns = "  TCP    0.0.0.0:135     0.0.0.0:0   LISTENING   1100\n  TCP    127.0.0.1:5173  0.0.0.0:0   LISTENING   42\n  TCP    [::]:5173       [::]:0      LISTENING   42\n  TCP    192.168.1.5:3000  0.0.0.0:0  LISTENING   7\n  TCP    127.0.0.1:8080  1.2.3.4:5  ESTABLISHED  9\n";
        assert_eq!(super::listening(ns), vec![(5173, 42)]);
    }

    #[test]
    fn context_rows_add_up() {
        let rows = super::breakdown(100_000.0, 200_000.0, 2_000.0, 3_000.0);
        assert_eq!(rows.iter().map(|r| r.2).sum::<f64>(), 200_000.0);
        assert_eq!(rows[2].2, 95_000.0);
        // estimates never exceed what is in use
        let small = super::breakdown(1_000.0, 200_000.0, 5_000.0, 5_000.0);
        assert_eq!((small[0].2, small[1].2, small[2].2), (1_000.0, 0.0, 0.0));
    }

    #[test]
    fn connectors_global_then_folder() {
        let cfg = serde_json::json!({"mcpServers": {"a": {"command": "x"}}, "projects": {"C:/Work/app": {"mcpServers": {"kaggle": {"type": "http"}, "a": {}}}}});
        assert_eq!(super::connectors(&cfg, r"C:\work\app\"), vec![("a".to_string(), "stdio".to_string()), ("kaggle".to_string(), "http".to_string())]);
        assert_eq!(super::connectors(&cfg, "D:/other").len(), 1);
    }

    #[test]
    fn task_card_and_compact_room() {
        let t = |s: &str| (String::from("x"), s.to_string());
        assert_eq!(super::task_card_h(&[]), 0.0);
        assert_eq!(super::task_card_h(&[t("completed"), t("completed")]), 0.0);
        assert_eq!(super::task_card_h(&[t("completed"), t("pending")]), 72.0);
        assert_eq!(super::until_compact(918_300.0, 1_000_000.0).round(), 48_700.0); // the app showed "48k" here
        assert_eq!(super::until_compact(990_000.0, 1_000_000.0), 0.0);
    }

    #[test]
    fn mentions_list_folders_first_and_skip_build_dirs() {
        let d = std::env::temp_dir().join("shellbits-mention-test");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::create_dir_all(d.join("target")).unwrap();
        std::fs::write(d.join("src").join("main.rs"), "").unwrap();
        std::fs::write(d.join("a b.txt"), "").unwrap();
        assert_eq!(super::mention_list(&d, 10), vec!["src/", "src/main.rs", "a b.txt"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn base64_round_trip() {
        for data in [&b""[..], b"a", b"ab", b"abc", b"abcd", &(0u8..=255).collect::<Vec<_>>()] {
            assert_eq!(super::unbase64(&super::base64(data)).unwrap(), data);
        }
        assert!(super::unbase64("ab$c").is_none());
    }

    #[test]
    fn slash_list_has_builtins_then_user_ones_once() {
        let v = super::slash_commands(&["ponytail".into(), "compact".into()], &["deploy".into()]);
        assert_eq!(v[0], "compact");
        assert_eq!(v.iter().filter(|n| *n == "compact").count(), 1);
        assert!(v.ends_with(&["ponytail".to_string(), "deploy".to_string()]));
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn streams_then_finalises_one_reply() {
        let (mut log, mut live) = (Vec::new(), None);
        apply(&json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hel"}}}), &mut log, &mut live, false);
        apply(&json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"lo"}}}), &mut log, &mut live, false);
        assert_eq!(log[0].text, "Hello");
        apply(&json!({"type":"assistant","message":{"content":[{"type":"text","text":"Hello!"}]}}), &mut log, &mut live, false);
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].text, "Hello!");
    }

    #[test]
    fn tool_summary_like_the_app() {
        assert_eq!(summary(&["Bash", "Read", "Read", "Edit"]), "Ran a command, read 2 files, edited a file");
        assert_eq!(summary(&["Grep"]), "Searched");
    }

    #[test]
    fn permission_request_becomes_a_prompt() {
        let (mut log, mut live) = (Vec::new(), None);
        apply(&json!({"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"rm x"}}}), &mut log, &mut live, false);
        assert_eq!(log[0].kind, Kind::Permission);
        assert_eq!(log[0].request_id, "r1");
        assert!(log[0].text.contains("Bash  rm x"));
    }

    #[test]
    fn markdown_blocks_and_inline() {
        // tables are split out of the prose around them
        let t = tables("Before\n\n| Area | Match |\n|---|---|\n| Display | 100% |\n| Keypad | 99.4% |\n\nAfter");
        assert_eq!(t.len(), 3);
        assert_eq!(t[0], Seg::Text("Before".into()));
        assert_eq!(t[1], Seg::Table(vec![vec!["Area".into(), "Match".into()], vec!["Display".into(), "100%".into()], vec!["Keypad".into(), "99.4%".into()]]));
        assert_eq!(t[2], Seg::Text("\nAfter".into()));
        assert_eq!(tables("a | b is not a table\n| nor is this alone |"), vec![Seg::Text("a | b is not a table\n| nor is this alone |".into())]);
        assert_eq!(column_widths(&[100.0, 200.0], 400.0), vec![100.0, 200.0]);
        assert_eq!(column_widths(&[400.0, 400.0], 400.0), vec![200.0, 200.0]);
        let b = blocks("Hi\n```\ncode\n```\nbye");
        assert_eq!(b, vec![(false, "Hi".to_string()), (true, "code".to_string()), (false, "bye".to_string())]);
        let (t, runs) = inline("a **b** `c`");
        assert_eq!(t, "a b c");
        assert_eq!(runs, vec![(2, 1, Style::Bold), (4, 1, Style::Code)]);
        // links show their text, italics need to hug the words, stray asterisks stay
        assert_eq!(inline("see [the docs](https://x.y/z) now"), ("see the docs now".to_string(), vec![(4, 8, Style::Link)]));
        assert_eq!(inline("an *important* point"), ("an important point".to_string(), vec![(3, 9, Style::Italic)]));
        assert_eq!(inline("2 * 3 * 4"), ("2 * 3 * 4".to_string(), vec![]));
        assert_eq!(inline("[not a link] at all"), ("[not a link] at all".to_string(), vec![]));
        assert_eq!(inline_links("a [b](https://x.y) and [c]( http://z )").2, vec!["https://x.y".to_string(), "http://z".to_string()]);
        assert!(safe_link("https://claude.com/x") && safe_link("HTTP://a.b"));
        assert!(!safe_link("file:///C:/Windows/system32/calc.exe") && !safe_link("C:/x.exe") && !safe_link("javascript:alert(1)") && !safe_link("ms-settings:"));
        // line level: headings by size, nested bullets, quotes, rules
        assert_eq!(inline(&lines("# Top\n## Mid **x**\n#### Deep")), ("Top\nMid x\nDeep".to_string(), vec![(0, 3, Style::H1), (4, 5, Style::H2), (10, 4, Style::H3)]));
        assert_eq!(lines("- a\n  - b\n1. c"), "\u{2022} a\n\u{a0}\u{a0}\u{a0}\u{a0}\u{25e6} b\n1. c");
        assert_eq!(inline(&lines("> quoted")), ("quoted".to_string(), vec![(0, 6, Style::Quote)]));
        assert_eq!(lines("---"), "\u{2500}".repeat(24));
    }

    #[test]
    fn untitled_sessions_take_their_first_message() {
        assert_eq!(prompt_title(&json!("Fix the login page\nand the footer")).as_deref(), Some("Fix the login page"));
        assert_eq!(prompt_title(&json!([{"type":"text","text":"<system-reminder>ignore</system-reminder>"},{"type":"text","text":"  What is the capital of France?"}])).as_deref(), Some("What is the capital of France?"));
        assert_eq!(prompt_title(&json!([{"type":"tool_result","content":"output"}])), None);
        assert_eq!(prompt_title(&json!("<command-name>/loop</command-name>")), None);
        assert_eq!(prompt_title(&json!("x".repeat(100))).map(|t| t.len()), Some(60));
    }

    #[test]
    fn session_list_cache_round_trips() {
        let list = vec![
            SessionInfo { id: "a1".into(), title: "Fix the\tlogin page".into(), cwd: r"C:\work\site".into(), modified: 1700000000 },
            SessionInfo { id: "b2".into(), title: "New session".into(), cwd: "C:/x y".into(), modified: 5 },
        ];
        let back = sessions_from_text(&sessions_to_text(&list));
        assert_eq!(back.len(), 2);
        assert_eq!((back[0].id.as_str(), back[0].title.as_str(), back[0].cwd.as_str(), back[0].modified), ("a1", "Fix the login page", r"C:\work\site", 1700000000));
        assert_eq!((back[1].cwd.as_str(), back[1].modified), ("C:/x y", 5));
        assert!(sessions_from_text("garbage\nnot\ta\tnumber\tx").is_empty());
    }

    #[test]
    fn renaming_sessions() {
        let v: serde_json::Value = serde_json::from_str(&title_line("abc", "My \"quoted\" name")).unwrap();
        assert_eq!((v["type"].as_str(), v["customTitle"].as_str(), v["sessionId"].as_str()), (Some("custom-title"), Some("My \"quoted\" name"), Some("abc")));
        let lines = [r#"{"type":"ai-title","aiTitle":"Auto"}"#, r#"{"type":"custom-title","customTitle":"First"}"#, r#"{"type":"user"}"#, r#"{"type":"custom-title","customTitle":"Second"}"#, r#"{"type":"ai-title","aiTitle":"Later auto"}"#].join("\n");
        assert_eq!(last_title(&lines).as_deref(), Some("Second")); // your newest name, not a later automatic one
        assert_eq!(last_title(r#"{"type":"ai-title","aiTitle":"Auto"}"#).as_deref(), Some("Auto"));
        assert_eq!(last_title(r#"{"type":"user"}"#), None);
        // on a real file: the line is appended on its own line and read back, the old content untouched
        let p = std::env::temp_dir().join(format!("shellbits-rename-test-{}.jsonl", std::process::id()));
        std::fs::write(&p, r#"{"type":"user","cwd":"C:/x"}"#).unwrap(); // no newline at the end
        rename_in(&p, "abc", "Renamed here").unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.starts_with("{\"type\":\"user\",\"cwd\":\"C:/x\"}\n{"));
        assert!(text.ends_with('\n'));
        assert_eq!(tail_title(&p).as_deref(), Some("Renamed here"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn pictures_in_messages() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&(0..=255u8).collect::<Vec<u8>>()), "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0+P0BBQkNERUZHSElKS0xNTk9QUVJTVFVWV1hZWltcXV5fYGFiY2RlZmdoaWprbG1ub3BxcnN0dXZ3eHl6e3x9fn+AgYKDhIWGh4iJiouMjY6PkJGSk5SVlpeYmZqbnJ2en6ChoqOkpaanqKmqq6ytrq+wsbKztLW2t7i5uru8vb6/wMHCw8TFxsfIycrLzM3Oz9DR0tPU1dbX2Nna29zd3t/g4eLj5OXm5+jp6uvs7e7v8PHy8/T19vf4+fr7/P3+/w=="); // every byte value
        let read = |p: &str| (p.ends_with("a.png") || p.ends_with("my shot.jpg")).then(|| b"hi".to_vec());
        // a picture becomes an image block and a label; the words around it stay
        let (shown, content) = message_content("look @C:/pics/a.png please", read);
        assert_eq!(shown, "look [image: a.png] please");
        assert_eq!(content[0]["source"]["media_type"], "image/png");
        assert_eq!(content[0]["source"]["data"], "aGk=");
        assert_eq!(content[1]["text"], "look [image: a.png] please");
        // quoted paths with spaces; files that are not pictures, or cannot be read, stay as typed
        let (shown, content) = message_content("@\"C:/x/my shot.jpg\" and @src/main.rs and @gone.png", read);
        assert_eq!(shown, "[image: my shot.jpg] and @src/main.rs and @gone.png");
        assert_eq!(content.as_array().unwrap().len(), 2);
        assert_eq!(message_content("mail me@example.com", read).0, "mail me@example.com");
    }

    #[test]
    fn nested_session_variables() {
        let v = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert_eq!(inherited_session_vars(&v(&["PATH", "ANTHROPIC_API_KEY", "CLAUDE_CONFIG_DIR"])), Vec::<&str>::new()); // not inside a session
        assert_eq!(inherited_session_vars(&v(&["PATH", "CLAUDECODE", "CLAUDE_CODE_SESSION_ID", "ANTHROPIC_BASE_URL", "TEMP"])), vec!["CLAUDECODE", "CLAUDE_CODE_SESSION_ID", "ANTHROPIC_BASE_URL"]);
    }

    #[test]
    fn idle_helper_is_released() {
        assert!(release_helper(false, true, 600, 600));
        assert!(!release_helper(false, true, 599, 600)); // not yet
        assert!(!release_helper(true, true, 9999, 600)); // never while it is working
        assert!(!release_helper(false, false, 9999, 600)); // nothing to stop
    }

    #[test]
    fn model_names() {
        assert_eq!(model_name("claude-opus-5-5").as_deref(), Some("Opus 5.5"));
        assert_eq!(model_name("claude-fable-5-2").as_deref(), Some("Fable 5.2"));
        assert_eq!(model_name("claude-sonnet-4-5-20250929").as_deref(), Some("Sonnet 4.5"));
        assert_eq!(model_name("claude-haiku-5-5[1m]").as_deref(), Some("Haiku 5.5 (1M)"));
        assert_eq!(model_name("claude-opus").as_deref(), Some("Opus"));
        assert_eq!(model_name("<synthetic>"), None);
        assert_eq!(model_name(""), None);
    }

    #[test]
    fn panes() {
        assert_eq!(diff_colour("+added line"), 0x6CCB8E);
        assert_eq!(diff_colour("-removed line"), 0xF0806F);
        assert_eq!(diff_colour("+++ b/src/main.rs"), 0x9B9B9B); // a file header, not an addition
        assert_eq!(diff_colour("@@ -1,4 +1,6 @@"), 0x6CB4EE);
        assert_eq!(diff_colour(" context"), 0xD8D8D8);
        let e = |n: &str, d: bool| (n.to_string(), d);
        assert_eq!(sorted_entries(vec![e("b.txt", false), e("src", true), e("A.md", false), e(".git", true), e("Zed", true)]), vec![e(".git", true), e("src", true), e("Zed", true), e("A.md", false), e("b.txt", false)]);
    }

    #[test]
    fn panel_numbers_and_outputs() {
        assert_eq!(tokens(273.0), "273");
        assert_eq!(tokens(1700.0), "1.7k");
        assert_eq!(tokens(10_000.0), "10k");
        assert_eq!(tokens(918_300.0), "918.3k");
        assert_eq!(tokens(1_000_000.0), "1M");
        assert_eq!(tokens(999_960.0), "1M");
        assert_eq!(context_tokens(&json!({"input_tokens": 50000}), "m", Some(500_000.0)), Some((50000.0, 500000.0)));
        // files written or edited, newest first, each once; other tools do not count
        let (mut log, mut live) = (Vec::new(), None);
        for (tool, path) in [("Write", "C:/p/b.txt"), ("Read", "C:/p/zzz.rs"), ("Edit", "C:/p/edited-only.txt"), ("Write", "C:/p/a.rs"), ("Write", "C:/p/b.txt")] {
            apply(&json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":tool,"input":{"file_path":path}}]}}), &mut log, &mut live, true);
        }
        assert_eq!(outputs(&log), vec!["C:/p/b.txt".to_string(), "C:/p/a.rs".to_string()]);
    }

    #[test]
    fn context_meter() {
        let u = json!({"input_tokens": 4, "cache_creation_input_tokens": 3609, "cache_read_input_tokens": 708490, "output_tokens": 765});
        assert_eq!(context_percent(&u, "claude-x", None).map(|p| p.round()), Some(71.0)); // over 200k: a 1M window
        assert_eq!(context_percent(&json!({"input_tokens": 50000, "output_tokens": 0}), "claude-x", None), Some(25.0));
        assert_eq!(context_percent(&json!({"input_tokens": 50000}), "claude-x[1m]", None), Some(5.0));
        assert_eq!(context_percent(&json!({"input_tokens": 50000}), "claude-x", Some(500_000.0)), Some(10.0)); // the reported window wins
        assert_eq!(context_percent(&json!({}), "claude-x", None), None);
        // history lines carry it too
        let (mut log, mut live) = (Vec::new(), None);
        *CONTEXT.lock().unwrap() = None;
        apply(&json!({"type":"assistant","message":{"model":"m","usage":{"input_tokens":100000},"content":[{"type":"text","text":"hi"}]}}), &mut log, &mut live, true);
        assert_eq!(*CONTEXT.lock().unwrap(), Some(50.0));
    }

    #[test]
    fn sidebar_groups() {
        assert!(plan_signed_in(r#"{"claudeAiOauth":{"accessToken":"x","subscriptionType":"max"},"mcpOAuth":{}}"#));
        assert!(!plan_signed_in(r#"{"mcpOAuth":{"a":{"accessToken":"x"}}}"#)); // only connector sign-ins: not a Claude plan
        assert!(!plan_signed_in(""));
        // the real app's record of a session, and what it does to our list
        let rec = r#"{"sessionId":"local_1","cliSessionId":"abc","title":"Part 1","isStarred":true,"isArchived":false,"lastActivityAt":5000000,"folderPlace":{"cwd":"C:/Users/me/Desktop/proj"},"postTurnSummary":{"status_category":"review_ready"}}"#;
        let (id, a) = parse_app_session(rec).unwrap();
        assert_eq!((id.as_str(), a.title.as_str(), a.folder.as_str(), a.starred, a.status.as_str(), a.last), ("abc", "Part 1", "C:/Users/me/Desktop/proj", true, "review_ready", 5000));
        assert_eq!(parse_app_session("{}"), None);
        let mut app = std::collections::HashMap::new();
        app.insert(id, a);
        app.insert("gone".to_string(), AppInfo { archived: true, ..Default::default() });
        let si = |id: &str, m: u64| SessionInfo { id: id.into(), title: "old".into(), cwd: "C:/scratch".into(), modified: m };
        let (list, starred) = overlay(vec![si("other", 9000), si("abc", 10), si("gone", 99)], &app);
        // the app's session first with the app's title and folder; the archived one is gone; the unknown one after
        assert_eq!(list.iter().map(|s| (s.id.as_str(), s.title.as_str())).collect::<Vec<_>>(), [("abc", "Part 1"), ("other", "old")]);
        assert_eq!(list[0].cwd, "C:/Users/me/Desktop/proj");
        assert_eq!(starred, ["abc"]);
        assert_eq!(status_dot("review_ready"), Some(0x4CC2FF));
        assert_eq!(status_dot("blocked"), Some(0xF2B04D));
        assert_eq!(status_dot("completed"), None);
        // the newest folder wins; lines without one, or broken ones, are skipped
        let log = "{\"type\":\"user\",\"cwd\":\"C:/scratch\"}
{\"type\":\"assistant\"}
{\"cwd\":\"C:/proj/site\",\"type\":\"user\"}
not json \"cwd\"
";
        assert_eq!(last_cwd(log).as_deref(), Some("C:/proj/site"));
        assert_eq!(last_cwd("{\"type\":\"user\"}"), None);
        let s = |id: &str, title: &str, cwd: &str| SessionInfo { id: id.into(), title: title.into(), cwd: cwd.into(), modified: 0 };
        let all = [s("a", "Alpha", "C:/work/one"), s("b", "Beta", "C:/work/two"), s("c", "Gamma ray", "C:/work/one")];
        assert_eq!(grouped(&all, &[], ""), vec![("one".to_string(), vec![0, 2]), ("two".to_string(), vec![1])]);
        // a pinned session moves to the Pinned group at the top and leaves its folder
        assert_eq!(grouped(&all, &["c".to_string()], ""), vec![("Pinned".to_string(), vec![2]), ("one".to_string(), vec![0]), ("two".to_string(), vec![1])]);
        // the filter applies to pinned ones too; empty groups disappear
        assert_eq!(grouped(&all, &["c".to_string()], "BET"), vec![("two".to_string(), vec![1])]);
    }

    #[test]
    fn history_keeps_typed_prompts_only() {
        let (mut log, mut live) = (Vec::new(), None);
        apply(&json!({"type":"user","message":{"content":"fix the bug"}}), &mut log, &mut live, true);
        apply(&json!({"type":"user","message":{"content":"<command-name>/clear</command-name>"}}), &mut log, &mut live, true);
        apply(&json!({"type":"user","isMeta":true,"message":{"content":"meta"}}), &mut log, &mut live, true);
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].kind, Kind::User);
    }
}

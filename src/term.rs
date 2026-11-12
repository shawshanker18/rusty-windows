//! A real console behind the Claude window's Terminal pane: cmd.exe on a Windows pseudo console
//! (ConPTY), and the small part of a VT screen that ConPTY's output needs (text, CR/LF/BS, cursor
//! moves, erase line/screen; colours ignored).
use std::sync::Mutex;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::Console::*;
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::*;

/// What the console shows: lines that scrolled off the top, then the screen.
pub struct Screen {
    pub cols: usize,
    pub rows: usize,
    pub history: Vec<String>,
    grid: Vec<Vec<char>>,
    cx: usize,
    cy: usize,
    esc: String, // an escape sequence still being read
}

impl Screen {
    pub fn new(cols: usize, rows: usize) -> Self {
        Screen { cols, rows, history: Vec::new(), grid: vec![vec![' '; cols]; rows], cx: 0, cy: 0, esc: String::new() }
    }

    fn line_feed(&mut self) {
        if self.cy + 1 < self.rows {
            self.cy += 1;
        } else {
            let top = self.grid.remove(0);
            self.history.push(top.iter().collect::<String>().trim_end().to_string());
            if self.history.len() > 5000 {
                self.history.remove(0);
            }
            self.grid.push(vec![' '; self.cols]);
        }
    }

    fn csi(&mut self, body: &str, fin: char) {
        let nums: Vec<usize> = body.trim_start_matches('?').split(';').map(|n| n.parse().unwrap_or(0)).collect();
        let n = |i: usize, d: usize| nums.get(i).copied().filter(|v| *v > 0).unwrap_or(d);
        match fin {
            'H' | 'f' => (self.cy, self.cx) = ((n(0, 1) - 1).min(self.rows - 1), (n(1, 1) - 1).min(self.cols - 1)),
            'A' => self.cy = self.cy.saturating_sub(n(0, 1)),
            'B' => self.cy = (self.cy + n(0, 1)).min(self.rows - 1),
            'C' => self.cx = (self.cx + n(0, 1)).min(self.cols - 1),
            'D' => self.cx = self.cx.saturating_sub(n(0, 1)),
            'G' => self.cx = (n(0, 1) - 1).min(self.cols - 1),
            'd' => self.cy = (n(0, 1) - 1).min(self.rows - 1),
            'K' => {
                let row = &mut self.grid[self.cy];
                let (a, b) = match nums.first().copied().unwrap_or(0) { 1 => (0, self.cx + 1), 2 => (0, self.cols), _ => (self.cx, self.cols) };
                row[a..b.min(self.cols)].iter_mut().for_each(|c| *c = ' ');
            }
            'X' => {
                let end = (self.cx + n(0, 1)).min(self.cols);
                self.grid[self.cy][self.cx..end].iter_mut().for_each(|c| *c = ' ');
            }
            'J' => {
                let mode = nums.first().copied().unwrap_or(0);
                for (y, row) in self.grid.iter_mut().enumerate() {
                    for (x, c) in row.iter_mut().enumerate() {
                        let after = (y, x) >= (self.cy, self.cx);
                        if mode == 2 || mode == 3 || (mode == 0 && after) || (mode == 1 && !after) {
                            *c = ' ';
                        }
                    }
                }
            }
            _ => {} // colours, cursor show/hide, modes
        }
    }

    pub fn feed(&mut self, text: &str) {
        for ch in text.chars() {
            if !self.esc.is_empty() {
                self.esc.push(ch);
                let e = self.esc.clone();
                let done = if e.starts_with("\x1b[") {
                    e.len() > 2 && ('@'..='~').contains(&ch)
                } else if e.starts_with("\x1b]") {
                    ch == '\x07' || e.ends_with("\x1b\\")
                } else {
                    e.len() >= 2 // two-character escapes
                };
                if done {
                    if let Some(body) = e.strip_prefix("\x1b[") {
                        self.csi(&body[..body.len() - 1], ch);
                    }
                    self.esc.clear();
                }
                continue;
            }
            match ch {
                '\x1b' => self.esc.push(ch),
                '\r' => self.cx = 0,
                '\n' => self.line_feed(),
                '\x08' => self.cx = self.cx.saturating_sub(1),
                '\x07' => {}
                c if c >= ' ' => {
                    if self.cx >= self.cols {
                        self.cx = 0;
                        self.line_feed();
                    }
                    self.grid[self.cy][self.cx] = c;
                    self.cx += 1;
                }
                _ => {}
            }
        }
    }

    /// History then the screen, without the empty rows at the bottom.
    pub fn lines(&self) -> Vec<String> {
        let mut v = self.history.clone();
        let mut rows: Vec<String> = self.grid.iter().map(|r| r.iter().collect::<String>().trim_end().to_string()).collect();
        let keep = rows.iter().rposition(|r| !r.is_empty()).map_or(0, |i| i + 1).max(self.cy + 1);
        rows.truncate(keep);
        v.extend(rows);
        v
    }
}

pub struct Console {
    pub screen: std::sync::Arc<Mutex<Screen>>,
    input: HANDLE,
    _pc: HPCON,
    process: HANDLE,
}
unsafe impl Send for Console {}

impl Console {
    pub fn alive(&self) -> bool {
        unsafe { WaitForSingleObject(self.process, 0) == WAIT_TIMEOUT }
    }

    pub fn write(&self, s: &str) {
        let mut n = 0;
        unsafe { let _ = WriteFile(self.input, Some(s.as_bytes()), Some(&mut n), None); }
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        unsafe {
            let _ = TerminateProcess(self.process, 0);
            ClosePseudoConsole(self._pc);
            let _ = CloseHandle(self.input);
            let _ = CloseHandle(self.process);
        }
    }
}

/// Start cmd.exe in `dir` on a pseudo console of `cols` x `rows`; `changed` runs on new output.
pub fn start(dir: &str, cols: usize, rows: usize, changed: fn()) -> Result<Console> {
    unsafe {
        let (mut in_r, mut in_w, mut out_r, mut out_w) = (HANDLE::default(), HANDLE::default(), HANDLE::default(), HANDLE::default());
        CreatePipe(&mut in_r, &mut in_w, None, 0)?;
        CreatePipe(&mut out_r, &mut out_w, None, 0)?;
        let pc = CreatePseudoConsole(COORD { X: cols as i16, Y: rows as i16 }, in_r, out_w, 0)?;
        let _ = CloseHandle(in_r);
        let _ = CloseHandle(out_w);
        let mut size = 0usize;
        let _ = InitializeProcThreadAttributeList(None, 1, None, &mut size);
        let mut buf = vec![0u8; size];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(buf.as_mut_ptr() as _);
        InitializeProcThreadAttributeList(Some(list), 1, None, &mut size)?;
        UpdateProcThreadAttribute(list, 0, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize, Some(pc.0 as *const _), std::mem::size_of::<HPCON>(), None, None)?;
        let si = STARTUPINFOEXW { StartupInfo: STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOEXW>() as u32, dwFlags: STARTF_USESTDHANDLES, ..Default::default() } /* no inherited std handles: everything goes through the pseudo console */, lpAttributeList: list };
        let mut pi = PROCESS_INFORMATION::default();
        let mut cmd: Vec<u16> = "cmd.exe\0".encode_utf16().collect();
        CreateProcessW(None, Some(PWSTR(cmd.as_mut_ptr())), None, None, false, EXTENDED_STARTUPINFO_PRESENT, None, &HSTRING::from(dir), &si.StartupInfo, &mut pi)?;
        DeleteProcThreadAttributeList(list);
        let _ = CloseHandle(pi.hThread);
        let screen = std::sync::Arc::new(Mutex::new(Screen::new(cols, rows)));
        let s2 = screen.clone();
        let out = out_r.0 as isize;
        std::thread::spawn(move || {
            let h = HANDLE(out as *mut _);
            let mut b = [0u8; 8192];
            let mut carry: Vec<u8> = Vec::new();
            loop {
                let mut n = 0u32;
                if ReadFile(h, Some(&mut b), Some(&mut n), None).is_err() || n == 0 {
                    break;
                }
                carry.extend_from_slice(&b[..n as usize]);
                // keep a UTF-8 character that was cut in half for the next read
                let cut = match std::str::from_utf8(&carry) { Ok(_) => carry.len(), Err(e) => e.valid_up_to() + if e.error_len().is_some() { 1 } else { 0 } };
                let text = String::from_utf8_lossy(&carry[..cut]).to_string();
                carry.drain(..cut);
                s2.lock().unwrap().feed(&text);
                changed();
            }
            let _ = CloseHandle(h);
        });
        Ok(Console { screen, input: in_w, _pc: pc, process: pi.hProcess })
    }
}

#[cfg(test)]
mod tests {
    use super::Screen;

    #[test]
    fn vt_basics() {
        let mut s = Screen::new(10, 3);
        s.feed("ab\r\ncd\x1b[1;1Hx\x1b[2;1H\x1b[K");
        assert_eq!(s.lines(), vec!["xb".to_string(), String::new()]); // the cursor row stays
        s.feed("\x1b]0;title\x07\x1b[32mok\x1b[0m\r\n1\r\n2\r\n3");
        assert_eq!(s.history, vec!["xb".to_string(), "ok".to_string()]);
        assert_eq!(s.lines(), ["xb", "ok", "1", "2", "3"].map(String::from).to_vec());
        s.feed("\x1b[2J");
        assert!(s.lines().iter().skip(s.history.len()).all(|l| l.is_empty()));
    }
}

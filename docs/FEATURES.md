# Features and how close each one is

"Match" is the share of pixels within a tolerance of 16 (per channel, 0 to 255) of a capture of the
real Windows window at the same size. The project target is 99.99%. "Not measured" means exactly that.

## Shell

| Piece | Source | What works | Match |
|---|---|---|---|
| Taskbar | `taskbar.rs` | running apps, pinned apps (right-click, Pin to taskbar), clock, tray area, jump lists | not measured |
| Start and Search | `launcher.rs` | pinned and all apps, search, calculator answers, routes Calculator, Settings and Explorer to the Rust versions | not measured |
| System tray | `tray.rs` | icons from running apps, overflow | not measured |
| Quick Settings | `quick.rs`, `qsx.rs` | Wi-Fi, Bluetooth, sound output, accessibility, performance and temperature pages, volume mixer | not measured |
| Notification center and calendar | `notify.rs` | live notifications from Windows, clear, calendar | not measured |
| Toasts | `toast.rs` | shown within about 40 ms of Windows storing them | not measured |
| Flyouts | `flyout.rs` | volume, media keys, lock keys, media card | not measured |
| Alt+Tab and Task View | `alttab.rs` | live thumbnails, keyboard and mouse | not measured |
| Win+X menu and Run | `winx.rs` | all entries, Run history | not measured |
| Snap layouts, Snap Assist, Win+arrows | `snap.rs`, `session.rs` | Win+Z picker, assist grid, halves, quarters, maximise, minimise | not measured |
| Clipboard history | `clip.rs` | Win+V list, paste | not measured |
| Emoji panel | `emoji.rs` | search, insert | not measured |
| Language switcher | `lang.rs` | list, click to switch | not measured |
| Desktop | `desktop.rs`, `wallpaper.rs`, `peek.rs` | icons, video wallpaper (picks a copy scaled to your screen), show desktop, peek | not measured |
| Screen snip | `snip.rs` | Win+Shift+S in shell mode (or `--snip`): drag a rectangle, copied to the clipboard and saved to Pictures\Screenshots | n/a |
| Shell mode | `session.rs`, `main.rs` | runs instead of Explorer, starts your sign-in apps, crash watchdog (restart in 4 to 8 s, at most 3 a minute), Ctrl+Alt+Shift+E back to Explorer | n/a |

## Apps

| App | Source | What works | Match |
|---|---|---|---|
| Calculator, Standard | `calc.rs` | full arithmetic, memory, history, keyboard | 99.52% |
| Calculator, Scientific | `calc.rs` | trig, powers, logs, angle units, second functions | 98.73% |
| Settings, System | `settings.rs` | live values, links | 97.4% |
| Settings, Display | `settings.rs` | scale, resolution, orientation | 97.3% |
| Settings, Sound | `settings.rs` | device lists that switch the default device, volume, mute | 97.3% |
| Settings, other pages | `settings.rs` | Apps 96.8%, Gaming 98.3%, Accessibility 96.8%, Privacy and security 97.5% | 96.8 to 98.3% |
| File Explorer | `files.rs` | tabs, navigation, preview, multi-select, open, rename, delete | not measured as a whole |
| Notepad | `notepad.rs` | open, save, encodings, find, replace, go to, word wrap, zoom, status bar | menu row 96.9%, status bar 94.3% |

## Claude Code window (`claude.rs`)

Drives the official `claude` command-line tool, so you need it installed and signed in.

Works: session list with search, pin, rename and folding; streaming replies; markdown with tables,
code blocks and links; permission prompts; model, effort and mode pickers; image attachments by
path; right panel (environment, context use, files written, task list); docked Files and Changes
panes; idle helpers are stopped after 10 minutes to free memory.

Match against the real app: whole window 90.61% (composer row 96.5%, right panel 92.6%, title bar
91.8%, conversation 91.7%, sidebar 80.2%).

## Not done yet

- A cold sign-in in shell mode has not been tested. Dragging files onto taskbar buttons
  or the desktop is missing. Pins are Rusty Windows' own list; apps pinned in Explorer are not read.
- As the shell, Windows still loads SearchHost and ShellHost (about 135 MB together).
- Flyout animations and blurred backdrops are basic.
- Notepad has one document per window: no tabs, no spell check, and it is not yet opened in place of
  Windows Notepad.
- Snipping Tool, Task Manager, Photos, Media Player, Paint, Terminal and most Settings sub-pages
  still open the Windows originals.
- Claude Code window: terminal and preview panes, slash menu, pasted images, connectors and archive
  are missing.
- Planned and not started: a browser window, a Spotify window.
- Tested on one PC, one monitor, dark theme, English. Light theme and high-DPI mixes are unverified.

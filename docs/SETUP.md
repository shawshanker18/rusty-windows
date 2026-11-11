# Setup guide

## You need

- Windows 11
- [Rust](https://rustup.rs) with the MSVC toolchain (`stable-x86_64-pc-windows-msvc`)
- Visual Studio Build Tools with the "Desktop development with C++" workload (the linker). If the
  Windows SDK is present the build also stamps the file as "Rust UI"; without it the build still works.
- Optional: the [`claude` CLI](https://claude.com/claude-code), signed in, for the Claude Code window

## 1. Build

```powershell
git clone https://github.com/shawshanker18/rusty-windows
cd rusty-windows
cargo build --release
cargo test --release
```

The result is one file: `target\release\shellbits.exe` (the program's internal name is shellbits; Task
Manager shows it as "Rust UI").

## 2. Level 1: run it next to Explorer (safe)

```powershell
.\target\release\shellbits.exe
```

Explorer keeps running. The Rust panels answer the same keys and clicks (Start, Search, Quick
Settings, notifications, Alt+Tab, Win+X, Win+V, Win+Z and so on). Quit with **Ctrl+Alt+Q**.

Single windows can be opened on their own:

```powershell
.\target\release\shellbits.exe --calc
.\target\release\shellbits.exe --notepad notes.txt
.\target\release\shellbits.exe --files C:\
.\target\release\shellbits.exe --settings
.\target\release\shellbits.exe --claude
```

## 3. Level 2: start it at every sign-in

Copy it somewhere permanent and add a sign-in task:

```powershell
New-Item -ItemType Directory -Force "$env:USERPROFILE\shellbits" | Out-Null
Copy-Item target\release\shellbits.exe "$env:USERPROFILE\shellbits\" -Force
Copy-Item scripts\restore-explorer.cmd "$env:USERPROFILE\shellbits\" -Force
schtasks /create /tn shellbits /sc onlogon /tr "$env:USERPROFILE\shellbits\shellbits.exe" /f
```

Video wallpaper (optional): put `wallpaper.mp4` next to the exe. If the video is larger than your
screen, also add a scaled copy named `wallpaper-<screen height>.mp4` (for example
`wallpaper-1080.mp4`); a 4K video on a 1080p screen costs about 100 MB more than it needs to.

## 4. Level 3: make it the shell (replaces Explorer's taskbar and desktop)

Read this whole section first. This changes what starts when you sign in, for your user only
(`HKCU\...\Winlogon\Shell`). It needs no administrator rights and does not touch system files.

```powershell
& "$env:USERPROFILE\shellbits\shellbits.exe" --install-shell
```

Sign out and in again. To go back, any one of these works:

- press **Ctrl+Alt+Shift+E**
- run `%USERPROFILE%\shellbits\restore-explorer.cmd`
- run `shellbits.exe --restore-explorer`, then sign out and in
- if nothing is on screen: Ctrl+Shift+Esc opens Task Manager, then Run new task, `explorer.exe`

Try a dry run before your first real sign-in: `scripts\dryrun.ps1` stops Explorer, runs the Rust
shell, checks it and always puts Explorer back.

## Shortcuts added by Rusty Windows

| Keys | Does |
|---|---|
| Ctrl+Alt+Q | quit Rusty Windows |
| Ctrl+Alt+Shift+E | back to Explorer (shell mode) |
| Ctrl+Alt+C | Claude Code window |

All the usual Windows shortcuts (Win, Win+A, Win+N, Win+V, Win+X, Win+R, Win+Z, Win+arrows, Win+D,
Alt+Tab, Win+Tab, Win+.) open the Rust versions.

## Switches for testing

- Parts can be left out to measure them: `$env:SHELLBITS_SKIP = 'tray,desktop,preview,toast,wall'`
- Pictures without opening a window: `--snapshot-calc out.bmp "12+7="`, also `--snapshot-qs`,
  `--snapshot-alttab`, `--snapshot-winx`, `--snapshot-run`, `--snapshot-settings`, `--snapshot-claude`,
  `--snapshot-desktop`, `--snapshot-tray`, `--snapshot-nc`, `--snapshot-taskbar`
- Self-tests: `--toast-selftest`, `--nc-selftest`, `--lang-selftest`, `--emoji-selftest`,
  `--clip-selftest`, `--zones-selftest`, `--bt-selftest`, `--mixer-selftest`, `--tray-selftest`
- Crash log: `%TEMP%\shellbits-crashes.txt`

## Test scripts

`scripts\` holds the PowerShell and Python checks used during development (live key tests, pixel
comparisons, memory probes). They were written for one PC: read a script before running it. Several
move the mouse, press keys or stop Explorer for a short time, and all of them restore it.

## Remove it

```powershell
& "$env:USERPROFILE\shellbits\shellbits.exe" --restore-explorer
schtasks /delete /tn shellbits /f
Stop-Process -Name shellbits
Remove-Item "$env:USERPROFILE\shellbits" -Recurse
```

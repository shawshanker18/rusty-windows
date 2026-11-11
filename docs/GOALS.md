# Goals

**The goal: rewrite in Rust everything on a Windows PC that can be replaced without reflashing the
firmware.** If it runs after the firmware hands over to Windows and it can be swapped on a normal
installation, it is on this list, however hard it is.

Out of scope, because they would need a firmware flash or are fixed in hardware: UEFI/BIOS, the
embedded controller, Intel ME / AMD PSP, device firmware (SSD, GPU, Wi-Fi, Bluetooth, touchpad),
CPU microcode, and the TPM.

Status: **Done** (built and in daily use), **Partial**, **Planned** (nothing built yet).
Difficulty: **1** a window or a tool, **2** a large app, **3** a system component that Windows
expects to be its own, **4** kernel-mode or boot-critical, needing driver signing or a test-signed
system. Levels 3 and 4 are long-term and may only ever run on machines whose owner opts in.

## 1. The shell

| Item | Status | Difficulty |
|---|---|---|
| Taskbar, running apps, clock | Done | 1 |
| Pinned taskbar apps | Done (own pin list, not Windows' pins) | 1 |
| Drag onto taskbar buttons, taskbar settings | Planned | 1 |
| Start menu and app list | Done | 1 |
| Search (apps, settings, files, calculator answers) | Done | 1 |
| Full file-content search index (replacing Windows Search) | Planned | 3 |
| System tray, overflow, tray menus | Done | 1 |
| Quick Settings and its pages (Wi-Fi, Bluetooth, sound, accessibility, performance) | Done | 1 |
| Volume mixer | Done | 1 |
| Notification center, calendar, toasts | Done | 1 |
| Volume, brightness, media and lock-key flyouts | Done | 1 |
| Alt+Tab | Done | 1 |
| Task View | Partial | 1 |
| Virtual desktops | Planned | 2 |
| Snap layouts, Snap Assist, Win+arrow snapping | Done | 1 |
| Drag-to-edge snapping and the top snap bar | Planned | 1 |
| Win+X menu, Run box | Done | 1 |
| Clipboard history | Partial (text only) | 1 |
| Emoji, GIF, kaomoji and symbols panel | Partial (emoji only) | 1 |
| Input language switcher | Done | 1 |
| Touch keyboard, handwriting panel, voice typing | Planned | 2 |
| Input method editors (IME) for East Asian languages | Planned | 3 |
| Desktop icons, show desktop, peek | Done | 1 |
| Desktop right-click menu (Windows' own menu) | Done | 1 |
| Drag and drop on the desktop | Planned | 1 |
| Wallpaper: picture, slideshow, video | Partial (video and picture) | 1 |
| Widgets board | Planned | 2 |
| Window thumbnails and hover previews | Done | 1 |
| Jump lists | Partial | 1 |
| Shell-mode sign-in apps, crash watchdog, way back to Explorer | Done | 1 |
| Screen-edge and touchpad gestures | Planned | 2 |
| Multi-monitor taskbars and per-monitor DPI | Planned | 1 |
| Light theme, accent colours, high contrast | Planned | 1 |
| Acrylic and Mica backdrops, flyout animations | Partial | 1 |

## 2. Built-in apps

| Item | Status | Difficulty |
|---|---|---|
| Settings: every top-level page | Done | 2 |
| Settings: every sub-page (about 200) | Partial | 2 |
| Control Panel and its applets | Planned | 2 |
| File Explorer (tabs, preview, commands) | Partial | 2 |
| File Explorer: tree, drag and drop, view modes, network, zip folders, properties dialogs | Planned | 2 |
| Open and Save dialogs used by every app | Planned | 3 |
| Calculator: Standard, Scientific | Done | 1 |
| Calculator: Programmer, Date, Graphing, converters | Planned | 1 |
| Notepad | Partial (no tabs, no spell check) | 1 |
| Task Manager | Planned | 2 |
| Snipping Tool and screen recording | Planned | 2 |
| Photos viewer and editor | Planned | 2 |
| Media Player, Movies & TV | Planned | 2 |
| Paint | Planned | 2 |
| Terminal (tabs, ConPTY, profiles) | Planned | 2 |
| Clock, alarms, timers | Planned | 1 |
| Sound Recorder | Planned | 1 |
| Camera | Planned | 2 |
| Sticky Notes | Planned | 1 |
| Character Map, Magnifier, On-Screen Keyboard, Narrator front end | Planned | 2 |
| Registry Editor | Planned | 1 |
| Event Viewer, Services, Device Manager, Disk Management, Task Scheduler (the MMC consoles) | Planned | 2 |
| Resource Monitor, Performance Monitor | Planned | 2 |
| Disk Cleanup, Defragment and Optimise, System Information | Planned | 1 |
| Remote Desktop client | Planned | 3 |
| Quick Assist, Feedback Hub, Get Help, Tips | Planned | 1 |
| Microsoft Store front end | Planned | 3 |
| Windows Security front end (the dashboard, not the engine) | Planned | 2 |
| Print queue, printer dialogs | Planned | 2 |
| Fonts viewer, Mail, Calendar, Weather, Maps, Phone Link front ends | Planned | 2 |
| Xbox Game Bar overlay | Planned | 2 |

## 3. System dialogs and surfaces

| Item | Status | Difficulty |
|---|---|---|
| Message boxes, task dialogs, shutdown dialog | Planned | 1 |
| Open with, Properties, copy and move progress, conflict dialogs | Planned | 2 |
| Context menus across the shell | Partial | 2 |
| Autoplay, eject, device connected prompts | Planned | 1 |
| Wi-Fi password, VPN and network prompts | Planned | 2 |
| Bluetooth pairing prompts | Planned | 2 |
| Project (Win+P), Cast, nearby sharing, Share sheet | Planned | 2 |
| Print dialog | Planned | 2 |
| Lock screen | Planned | 3 |
| Sign-in screen and credential prompts (credential providers) | Planned | 3 |
| User Account Control prompt (runs on the secure desktop) | Planned | 3 |
| Ctrl+Alt+Del screen | Planned | 3 |
| First-run setup (OOBE) | Planned | 3 |
| Windows Update restart and progress screens | Planned | 3 |

## 4. Third-party apps (their windows, on top of their own services)

| Item | Status | Difficulty |
|---|---|---|
| Claude Code desktop window | Partial | 2 |
| A browser window around the system web engine | Planned | 2 |
| Spotify window | Planned | 2 |

## 5. System components behind the front end

These have no window. They are on the list because they can be replaced on a running Windows
installation, and several of them are where the remaining memory is.

| Item | Status | Difficulty |
|---|---|---|
| Notification platform host | Planned | 3 |
| Search indexer | Planned | 3 |
| Text input host and text services | Planned | 3 |
| Console host (conhost) | Planned | 3 |
| Shell infrastructure and runtime broker roles | Planned | 3 |
| Thumbnail and icon caches, file-type handlers, preview handlers | Planned | 2 |
| Shell extensions API so existing extensions still load | Planned | 3 |
| Print spooler | Planned | 3 |
| Audio session and device management service | Planned | 3 |
| Font cache service | Planned | 3 |
| Windows services written by third parties (updaters, tray helpers) replaced by one small scheduler | Planned | 2 |
| Task Scheduler service | Planned | 3 |
| Windows Error Reporting and diagnostics front ends | Planned | 2 |
| Desktop Window Manager (the compositor) | Planned | 4 |
| Session manager, logon and service control (winlogon, services) | Planned | 4 |
| Win32 window manager and graphics subsystem (win32k) | Planned | 4 |

## 6. Kernel mode and boot

Possible without touching firmware, since Windows loads these from disk, but they need signed
drivers or a test-signed system and a mistake means a machine that does not start.

| Item | Status | Difficulty |
|---|---|---|
| File-system filter drivers and minifilters | Planned | 4 |
| Network drivers and filters | Planned | 4 |
| Input, audio and display class drivers | Planned | 4 |
| Storage and file-system drivers | Planned | 4 |
| Boot manager and OS loader (files on the EFI partition, not firmware) | Planned | 4 |
| Recovery environment | Planned | 4 |
| The NT kernel and hardware abstraction layer | Planned | 4 |

## How the goal is judged

- **Same look:** each window is compared pixel by pixel with the original; the target is 99.99%.
- **Same behaviour:** everything shown is live system state; nothing is hard-coded.
- **Less memory:** measured in private bytes against the component it replaces, and published.
- **Faster:** animations and open times at least as fast as the original.
- **Always a way back:** every replacement can be undone by its owner without reinstalling Windows.

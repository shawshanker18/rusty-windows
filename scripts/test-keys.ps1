# shellbits live test: fires every keybind / taskbar click, times how fast the right panel appears,
# probes the UI thread for hangs, screenshots each step into snap\test, and checks it closes again.
# Leaves volume, media, Caps Lock and the mouse where it found them.
param([switch]$NoShots)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices; using System.Diagnostics; using System.Threading;
public static class T {
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int x, int y, uint d, UIntPtr e);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
  [DllImport("user32.dll")] public static extern IntPtr FindWindow(string c, IntPtr t);
  [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "FindWindowW")] public static extern IntPtr FindWindowT(string c, string t);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern short GetKeyState(int vk);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern IntPtr SendMessageTimeout(IntPtr h, uint m, IntPtr w, IntPtr l, uint f, uint t, out IntPtr r);
  public struct POINT { public int X, Y; }
  public static void Key(byte vk, bool up) { keybd_event(vk, 0, up ? 2u : 0u, UIntPtr.Zero); }
  public static void Click(int x, int y) { SetCursorPos(x, y); Thread.Sleep(30); mouse_event(2, 0, 0, 0, UIntPtr.Zero); Thread.Sleep(30); mouse_event(4, 0, 0, 0, UIntPtr.Zero); }
  // ms until window visibility == want (or -1 on timeout)
  public static long WaitVis(IntPtr h, bool want, int timeout) { var sw = Stopwatch.StartNew(); while (sw.ElapsedMilliseconds < timeout) { if (IsWindowVisible(h) == want) return sw.ElapsedMilliseconds; Thread.Sleep(5); } return -1; }
  // worst UI-thread reply time while probing for ms
  public static long Probe(IntPtr h, int ms) { long max = 0; var end = Stopwatch.StartNew(); while (end.ElapsedMilliseconds < ms) { var sw = Stopwatch.StartNew(); IntPtr r; SendMessageTimeout(h, 0, IntPtr.Zero, IntPtr.Zero, 2, 5000, out r); max = Math.Max(max, sw.ElapsedMilliseconds); Thread.Sleep(15); } return max; }
}
'@
$dir = Join-Path $PSScriptRoot 'snap\test'; New-Item -ItemType Directory -Force $dir | Out-Null
$W = @{}; foreach ($c in 'flyout','launcher','qs','nc','media','cal') { $W[$c] = [T]::FindWindow("shellbits_$c", [IntPtr]::Zero) }
if ($W.flyout -eq [IntPtr]::Zero) { throw 'shellbits is not running' }
$p = New-Object T+POINT; [void][T]::GetCursorPos([ref]$p)

function shot($name) { if ($NoShots) { return }; $b = New-Object System.Drawing.Bitmap 1920,1080; $g = [System.Drawing.Graphics]::FromImage($b); $g.CopyFromScreen(0,0,0,0,$b.Size); $b.Save("$dir\$name.png"); $g.Dispose(); $b.Dispose() }
function combo([byte[]]$keys) { foreach ($k in $keys) { [T]::Key($k, $false) }; [array]::Reverse($keys); foreach ($k in $keys) { [T]::Key($k, $true) } }
function uia { $script:zones = @{} } # zone positions come from the live taskbar
# Where the taskbar buttons are: asked through the Rust UI's own self-test (COM UI Automation).
# PowerShell's managed UI Automation was seen returning nothing for the taskbar, so it is not used.
$zoneFile = Join-Path $env:TEMP 'shellbits-zones.txt'
Start-Process "$env:USERPROFILE\shellbits\shellbits.exe" -ArgumentList '--zones-selftest', $zoneFile -Wait
$zone = @{}; Get-Content $zoneFile | ForEach-Object { $p = $_ -split ' '; if ($p.Count -eq 5 -and [int]$p[3] -gt [int]$p[1]) { $zone[$p[0]] = @([int](([int]$p[1] + [int]$p[3]) / 2), [int](([int]$p[2] + [int]$p[4]) / 2)) } }
$start = $zone['StartButton']; $search = $zone['SearchButton']; $clock = $zone['SystemTray.OmniButtonLeft']; $bell = $zone['SystemTray.OmniButtonRight']; $vol = $zone['SystemTray.OmniButtonCenter']
if (-not ($start -and $search -and $clock -and $bell -and $vol)) { 'could not locate the taskbar buttons; stopping before any click'; exit 1 }
$empty = @(300, 1056) # bare taskbar: a safe place to click "outside"

$results = @()
function step($name, [scriptblock]$open, $win, [scriptblock]$close, $closeName) {
  Start-Sleep -Milliseconds 400
  & $open
  $t = [T]::WaitVis($W[$win], $true, 2500)
  $hang = [T]::Probe($W.flyout, 600)
  $focus = [T]::GetForegroundWindow() -eq $W[$win]
  shot $name
  & $close
  $c = [T]::WaitVis($W[$win], $false, 2500)
  $script:results += [pscustomobject]@{ Test = $name; Opens = $(if ($t -ge 0) { "$t ms" } else { 'NO' }); Focused = $focus; WorstUiReply = "$hang ms"; Closes = $(if ($c -ge 0) { "$closeName $c ms" } else { "NO ($closeName)" }) }
  if ($c -lt 0) { & $esc; Start-Sleep -Milliseconds 300 }
}
function escTo($win) { [void][T]::PostMessage($W[$win], 0x100, [IntPtr]0x1B, [IntPtr]::Zero) }
$esc = { foreach ($k in 'launcher','qs','nc') { if ([T]::IsWindowVisible($W[$k])) { escTo $k } } }
$escEsc = { & $esc; Start-Sleep -Milliseconds 150; & $esc }
$clickOut = { [T]::Click($empty[0], $empty[1]) }

step 'win-tap-start'   { combo 0x5B }                    launcher $esc 'Esc'
step 'win-s-search'    { combo 0x5B,0x53; Start-Sleep -Milliseconds 300; foreach ($c in 'note'.ToCharArray()) { combo ([byte][char]::ToUpper($c)) } } launcher $escEsc 'Esc Esc'
step 'start-click'     { [T]::Click($start[0], $start[1]) }   launcher $clickOut 'click outside'
step 'search-click'    { [T]::Click($search[0], $search[1]) } launcher $clickOut 'click outside'
step 'win-a-quick'     { combo 0x5B,0x41 }               qs $esc 'Esc'
step 'volume-click'    { [T]::Click($vol[0], $vol[1]) }  qs $clickOut 'click outside'
step 'win-n-notify'    { combo 0x5B,0x4E }               nc $esc 'Esc'
step 'clock-click'     { [T]::Click($clock[0], $clock[1]) } nc $clickOut 'click outside'
step 'bell-click'      { [T]::Click($bell[0], $bell[1]) }   nc { [T]::Click($bell[0], $bell[1]) } 'click again'
$W.winx = [T]::FindWindowT('shellbits_winx', 'Quick Link')
$W.run = [T]::FindWindow('shellbits_run', [IntPtr]::Zero)
step 'win-x-menu'      { combo 0x5B,0x58 }               winx { [void][T]::PostMessage($W.winx, 0x8000 + 40, [IntPtr]::Zero, [IntPtr]::Zero) } 'dismiss'
step 'start-rightclick' { [T]::SetCursorPos($start[0], $start[1]); Start-Sleep -Milliseconds 30; [T]::mouse_event(8,0,0,0,[UIntPtr]::Zero); Start-Sleep -Milliseconds 30; [T]::mouse_event(16,0,0,0,[UIntPtr]::Zero) } winx $clickOut 'click outside'
step 'win-r-run'       { combo 0x5B,0x52 }               run { [void][T]::PostMessage($W.run, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) } 'close'
step 'volume-keys'    { combo 0xAF; Start-Sleep -Milliseconds 80; combo 0xAE } flyout { } 'auto-hide'
step 'caps-lock'       { combo 0x14; Start-Sleep -Milliseconds 80; combo 0x14 } flyout { } 'auto-hide'
step 'ctrl-alt-up'     { combo 0x11,0x12,0x26; Start-Sleep -Milliseconds 80; combo 0x11,0x12,0x28 } flyout { } 'auto-hide'

# Alt+Tab: hold Alt, Tab, look, Esc cancels (no switch), release Alt
$W.alttab = [T]::FindWindow('shellbits_alttab', [IntPtr]::Zero)
Start-Sleep -Milliseconds 400
$before = [T]::GetForegroundWindow()
[T]::Key(0x12, $false); [T]::Key(0x09, $false); [T]::Key(0x09, $true)
$t = [T]::WaitVis($W.alttab, $true, 1500); Start-Sleep -Milliseconds 250; shot 'alt-tab'
[T]::Key(0x12, $true)
$c = [T]::WaitVis($W.alttab, $false, 1500); Start-Sleep -Milliseconds 300
$switched = [T]::GetForegroundWindow() -ne $before
[T]::Key(0x12, $false); [T]::Key(0x09, $false); [T]::Key(0x09, $true); [T]::Key(0x12, $true); Start-Sleep -Milliseconds 400
$back = [T]::GetForegroundWindow() -eq $before
$results += [pscustomobject]@{ Test = 'alt-tab'; Opens = $(if ($t -ge 0) { "$t ms" } else { 'NO' }); Focused = "switched: $switched"; WorstUiReply = "$([T]::Probe($W.flyout, 300)) ms"; Closes = $(if ($c -ge 0) { "release Alt $c ms, quick Alt+Tab back: $back" } else { 'NO' }) }

# Windows' own shortcuts must pass through untouched: Win+Shift+S must not open our search
Start-Sleep -Milliseconds 300
combo 0x5B,0x10,0x53; Start-Sleep -Milliseconds 900
$leak = @('launcher','qs','nc') | Where-Object { [T]::IsWindowVisible($W[$_]) }
$snip = (Get-Process ScreenClippingHost -ErrorAction SilentlyContinue) -ne $null
shot 'win-shift-s'
Stop-Process -Name ScreenClippingHost -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 300
$results += [pscustomobject]@{ Test = 'win-shift-s passthrough'; Opens = $(if ($snip) { 'snipping overlay' } else { 'no snip' }); Focused = '-'; WorstUiReply = '-'; Closes = $(if ($leak) { "LEAKED: $leak" } else { 'ours stayed closed' }) }

# stress: 12 fast toggles each, must end closed with no hang
$worst = 0
foreach ($k in 0x41, 0x4E) { for ($i = 0; $i -lt 12; $i++) { combo 0x5B,$k; Start-Sleep -Milliseconds 60 } }
$worst = [T]::Probe($W.flyout, 1500)
$open = @('launcher','qs','nc') | Where-Object { [T]::IsWindowVisible($W[$_]) }
$results += [pscustomobject]@{ Test = 'stress 24 fast toggles'; Opens = '-'; Focused = '-'; WorstUiReply = "$worst ms"; Closes = $(if ($open) { "left open: $open" } else { 'all closed' }) }
& $esc

[void][T]::SetCursorPos($p.X, $p.Y)
$results | Format-Table -AutoSize | Out-String -Width 200
$results | ConvertTo-Json | Set-Content -Encoding utf8 "$dir\results.json"

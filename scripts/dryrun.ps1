# Explorer-off dry run: run Rust UI as the shell for ~30 s, record what works, then ALWAYS put
# Explorer back. Your sign-in setting is never touched. Runs detached so the restore happens even if
# the session that started it goes away.
$ErrorActionPreference = 'Continue'
$dir = Join-Path $PSScriptRoot 'snap\dryrun'; New-Item -ItemType Directory -Force $dir | Out-Null
$log = Join-Path $dir 'log.txt'; Set-Content $log "dry run $(Get-Date -Format s)" -Encoding utf8
function note($m) { Add-Content $log "$(Get-Date -Format HH:mm:ss.fff) $m" -Encoding utf8 }
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class DR { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, string t);
 [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l); }
'@
function shot($n) { $b = New-Object System.Drawing.Bitmap 1920,1080; $g = [System.Drawing.Graphics]::FromImage($b); $g.CopyFromScreen(0,0,0,0,$b.Size); $b.Save("$dir\$n.png"); $g.Dispose(); $b.Dispose() }
function owner($cls, $title) { $h = [DR]::FindWindow($cls, $title); if ($h -eq [IntPtr]::Zero) { return 'none' }; $p = 0; [void][DR]::GetWindowThreadProcessId($h, [ref]$p); try { (Get-Process -Id $p).ProcessName } catch { "pid $p" } }
$exe = "$env:USERPROFILE\shellbits\shellbits.exe"
try {
  shot '0-before'
  Stop-Process -Name shellbits -Force -ErrorAction SilentlyContinue
  # clean exit (Exit Explorer): unlike killing it, Windows doesn't restart it automatically
  $tray = [DR]::FindWindow('Shell_TrayWnd', $null)
  [void][DR]::PostMessage($tray, 0x5B4, [IntPtr]::Zero, [IntPtr]::Zero)
  $t = [Diagnostics.Stopwatch]::StartNew(); while ((Get-Process explorer -ErrorAction SilentlyContinue) -and $t.Elapsed.TotalSeconds -lt 8) { Start-Sleep -Milliseconds 200 }
  Start-Sleep -Milliseconds 800
  note ("explorer running after kill: " + [bool](Get-Process explorer -ErrorAction SilentlyContinue))
  # the taskbar going away is what counts: Explorer's process can linger without it (that once left the
  # user with no taskbar when the run was 'skipped'), so a leftover process is stopped and the run goes on
  if ([DR]::FindWindow('Shell_TrayWnd', $null) -ne [IntPtr]::Zero) { throw 'Explorer kept its taskbar: test skipped' }
  Stop-Process -Name explorer -Force -ErrorAction SilentlyContinue; Start-Sleep 1
  Remove-Item "$env:TEMP\shellbits-tray.txt","$env:TEMP\shellbits-toast.txt" -ErrorAction SilentlyContinue
  $env:SHELLBITS_TOAST_TEST = '1'   # show the test banner even though Do Not Disturb is on
  Start-Process $exe -ArgumentList '--shell','--restarted'   # --restarted: do not launch the sign-in startup apps again on every test run
  Start-Sleep 6
  note ("Shell_TrayWnd owner: " + (owner 'Shell_TrayWnd'))
  note ("taskbar window: " + (owner 'shellbits_taskbar' '') + ", desktop window: " + (owner 'shellbits_desktop' 'Desktop') + ", wallpaper: " + (owner 'shellbits_wall' 'Wallpaper'))
  note ("explorer running during test: " + [bool](Get-Process explorer -ErrorAction SilentlyContinue))
  shot '1-shell-6s'
  Start-Sleep 8   # let the shell finish starting; banners only fire for notifications that arrive after that
  try {
    [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType=WindowsRuntime] | Out-Null
    $xml = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02)
    $tx = $xml.GetElementsByTagName('text'); [void]$tx.Item(0).AppendChild($xml.CreateTextNode('Rust UI test')); [void]$tx.Item(1).AppendChild($xml.CreateTextNode('Toast banner check'))
    [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe').Show([Windows.UI.Notifications.ToastNotification]::new($xml))
    $sw = [Diagnostics.Stopwatch]::StartNew(); $tw = [DR]::FindWindow('shellbits_toast', 'New notification')
    while (-not [DR]::IsWindowVisible($tw) -and $sw.ElapsedMilliseconds -lt 8000) { Start-Sleep -Milliseconds 50 }
    note ("toast window visible: " + [DR]::IsWindowVisible($tw) + " after $($sw.ElapsedMilliseconds) ms")
    note ("toast watcher: " + ((Get-Content "$env:TEMP/shellbits-toast.txt" -ErrorAction SilentlyContinue) -join ' ; '))
    shot '1b-toast'
  } catch { note ("toast test error: " + $_) }
  Start-Sleep 8
  shot '2-shell-14s'
  # shell shortcuts: Win+I must open Settings, Win+D must minimise everything
  Add-Type @'
using System; using System.Runtime.InteropServices;
public static class KB { [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra); [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow(); [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
 public static void Win(byte vk){ keybd_event(0x5B,0,0,UIntPtr.Zero); keybd_event(vk,0,0,UIntPtr.Zero); keybd_event(vk,0,2,UIntPtr.Zero); keybd_event(0x5B,0,2,UIntPtr.Zero); } }
'@
  $had = [bool](Get-Process SystemSettings -ErrorAction SilentlyContinue)
  [KB]::Win(0x49); Start-Sleep 4
  $rs = [DR]::FindWindow('shellbits_settings', 'Settings')
  note ("Win+I opened Rust Settings: " + [DR]::IsWindowVisible($rs) + "; Windows' own Settings started: " + ([bool](Get-Process SystemSettings -ErrorAction SilentlyContinue) -and -not $had))
  [void][DR]::PostMessage($rs, 0x10, [IntPtr]::Zero, [IntPtr]::Zero); Start-Sleep -Milliseconds 500
  # Win+D must cover every open app window with the desktop, and a second Win+D must uncover them
  Add-Type @'
using System; using System.Runtime.InteropServices; using System.Text;
public static class WP { [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(POINT p); public struct POINT { public int X, Y; }
 [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr h, uint f);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
 public static string TopAt(int x, int y){ POINT p; p.X=x; p.Y=y; IntPtr h=GetAncestor(WindowFromPoint(p),2); var s=new StringBuilder(128); GetClassName(h,s,128); return s.ToString(); } }
'@
  $pts = @(@(300,300), @(960,500), @(1500,700))
  note ("window classes on screen before Win+D: " + (($pts | ForEach-Object { [WP]::TopAt($_[0], $_[1]) }) -join ', '))
  [KB]::Win(0x44); Start-Sleep -Milliseconds 1500
  note ("after Win+D: " + (($pts | ForEach-Object { [WP]::TopAt($_[0], $_[1]) }) -join ', ') + " (expect the Rust desktop everywhere)")
  $sb = Get-Process shellbits -ErrorAction SilentlyContinue | Select-Object -First 1
  note ("Rust UI after Win+D: running=" + [bool]$sb + " threads=" + $sb.Threads.Count)
  [KB]::Win(0x44); Start-Sleep -Milliseconds 1500
  note ("after Win+D again: " + (($pts | ForEach-Object { [WP]::TopAt($_[0], $_[1]) }) -join ', ') + " (expect the same as before)")
  note ("key log: " + ((Get-Content "$env:TEMP/shellbits-keys.txt" -ErrorAction SilentlyContinue) -join ' ; '))
  Remove-Item "$env:TEMP/shellbits-keys.txt" -ErrorAction SilentlyContinue
  note ("tray icons registered: " + ((Get-Content "$env:TEMP\shellbits-tray.txt" -ErrorAction SilentlyContinue) -join ' || '))
  # open the ^ overflow like a user would
  Add-Type @'
using System; using System.Runtime.InteropServices;
public static class CL { [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y); [DllImport("user32.dll")] public static extern void mouse_event(uint f, int x, int y, uint d, UIntPtr e); }
'@
  [void][CL]::SetCursorPos(1650, 1056); Start-Sleep -Milliseconds 100; [CL]::mouse_event(2,0,0,0,[UIntPtr]::Zero); [CL]::mouse_event(4,0,0,0,[UIntPtr]::Zero)
  Start-Sleep -Milliseconds 800
  shot '2b-overflow'
  # hover a taskbar app: its live preview should appear
  [void][CL]::SetCursorPos(300, 1056); [CL]::mouse_event(2,0,0,0,[UIntPtr]::Zero); [CL]::mouse_event(4,0,0,0,[UIntPtr]::Zero); Start-Sleep -Milliseconds 300
  # app icons are centred, so where they sit depends on how many there are: sweep until a preview appears
  $appX = 0; foreach ($x in 1010,1032,1054,1076,1098,1120,1142,1164) { [void][CL]::SetCursorPos($x - 4, 1050); Start-Sleep -Milliseconds 80; [void][CL]::SetCursorPos($x, 1056); Start-Sleep -Milliseconds 900; if ([DR]::IsWindowVisible([DR]::FindWindow('shellbits_peek', ''))) { $appX = $x; break } }
  note ("taskbar app found at x=$appX")
  note ("heartbeats: " + ((Get-Content "$env:TEMP/shellbits-keys.txt" -ErrorAction SilentlyContinue | Where-Object { $_ -match '^hb ' }) -join ' ; '))
  note ("hover preview visible: " + [DR]::IsWindowVisible([DR]::FindWindow('shellbits_peek', '')))
  note ("peek log: " + ((Get-Content "$env:TEMP/shellbits-keys.txt" -ErrorAction SilentlyContinue) -join ' ; '))
  shot '2d-peek'
  # right-click that taskbar app: its menu should list recent items; read the menu, then cancel it (no key sent)
  Add-Type @'
using System; using System.Runtime.InteropServices; using System.Text;
public static class MN { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern int GetMenuItemCount(IntPtr m);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetMenuString(IntPtr m, uint i, StringBuilder s, int n, uint f);
 public static string Items(IntPtr menuWnd){ IntPtr m=SendMessage(menuWnd,0x01E1,IntPtr.Zero,IntPtr.Zero); if(m==IntPtr.Zero) return "no menu handle"; int n=GetMenuItemCount(m); var o=new StringBuilder(); for(uint i=0;i<n;i++){ var s=new StringBuilder(260); GetMenuString(m,i,s,260,0x400); o.Append(s.Length==0?"---":s.ToString()); o.Append(" | "); } return n+" items: "+o; } }
'@
  [CL]::mouse_event(8,0,0,0,[UIntPtr]::Zero); [CL]::mouse_event(16,0,0,0,[UIntPtr]::Zero); Start-Sleep -Milliseconds 900
  $menu = [MN]::FindWindow('#32768', [IntPtr]::Zero)
  note ("taskbar right-click menu: " + $(if ($menu -ne [IntPtr]::Zero) { [MN]::Items($menu) } else { 'did not open' }))
  shot '2e-menu'
  [void][MN]::PostMessage([MN]::FindWindow('shellbits_taskbar', [IntPtr]::Zero), 0x001F, [IntPtr]::Zero, [IntPtr]::Zero); Start-Sleep -Milliseconds 400
  note ("menu closed again: " + ([MN]::FindWindow('#32768', [IntPtr]::Zero) -eq [IntPtr]::Zero))
  # Win+Tab (Task View) and Win+Z (snap picker) must also work when Rust UI is the shell
  [KB]::Win(0x09); Start-Sleep -Milliseconds 800
  note ("Win+Tab opened Task View: " + [DR]::IsWindowVisible([MN]::FindWindow('shellbits_alttab', [IntPtr]::Zero)))
  shot '2f-taskview'
  [KB]::Win(0x09); Start-Sleep -Milliseconds 600
  note ("Win+Tab closed it: " + (-not [DR]::IsWindowVisible([MN]::FindWindow('shellbits_alttab', [IntPtr]::Zero))))
  # Win+arrows on a window this script owns: right half (+ Snap Assist on the left), then the top-right quarter
  Add-Type -AssemblyName System.Windows.Forms
  Add-Type @'
using System; using System.Runtime.InteropServices;
public static class AR { [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out RECT r, int n); public struct RECT { public int L,T,R,B; }
 [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h); }
'@
  function Pump($ms) { $t=[Diagnostics.Stopwatch]::StartNew(); while ($t.ElapsedMilliseconds -lt $ms) { [System.Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 30 } }
  $f = New-Object System.Windows.Forms.Form; $f.Text = 'arrow test'; $f.StartPosition = 'Manual'; $f.SetBounds(400,200,620,420); $f.Show(); $f.Activate(); Pump 600
  [void][AR]::SetForegroundWindow($f.Handle); Pump 300
  if ([KB]::GetForegroundWindow() -ne $f.Handle) { note 'Win+arrows: skipped, the test window did not come to the front' } else {
    $wa = [System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea; $mid = [int]($wa.Left + $wa.Width/2); $half = [int]($wa.Top + $wa.Height/2)
    $as = [MN]::FindWindow('shellbits_assist', [IntPtr]::Zero); $q = New-Object AR+RECT
    [KB]::Win(0x27); Pump 900; [void][AR]::DwmGetWindowAttribute($f.Handle, 9, [ref]$q, 16)
    note ("Win+Right -> right half: " + ($q.L -eq $mid -and $q.T -eq $wa.Top -and $q.R -eq $wa.Right -and $q.B -eq $wa.Bottom) + " (window $($q.L),$($q.T) - $($q.R),$($q.B); work area $($wa.Left),$($wa.Top) - $($wa.Right),$($wa.Bottom))")
    note ("Snap Assist opened on the other half: " + [DR]::IsWindowVisible($as))
    shot '2g-assist'
    [void][DR]::PostMessage($as, 0x8000+69, [IntPtr]0x1B, [IntPtr]::Zero); Pump 300
    note ("Snap Assist closed: " + (-not [DR]::IsWindowVisible($as)))
    [KB]::Win(0x26); Pump 700; [void][AR]::DwmGetWindowAttribute($f.Handle, 9, [ref]$q, 16)
    note ("Win+Up -> top-right quarter: " + ($q.L -eq $mid -and $q.T -eq $wa.Top -and $q.R -eq $wa.Right -and $q.B -eq $half) + " (window $($q.L),$($q.T) - $($q.R),$($q.B))")
  }
  $f.Close(); $f.Dispose()
  [void][CL]::SetCursorPos(300, 900)
  [void][CL]::SetCursorPos(300, 1056); [CL]::mouse_event(2,0,0,0,[UIntPtr]::Zero); [CL]::mouse_event(4,0,0,0,[UIntPtr]::Zero)
  # crash recovery as the shell: make it crash on purpose (flag file + message to its own window); the
  # watchdog copy must bring the taskbar and desktop back, without launching the startup apps again
  $fly = [MN]::FindWindow('shellbits_flyout', [IntPtr]::Zero); $oldPid = 0; [void][DR]::GetWindowThreadProcessId($fly, [ref]$oldPid)
  $appsBefore = @(Get-Process | Where-Object { $_.MainWindowTitle }).Count
  Set-Content "$env:TEMP\shellbits-crash-test" 'x'; $sw = [Diagnostics.Stopwatch]::StartNew()
  [void][DR]::PostMessage($fly, 0x312, [IntPtr]91, [IntPtr]::Zero)
  $newPid = 0; while ($sw.ElapsedMilliseconds -lt 20000) { Start-Sleep -Milliseconds 100; $t2 = [MN]::FindWindow('shellbits_taskbar', [IntPtr]::Zero); $newPid = 0; if ($t2 -ne [IntPtr]::Zero) { [void][DR]::GetWindowThreadProcessId($t2, [ref]$newPid) }; if ($newPid -ne 0 -and $newPid -ne $oldPid) { break } }
  note ("after a forced crash: taskbar back from a new copy: " + ($newPid -ne 0 -and $newPid -ne $oldPid) + " in $($sw.ElapsedMilliseconds) ms; Shell_TrayWnd owner: " + (owner 'Shell_TrayWnd') + "; desktop window: " + (owner 'shellbits_desktop' 'Desktop'))
  Start-Sleep 4
  $cmd = (Get-CimInstance Win32_Process -Filter "ProcessId=$newPid" -ErrorAction SilentlyContinue).CommandLine
  note ("restarted copy's command line: $cmd; windows with a title before/after: $appsBefore / " + @(Get-Process | Where-Object { $_.MainWindowTitle }).Count)
  shot '2h-after-crash'
  # what is actually in memory while Rust UI is the shell (private MB): Windows' own hosts that survive, and Rust UI
  $hosts = foreach ($n in 'explorer','StartMenuExperienceHost','SearchHost','ShellExperienceHost','TextInputHost','ShellHost') { $q = Get-Process $n -ErrorAction SilentlyContinue; "$n=" + $(if ($q) { [math]::Round((($q | Measure-Object PrivateMemorySize64 -Sum).Sum / 1MB)) } else { 'gone' }) }
  note ("memory as the shell: " + ($hosts -join ', ') + "; Rust UI=" + [math]::Round(((Get-Process shellbits | Measure-Object PrivateMemorySize64 -Sum).Sum / 1MB)) + " MB, threads " + ((Get-Process shellbits | ForEach-Object { $_.Threads.Count }) -join '+'))
  note ("startup apps seen: " + ((Get-Process | Where-Object { $_.StartTime -gt (Get-Date).AddSeconds(-20) } | Select-Object -ExpandProperty ProcessName -Unique) -join ', '))
  Start-Sleep 10
} catch { note ("error: " + $_) }
finally {
  # always restore Explorer, then Rust UI in its normal mode
  Stop-Process -Name shellbits -Force -ErrorAction SilentlyContinue
  Start-Sleep -Milliseconds 800
  # give the screen back its full height (our shell reserved the taskbar strip)
  if (-not (Get-Process explorer -ErrorAction SilentlyContinue)) { Start-Process explorer.exe }
  Start-Sleep 5
  # Explorer running but without its taskbar = it isn't the shell: restart it so it is
  if ((owner 'Shell_TrayWnd') -ne 'explorer') { Stop-Process -Name explorer -Force -ErrorAction SilentlyContinue; Start-Sleep 2; if (-not (Get-Process explorer -ErrorAction SilentlyContinue)) { Start-Process explorer.exe }; Start-Sleep 5 }
  schtasks /run /tn shellbits | Out-Null
  Start-Sleep 3
  note ("restored: explorer=" + [bool](Get-Process explorer -ErrorAction SilentlyContinue) + " shellbits=" + [bool](Get-Process shellbits -ErrorAction SilentlyContinue) + " tray owner=" + (owner 'Shell_TrayWnd'))
  shot '3-restored'
  note 'done'
}

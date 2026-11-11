# Launch apps through the Rust Search the way a person does (type a name, Enter), posted to our own
# launcher window - no real keys. For each: did its process start, did its window appear, and did it
# end up IN FRONT (an app that opens behind everything looks like "nothing happened").
# Each app opened here is closed again by WM_CLOSE to its own window.
param([string[]]$Apps = @('character map|charmap|', 'media player|Microsoft.Media.Player|', 'control panel||CabinetWClass', 'calculator||shellbits_calc'))
Add-Type @'
using System; using System.Text; using System.Runtime.InteropServices;
public static class LT { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
 public static string Desc(IntPtr h) { var c = new StringBuilder(128); var t = new StringBuilder(128); GetClassName(h, c, 128); GetWindowText(h, t, 128); uint p; GetWindowThreadProcessId(h, out p); string n = ""; try { n = System.Diagnostics.Process.GetProcessById((int)p).ProcessName; } catch {} return n + " | " + c + " | " + t; } }
'@
$l = [LT]::FindWindow('shellbits_launcher', [IntPtr]::Zero)
if ($l -eq [IntPtr]::Zero) { 'no Rust launcher window'; exit 1 }
foreach ($a in $Apps) {
  $q, $proc, $cls = $a -split '\|'
  $fgBefore = [LT]::GetForegroundWindow()
  $before = if ($proc) { @(Get-Process $proc -ErrorAction SilentlyContinue).Count } else { 0 }
  [void][LT]::PostMessage($l, 0x8000 + 3, [IntPtr]1, [IntPtr]::Zero); Start-Sleep -Milliseconds 800
  foreach ($c in $q.ToCharArray()) { [void][LT]::PostMessage($l, 0x102, [IntPtr][int]$c, [IntPtr]::Zero) }; Start-Sleep -Milliseconds 1200
  [void][LT]::PostMessage($l, 0x100, [IntPtr]0x0D, [IntPtr]::Zero)
  Start-Sleep -Seconds 4
  $fg = [LT]::GetForegroundWindow()
  $started = if ($proc) { @(Get-Process $proc -ErrorAction SilentlyContinue).Count -gt $before } else { '-' }
  $win = if ($cls) { $h = [LT]::FindWindow($cls, [IntPtr]::Zero); $h -ne [IntPtr]::Zero -and [LT]::IsWindowVisible($h) } else { '-' }
  "{0,-14} process started: {1,-5} window: {2,-5} in front now: {3}" -f $q, $started, $win, [LT]::Desc($fg)
  # close what was opened: the window in front if it is not what was in front before
  if ($fg -ne $fgBefore -and ([LT]::Desc($fg) -notmatch 'Claude|explorer \| Shell_TrayWnd|shellbits_launcher')) { [void][LT]::PostMessage($fg, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) }
  if ($cls) { $h = [LT]::FindWindow($cls, [IntPtr]::Zero); if ($h -ne [IntPtr]::Zero -and $h -ne $fg) { [void][LT]::PostMessage($h, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) } }
  if ([LT]::IsWindowVisible($l)) { [void][LT]::PostMessage($l, 0x8000 + 3, [IntPtr]1, [IntPtr]::Zero) }
  Start-Sleep -Milliseconds 800
}

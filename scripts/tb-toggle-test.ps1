# Shell mode: a real click on a taskbar button opens its panel and a second real click closes it.
# Uses real mouse clicks (the bug lived in the mouse hook), then puts the pointer back.
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class TT {
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
 [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
 [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint x, uint y, uint d, UIntPtr e);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
 public struct POINT { public int X, Y; } public struct RECT { public int L, T, R, B; }
 public static void Click(int x, int y) { SetCursorPos(x, y); System.Threading.Thread.Sleep(120); mouse_event(2,0,0,0,UIntPtr.Zero); System.Threading.Thread.Sleep(60); mouse_event(4,0,0,0,UIntPtr.Zero); }
}
'@
$bar = [TT]::FindWindow('shellbits_taskbar', [IntPtr]::Zero)
if ($bar -eq [IntPtr]::Zero) { 'no Rust taskbar window: not run'; exit 1 }
$r = New-Object TT+RECT; [void][TT]::GetWindowRect($bar, [ref]$r)
$old = New-Object TT+POINT; [void][TT]::GetCursorPos([ref]$old)
$y = [int](($r.T + $r.B) / 2)
# clock sits ~75 px from the right edge, the volume group ~170 px
foreach ($t in @(@('notification center', 'shellbits_nc', 75), @('Quick Settings', 'shellbits_qs', 170))) {
  $x = $r.R - $t[2]
  $vis = { [TT]::IsWindowVisible([TT]::FindWindow($t[1], [IntPtr]::Zero)) }
  [TT]::Click($x, $y); Start-Sleep -Milliseconds 900; $a = & $vis
  [TT]::Click($x, $y); Start-Sleep -Milliseconds 900; $b = & $vis
  "{0}: open after first click = {1}, open after second click = {2}  ->  {3}" -f $t[0], $a, $b, $(if ($a -and -not $b) { 'PASS' } else { 'FAIL' })
  if ($b) { [TT]::Click($x, $y); Start-Sleep -Milliseconds 400 }
}
[void][TT]::SetCursorPos($old.X, $old.Y)

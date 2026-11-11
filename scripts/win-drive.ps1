# Drive ONE Rust window without the mouse or keyboard and capture it: clicks are posted to the window
# itself, pictures come from PrintWindow. Usage:
#   win-drive.ps1 -Class shellbits_settings -Open 96 -Do "shot a; click 120 300; wait 600; shot b" [-Keep]
# click/dbl/rclick/wheel take window coordinates in real pixels. Pictures: shellbits\snap\drive\<name>.png
param([string]$Class, [int]$Open = 0, [string]$Do, [switch]$Keep)
Add-Type -AssemblyName System.Drawing
Add-Type -ReferencedAssemblies System.Drawing @'
using System; using System.Drawing; using System.Runtime.InteropServices;
public static class WD { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
 [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
 public struct RECT { public int L, T, R, B; }
 public static string Shot(IntPtr h, string path) { RECT r; GetWindowRect(h, out r); int w = r.R - r.L, hh = r.B - r.T; if (w <= 0 || hh <= 0) return "no size"; using (var b = new Bitmap(w, hh)) { using (var g = Graphics.FromImage(b)) { var dc = g.GetHdc(); PrintWindow(h, dc, 2); g.ReleaseHdc(dc); } b.Save(path); } return w + "x" + hh; } }
'@
$out = "$PSScriptRoot\shellbits\snap\drive"; New-Item -ItemType Directory -Force $out | Out-Null
$h = [WD]::FindWindow($Class, [IntPtr]::Zero); $wasOpen = $h -ne [IntPtr]::Zero -and [WD]::IsWindowVisible($h)
if (-not $wasOpen -and $Open) {
  [void][WD]::PostMessage([WD]::FindWindow('shellbits_flyout', [IntPtr]::Zero), 0x312, [IntPtr]$Open, [IntPtr]::Zero)
  foreach ($i in 1..40) { Start-Sleep -Milliseconds 150; $h = [WD]::FindWindow($Class, [IntPtr]::Zero); if ($h -ne [IntPtr]::Zero -and [WD]::IsWindowVisible($h)) { break } }
  Start-Sleep -Milliseconds 900
}
if ($h -eq [IntPtr]::Zero) { "window $Class not found"; exit 1 }
$lp = { param($x, $y) [IntPtr]((([int]$y) -shl 16) -bor (([int]$x) -band 0xFFFF)) }
foreach ($a in ($Do -split ';')) {
  $n = $a.Trim() -split '\s+'
  switch ($n[0]) {
    'click'  { $l = & $lp $n[1] $n[2]; [void][WD]::PostMessage($h, 0x200, [IntPtr]0, $l); Start-Sleep -Milliseconds 60; [void][WD]::PostMessage($h, 0x201, [IntPtr]1, $l); Start-Sleep -Milliseconds 60; [void][WD]::PostMessage($h, 0x202, [IntPtr]0, $l) }
    'dbl'    { $l = & $lp $n[1] $n[2]; [void][WD]::PostMessage($h, 0x201, [IntPtr]1, $l); [void][WD]::PostMessage($h, 0x202, [IntPtr]0, $l); [void][WD]::PostMessage($h, 0x203, [IntPtr]1, $l); [void][WD]::PostMessage($h, 0x202, [IntPtr]0, $l) }
    'rclick' { $l = & $lp $n[1] $n[2]; [void][WD]::PostMessage($h, 0x204, [IntPtr]2, $l); [void][WD]::PostMessage($h, 0x205, [IntPtr]0, $l) }
    'move'   { [void][WD]::PostMessage($h, 0x200, [IntPtr]0, (& $lp $n[1] $n[2])) }
    'wheel'  { [void][WD]::PostMessage($h, 0x20A, [IntPtr]((120 * [int]$n[3]) -shl 16), (& $lp $n[1] $n[2])) }
    'msg'    { [void][WD]::PostMessage($h, [uint32]$n[1], [IntPtr][int]$n[2], [IntPtr]::Zero) }
    'wait'   { Start-Sleep -Milliseconds ([int]$n[1]) }
    'shot'   { "$($n[1]): $([WD]::Shot($h, "$out\$($n[1]).png"))" }
    ''       { }
    default  { "unknown action $($n[0])" }
  }
}
"window still there: $([WD]::IsWindowVisible($h))"
if (-not $Keep -and -not $wasOpen) { [void][WD]::PostMessage($h, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) }

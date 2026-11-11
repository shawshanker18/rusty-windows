# Capture a Windows app's window (found by window class) to shellbits\snap\<Out>.png.
# If the app was not already open it is started with -Run / -RunArgs and closed again afterwards
# (WM_CLOSE posted to that one window). Nothing is typed or clicked.
param([Parameter(Mandatory)][string]$Class, [Parameter(Mandatory)][string]$Run, [string]$RunArgs = '', [Parameter(Mandatory)][string]$Out, [int]$Wait = 3000)
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Text; using System.Runtime.InteropServices;
public static class CW { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
 [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out RECT r, int n);
 public struct RECT { public int L,T,R,B; } }
'@
$had = [CW]::FindWindow($Class, [IntPtr]::Zero) -ne [IntPtr]::Zero
if (-not $had) { if ($RunArgs) { Start-Process $Run -ArgumentList $RunArgs } else { Start-Process $Run } }
$h = [IntPtr]::Zero; foreach ($i in 1..60) { $h = [CW]::FindWindow($Class, [IntPtr]::Zero); if ($h -ne [IntPtr]::Zero) { break }; Start-Sleep -Milliseconds 250 }
if ($h -eq [IntPtr]::Zero) { 'window not found'; exit }
Start-Sleep -Milliseconds $Wait
$t = New-Object Text.StringBuilder 256; [void][CW]::GetWindowText($h, $t, 256)
$w = New-Object CW+RECT; [void][CW]::GetWindowRect($h, [ref]$w); $f = New-Object CW+RECT; [void][CW]::DwmGetWindowAttribute($h, 9, [ref]$f, 16)
$bmp = New-Object System.Drawing.Bitmap ($w.R-$w.L), ($w.B-$w.T); $g = [System.Drawing.Graphics]::FromImage($bmp)
$dc = $g.GetHdc(); [void][CW]::PrintWindow($h, $dc, 2); $g.ReleaseHdc($dc); $g.Dispose()
$crop = $bmp.Clone((New-Object System.Drawing.Rectangle ($f.L-$w.L), ($f.T-$w.T), ($f.R-$f.L), ($f.B-$f.T)), $bmp.PixelFormat)
$crop.Save("$PSScriptRoot\..\snap\$Out.png"); "saved $Out.png: $($crop.Width)x$($crop.Height); title: $($t.ToString()); was already open: $had"
$crop.Dispose(); $bmp.Dispose()
if (-not $had) { [void][CW]::PostMessage($h, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) }

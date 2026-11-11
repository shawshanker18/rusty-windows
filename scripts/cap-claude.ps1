# Capture the real Claude desktop app's window (as it is, nothing typed or clicked) to shellbits\snap\native-claude.png
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class CL2 { [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r); [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h); [DllImport("user32.dll")] public static extern bool IsZoomed(IntPtr h);
 [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out RECT r, int n);
 public struct RECT { public int L,T,R,B; } }
'@
$p = Get-Process claude -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $p) { 'Claude window not found'; exit }
$h = $p.MainWindowHandle
"minimised: " + [CL2]::IsIconic($h) + "; maximised: " + [CL2]::IsZoomed($h)
if ([CL2]::IsIconic($h)) { exit }
$w = New-Object CL2+RECT; [void][CL2]::GetWindowRect($h, [ref]$w); $f = New-Object CL2+RECT; [void][CL2]::DwmGetWindowAttribute($h, 9, [ref]$f, 16)
$bmp = New-Object System.Drawing.Bitmap ($w.R-$w.L), ($w.B-$w.T); $g = [System.Drawing.Graphics]::FromImage($bmp)
$dc = $g.GetHdc(); [void][CL2]::PrintWindow($h, $dc, 2); $g.ReleaseHdc($dc); $g.Dispose()
$crop = $bmp.Clone((New-Object System.Drawing.Rectangle ([math]::Max(0,$f.L-$w.L)), ([math]::Max(0,$f.T-$w.T)), ([math]::Min($f.R-$f.L,$bmp.Width)), ([math]::Min($f.B-$f.T,$bmp.Height))), $bmp.PixelFormat)
$crop.Save("$PSScriptRoot\..\snap\native-claude.png"); "saved native-claude.png: $($crop.Width)x$($crop.Height)"

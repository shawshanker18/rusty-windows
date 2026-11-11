# Capture Windows' own Calculator (as it opens) to shellbits\snap\native-calc.png, then close it again.
# Only that window is touched: it is closed by posting WM_CLOSE to it.
param([string]$Class = 'ApplicationFrameWindow', [string]$Title = 'Calculator', [string]$Run = 'calc.exe', [string]$Out = 'native-calc', [int]$Mode = 0)   # -Mode 2: switch to Scientific (Alt+2) for the capture, then back to Standard (Alt+1)
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class CC { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, string t);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
 [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out RECT r, int n);
 public struct RECT { public int L,T,R,B; }
 [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h); [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
 public static void Alt(byte vk){ keybd_event(0x12,0,0,UIntPtr.Zero); keybd_event(vk,0,0,UIntPtr.Zero); keybd_event(vk,0,2,UIntPtr.Zero); keybd_event(0x12,0,2,UIntPtr.Zero); } }
'@
$had = [CC]::FindWindow($Class, $Title) -ne [IntPtr]::Zero
if (-not $had) { Start-Process $Run }
$h = [IntPtr]::Zero; foreach ($i in 1..40) { $h = [CC]::FindWindow($Class, $Title); if ($h -ne [IntPtr]::Zero) { break }; Start-Sleep -Milliseconds 250 }
if ($h -eq [IntPtr]::Zero) { 'window not found'; exit }
Start-Sleep -Milliseconds 2500
$switched = $false
if ($Mode -gt 0) {
  [void][CC]::SetForegroundWindow($h); Start-Sleep -Milliseconds 400
  # the shortcut is only sent when Calculator really is the window in front
  if ([CC]::GetForegroundWindow() -eq $h) { [CC]::Alt([byte](0x30 + $Mode)); $switched = $true; Start-Sleep -Milliseconds 1200 } else { 'Calculator is not in front: mode not switched'; }
}
$w = New-Object CC+RECT; [void][CC]::GetWindowRect($h, [ref]$w); $f = New-Object CC+RECT; [void][CC]::DwmGetWindowAttribute($h, 9, [ref]$f, 16)
$bmp = New-Object System.Drawing.Bitmap ($w.R-$w.L), ($w.B-$w.T); $g = [System.Drawing.Graphics]::FromImage($bmp)
$dc = $g.GetHdc(); [void][CC]::PrintWindow($h, $dc, 2); $g.ReleaseHdc($dc); $g.Dispose()
$crop = $bmp.Clone((New-Object System.Drawing.Rectangle ($f.L-$w.L), ($f.T-$w.T), ($f.R-$f.L), ($f.B-$f.T)), $bmp.PixelFormat)
$crop.Save("$PSScriptRoot\..\snap\$Out.png"); "saved $Out.png: $($crop.Width)x$($crop.Height) (visible frame)"
$crop.Dispose(); $bmp.Dispose()
if ($switched) { [void][CC]::SetForegroundWindow($h); Start-Sleep -Milliseconds 300; if ([CC]::GetForegroundWindow() -eq $h) { [CC]::Alt(0x31); Start-Sleep -Milliseconds 800; 'back to Standard' } else { 'could not switch back to Standard' } }
if (-not $had) { [void][CC]::PostMessage($h, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) }

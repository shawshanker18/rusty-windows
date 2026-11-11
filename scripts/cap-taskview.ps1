# Capture Windows' own Task View (Win+Tab) for comparison. Rust UI is stopped for a few seconds so
# the key reaches Windows; Win+Tab is pressed again to close it. No Esc/Enter is sent.
param($outDir)
Add-Type -AssemblyName System.Drawing, System.Windows.Forms
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class CT { [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
 public static void WinTab(){ keybd_event(0x5B,0,0,UIntPtr.Zero); keybd_event(0x09,0,0,UIntPtr.Zero); keybd_event(0x09,0,2,UIntPtr.Zero); keybd_event(0x5B,0,2,UIntPtr.Zero); } }
'@
$scr=[System.Windows.Forms.Screen]::PrimaryScreen.Bounds
function Shot($name) { $b=New-Object System.Drawing.Bitmap $scr.Width,$scr.Height; $g=[System.Drawing.Graphics]::FromImage($b); $g.CopyFromScreen(0,0,0,0,$b.Size); $b.Save((Join-Path $outDir $name)) }
if (-not (Get-Process explorer -ErrorAction SilentlyContinue)) { 'Rust UI is the shell right now and this script stops it (the taskbar would vanish): not run.'; exit 1 }
Stop-Process -Name shellbits -Force -ErrorAction SilentlyContinue; Start-Sleep 1
[CT]::WinTab(); Start-Sleep -Milliseconds 1800
Shot 'taskview-native.png'
[CT]::WinTab(); Start-Sleep -Milliseconds 1200
Shot 'taskview-native-after.png'
Start-ScheduledTask shellbits; Start-Sleep 5
"Rust UI running again: " + [bool](Get-Process shellbits -ErrorAction SilentlyContinue)

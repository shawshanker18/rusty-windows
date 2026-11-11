# Win+Z on the Rust File Explorer window opens the snap picker; choosing layout 1, zone 2 (posted to
# our own picker window) should put the window on the right half of the screen. No Esc/Enter is sent.
Add-Type -AssemblyName System.Windows.Forms
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class SN { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int w, int hh, uint f);
 [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out RECT r, int n);
 public struct RECT { public int L,T,R,B; }
 public static void WinZ(){ keybd_event(0x5B,0,0,UIntPtr.Zero); keybd_event(0x5A,0,0,UIntPtr.Zero); keybd_event(0x5A,0,2,UIntPtr.Zero); keybd_event(0x5B,0,2,UIntPtr.Zero); } }
'@
Start-Process "$env:USERPROFILE\shellbits\shellbits.exe" -ArgumentList '--files',$env:USERPROFILE; Start-Sleep 3
$f=[SN]::FindWindow('shellbits_files',[IntPtr]::Zero); $p=[SN]::FindWindow('shellbits_snap',[IntPtr]::Zero)
[void][SN]::SetWindowPos($f,[IntPtr]::Zero,200,120,1000,640,0x14); Start-Sleep -Milliseconds 500
"File Explorer is in front: " + ([SN]::GetForegroundWindow() -eq $f)
if ([SN]::GetForegroundWindow() -ne $f) { [void][SN]::PostMessage($f,0x10,[IntPtr]::Zero,[IntPtr]::Zero); 'skipped: could not bring the test window to the front'; exit }
[SN]::WinZ(); Start-Sleep -Milliseconds 600
"Win+Z opened the snap picker: " + [SN]::IsWindowVisible($p)
[void][SN]::PostMessage($p,0x8000+61,[IntPtr]1,[IntPtr]::Zero); Start-Sleep -Milliseconds 200
[void][SN]::PostMessage($p,0x8000+61,[IntPtr]2,[IntPtr]::Zero); Start-Sleep -Milliseconds 700
"picker closed after choosing: " + (-not [SN]::IsWindowVisible($p))
$r=New-Object SN+RECT; [void][SN]::DwmGetWindowAttribute($f,9,[ref]$r,16)
$wa=[System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea
"window now at $($r.L),$($r.T) - $($r.R),$($r.B); right half of the work area is $($wa.Left + $wa.Width/2),$($wa.Top) - $($wa.Right),$($wa.Bottom)"
"snapped to the right half: " + ($r.L -eq ($wa.Left + $wa.Width/2) -and $r.T -eq $wa.Top -and $r.R -eq $wa.Right -and $r.B -eq $wa.Bottom)
[void][SN]::PostMessage($f,0x10,[IntPtr]::Zero,[IntPtr]::Zero)

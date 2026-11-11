# Win+Tab opens the Rust Task View, the right arrow moves the selection, Win+Tab closes it.
# Only Win, Tab and an arrow key are sent (never Esc or Enter, which could reach another app).
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class TV { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
 [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r); public struct RECT { public int L,T,R,B; }
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
 public static void WinTab(){ keybd_event(0x5B,0,0,UIntPtr.Zero); keybd_event(0x09,0,0,UIntPtr.Zero); keybd_event(0x09,0,2,UIntPtr.Zero); keybd_event(0x5B,0,2,UIntPtr.Zero); }
 public static void Key(byte vk){ keybd_event(vk,0,0,UIntPtr.Zero); keybd_event(vk,0,2,UIntPtr.Zero); } }
'@
$h=[TV]::FindWindow('shellbits_alttab',[IntPtr]::Zero)
$fg=[TV]::GetForegroundWindow()
$native=[TV]::FindWindow('XamlExplorerHostIslandWindow',[IntPtr]::Zero)
[TV]::WinTab(); Start-Sleep -Milliseconds 700
"Win+Tab opened Rust Task View: " + [TV]::IsWindowVisible($h)
if ([TV]::IsWindowVisible($h)) {
  [TV]::Key(0x27); Start-Sleep -Milliseconds 300
  $r=New-Object TV+RECT; [void][TV]::GetWindowRect($h,[ref]$r)
  $b=New-Object System.Drawing.Bitmap ($r.R-$r.L),($r.B-$r.T); $g=[System.Drawing.Graphics]::FromImage($b); $g.CopyFromScreen($r.L,$r.T,0,0,$b.Size); $b.Save((Join-Path $args[0] 'snap\taskview-live.png'))
  "size: $($b.Width)x$($b.Height)"
  [TV]::WinTab(); Start-Sleep -Milliseconds 500
  "Win+Tab again closed it: " + (-not [TV]::IsWindowVisible($h))
}
"focus stayed with the same app: " + ([TV]::GetForegroundWindow() -eq $fg)

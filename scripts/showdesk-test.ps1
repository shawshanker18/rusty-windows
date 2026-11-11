# Normal mode (Explorer running): ask Rust UI to "show desktop", then check its keyboard hook still
# answers (Win+Tab opens Task View), then bring the windows back. No Esc/Enter is sent.
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class SD { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h); [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
 public static void WinTab(){ keybd_event(0x5B,0,0,UIntPtr.Zero); keybd_event(0x09,0,0,UIntPtr.Zero); keybd_event(0x09,0,2,UIntPtr.Zero); keybd_event(0x5B,0,2,UIntPtr.Zero); } }
'@
$f=[SD]::FindWindow('shellbits_flyout',[IntPtr]::Zero); $a=[SD]::FindWindow('shellbits_alttab',[IntPtr]::Zero)
$open=@(Get-Process | Where-Object { $_.MainWindowHandle -ne 0 -and $_.Name -ne 'shellbits' -and -not [SD]::IsIconic($_.MainWindowHandle) } | ForEach-Object { $_.MainWindowHandle })
[void][SD]::PostMessage($f,0x0312,[IntPtr]94,[IntPtr]::Zero); Start-Sleep -Milliseconds 1500
"show desktop: " + @($open | Where-Object { [SD]::IsIconic($_) }).Count + " of " + $open.Count + " open windows minimised"
[SD]::WinTab(); Start-Sleep -Milliseconds 800
"keyboard hook still answers (Win+Tab opened Task View): " + [SD]::IsWindowVisible($a)
[SD]::WinTab(); Start-Sleep -Milliseconds 600
[void][SD]::PostMessage($f,0x0312,[IntPtr]94,[IntPtr]::Zero); Start-Sleep -Milliseconds 1500
"brought back: " + @($open | Where-Object { -not [SD]::IsIconic($_) }).Count + " of " + $open.Count

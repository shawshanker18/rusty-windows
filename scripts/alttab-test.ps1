# Alt+Tab still works after the Task View changes: hold Alt, press Tab, the Rust switcher shows;
# release Alt and the next window comes forward; a quick Alt+Tab then goes back to where we were.
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class AT { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r); public struct RECT { public int L,T,R,B; }
 [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
 public static void Down(byte k){ keybd_event(k,0,0,UIntPtr.Zero); } public static void Up(byte k){ keybd_event(k,0,2,UIntPtr.Zero); } }
'@
$a=[AT]::FindWindow('shellbits_alttab',[IntPtr]::Zero); $before=[AT]::GetForegroundWindow()
[AT]::Down(0x12); [AT]::Down(0x09); [AT]::Up(0x09); Start-Sleep -Milliseconds 500
$r=New-Object AT+RECT; [void][AT]::GetWindowRect($a,[ref]$r)
"Alt+Tab showed the Rust switcher: " + [AT]::IsWindowVisible($a) + " (size $($r.R-$r.L)x$($r.B-$r.T), not full screen: " + (($r.R-$r.L) -lt 1900) + ")"
[AT]::Up(0x12); Start-Sleep -Milliseconds 600
"switched to another window: " + ([AT]::GetForegroundWindow() -ne $before) + "; switcher hidden: " + (-not [AT]::IsWindowVisible($a))
[AT]::Down(0x12); [AT]::Down(0x09); [AT]::Up(0x09); [AT]::Up(0x12); Start-Sleep -Milliseconds 600
"quick Alt+Tab came back: " + ([AT]::GetForegroundWindow() -eq $before)

# Does a click on the Start button still open the Rust Start menu? (Position taken from a screenshot,
# because UI Automation was returning nothing for the taskbar.) Closed again with a Win-key tap.
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class SC { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
 [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p); public struct POINT { public int X, Y; }
 [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint x, uint y, uint d, UIntPtr e);
 [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra); }
'@
$l=[SC]::FindWindow('shellbits_launcher',[IntPtr]::Zero)
$p=New-Object SC+POINT; [void][SC]::GetCursorPos([ref]$p)
[void][SC]::SetCursorPos(782,1056); Start-Sleep -Milliseconds 150
[SC]::mouse_event(2,0,0,0,[UIntPtr]::Zero); [SC]::mouse_event(4,0,0,0,[UIntPtr]::Zero); Start-Sleep -Milliseconds 700
"click on Start opened the Rust Start menu: " + [SC]::IsWindowVisible($l)
$native=[SC]::FindWindow('Windows.UI.Core.CoreWindow',[IntPtr]::Zero)
[SC]::keybd_event(0x5B,0,0,[UIntPtr]::Zero); [SC]::keybd_event(0x5B,0,2,[UIntPtr]::Zero); Start-Sleep -Milliseconds 700
"Win tap closed it: " + (-not [SC]::IsWindowVisible($l))
[void][SC]::SetCursorPos($p.X,$p.Y)

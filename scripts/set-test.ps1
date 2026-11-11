Add-Type @'
using System; using System.Runtime.InteropServices;
public static class ST2 { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, string t);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h); [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra); }
'@
[ST2]::keybd_event(0x5B,0,0,[UIntPtr]::Zero); [ST2]::keybd_event(0x49,0,0,[UIntPtr]::Zero); [ST2]::keybd_event(0x49,0,2,[UIntPtr]::Zero); [ST2]::keybd_event(0x5B,0,2,[UIntPtr]::Zero)
Start-Sleep 2
$h=[ST2]::FindWindow('shellbits_settings','Settings'); "Win+I opened Rust Settings: " + [ST2]::IsWindowVisible($h) + "; Windows' own Settings also opened: " + [bool](Get-Process SystemSettings -ErrorAction SilentlyContinue)
[void][ST2]::PostMessage($h, 0x10, [IntPtr]::Zero, [IntPtr]::Zero); Start-Sleep 1
"closed: " + (-not [ST2]::IsWindowVisible($h)) + "; Rust UI RAM: " + [math]::Round((Get-Process shellbits).WorkingSet64/1MB,1) + " MB"

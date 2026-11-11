Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class FT { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h); [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
 [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
 [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int w, int hh, uint f);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r); public struct RECT { public int L,T,R,B; } }
'@
$before=@(Get-Process explorer).Count; $cab=[FT]::FindWindow('CabinetWClass',[IntPtr]::Zero)
[FT]::keybd_event(0x5B,0,0,[UIntPtr]::Zero); [FT]::keybd_event(0x45,0,0,[UIntPtr]::Zero); [FT]::keybd_event(0x45,0,2,[UIntPtr]::Zero); [FT]::keybd_event(0x5B,0,2,[UIntPtr]::Zero)
Start-Sleep 3
$h=[FT]::FindWindow('shellbits_files',[IntPtr]::Zero)
"Win+E opened Rust File Explorer: " + [FT]::IsWindowVisible($h) + "; a new Windows Explorer window opened: " + ([FT]::FindWindow('CabinetWClass',[IntPtr]::Zero) -ne $cab)
Start-Process "$env:USERPROFILE\shellbits\shellbits.exe" -ArgumentList '--files',$env:USERPROFILE; Start-Sleep 2
[void][FT]::SetWindowPos($h,[IntPtr]::Zero,108,60,1104,692,0x14); Start-Sleep 1
$r=New-Object FT+RECT; [void][FT]::GetWindowRect($h,[ref]$r)
$b=New-Object System.Drawing.Bitmap ($r.R-$r.L),($r.B-$r.T); $g=[System.Drawing.Graphics]::FromImage($b); $dc=$g.GetHdc(); [void][FT]::PrintWindow($h,$dc,2); $g.ReleaseHdc($dc); $b.Save((Join-Path $args[0] 'snap\files-live.png'))
[void][FT]::PostMessage($h,0x10,[IntPtr]::Zero,[IntPtr]::Zero); Start-Sleep 1
"closed: " + (-not [FT]::IsWindowVisible($h)) + "; Rust UI RAM: " + [math]::Round((Get-Process shellbits).WorkingSet64/1MB,1) + " MB"

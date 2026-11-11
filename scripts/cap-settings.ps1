param($uri, $out)
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class PW4 { [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, string t);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r); public struct RECT { public int L,T,R,B; }
 [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int w, int hh, uint f);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l); }
'@
Start-Process $uri; Start-Sleep 4
$w=[PW4]::FindWindow('ApplicationFrameWindow','Settings')
[void][PW4]::SetWindowPos($w,[IntPtr]::Zero,100,60,1180,790,0x14); Start-Sleep 2
$r=New-Object PW4+RECT; [void][PW4]::GetWindowRect($w,[ref]$r)
$b=New-Object System.Drawing.Bitmap ($r.R-$r.L),($r.B-$r.T); $g=[System.Drawing.Graphics]::FromImage($b); $dc=$g.GetHdc(); [void][PW4]::PrintWindow($w,$dc,2); $g.ReleaseHdc($dc); $b.Save($out); "$($b.Width)x$($b.Height)"
[void][PW4]::PostMessage($w,0x10,[IntPtr]::Zero,[IntPtr]::Zero)

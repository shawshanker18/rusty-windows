Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices; using System.Collections.Generic;
public static class CE { public delegate bool EnumProc(IntPtr h, IntPtr l);
 [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, System.Text.StringBuilder s, int n);
 [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r); public struct RECT { public int L,T,R,B; }
 [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int w, int hh, uint f);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 public static List<IntPtr> Cabs(){ var l=new List<IntPtr>(); EnumWindows((h,p)=>{ var s=new System.Text.StringBuilder(64); GetClassName(h,s,64); if(s.ToString()=="CabinetWClass") l.Add(h); return true; }, IntPtr.Zero); return l; } }
'@
$old=[CE]::Cabs()
Start-Process explorer.exe $env:USERPROFILE; Start-Sleep 4
$w=[CE]::Cabs() | Where-Object { $old -notcontains $_ } | Select-Object -First 1
if(-not $w){ "no new explorer window"; exit }
[void][CE]::SetWindowPos($w,[IntPtr]::Zero,100,60,1120,700,0x14); Start-Sleep 2
$r=New-Object CE+RECT; [void][CE]::GetWindowRect($w,[ref]$r)
$b=New-Object System.Drawing.Bitmap ($r.R-$r.L),($r.B-$r.T); $g=[System.Drawing.Graphics]::FromImage($b); $dc=$g.GetHdc(); [void][CE]::PrintWindow($w,$dc,2); $g.ReleaseHdc($dc); $b.Save((Join-Path $args[0] 'snap\native-explorer.png')); "$($b.Width)x$($b.Height)"
[void][CE]::PostMessage($w,0x10,[IntPtr]::Zero,[IntPtr]::Zero)

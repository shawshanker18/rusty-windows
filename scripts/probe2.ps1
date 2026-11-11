Add-Type @'
using System; using System.Runtime.InteropServices; using System.Text;
public static class P2 { [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(POINT p); public struct POINT { public int X, Y; }
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
 [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr h, uint f);
 [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out int v, int n);
 [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr h, int i);
 public static string Cls(IntPtr h){ var s=new StringBuilder(128); GetClassName(h,s,128); return s.ToString(); } }
'@
foreach ($x in 782,1100,1850) { $p=New-Object P2+POINT; $p.X=$x; $p.Y=1056; $h=[P2]::WindowFromPoint($p); $root=[P2]::GetAncestor($h,2); $procId=0; [void][P2]::GetWindowThreadProcessId($root,[ref]$procId)
  "at $x,1056: $([P2]::Cls($h)) in top-level $([P2]::Cls($root)) owned by $((Get-Process -Id $procId -ErrorAction SilentlyContinue).Name)" }
$t=[P2]::FindWindow('Shell_TrayWnd',[IntPtr]::Zero); $c=0; [void][P2]::DwmGetWindowAttribute($t,14,[ref]$c,4)
"Shell_TrayWnd cloaked=$c exstyle=0x$('{0:X}' -f [P2]::GetWindowLong($t,-20))"
(Get-Process -Id $PID).Path; [Environment]::Is64BitProcess
"UIA-related processes: " + ((Get-Process | ? { $_.Name -match 'Narrator|Magnify|osk|TextInputHost|ctfmon' } | % Name) -join ', ')

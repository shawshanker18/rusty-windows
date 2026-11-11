param($cls,$title,$vk)
Add-Type @'
using System; using System.Runtime.InteropServices; using System.Text;
public static class WX3 { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, string t);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h); [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow(); [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 public static string Fg(){ var c=new StringBuilder(100); GetClassName(GetForegroundWindow(),c,100); return c.ToString(); } }
'@
$m=[WX3]::FindWindow($cls,$title)
[WX3]::keybd_event(0x5B,0,0,[UIntPtr]::Zero); [WX3]::keybd_event($vk,0,0,[UIntPtr]::Zero); [WX3]::keybd_event($vk,0,2,[UIntPtr]::Zero); [WX3]::keybd_event(0x5B,0,2,[UIntPtr]::Zero)
$sw=[Diagnostics.Stopwatch]::StartNew(); $prev=''
while($sw.ElapsedMilliseconds -lt 1500){ $s="vis=" + [WX3]::IsWindowVisible($m) + " fg=" + [WX3]::Fg(); if($s -ne $prev){ "$($sw.ElapsedMilliseconds)ms $s"; $prev=$s }; Start-Sleep -Milliseconds 10 }
[void][WX3]::PostMessage($m, 0x8000+40, [IntPtr]::Zero, [IntPtr]::Zero)

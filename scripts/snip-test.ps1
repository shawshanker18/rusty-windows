Add-Type @'
using System; using System.Runtime.InteropServices; using System.Text;
public static class SN2 { [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow(); [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
 [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, StringBuilder s, int n); [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 public static string Fg(out IntPtr h){ h=GetForegroundWindow(); uint pid; GetWindowThreadProcessId(h,out pid); var c=new StringBuilder(100); GetClassName(h,c,100); string n=""; try{ n=System.Diagnostics.Process.GetProcessById((int)pid).ProcessName; }catch{} return n+" ["+c+"]"; } }
'@
function k($vk,$up){ [SN2]::keybd_event($vk,0,$(if($up){2}else{0}),[UIntPtr]::Zero) }
"snipping processes before: " + @(Get-Process SnippingTool -ErrorAction SilentlyContinue).Count
k 0x5B $false; k 0x10 $false; k 0x53 $false; k 0x53 $true; k 0x10 $true; k 0x5B $true
$sw=[Diagnostics.Stopwatch]::StartNew(); $prev=''; $last=[IntPtr]::Zero
while($sw.ElapsedMilliseconds -lt 3000){ $h=[IntPtr]::Zero; $s=[SN2]::Fg([ref]$h); if($s -ne $prev){ "$($sw.ElapsedMilliseconds)ms fg=$s"; $prev=$s; $last=$h }; Start-Sleep -Milliseconds 20 }
if($prev -like 'SnippingTool*'){ [void][SN2]::PostMessage($last, 0x10, [IntPtr]::Zero, [IntPtr]::Zero); Start-Sleep 1; "closed overlay with WM_CLOSE" }
"snipping processes after: " + @(Get-Process SnippingTool -ErrorAction SilentlyContinue).Count

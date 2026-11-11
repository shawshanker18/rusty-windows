Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class CT { [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, string t);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr p, IntPtr a, string c, string t);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, string l);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r); public struct RECT { public int L,T,R,B; } }
'@
function k($vk,$up){ [CT]::keybd_event($vk,0,$(if($up){2}else{0}),[UIntPtr]::Zero) }
Start-Process "$env:USERPROFILE\shellbits\shellbits.exe" -ArgumentList '--claude'; Start-Sleep 2
$w=[CT]::FindWindow('shellbits_claude','Claude Code'); "window: $w"
$e=[CT]::FindWindowEx($w,[IntPtr]::Zero,'Edit',$null); "edit: $e"
[void][CT]::SendMessage($e, 0x000C, [IntPtr]::Zero, 'Reply with exactly the word: pong')
[void][CT]::PostMessage($e, 0x0100, [IntPtr]0x0D, [IntPtr]::Zero)
$t=[Diagnostics.Stopwatch]::StartNew()
while($t.Elapsed.TotalSeconds -lt 45){ Start-Sleep 2; $c=Get-Process claude -ErrorAction SilentlyContinue | Where-Object { $_.Path -like '*\.local\bin\claude.exe' }; if($c){ $mb=[math]::Round(($c|Measure-Object WorkingSet64 -Sum).Sum/1MB) } }
$r=New-Object CT+RECT; [void][CT]::GetWindowRect($w,[ref]$r)
$b=New-Object System.Drawing.Bitmap ($r.R-$r.L),($r.B-$r.T); $g=[System.Drawing.Graphics]::FromImage($b); $g.CopyFromScreen($r.L,$r.T,0,0,$b.Size); $b.Save("$PWD\snap\claude-live.png")
"claude CLI RAM: $mb MB;  Rust UI RAM: " + [math]::Round((Get-Process shellbits).WorkingSet64/1MB,1) + " MB"

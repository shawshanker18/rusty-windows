# Opens the Rust Claude window, types '/' into its box by message, picks the first command from OUR
# menu (keys posted to the menu window only), reads the box, clears it and closes. No model calls.
Add-Type @'
using System;using System.Runtime.InteropServices;using System.Text;
public class W2{[DllImport("user32.dll",CharSet=CharSet.Unicode)]public static extern IntPtr FindWindow(string c,IntPtr t);
[DllImport("user32.dll",CharSet=CharSet.Unicode)]public static extern IntPtr FindWindowEx(IntPtr p,IntPtr a,string c,IntPtr t);
[DllImport("user32.dll")]public static extern bool PostMessage(IntPtr h,uint m,IntPtr w,IntPtr l);
[DllImport("user32.dll")]public static extern IntPtr SendMessage(IntPtr h,uint m,IntPtr w,IntPtr l);
[DllImport("user32.dll",CharSet=CharSet.Unicode)]public static extern int GetWindowText(IntPtr h,StringBuilder s,int n);[DllImport("user32.dll")]public static extern bool IsWindowVisible(IntPtr h);[DllImport("user32.dll",CharSet=CharSet.Unicode)]public static extern IntPtr SendMessage(IntPtr h,uint m,IntPtr w,StringBuilder l);}
'@
$z=[IntPtr]::Zero
$fly=[W2]::FindWindow("shellbits_flyout",$z); [W2]::PostMessage($fly,0x312,[IntPtr]97,$z)|Out-Null
$cw=$z; foreach($i in 1..20){Start-Sleep -m 500; $cw=[W2]::FindWindow("shellbits_claude",$z); if($cw -ne $z -and [W2]::IsWindowVisible($cw)){break}}; Start-Sleep 1
$e=[W2]::FindWindowEx($cw,$z,"Edit",$z)
[W2]::PostMessage($e,0x102,[IntPtr]47,$z)|Out-Null; Start-Sleep 2
$m=[W2]::FindWindow("#32768",$z); $s0=New-Object Text.StringBuilder 200; [W2]::GetWindowText($e,$s0,200)|Out-Null; "window $cw visible $([W2]::IsWindowVisible($cw)) edit $e text '$s0'"; "menu up: $($m -ne $z)"
[W2]::PostMessage($m,0x100,[IntPtr]0x28,$z)|Out-Null; Start-Sleep -m 200
[W2]::PostMessage($m,0x100,[IntPtr]0x0D,$z)|Out-Null; Start-Sleep 2
$sb=New-Object Text.StringBuilder 200; [W2]::SendMessage($e,0x0D,[IntPtr]200,$sb)|Out-Null
"box after picking: '$sb' -> " + $(if ("$sb" -eq "/compact ") {"PASS"} else {"look"})
[W2]::SendMessage($e,0x0C,$z,$z)|Out-Null
[W2]::PostMessage($cw,0x10,$z,$z)|Out-Null



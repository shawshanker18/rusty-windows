# Rust Claude Code window, live, without sending anything to Claude: open it, scroll the session list
# with the wheel, click a session, and check each step changed the right part of the window. All input
# is posted to our own window; nothing is typed, no message is sent, no session is changed.
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class CU { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
 [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int w, int hh, uint f);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
 [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r); public struct RECT { public int L,T,R,B; } }
'@
$snap = "$PSScriptRoot\..\snap"
Start-Process "$env:USERPROFILE\shellbits\shellbits.exe" -ArgumentList '--claude'
$w = [IntPtr]::Zero; foreach ($i in 1..20) { Start-Sleep -Milliseconds 500; $w = [CU]::FindWindow('shellbits_claude', [IntPtr]::Zero); if ($w -ne [IntPtr]::Zero) { break } }
if ($w -eq [IntPtr]::Zero) { 'the Claude window did not open'; exit }
[void][CU]::SetWindowPos($w, [IntPtr]::Zero, 40, 40, 1400, 900, 0x14); Start-Sleep -Milliseconds 1500
function Shot($name) { $r = New-Object CU+RECT; [void][CU]::GetClientRect($w, [ref]$r); $b = New-Object System.Drawing.Bitmap $r.R, $r.B; $g = [System.Drawing.Graphics]::FromImage($b); $dc = $g.GetHdc(); [void][CU]::PrintWindow($w, $dc, 3); $g.ReleaseHdc($dc); $g.Dispose(); $b.Save("$snap\$name.png"); $b.Dispose() }
function Changed($a, $b, $box) { python -W ignore -c "
from PIL import Image, ImageChops
a=Image.open(r'$snap\$a.png').convert('RGB').crop(($box)); b=Image.open(r'$snap\$b.png').convert('RGB').crop(($box))
d=ImageChops.difference(a,b).convert('L').point(lambda v:255 if v>16 else 0); n=a.size[0]*a.size[1]
print('%.1f' % (100.0*sum(1 for v in d.get_flattened_data() if v)/n))" }
Shot 'cu-0'
$wr = New-Object CU+RECT; [void][CU]::GetWindowRect($w, [ref]$wr)
# wheel down three notches with the pointer position given as over the sidebar
$pos = [IntPtr]((($wr.T + 500) -shl 16) -bor ($wr.L + 130))
1..3 | ForEach-Object { [void][CU]::PostMessage($w, 0x020A, [IntPtr](-120 * 65536), $pos) }; Start-Sleep -Milliseconds 500
Shot 'cu-1'
"sidebar list changed after wheel over it: " + (Changed 'cu-0' 'cu-1' '0,240,266,840') + "% of its pixels; conversation changed: " + (Changed 'cu-0' 'cu-1' '280,40,1060,800') + "%"
# wheel back up, then click the second session row of the list (posted mouse up at that row)
1..3 | ForEach-Object { [void][CU]::PostMessage($w, 0x020A, [IntPtr](120 * 65536), $pos) }; Start-Sleep -Milliseconds 400
Shot 'cu-2'
"list back where it was after wheel up: " + ((Changed 'cu-0' 'cu-2' '0,240,266,840') -eq '0.0')
$row = [IntPtr]((324 -shl 16) -bor 120)
[void][CU]::PostMessage($w, 0x0202, [IntPtr]::Zero, $row); Start-Sleep -Milliseconds 2500
Shot 'cu-3'
"after clicking another session: title bar changed " + (Changed 'cu-2' 'cu-3' '270,0,900,36') + "%, conversation changed " + (Changed 'cu-2' 'cu-3' '280,40,1060,800') + "%"
# click the first group heading: the group folds (the list below it changes), click again: it is back
$head = [IntPtr]((244 -shl 16) -bor 60)
[void][CU]::PostMessage($w, 0x0202, [IntPtr]::Zero, $head); Start-Sleep -Milliseconds 500; Shot 'cu-4'
"group folded after clicking its heading: list changed " + (Changed 'cu-3' 'cu-4' '0,260,266,840') + "%"
[void][CU]::PostMessage($w, 0x0202, [IntPtr]::Zero, $head); Start-Sleep -Milliseconds 500; Shot 'cu-5'
"unfolded again, list exactly as before: " + ((Changed 'cu-3' 'cu-5' '0,230,266,840') -eq '0.0')
[void][CU]::PostMessage($w, 0x10, [IntPtr]::Zero, [IntPtr]::Zero); Start-Sleep -Milliseconds 600
"Rust UI still running: " + [bool](Get-Process shellbits -ErrorAction SilentlyContinue) + "; helper processes: " + @(Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -in (Get-Process shellbits).Id -and $_.Name -ne 'shellbits.exe' }).Count

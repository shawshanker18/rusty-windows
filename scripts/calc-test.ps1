# Rust Calculator, live: open it, type 12*3 (characters posted to our own window), click "=" (mouse
# messages posted to our own window), capture the window, and compare with an off-screen snapshot of
# the same key presses. Then close it (WM_CLOSE to our window) and check it is gone.
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class CT { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
 [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int w, int hh, uint f);
 [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r); public struct RECT { public int L,T,R,B; } }
'@
$exe = "$env:USERPROFILE\shellbits\shellbits.exe"; $snap = "$PSScriptRoot\..\snap"
Start-Process $exe -ArgumentList '--calc'; Start-Sleep -Milliseconds 1500
$h = [CT]::FindWindow('shellbits_calc', [IntPtr]::Zero)
"Calculator window opened: " + ($h -ne [IntPtr]::Zero -and [CT]::IsWindowVisible($h))
if ($h -eq [IntPtr]::Zero) { exit }
[void][CT]::SetWindowPos($h, [IntPtr]::Zero, 0, 0, 1202, 934, 0x16); Start-Sleep -Milliseconds 300
foreach ($c in '12*3'.ToCharArray()) { [void][CT]::PostMessage($h, 0x102, [IntPtr][int]$c, [IntPtr]::Zero) }
$lp = [IntPtr]((880 -shl 16) -bor 768)   # the "=" button
[void][CT]::PostMessage($h, 0x201, [IntPtr]1, $lp); [void][CT]::PostMessage($h, 0x202, [IntPtr]::Zero, $lp); Start-Sleep -Milliseconds 500
$r = New-Object CT+RECT; [void][CT]::GetClientRect($h, [ref]$r)
$bmp = New-Object System.Drawing.Bitmap $r.R, $r.B; $g = [System.Drawing.Graphics]::FromImage($bmp); $dc = $g.GetHdc(); [void][CT]::PrintWindow($h, $dc, 3); $g.ReleaseHdc($dc); $g.Dispose()
$bmp.Save("$snap\calc-live.png"); $bmp.Dispose()
& $exe --snapshot-calc "$snap\calc-expected.bmp" '12*3=' | Out-Null; Start-Sleep -Milliseconds 800
python -W ignore -c "
from PIL import Image, ImageChops
a=Image.open(r'$snap\calc-live.png').convert('RGB'); b=Image.open(r'$snap\calc-expected.bmp').convert('RGB')
print('live window size', a.size)
d=ImageChops.difference(a,b.resize(a.size) if a.size!=b.size else b).convert('L').point(lambda v:255 if v>16 else 0)
n=a.size[0]*a.size[1]; bad=sum(1 for v in d.get_flattened_data() if v) if hasattr(d,'get_flattened_data') else sum(1 for v in d.getdata() if v)
print('live window after typing 12*3 and clicking = matches the snapshot showing 36: %.2f%%' % (100.0*(n-bad)/n))
"
# clear (Esc character posted to our window), then click the history row: 36 should come back
function Click($x, $y) { $p = [IntPtr](($y -shl 16) -bor $x); [void][CT]::PostMessage($h, 0x201, [IntPtr]1, $p); [void][CT]::PostMessage($h, 0x202, [IntPtr]::Zero, $p); Start-Sleep -Milliseconds 300 }
[void][CT]::PostMessage($h, 0x102, [IntPtr]0x1B, [IntPtr]::Zero); Start-Sleep -Milliseconds 200
Click 1000 120
$bmp = New-Object System.Drawing.Bitmap $r.R, $r.B; $g = [System.Drawing.Graphics]::FromImage($bmp); $dc = $g.GetHdc(); [void][CT]::PrintWindow($h, $dc, 3); $g.ReleaseHdc($dc); $g.Dispose()
$bmp.Save("$snap\calc-live2.png"); $bmp.Dispose()
& $exe --snapshot-calc "$snap\calc-expected2.bmp" '12*3=CH' | Out-Null; Start-Sleep -Milliseconds 800
python -W ignore -c "
from PIL import Image, ImageChops
a=Image.open(r'$snap\calc-live2.png').convert('RGB'); b=Image.open(r'$snap\calc-expected2.bmp').convert('RGB')
d=ImageChops.difference(a,b).convert('L').point(lambda v:255 if v>16 else 0); n=a.size[0]*a.size[1]
print('after clearing and clicking the history row, matches the snapshot with 36 recalled: %.2f%%' % (100.0*(n-sum(1 for v in d.get_flattened_data() if v))/n))
"
# the bin at the foot of the side panel clears the history
Click 1174 906
$bmp = New-Object System.Drawing.Bitmap $r.R, $r.B; $g = [System.Drawing.Graphics]::FromImage($bmp); $dc = $g.GetHdc(); [void][CT]::PrintWindow($h, $dc, 3); $g.ReleaseHdc($dc); $g.Dispose()
$bmp.Save("$snap\calc-live4.png"); $bmp.Dispose()
& $exe --snapshot-calc "$snap\calc-expected4.bmp" '12*3=CHK' | Out-Null; Start-Sleep -Milliseconds 800
python -W ignore -c "
from PIL import Image, ImageChops
a=Image.open(r'$snap\calc-live4.png').convert('RGB'); b=Image.open(r'$snap\calc-expected4.bmp').convert('RGB')
d=ImageChops.difference(a,b).convert('L').point(lambda v:255 if v>16 else 0); n=a.size[0]*a.size[1]
print('after clicking the bin, matches the snapshot with the history cleared: %.2f%%' % (100.0*(n-sum(1 for v in d.get_flattened_data() if v))/n))
"
# Scientific: Alt+2 (posted to our window), then 2+3*4= must give 14 (multiplication first)
[void][CT]::PostMessage($h, 0x104, [IntPtr]0x32, [IntPtr]0x20000001); Start-Sleep -Milliseconds 300
foreach ($c in '2+3*4='.ToCharArray()) { [void][CT]::PostMessage($h, 0x102, [IntPtr][int]$c, [IntPtr]::Zero) }; Start-Sleep -Milliseconds 400
$bmp = New-Object System.Drawing.Bitmap $r.R, $r.B; $g = [System.Drawing.Graphics]::FromImage($bmp); $dc = $g.GetHdc(); [void][CT]::PrintWindow($h, $dc, 3); $g.ReleaseHdc($dc); $g.Dispose()
$bmp.Save("$snap\calc-live3.png"); $bmp.Dispose()
& $exe --snapshot-calc "$snap\calc-expected3.bmp" '12*3=CHKZ2+3*4=' | Out-Null; Start-Sleep -Milliseconds 800
python -W ignore -c "
from PIL import Image, ImageChops
a=Image.open(r'$snap\calc-live3.png').convert('RGB'); b=Image.open(r'$snap\calc-expected3.bmp').convert('RGB')
d=ImageChops.difference(a,b).convert('L').point(lambda v:255 if v>16 else 0); n=a.size[0]*a.size[1]
print('Scientific after 2+3*4= matches the snapshot showing 14: %.2f%%' % (100.0*(n-sum(1 for v in d.get_flattened_data() if v))/n))
"
[void][CT]::PostMessage($h, 0x104, [IntPtr]0x31, [IntPtr]0x20000001); Start-Sleep -Milliseconds 300   # back to Standard for the pin test
Add-Type 'using System; using System.Runtime.InteropServices; public static class CX { [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr h, int i); }'
Click 157 57; "keep on top switched on: " + (([CX]::GetWindowLong($h, -20) -band 8) -ne 0)
Click 157 57; "keep on top switched off: " + (([CX]::GetWindowLong($h, -20) -band 8) -eq 0)
# the mode is remembered: leave it in Scientific, close, open again
[void][CT]::PostMessage($h, 0x104, [IntPtr]0x32, [IntPtr]0x20000001); Start-Sleep -Milliseconds 300
[void][CT]::PostMessage($h, 0x10, [IntPtr]::Zero, [IntPtr]::Zero); Start-Sleep -Milliseconds 700
Start-Process $exe -ArgumentList '--calc'; Start-Sleep -Milliseconds 1500
$h = [CT]::FindWindow('shellbits_calc', [IntPtr]::Zero); [void][CT]::SetWindowPos($h, [IntPtr]::Zero, 0, 0, 1202, 934, 0x16); Start-Sleep -Milliseconds 400
$bmp = New-Object System.Drawing.Bitmap $r.R, $r.B; $g = [System.Drawing.Graphics]::FromImage($bmp); $dc = $g.GetHdc(); [void][CT]::PrintWindow($h, $dc, 3); $g.ReleaseHdc($dc); $g.Dispose()
$bmp.Save("$snap\calc-live5.png"); $bmp.Dispose()
& $exe --snapshot-calc "$snap\calc-expected5.bmp" 'Z' | Out-Null; Start-Sleep -Milliseconds 800
python -W ignore -c "
from PIL import Image, ImageChops
a=Image.open(r'$snap\calc-live5.png').convert('RGB'); b=Image.open(r'$snap\calc-expected5.bmp').convert('RGB')
d=ImageChops.difference(a,b).convert('L').point(lambda v:255 if v>16 else 0); n=a.size[0]*a.size[1]
print('reopened in Scientific, as it was left: %.2f%%' % (100.0*(n-sum(1 for v in d.get_flattened_data() if v))/n))
"
[void][CT]::PostMessage($h, 0x104, [IntPtr]0x31, [IntPtr]0x20000001); Start-Sleep -Milliseconds 300   # leave it in Standard
[void][CT]::PostMessage($h, 0x10, [IntPtr]::Zero, [IntPtr]::Zero); Start-Sleep -Milliseconds 600
"closed: " + ([CT]::FindWindow('shellbits_calc', [IntPtr]::Zero) -eq [IntPtr]::Zero)

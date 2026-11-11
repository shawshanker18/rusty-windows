# Rust screen snip: open it in its own process, drag a 200x100 rectangle by posting mouse messages to
# OUR window, and check a 200x100 PNG lands in Pictures\Screenshots. The clipboard is left alone and
# the test picture is deleted again.
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class SN { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l); }
'@
Add-Type -AssemblyName System.Drawing
$dir = "$env:USERPROFILE\Pictures\Screenshots"; $start = Get-Date
$env:SHELLBITS_SNIP_NO_CLIP = '1'
$p = Start-Process "$PSScriptRoot\..\target\release\shellbits.exe" -ArgumentList '--snip' -PassThru
$h = [IntPtr]::Zero; foreach ($i in 1..30) { $h = [SN]::FindWindow('shellbits_snip', [IntPtr]::Zero); if ($h -ne [IntPtr]::Zero) { break }; Start-Sleep -Milliseconds 100 }
"overlay opened: $($h -ne [IntPtr]::Zero)"
$lp = { param($x, $y) [IntPtr](($y -shl 16) -bor $x) }
[void][SN]::PostMessage($h, 0x201, [IntPtr]1, (& $lp 300 300)); Start-Sleep -Milliseconds 150
[void][SN]::PostMessage($h, 0x200, [IntPtr]1, (& $lp 500 400)); Start-Sleep -Milliseconds 150
[void][SN]::PostMessage($h, 0x202, [IntPtr]0, (& $lp 500 400))
$exited = $p.WaitForExit(5000); "process ended by itself: $exited"; if (-not $exited) { $p.Kill() }
$f = Get-ChildItem $dir -Filter 'Screenshot *.png' -ErrorAction SilentlyContinue | Where-Object { $_.LastWriteTime -ge $start.AddSeconds(-1) } | Select-Object -First 1
if ($f) { $img = [Drawing.Image]::FromFile($f.FullName); "saved: $($f.Name) $($img.Width)x$($img.Height) -> $(if ($img.Width -eq 200 -and $img.Height -eq 100) {'PASS'} else {'FAIL'})"; $img.Dispose(); Remove-Item $f.FullName }
else { 'no picture saved -> FAIL' }

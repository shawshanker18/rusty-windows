# Right-click in File Explorer must open a menu, not end the program. No mouse or keys: the right-click
# is posted to OUR window, the menu is looked for by window class and closed again with WM_CANCELMODE.
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class FR { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p);
 [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h); }
'@
$dir = Join-Path $env:TEMP 'rustui-rclick-test'; Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $dir | Out-Null; 'x' | Set-Content "$dir\a.txt"
Start-Process "$env:USERPROFILE\shellbits\shellbits.exe" -ArgumentList '--files', $dir
$h = [IntPtr]::Zero; foreach ($i in 1..40) { $h = [FR]::FindWindow('shellbits_files', [IntPtr]::Zero); if ($h -ne [IntPtr]::Zero -and [FR]::IsWindowVisible($h)) { break }; Start-Sleep -Milliseconds 150 }
"window opened: $($h -ne [IntPtr]::Zero)"; Start-Sleep 1
$p = 0; [void][FR]::GetWindowThreadProcessId($h, [ref]$p); $s = [FR]::GetDpiForWindow($h) / 96.0
$lp = [IntPtr]((([int](196 * $s)) -shl 16) -bor ([int](300 * $s)))      # the first row (y 196 of 100%), in the name column
[void][FR]::PostMessage($h, 0x205, [IntPtr]::Zero, $lp); Start-Sleep -Milliseconds 2500
$alive = [bool](Get-Process -Id $p -ErrorAction SilentlyContinue)
$menu = [FR]::FindWindow('#32768', [IntPtr]::Zero); $mp = 0; if ($menu -ne [IntPtr]::Zero) { [void][FR]::GetWindowThreadProcessId($menu, [ref]$mp) }
"program still running after the right-click: $alive"
"a menu opened and it is ours: $($menu -ne [IntPtr]::Zero -and $mp -eq $p)"
[void][FR]::PostMessage($h, 0x1F, [IntPtr]::Zero, [IntPtr]::Zero); Start-Sleep -Milliseconds 600   # WM_CANCELMODE closes the menu
[void][FR]::PostMessage($h, 0x10, [IntPtr]::Zero, [IntPtr]::Zero); Start-Sleep -Milliseconds 600
Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
"result: $(if ($alive -and $menu -ne [IntPtr]::Zero) { 'PASS' } else { 'FAIL' })"

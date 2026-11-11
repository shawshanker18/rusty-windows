# Does Rust UI come back by itself after a crash? Makes the running copy crash on purpose (a flag
# file + a message to its own window; nothing else is touched) and checks a new copy takes over.
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class CRT { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l); }
'@
function MainPid { $h = [CRT]::FindWindow('shellbits_flyout', [IntPtr]::Zero); $p = 0; if ($h -ne [IntPtr]::Zero) { [void][CRT]::GetWindowThreadProcessId($h, [ref]$p) }; $p }
$before = MainPid
"Rust UI running before: " + ($before -ne 0) + "; copies of the program: " + @(Get-Process shellbits -ErrorAction SilentlyContinue).Count + " (main + watchdog)"
if ($before -eq 0) { exit }
Set-Content "$env:TEMP\shellbits-crash-test" 'x'
$sw = [Diagnostics.Stopwatch]::StartNew()
[void][CRT]::PostMessage([CRT]::FindWindow('shellbits_flyout', [IntPtr]::Zero), 0x312, [IntPtr]91, [IntPtr]::Zero)
$after = 0; while ($sw.ElapsedMilliseconds -lt 15000) { Start-Sleep -Milliseconds 100; $after = MainPid; if ($after -ne 0 -and $after -ne $before) { break } }
"old copy gone: " + (-not (Get-Process -Id $before -ErrorAction SilentlyContinue))
"a new copy took over: " + ($after -ne 0 -and $after -ne $before) + " after $($sw.ElapsedMilliseconds) ms"
Start-Sleep 3
"copies of the program now: " + @(Get-Process shellbits -ErrorAction SilentlyContinue).Count
"crash log: " + (Get-Content "$env:TEMP\shellbits-crashes.txt" -Tail 1)
"watchdog memory (private): " + ((Get-Process shellbits | Sort-Object PrivateMemorySize64 | Select-Object -First 1).PrivateMemorySize64 / 1MB).ToString('0.0') + " MB"

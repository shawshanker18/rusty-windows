# Searching for "calculator" in the Rust Search and choosing the top result should open the Rust
# Calculator, not Windows' own. Everything is posted to our own windows (no global Enter/Esc).
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class CR { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l); }
'@
$before = @(Get-Process CalculatorApp -ErrorAction SilentlyContinue).Count
$l = [CR]::FindWindow('shellbits_launcher', [IntPtr]::Zero)
[void][CR]::PostMessage($l, 0x8000+3, [IntPtr]1, [IntPtr]::Zero); Start-Sleep -Milliseconds 900
"Search opened: " + [CR]::IsWindowVisible($l)
foreach ($c in 'calculator'.ToCharArray()) { [void][CR]::PostMessage($l, 0x102, [IntPtr][int]$c, [IntPtr]::Zero) }; Start-Sleep -Milliseconds 900
[void][CR]::PostMessage($l, 0x100, [IntPtr]0x0D, [IntPtr]::Zero); Start-Sleep -Milliseconds 2500
$c = [CR]::FindWindow('shellbits_calc', [IntPtr]::Zero)
"Search closed after choosing: " + (-not [CR]::IsWindowVisible($l))
"Rust Calculator opened: " + ($c -ne [IntPtr]::Zero -and [CR]::IsWindowVisible($c))
"Windows' Calculator started: " + (@(Get-Process CalculatorApp -ErrorAction SilentlyContinue).Count -gt $before)
if ($c -ne [IntPtr]::Zero) { [void][CR]::PostMessage($c, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) }
if ([CR]::IsWindowVisible($l)) { [void][CR]::PostMessage($l, 0x8000+3, [IntPtr]1, [IntPtr]::Zero) }

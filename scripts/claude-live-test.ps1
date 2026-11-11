# Rust Claude Code window, end to end: open it, send one tiny prompt (typed into our own edit box),
# wait for the reply to land in the new session's file, look at the helper process it started and its
# memory, close the window and check the helper is gone. The throwaway session file is deleted again.
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class CLT { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr p, IntPtr a, string c, IntPtr t);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, string l);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l); }
'@
$main = (Get-Process shellbits | Sort-Object PrivateMemorySize64 -Descending | Select-Object -First 1).Id
function Helpers { @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$main" | Where-Object { $_.Name -ne 'shellbits.exe' }) }
$before = @(Helpers).Count; "helper processes before the test: $before"; $start = Get-Date; $marker = "pong-$([guid]::NewGuid().ToString('N').Substring(0,6))"
Start-Process "$env:USERPROFILE\shellbits\shellbits.exe" -ArgumentList '--claude'; Start-Sleep 2
$w = [CLT]::FindWindow('shellbits_claude', [IntPtr]::Zero); $e = [CLT]::FindWindowEx($w, [IntPtr]::Zero, 'Edit', [IntPtr]::Zero)
"window opened: " + ($w -ne [IntPtr]::Zero -and [CLT]::IsWindowVisible($w))
[void][CLT]::SendMessage($e, 0x000C, [IntPtr]::Zero, "Reply with exactly this word and nothing else: $marker")
[void][CLT]::PostMessage($e, 0x0100, [IntPtr]0x0D, [IntPtr]::Zero)
$file = $null; $sw = [Diagnostics.Stopwatch]::StartNew(); $replied = $false
while ($sw.Elapsed.TotalSeconds -lt 90 -and -not $replied) { Start-Sleep 2
  $file = Get-ChildItem "$env:USERPROFILE\.claude\projects" -Recurse -Filter *.jsonl | Where-Object { $_.LastWriteTime -gt $start } | Where-Object { Select-String -Path $_.FullName -SimpleMatch $marker -Quiet } | Select-Object -First 1
  if ($file) { $replied = [bool](Get-Content $file.FullName | Where-Object { $_ -match '"role":"assistant"' -and $_ -match $marker }) } }
"reply arrived: $replied after $([int]$sw.Elapsed.TotalSeconds) s"
$h = @(Helpers); "helper processes now: " + $h.Count + " (" + (($h | ForEach-Object { $_.Name }) -join ', ') + ")"
$tree = @($h); foreach ($p in $h) { $tree += @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$($p.ProcessId)") }
"their private memory: " + [math]::Round((($tree | ForEach-Object { (Get-Process -Id $_.ProcessId -ErrorAction SilentlyContinue).PrivateMemorySize64 } | Measure-Object -Sum).Sum / 1MB)) + " MB"
Start-Sleep 3
[void][CLT]::PostMessage($w, 0x10, [IntPtr]::Zero, [IntPtr]::Zero); Start-Sleep 3
"window hidden: " + (-not [CLT]::IsWindowVisible($w)) + "; helper processes after closing: " + @(Helpers).Count
if ($file -and (Select-String -Path $file.FullName -SimpleMatch $marker -Quiet)) { Remove-Item $file.FullName; "throwaway session file removed: " + (-not (Test-Path $file.FullName)) }

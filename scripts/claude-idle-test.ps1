# With the idle limit set to 8 s: send a prompt, wait for the reply, leave the window OPEN and idle ->
# the helper process should be let go; a second prompt should bring it back and be answered in the
# same session. The throwaway session file is deleted at the end. Runs the freshly built copy.
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class CI { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr p, IntPtr a, string c, IntPtr t);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, string l);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l); }
'@
if (-not (Get-Process explorer -ErrorAction SilentlyContinue)) { 'Rust UI is the shell right now and this script stops it (the taskbar would vanish): not run.'; exit 1 }
Stop-Process -Name shellbits -ErrorAction SilentlyContinue; Start-Sleep 2
# (this script is often run from inside a Claude Code session; Rust UI itself drops that session's
# CLAUDE* / ANTHROPIC* variables before starting its helper, so nothing is cleaned here)
$env:SHELLBITS_CLAUDE_IDLE_SECS = '8'; $exe = "$PSScriptRoot\..\target\release\shellbits.exe"
Start-Process $exe; Remove-Item env:SHELLBITS_CLAUDE_IDLE_SECS; Start-Sleep 5
$main = (Get-Process shellbits | Sort-Object PrivateMemorySize64 -Descending | Select-Object -First 1).Id
function Helpers { @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$main" | Where-Object { $_.Name -ne 'shellbits.exe' }).Count }
$start = Get-Date; $m1 = "ping-$([guid]::NewGuid().ToString('N').Substring(0,6))"; $m2 = "pong-$([guid]::NewGuid().ToString('N').Substring(0,6))"
Start-Process $exe -ArgumentList '--claude'
$w = [IntPtr]::Zero; foreach ($i in 1..20) { Start-Sleep -Milliseconds 500; $w = [CI]::FindWindow('shellbits_claude', [IntPtr]::Zero); if ($w -ne [IntPtr]::Zero) { break } }
Start-Sleep 1; $e = [CI]::FindWindowEx($w, [IntPtr]::Zero, 'Edit', [IntPtr]::Zero)
if ($e -eq [IntPtr]::Zero) { 'the Claude window did not open: test stopped'; Stop-Process -Name shellbits -ErrorAction SilentlyContinue; Start-Sleep 1; Start-ScheduledTask shellbits; exit }
function Ask($marker) {
  [void][CI]::SendMessage($e, 0x000C, [IntPtr]::Zero, "Reply with exactly this word and nothing else: $marker"); [void][CI]::PostMessage($e, 0x0100, [IntPtr]0x0D, [IntPtr]::Zero)
  $sw = [Diagnostics.Stopwatch]::StartNew()
  while ($sw.Elapsed.TotalSeconds -lt 90) { Start-Sleep 2
    $f = Get-ChildItem "$env:USERPROFILE\.claude\projects" -Recurse -Filter *.jsonl | Where-Object { $_.LastWriteTime -gt $start } | Where-Object { Select-String -Path $_.FullName -SimpleMatch $marker -Quiet } | Select-Object -First 1
    if ($f -and (Get-Content $f.FullName | Where-Object { $_ -match '"role":"assistant"' -and $_ -match $marker })) { return $f } }
  $null }
$f1 = Ask $m1; "first reply arrived: " + [bool]$f1 + "; helpers running: " + (Helpers)
Start-Sleep 14
"after 14 s idle with the window still open, helpers running: " + (Helpers)
$f2 = Ask $m2; "second reply arrived: " + [bool]$f2 + "; helpers running: " + (Helpers)
"both replies are in the same session file: " + ($f1 -and $f2 -and $f1.FullName -eq $f2.FullName)
[void][CI]::PostMessage($w, 0x10, [IntPtr]::Zero, [IntPtr]::Zero); Start-Sleep 2
foreach ($f in @($f1, $f2) | Where-Object { $_ } | Select-Object -Unique -ExpandProperty FullName) { if (Select-String -Path $f -SimpleMatch $m1 -Quiet) { Remove-Item $f } }
Stop-Process -Name shellbits -ErrorAction SilentlyContinue; Start-Sleep 1
Copy-Item $exe $env:USERPROFILE\shellbits\shellbits.exe -Force; Start-ScheduledTask shellbits; Start-Sleep 4
"installed copy running: " + [bool](Get-Process shellbits -ErrorAction SilentlyContinue)

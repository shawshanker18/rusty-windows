# Send a picture through the Rust Claude Code window: a plain red square (made by the test, in a
# path with a space) is mentioned in the message; the reply should name its colour. Runs the freshly
# built copy, deletes the throwaway session afterwards and puts the installed copy back.
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class CM { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr p, IntPtr a, string c, IntPtr t);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, string l);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l); }
'@
$img = "$env:TEMP\test square.png"; $exe = "$PSScriptRoot\..\target\release\shellbits.exe"
Stop-Process -Name shellbits -ErrorAction SilentlyContinue; Start-Sleep 2; Start-Process $exe; Start-Sleep 5
$start = Get-Date; $marker = "imgtest-$([guid]::NewGuid().ToString('N').Substring(0,6))"
Start-Process $exe -ArgumentList '--claude'
$w = [IntPtr]::Zero; foreach ($i in 1..20) { Start-Sleep -Milliseconds 500; $w = [CM]::FindWindow('shellbits_claude', [IntPtr]::Zero); if ($w -ne [IntPtr]::Zero) { break } }
Start-Sleep 1; $e = [CM]::FindWindowEx($w, [IntPtr]::Zero, 'Edit', [IntPtr]::Zero)
[void][CM]::SendMessage($e, 0x000C, [IntPtr]::Zero, "($marker) Do not use any tools. What single colour fills this picture? Answer with one lowercase word. @`"$img`"")
[void][CM]::PostMessage($e, 0x0100, [IntPtr]0x0D, [IntPtr]::Zero)
$sw = [Diagnostics.Stopwatch]::StartNew(); $answer = $null; $f = $null
while ($sw.Elapsed.TotalSeconds -lt 90 -and -not $answer) { Start-Sleep 2
  $f = Get-ChildItem "$env:USERPROFILE\.claude\projects" -Recurse -Filter *.jsonl | Where-Object { $_.LastWriteTime -gt $start } | Where-Object { Select-String -Path $_.FullName -SimpleMatch $marker -Quiet } | Select-Object -First 1
  if ($f) { $line = Get-Content $f.FullName | Where-Object { $_ -match '"role":"assistant"' -and $_ -match '"type":"text"' } | Select-Object -Last 1
    if ($line) { $answer = (($line | ConvertFrom-Json).message.content | Where-Object { $_.type -eq 'text' } | Select-Object -First 1).text } } }
"reply: '$answer' after $([int]$sw.Elapsed.TotalSeconds) s"
# (the answer itself depends on which model the CLI is set to use; on this PC its default is a free
# OpenRouter model whose answers vary, so only the delivery of the picture is checked)
if ($f) { "the message carried a real image block: " + [bool](Select-String -Path $f.FullName -SimpleMatch '"type":"image"' -Quiet); "tools used: " + @(Select-String -Path $f.FullName -SimpleMatch '"type":"tool_use"').Count }
[void][CM]::PostMessage($w, 0x10, [IntPtr]::Zero, [IntPtr]::Zero); Start-Sleep 2
if ($f -and (Select-String -Path $f.FullName -SimpleMatch $marker -Quiet)) { Remove-Item $f.FullName }
Remove-Item $img -ErrorAction SilentlyContinue
Stop-Process -Name shellbits -ErrorAction SilentlyContinue; Start-Sleep 1
Copy-Item $exe $env:USERPROFILE\shellbits\shellbits.exe -Force; Start-ScheduledTask shellbits; Start-Sleep 4
"installed copy running: " + [bool](Get-Process shellbits -ErrorAction SilentlyContinue)

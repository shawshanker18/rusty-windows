# What does each part of shell mode cost? With Explorer off (it is always put back at the end), start
# Rust UI as the shell several times, each time leaving one part out, and note its private memory and
# thread count after 25 s. Uses the installed copy (so the video wallpaper plays); startup apps are
# not launched (--restarted).
param([string[]]$Runs = @('', 'wall', 'toast', 'desktop', 'tray', 'preview'))
$ErrorActionPreference = 'Continue'
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class SM { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, string t);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l); }
'@
$exe = "$env:USERPROFILE\shellbits\shellbits.exe"
try {
  Stop-Process -Name shellbits -Force -ErrorAction SilentlyContinue
  [void][SM]::PostMessage([SM]::FindWindow('Shell_TrayWnd', $null), 0x5B4, [IntPtr]::Zero, [IntPtr]::Zero)   # Exit Explorer, the clean way
  $t = [Diagnostics.Stopwatch]::StartNew(); while ((Get-Process explorer -ErrorAction SilentlyContinue) -and $t.Elapsed.TotalSeconds -lt 8) { Start-Sleep -Milliseconds 200 }
  if (Get-Process explorer -ErrorAction SilentlyContinue) {
    # it sometimes ignores the first request: ask once more
    'Explorer still running after the first request: ' + ((Get-Process explorer | ForEach-Object { "$($_.Id) started $($_.StartTime.ToString('HH:mm:ss')) window=$($_.MainWindowHandle)" }) -join '; ')
    [void][SM]::PostMessage([SM]::FindWindow('Shell_TrayWnd', $null), 0x5B4, [IntPtr]::Zero, [IntPtr]::Zero)
    $t.Restart(); while ((Get-Process explorer -ErrorAction SilentlyContinue) -and $t.Elapsed.TotalSeconds -lt 8) { Start-Sleep -Milliseconds 200 }
  }
  # the taskbar going away is what counts; a leftover Explorer process without one is stopped
  if ([SM]::FindWindow('Shell_TrayWnd', $null) -ne [IntPtr]::Zero) { throw 'Explorer kept its taskbar: nothing measured' }
  Stop-Process -Name explorer -Force -ErrorAction SilentlyContinue; Start-Sleep 1
  foreach ($skip in $Runs) {
    if ($skip) { $env:SHELLBITS_SKIP = $skip } else { Remove-Item env:SHELLBITS_SKIP -ErrorAction SilentlyContinue }
    Start-Process $exe -ArgumentList '--shell','--restarted'; Start-Sleep 25
    $m = Get-Process shellbits | Sort-Object PrivateMemorySize64 -Descending | Select-Object -First 1
    "{0,-16} private {1,6:N1} MB   working set {2,6:N1} MB   threads {3}" -f $(if ($skip) { "without $skip" } else { 'everything' }), ($m.PrivateMemorySize64/1MB), ($m.WorkingSet64/1MB), $m.Threads.Count
    Stop-Process -Name shellbits -Force -ErrorAction SilentlyContinue; Start-Sleep 2
  }
} catch { "error: $_" }
finally {
  Remove-Item env:SHELLBITS_SKIP -ErrorAction SilentlyContinue
  Stop-Process -Name shellbits -Force -ErrorAction SilentlyContinue; Start-Sleep 1
  if (-not (Get-Process explorer -ErrorAction SilentlyContinue)) { Start-Process explorer.exe }
  Start-Sleep 5
  # an Explorer process can linger WITHOUT its taskbar after being asked to exit: never leave the user
  # like that - if there is no taskbar now, restart Explorer until there is one
  foreach ($try in 1..3) {
    if ([SM]::FindWindow('Shell_TrayWnd', $null) -ne [IntPtr]::Zero) { break }
    Stop-Process -Name explorer -Force -ErrorAction SilentlyContinue; Start-Sleep 2
    if (-not (Get-Process explorer -ErrorAction SilentlyContinue)) { Start-Process explorer.exe }; Start-Sleep 6
  }
  "taskbar present: " + ([SM]::FindWindow('Shell_TrayWnd', $null) -ne [IntPtr]::Zero)
  schtasks /run /tn shellbits | Out-Null; Start-Sleep 3
  "restored: explorer=" + [bool](Get-Process explorer -ErrorAction SilentlyContinue) + " shellbits=" + [bool](Get-Process shellbits -ErrorAction SilentlyContinue)
}

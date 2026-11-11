# Start the freshly built Rust UI (optionally with an environment switch), sample its private memory
# for a minute, then put the installed copy back. usage: mem-probe.ps1 [-Env NAME]
param([string]$Env = '')
if (-not (Get-Process explorer -ErrorAction SilentlyContinue)) { 'Rust UI is the shell right now and this script stops it (the taskbar would vanish): not run.'; exit 1 }
Stop-Process -Name shellbits -ErrorAction SilentlyContinue; Start-Sleep 2
if ($Env) { Set-Item "env:$Env" '1' }
$exe = "$PSScriptRoot\..\target\release\shellbits.exe"
Start-Process $exe; if ($Env) { Remove-Item "env:$Env" }
foreach ($t in 5,15,30,60) { Start-Sleep ($t - $last); $last = $t
  $m = Get-Process shellbits | Sort-Object PrivateMemorySize64 -Descending | Select-Object -First 1
  "{0,3}s: private {1:N1} MB, working set {2:N1} MB, threads {3}" -f $t, ($m.PrivateMemorySize64/1MB), ($m.WorkingSet64/1MB), $m.Threads.Count }
Stop-Process -Name shellbits -ErrorAction SilentlyContinue; Start-Sleep 1; Start-ScheduledTask shellbits

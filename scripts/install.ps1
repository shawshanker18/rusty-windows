# Install the built Rust UI and restart it in the SAME mode it was running in.
# If Explorer is not running, Rust UI is the shell: restarting it without --shell leaves no taskbar.
$shell = -not (Get-Process explorer -ErrorAction SilentlyContinue)
$src = "$PSScriptRoot\shellbits\target\release\shellbits.exe"; $dst = "$env:USERPROFILE\shellbits\shellbits.exe"
$copied = $false
foreach ($try in 1..8) {
  Stop-Process -Name shellbits -Force -ErrorAction SilentlyContinue
  # every copy (shell, watchdog, apps, snip) must be gone before the file can be replaced
  foreach ($i in 1..20) { if (-not (Get-Process shellbits -ErrorAction SilentlyContinue)) { break }; Start-Sleep -Milliseconds 150 }
  try { Copy-Item $src $dst -Force -ErrorAction Stop; $copied = $true; break } catch { Start-Sleep -Milliseconds 400 }
}
if ($shell) { Start-Process $dst -ArgumentList '--shell','--restarted' } else { Start-ScheduledTask shellbits }
Start-Sleep 6
$same = $copied -and ((Get-FileHash $src).Hash -eq (Get-FileHash $dst).Hash)
"mode: $(if ($shell) {'shell'} else {'normal'}); new build installed: $same; running: $([bool](Get-Process shellbits -ErrorAction SilentlyContinue))"
if (-not $same) { exit 1 }

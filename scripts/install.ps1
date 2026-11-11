# Install the built Rust UI and restart it in the SAME mode it was running in.
# If Explorer is not running, Rust UI is the shell: restarting it without --shell leaves no taskbar.
$shell = -not (Get-Process explorer -ErrorAction SilentlyContinue)
Stop-Process -Name shellbits -Force -ErrorAction SilentlyContinue; Start-Sleep 1
Copy-Item "$PSScriptRoot\..\target\release\shellbits.exe" "$env:USERPROFILE\shellbits\shellbits.exe" -Force
if ($shell) { Start-Process "$env:USERPROFILE\shellbits\shellbits.exe" -ArgumentList '--shell','--restarted' } else { Start-ScheduledTask shellbits }
Start-Sleep 6
"mode: $(if ($shell) {'shell'} else {'normal'}); running: $([bool](Get-Process shellbits -ErrorAction SilentlyContinue))"

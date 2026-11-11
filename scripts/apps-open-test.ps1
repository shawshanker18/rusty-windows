# Does each Rust app open when the shell is asked for it? No mouse or keys: the same message the
# hotkeys and the Start menu send is posted to the shell; each window is closed again by message.
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class AO { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h); }
'@
$fly = [AO]::FindWindow('shellbits_flyout', [IntPtr]::Zero)
"shell message window found: $($fly -ne [IntPtr]::Zero)"
foreach ($a in @(@('Calculator', 93, 'shellbits_calc'), @('Settings', 96, 'shellbits_settings'), @('Notepad', 92, 'shellbits_notepad'), @('File Explorer', 95, 'shellbits_files'), @('Claude', 97, 'shellbits_claude'))) {
  $before = [AO]::FindWindow($a[2], [IntPtr]::Zero); $wasOpen = $before -ne [IntPtr]::Zero -and [AO]::IsWindowVisible($before)
  [void][AO]::PostMessage($fly, 0x312, [IntPtr]$a[1], [IntPtr]::Zero)
  $h = [IntPtr]::Zero; $ms = 0
  foreach ($i in 1..40) { Start-Sleep -Milliseconds 150; $ms += 150; $h = [AO]::FindWindow($a[2], [IntPtr]::Zero); if ($h -ne [IntPtr]::Zero -and [AO]::IsWindowVisible($h)) { break } }
  $ok = $h -ne [IntPtr]::Zero -and [AO]::IsWindowVisible($h)
  "{0,-14} opened: {1}  ({2} ms){3}" -f $a[0], $ok, $ms, $(if ($wasOpen) { '  [was already open]' } else { '' })
  if ($ok -and -not $wasOpen) { Start-Sleep -Milliseconds 400; [void][AO]::PostMessage($h, 0x10, [IntPtr]::Zero, [IntPtr]::Zero); Start-Sleep -Milliseconds 400 }
}
"apps process running: $(@(Get-CimInstance Win32_Process -Filter "name='shellbits.exe'" | Where-Object { $_.CommandLine -match '--apps' }).Count); last crash-log line: $((Get-Content "$env:TEMP\shellbits-crashes.txt" -Tail 1))"

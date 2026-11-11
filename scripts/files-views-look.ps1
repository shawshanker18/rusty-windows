# Look at File Explorer's Home (recent files) and its three views without mouse or keys: the window is
# opened by command line, views are switched by a message to OUR window, pictures come from PrintWindow.
Add-Type -AssemblyName System.Drawing
Add-Type -ReferencedAssemblies System.Drawing @'
using System; using System.Drawing; using System.Runtime.InteropServices;
public static class FV { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
 [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
 public struct RECT { public int L, T, R, B; }
 public static void Shot(IntPtr h, string path) { RECT r; GetWindowRect(h, out r); using (var b = new Bitmap(r.R - r.L, r.B - r.T)) { using (var g = Graphics.FromImage(b)) { var dc = g.GetHdc(); PrintWindow(h, dc, 2); g.ReleaseHdc(dc); } b.Save(path); } } }
'@
$out = "$PSScriptRoot\..\snap\look"; New-Item -ItemType Directory -Force $out | Out-Null
Start-Process "$env:USERPROFILE\shellbits\shellbits.exe" -ArgumentList '--files', "$env:USERPROFILE\Pictures"
$h = [IntPtr]::Zero; foreach ($i in 1..40) { $h = [FV]::FindWindow('shellbits_files', [IntPtr]::Zero); if ($h -ne [IntPtr]::Zero -and [FV]::IsWindowVisible($h)) { break }; Start-Sleep -Milliseconds 150 }
Start-Sleep 1
foreach ($v in @(@(1, 'icons'), @(2, 'list'), @(0, 'details'))) { [void][FV]::PostMessage($h, 0x8000 + 77, [IntPtr]$v[0], [IntPtr]::Zero); Start-Sleep -Milliseconds 1500; [FV]::Shot($h, "$out\files-$($v[1]).png") }
[void][FV]::PostMessage($h, 0x10, [IntPtr]::Zero, [IntPtr]::Zero); Start-Sleep 1
Start-Process "$env:USERPROFILE\shellbits\shellbits.exe" -ArgumentList '--files'
Start-Sleep 2; $h = [FV]::FindWindow('shellbits_files', [IntPtr]::Zero); [FV]::Shot($h, "$out\files-home.png")
[void][FV]::PostMessage($h, 0x10, [IntPtr]::Zero, [IntPtr]::Zero)
"still running: $([bool](Get-Process shellbits -ErrorAction SilentlyContinue)); last crash line: $((Get-Content "$env:TEMP\shellbits-crashes.txt" -Tail 1))"

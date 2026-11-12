# Rust Notepad tabs, live, by messages to our own window only: open a throwaway file, new tab, type,
# switch back and forth, close (session saved), reopen (both tabs back, the unsaved text too), then
# close the extra tab and clean up. Leaves the user's own session file as it was.
Add-Type @'
using System; using System.Text; using System.Runtime.InteropServices;
public static class NB { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr p, IntPtr a, string c, IntPtr t);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, StringBuilder l);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, string l);
 [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, IntPtr l); }
'@
$z = [IntPtr]::Zero
$exe = 'C:\Users\shash\shellbits\shellbits.exe'
$sess = "$env:LOCALAPPDATA\shellbits\notepad-tabs"; $keep = "$env:TEMP\notepad-tabs-keep"
Remove-Item $keep -Recurse -Force -ErrorAction SilentlyContinue
if (Test-Path $sess) { Copy-Item $sess $keep -Recurse; Remove-Item $sess -Recurse -Force }
$file = "$env:TEMP\rust-notepad-tabs.txt"; Set-Content $file "first file" -NoNewline -Encoding utf8
function text($e) { $b = New-Object Text.StringBuilder 4096; [void][NB]::SendMessage($e, 0x0D, [IntPtr]4096, $b); $b.ToString() }
function title($h) { $t = New-Object Text.StringBuilder 256; [void][NB]::GetWindowText($h, $t, 256); "$t" }
function open { Start-Process $exe -ArgumentList '--notepad', "`"$file`""; Start-Sleep -Milliseconds 1800; [NB]::FindWindow('shellbits_notepad', $z) }
$h = open; $e = [NB]::FindWindowEx($h, $z, 'Edit', $z)
"opened: " + (title $h)
[void][NB]::SendMessage($h, 0x111, [IntPtr]1, $z); Start-Sleep -Milliseconds 300            # New tab
"new tab is Untitled and empty: " + ((title $h) -like 'Untitled*' -and (text $e) -eq '')
[void][NB]::SendMessage($e, 0xC2, [IntPtr]1, "scratch notes"); Start-Sleep -Milliseconds 300 # type
[void][NB]::SendMessage($h, 0x111, [IntPtr]41, $z); Start-Sleep -Milliseconds 300            # next tab (wraps to the file)
"switched back to the file: " + ((text $e) -eq 'first file')
[void][NB]::SendMessage($h, 0x111, [IntPtr]41, $z); Start-Sleep -Milliseconds 300
"the new tab kept its unsaved text: " + ((text $e) -eq 'scratch notes') + " title: " + (title $h)
[void][NB]::SendMessage($h, 0x10, $z, $z); Start-Sleep -Milliseconds 800                     # close: session saved
"closed, session written: " + (Test-Path "$sess\session.txt")
$h = open; $e = [NB]::FindWindowEx($h, $z, 'Edit', $z)
"reopened, the file's tab in front: " + ((text $e) -eq 'first file')
[void][NB]::SendMessage($h, 0x111, [IntPtr]41, $z); Start-Sleep -Milliseconds 300
"and the unsaved tab came back with its text: " + ((text $e) -eq 'scratch notes')
# tidy: empty the scratch tab, close the window, restore the user's session
[void][NB]::SendMessage($e, 0x0C, $z, "");
Remove-Item $sess -Recurse -Force -ErrorAction SilentlyContinue
[void][NB]::SendMessage($h, 0x10, $z, $z); Start-Sleep -Milliseconds 600
Remove-Item $sess -Recurse -Force -ErrorAction SilentlyContinue
if (Test-Path $keep) { Copy-Item $keep $sess -Recurse; Remove-Item $keep -Recurse -Force }
Remove-Item $file -ErrorAction SilentlyContinue
"new crash entries: " + ((Get-Item "$env:TEMP\shellbits-crashes.txt" -ErrorAction SilentlyContinue).LastWriteTime -gt (Get-Date).AddMinutes(-2))

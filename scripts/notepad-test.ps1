# Rust Notepad, live, on a throwaway file this script creates (Unix line endings, UTF-8):
# open it, check the text and title, add a line (sent to our own edit control), save (command sent to
# our own window), check the file on disk kept its LF endings, capture the window, close it.
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Text; using System.Runtime.InteropServices;
public static class NT { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr p, IntPtr a, string c, IntPtr t);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, StringBuilder l);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, string l);
 [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
 [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int w, int hh, uint f);
 [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r); public struct RECT { public int L,T,R,B; } }
'@
$exe = "$env:USERPROFILE\shellbits\shellbits.exe"; $snap = "$PSScriptRoot\..\snap"
$file = "$env:TEMP\rust-notepad-test.txt"; [IO.File]::WriteAllBytes($file, [Text.Encoding]::UTF8.GetBytes("hello`nworld`n"))
Start-Process $exe -ArgumentList '--notepad', "`"$file`""; Start-Sleep -Milliseconds 1800
$h = [NT]::FindWindow('shellbits_notepad', [IntPtr]::Zero)
"Notepad window opened: " + ($h -ne [IntPtr]::Zero)
if ($h -eq [IntPtr]::Zero) { exit }
[void][NT]::SetWindowPos($h, [IntPtr]::Zero, 0, 0, 1140, 583, 0x16); Start-Sleep -Milliseconds 300
$t = New-Object Text.StringBuilder 256; [void][NT]::GetWindowText($h, $t, 256); "title: $t"
$e = [NT]::FindWindowEx($h, [IntPtr]::Zero, 'Edit', [IntPtr]::Zero)
$b = New-Object Text.StringBuilder 4096; [void][NT]::SendMessage($e, 0x0D, [IntPtr]4096, $b)
"text loaded with its two lines: " + ($b.ToString() -eq "hello`r`nworld`r`n")
# Find: open the bar (command to our window), put WORLD in its field, Find next -> "world" selected (case ignored)
[void][NT]::SendMessage($h, 0x111, [IntPtr]30, [IntPtr]::Zero); Start-Sleep -Milliseconds 300
$f = [NT]::FindWindowEx($h, $e, 'Edit', [IntPtr]::Zero)
[void][NT]::SendMessage($f, 0x0C, [IntPtr]::Zero, 'WORLD'); [void][NT]::SendMessage($h, 0x111, [IntPtr]31, [IntPtr]::Zero); Start-Sleep -Milliseconds 300
$sel = [NT]::SendMessage($e, 0xB0, [IntPtr]::Zero, [IntPtr]::Zero).ToInt64(); "Find selected 'world' (characters 7-12): " + (($sel -band 0xFFFF) -eq 7 -and (($sel -shr 16) -band 0xFFFF) -eq 12)
[void][NT]::SendMessage($h, 0x111, [IntPtr]31, [IntPtr]::Zero); Start-Sleep -Milliseconds 200
$sel = [NT]::SendMessage($e, 0xB0, [IntPtr]::Zero, [IntPtr]::Zero).ToInt64(); "Find next wrapped round to the same match: " + (($sel -band 0xFFFF) -eq 7)
[void][NT]::SendMessage($f, 0x0C, [IntPtr]::Zero, 'zzz'); [void][NT]::SendMessage($h, 0x111, [IntPtr]31, [IntPtr]::Zero); Start-Sleep -Milliseconds 300
$r0 = New-Object NT+RECT; [void][NT]::GetClientRect($h, [ref]$r0)
$bmp = New-Object System.Drawing.Bitmap $r0.R, $r0.B; $g = [System.Drawing.Graphics]::FromImage($bmp); $dc = $g.GetHdc(); [void][NT]::PrintWindow($h, $dc, 3); $g.ReleaseHdc($dc); $g.Dispose()
$bmp.Save("$snap\rs-notepad-find.png"); $bmp.Dispose()
# Replace all: o -> 0 (two of them), check, then undo it in one step
[void][NT]::SendMessage($h, 0x111, [IntPtr]34, [IntPtr]::Zero); Start-Sleep -Milliseconds 300
$rp = [NT]::FindWindowEx($h, $f, 'Edit', [IntPtr]::Zero)
[void][NT]::SendMessage($f, 0x0C, [IntPtr]::Zero, 'O'); [void][NT]::SendMessage($rp, 0x0C, [IntPtr]::Zero, '0'); [void][NT]::SendMessage($h, 0x111, [IntPtr]36, [IntPtr]::Zero); Start-Sleep -Milliseconds 300
[void]$b.Clear(); [void][NT]::SendMessage($e, 0x0D, [IntPtr]4096, $b); "Replace all turned both o's into 0: " + ($b.ToString() -eq "hell0`r`nw0rld`r`n")
$bmp = New-Object System.Drawing.Bitmap $r0.R, $r0.B; $g = [System.Drawing.Graphics]::FromImage($bmp); $dc = $g.GetHdc(); [void][NT]::PrintWindow($h, $dc, 3); $g.ReleaseHdc($dc); $g.Dispose()
$bmp.Save("$snap\rs-notepad-replace.png"); $bmp.Dispose()
[void][NT]::SendMessage($e, 0xC7, [IntPtr]::Zero, [IntPtr]::Zero); Start-Sleep -Milliseconds 200
[void]$b.Clear(); [void][NT]::SendMessage($e, 0x0D, [IntPtr]4096, $b); "one Undo brought the text back: " + ($b.ToString() -eq "hello`r`nworld`r`n")
# Go to line 2: the caret lands at the start of "world" and the bar closes
[void][NT]::SendMessage($h, 0x111, [IntPtr]37, [IntPtr]::Zero); Start-Sleep -Milliseconds 300
[void][NT]::SendMessage($f, 0x0C, [IntPtr]::Zero, '2'); [void][NT]::SendMessage($h, 0x111, [IntPtr]31, [IntPtr]::Zero); Start-Sleep -Milliseconds 300
$sel = [NT]::SendMessage($e, 0xB0, [IntPtr]::Zero, [IntPtr]::Zero).ToInt64(); "Go to line 2 put the caret at the start of that line: " + (($sel -band 0xFFFF) -eq 7 -and (($sel -shr 16) -band 0xFFFF) -eq 7)
[void][NT]::SendMessage($h, 0x111, [IntPtr]33, [IntPtr]::Zero); Start-Sleep -Milliseconds 300
# Word wrap off and on again: the text control is rebuilt each time and must keep the text
Add-Type 'using System; using System.Runtime.InteropServices; public static class NS { [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr h, int i); }'
function MainEdit { $c = [IntPtr]::Zero; do { $c = [NT]::FindWindowEx($h, $c, 'Edit', [IntPtr]::Zero); if ($c -ne [IntPtr]::Zero -and ([NS]::GetWindowLong($c, -16) -band 4)) { return $c } } while ($c -ne [IntPtr]::Zero); [IntPtr]::Zero }
[void][NT]::SendMessage($h, 0x111, [IntPtr]24, [IntPtr]::Zero); Start-Sleep -Milliseconds 300; $e = MainEdit
[void]$b.Clear(); [void][NT]::SendMessage($e, 0x0D, [IntPtr]4096, $b)
"word wrap off: text kept, horizontal scroll bar present: " + ($b.ToString() -eq "hello`r`nworld`r`n" -and (([NS]::GetWindowLong($e, -16) -band 0x100000) -ne 0))
[void][NT]::SendMessage($h, 0x111, [IntPtr]24, [IntPtr]::Zero); Start-Sleep -Milliseconds 300; $e = MainEdit
[void]$b.Clear(); [void][NT]::SendMessage($e, 0x0D, [IntPtr]4096, $b)
"word wrap on again: text kept, no horizontal scroll bar: " + ($b.ToString() -eq "hello`r`nworld`r`n" -and (([NS]::GetWindowLong($e, -16) -band 0x100000) -eq 0))
# caret to the end, type a third line, then Save
[void][NT]::SendMessage($e, 0xB1, [IntPtr]100000, [IntPtr]100000); [void][NT]::SendMessage($e, 0xC2, [IntPtr]1, "third line`r`n"); Start-Sleep -Milliseconds 500
[void][NT]::GetWindowText($h, $t, 256); "title after typing (star = unsaved): $t"
$r = New-Object NT+RECT; [void][NT]::GetClientRect($h, [ref]$r)
$bmp = New-Object System.Drawing.Bitmap $r.R, $r.B; $g = [System.Drawing.Graphics]::FromImage($bmp); $dc = $g.GetHdc(); [void][NT]::PrintWindow($h, $dc, 3); $g.ReleaseHdc($dc); $g.Dispose()
$bmp.Save("$snap\rs-notepad.png"); $bmp.Dispose()
[void][NT]::SendMessage($h, 0x111, [IntPtr]3, [IntPtr]::Zero); Start-Sleep -Milliseconds 400
[void][NT]::GetWindowText($h, $t, 256); "title after saving: $t"
$bytes = [IO.File]::ReadAllBytes($file); $text = [Text.Encoding]::UTF8.GetString($bytes)
"file on disk has the new line and still Unix line endings: " + ($text -eq "hello`nworld`nthird line`n")
[void][NT]::PostMessage($h, 0x10, [IntPtr]::Zero, [IntPtr]::Zero); Start-Sleep -Milliseconds 700
"closed without a prompt: " + ([NT]::FindWindow('shellbits_notepad', [IntPtr]::Zero) -eq [IntPtr]::Zero)
# preferences survive a restart: zoom in once and hide the status bar, close, reopen, check, then put them back
$prefs = "$env:LOCALAPPDATA\shellbits\notepad.txt"
Start-Process $exe -ArgumentList '--notepad'; Start-Sleep -Milliseconds 1500; $h = [NT]::FindWindow('shellbits_notepad', [IntPtr]::Zero)
[void][NT]::SendMessage($h, 0x111, [IntPtr]20, [IntPtr]::Zero); [void][NT]::SendMessage($h, 0x111, [IntPtr]23, [IntPtr]::Zero); Start-Sleep -Milliseconds 300
[void][NT]::PostMessage($h, 0x10, [IntPtr]::Zero, [IntPtr]::Zero); Start-Sleep -Milliseconds 700
"saved preferences: " + (Get-Content $prefs)
Start-Process $exe -ArgumentList '--notepad'; Start-Sleep -Milliseconds 1500; $h = [NT]::FindWindow('shellbits_notepad', [IntPtr]::Zero)
$e = [IntPtr]::Zero; $c = [IntPtr]::Zero; do { $c = [NT]::FindWindowEx($h, $c, 'Edit', [IntPtr]::Zero); if ($c -ne [IntPtr]::Zero -and ([NS]::GetWindowLong($c, -16) -band 4)) { $e = $c } } while ($c -ne [IntPtr]::Zero)
$wr = New-Object NT+RECT; [void][NT]::GetClientRect($h, [ref]$wr); $er = New-Object NT+RECT; [void][NT]::GetClientRect($e, [ref]$er)
"reopened with the status bar still hidden (text reaches the bottom): " + (($wr.B - 75 - $er.B) -lt 4)
[void][NT]::SendMessage($h, 0x111, [IntPtr]22, [IntPtr]::Zero); [void][NT]::SendMessage($h, 0x111, [IntPtr]23, [IntPtr]::Zero); Start-Sleep -Milliseconds 300
[void][NT]::PostMessage($h, 0x10, [IntPtr]::Zero, [IntPtr]::Zero); Start-Sleep -Milliseconds 700
"preferences put back: " + (Get-Content $prefs)
Remove-Item $file -ErrorAction SilentlyContinue

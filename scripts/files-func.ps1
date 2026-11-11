Add-Type @'
using System; using System.Runtime.InteropServices;
public static class FF { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr p, IntPtr a, string c, IntPtr t);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, string l);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int w, int hh, uint f);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, System.Text.StringBuilder s, int n);
 public static void Click(IntPtr h, int x, int y){ IntPtr lp=(IntPtr)((y<<16)|x); PostMessage(h,0x200,IntPtr.Zero,lp); PostMessage(h,0x201,(IntPtr)1,lp); PostMessage(h,0x202,IntPtr.Zero,lp); }
 public static string Title(IntPtr h){ var s=new System.Text.StringBuilder(200); GetWindowText(h,s,200); return s.ToString(); } }
'@
$dir = Join-Path $env:TEMP 'rustui-explorer-test'; Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $dir | Out-Null; Set-Content (Join-Path $dir 'b-note.txt') 'hello'; Set-Content (Join-Path $dir 'a-first.txt') 'x'
Start-Process "$env:USERPROFILE\shellbits\shellbits.exe" -ArgumentList '--files'; Start-Sleep 2
$h=[FF]::FindWindow('shellbits_files',[IntPtr]::Zero); if($h -eq [IntPtr]::Zero){ 'window not found'; exit }
[void][FF]::SetWindowPos($h,[IntPtr]::Zero,108,60,1104,692,0x14); Start-Sleep -Milliseconds 600
# 1. type a path in the address bar
[FF]::Click($h,700,64); Start-Sleep -Milliseconds 500
$edit=[FF]::FindWindowEx($h,[IntPtr]::Zero,'Edit',[IntPtr]::Zero)
"address box opened for typing: " + [FF]::IsWindowVisible($edit)
[void][FF]::SendMessage($edit,0x000C,[IntPtr]::Zero,$dir); [void][FF]::PostMessage($edit,0x100,[IntPtr]0x0D,[IntPtr]::Zero); Start-Sleep -Milliseconds 800
"navigated to test folder (window title): " + [FF]::Title($h)
# 2. New folder
[FF]::Click($h,45,112); Start-Sleep -Milliseconds 800
"New created 'New folder' on disk: " + (Test-Path (Join-Path $dir 'New folder'))
# 3. select the first row (the new folder sorts first) and press Delete
[FF]::Click($h,300,180); Start-Sleep -Milliseconds 300
[void][FF]::PostMessage($h,0x100,[IntPtr]0x2E,[IntPtr]::Zero); Start-Sleep -Milliseconds 2500
"Delete removed it (sent to Recycle Bin): " + (-not (Test-Path (Join-Path $dir 'New folder')))
# 4. Enter opens a file's parent? select first file and check it's still listed; then go up with Backspace
[void][FF]::PostMessage($h,0x100,[IntPtr]0x08,[IntPtr]::Zero); Start-Sleep -Milliseconds 600
"Backspace went up to: " + [FF]::Title($h)
[void][FF]::PostMessage($h,0x10,[IntPtr]::Zero,[IntPtr]::Zero)
Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue

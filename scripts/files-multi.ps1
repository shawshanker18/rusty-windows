Add-Type @'
using System; using System.Runtime.InteropServices;
public static class FF { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr p, IntPtr a, string c, IntPtr t);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, string l);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int w, int hh, uint f);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, System.Text.StringBuilder s, int n);
 public static void ClickM(IntPtr h, int x, int y, int mk){ IntPtr lp=(IntPtr)((y<<16)|x); PostMessage(h,0x200,IntPtr.Zero,lp); PostMessage(h,0x201,(IntPtr)(1|mk),lp); PostMessage(h,0x202,IntPtr.Zero,lp); }
 public static void Click(IntPtr h, int x, int y){ IntPtr lp=(IntPtr)((y<<16)|x); PostMessage(h,0x200,IntPtr.Zero,lp); PostMessage(h,0x201,(IntPtr)1,lp); PostMessage(h,0x202,IntPtr.Zero,lp); }
 public static string Title(IntPtr h){ var s=new System.Text.StringBuilder(200); GetWindowText(h,s,200); return s.ToString(); } }
'@
$dir = Join-Path $env:TEMP 'rustui-multi-test'; Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $dir | Out-Null; 'a','b','c','d' | % { Set-Content (Join-Path $dir "$_.txt") $_ }
Start-Process "$env:USERPROFILE\shellbits\shellbits.exe" -ArgumentList '--files',$dir; Start-Sleep 3
$h=[FF]::FindWindow('shellbits_files',[IntPtr]::Zero); if($h -eq [IntPtr]::Zero){ 'window not found'; exit }
[void][FF]::SetWindowPos($h,[IntPtr]::Zero,108,60,1104,692,0x14); Start-Sleep -Milliseconds 600
"opened: " + [FF]::Title($h)
[FF]::Click($h,300,180); Start-Sleep -Milliseconds 200
[FF]::ClickM($h,300,236,4); Start-Sleep -Milliseconds 300   # Shift+click row 3 -> a,b,c
[FF]::ClickM($h,300,208,8); Start-Sleep -Milliseconds 300   # Ctrl+click row 2 -> drops b
[void][FF]::PostMessage($h,0x100,[IntPtr]0x2E,[IntPtr]::Zero); Start-Sleep -Milliseconds 3000
"left after Shift-range, Ctrl-toggle, Delete (expect b.txt d.txt): " + ((Get-ChildItem $dir).Name -join ' ')
[void][FF]::PostMessage($h,0x10,[IntPtr]::Zero,[IntPtr]::Zero)
Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue

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
Start-Process "$env:USERPROFILE/shellbits/shellbits.exe" -ArgumentList '--files',$env:USERPROFILE; Start-Sleep 3
$h=[FF]::FindWindow('shellbits_files',[IntPtr]::Zero); if($h -eq [IntPtr]::Zero){ 'window not found'; exit }
[void][FF]::SetWindowPos($h,[IntPtr]::Zero,108,60,1104,692,0x14); Start-Sleep -Milliseconds 600
"first tab: " + [FF]::Title($h)
[FF]::Click($h,270,24); Start-Sleep -Milliseconds 700
"after + (expect Home): " + [FF]::Title($h)
[FF]::Click($h,100,24); Start-Sleep -Milliseconds 700
"after clicking tab 1 (expect user folder): " + [FF]::Title($h)
[FF]::Click($h,470,24); Start-Sleep -Milliseconds 700
"closed tab 2, window still open: " + [FF]::IsWindowVisible($h) + ", showing " + [FF]::Title($h)
[FF]::Click($h,230,24); Start-Sleep -Milliseconds 900
"closed last tab, window closed: " + (-not [FF]::IsWindowVisible($h))

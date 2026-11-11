Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class FP { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
 [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int w, int hh, uint f);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r); public struct RECT { public int L,T,R,B; }
 public static void Click(IntPtr h, int x, int y){ IntPtr lp=(IntPtr)((y<<16)|x); PostMessage(h,0x200,IntPtr.Zero,lp); PostMessage(h,0x201,(IntPtr)1,lp); PostMessage(h,0x202,IntPtr.Zero,lp); } }
'@
function Cap($h, $out) { $r=New-Object FP+RECT; [void][FP]::GetWindowRect($h,[ref]$r)
  $b=New-Object System.Drawing.Bitmap ($r.R-$r.L),($r.B-$r.T); $g=[System.Drawing.Graphics]::FromImage($b); $dc=$g.GetHdc(); [void][FP]::PrintWindow($h,$dc,2); $g.ReleaseHdc($dc); $b.Save($out) }
$root=$args[0]
$dir = Join-Path $env:TEMP 'rustui-preview-test'; Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $dir | Out-Null; Copy-Item (Join-Path $root 'compare.png') (Join-Path $dir 'picture.png')
Start-Process "$env:USERPROFILE\shellbits\shellbits.exe" -ArgumentList '--files',$dir; Start-Sleep 3
$h=[FP]::FindWindow('shellbits_files',[IntPtr]::Zero); if($h -eq [IntPtr]::Zero){ 'window not found'; exit }
[void][FP]::SetWindowPos($h,[IntPtr]::Zero,108,60,1104,692,0x14); Start-Sleep -Milliseconds 600
[FP]::Click($h,300,180); Start-Sleep -Milliseconds 1500
Cap $h (Join-Path $root 'snap\files-preview.png')
[FP]::Click($h,1040,112); Start-Sleep -Milliseconds 600   # Preview switch: pane closes
Cap $h (Join-Path $root 'snap\files-preview-off.png')
[FP]::Click($h,1040,112); Start-Sleep -Milliseconds 300
[void][FP]::PostMessage($h,0x10,[IntPtr]::Zero,[IntPtr]::Zero)
Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
'captured'

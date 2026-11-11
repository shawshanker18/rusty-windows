# Snap Assist: two windows this script owns. Win+Z on A, layout 1 zone 2 (posted to our picker) puts A on the
# right half; the Rust Snap Assist should then offer the other windows in the left half. B is picked by
# posting its handle to our own assist window (so none of the user's windows is moved). No Esc/Enter is typed.
param([switch]$Arrow)   # -Arrow: snap A the way Win+Right does in shell mode (posted to our snap window) instead of Win+Z
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class SA { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
 [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out RECT r, int n);
 public struct RECT { public int L,T,R,B; }
 [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
 public static void WinZ(){ keybd_event(0x5B,0,0,UIntPtr.Zero); keybd_event(0x5A,0,0,UIntPtr.Zero); keybd_event(0x5A,0,2,UIntPtr.Zero); keybd_event(0x5B,0,2,UIntPtr.Zero); } }
'@
function Pump($ms) { $t=[Diagnostics.Stopwatch]::StartNew(); while ($t.ElapsedMilliseconds -lt $ms) { [System.Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 30 } }
function Mk($title,$x,$col) { $f=New-Object System.Windows.Forms.Form; $f.Text=$title; $f.StartPosition='Manual'; $f.SetBounds($x,150,620,420); $f.BackColor=$col; $f.Show(); $f }
$b=Mk 'assist test B' 260 ([System.Drawing.Color]::SteelBlue); $a=Mk 'assist test A' 700 ([System.Drawing.Color]::DarkOrange)
$a.Activate(); Pump 700; [void][SA]::SetForegroundWindow($a.Handle); Pump 300
$p=[SA]::FindWindow('shellbits_snap',[IntPtr]::Zero); $as=[SA]::FindWindow('shellbits_assist',[IntPtr]::Zero)
if ([SA]::GetForegroundWindow() -ne $a.Handle) { 'skipped: could not bring the test window to the front'; $a.Close(); $b.Close(); exit }
if ($Arrow) { [void][SA]::PostMessage($p,0x8000+71,[IntPtr]0x27,$a.Handle); Pump 900 } else {
[SA]::WinZ(); Pump 600
"Win+Z opened the snap picker: " + [SA]::IsWindowVisible($p)
[void][SA]::PostMessage($p,0x8000+61,[IntPtr]1,[IntPtr]::Zero); Pump 200
[void][SA]::PostMessage($p,0x8000+61,[IntPtr]2,[IntPtr]::Zero); Pump 900 }
$wa=[System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea; $mid=$wa.Left+$wa.Width/2
$r=New-Object SA+RECT; [void][SA]::GetWindowRect($as,[ref]$r)
"Snap Assist opened: " + [SA]::IsWindowVisible($as) + " at $($r.L),$($r.T) - $($r.R),$($r.B) (left half is $($wa.Left),$($wa.Top) - $mid,$($wa.Bottom))"
if ([SA]::IsWindowVisible($as)) {
  $bmp=New-Object System.Drawing.Bitmap ($r.R-$r.L),($r.B-$r.T); $g=[System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($r.L,$r.T,0,0,$bmp.Size); $bmp.Save("$PSScriptRoot\..\snap\assist.png"); $g.Dispose(); $bmp.Dispose()
  # move the keyboard choice right and back left (posted to our own window), then pick B by its handle
  [void][SA]::PostMessage($as,0x8000+69,[IntPtr]0x27,[IntPtr]::Zero); [void][SA]::PostMessage($as,0x8000+69,[IntPtr]0x25,[IntPtr]::Zero); Pump 200
  "still open after arrow keys: " + [SA]::IsWindowVisible($as)
  [void][SA]::PostMessage($as,0x8000+70,$b.Handle,[IntPtr]::Zero); Pump 900
  "Snap Assist closed once the layout was full: " + (-not [SA]::IsWindowVisible($as))
  $q=New-Object SA+RECT; [void][SA]::DwmGetWindowAttribute($b.Handle,9,[ref]$q,16)
  "B now at $($q.L),$($q.T) - $($q.R),$($q.B)"
  "B fills the left half: " + ($q.L -eq $wa.Left -and $q.T -eq $wa.Top -and $q.R -eq $mid -and $q.B -eq $wa.Bottom)
  "B is in front: " + ([SA]::GetForegroundWindow() -eq $b.Handle)
}
$q=New-Object SA+RECT; [void][SA]::DwmGetWindowAttribute($a.Handle,9,[ref]$q,16)
"A fills the right half: " + ($q.L -eq $mid -and $q.T -eq $wa.Top -and $q.R -eq $wa.Right -and $q.B -eq $wa.Bottom)
if ($Arrow) {
  # Up on the right half -> top-right quarter; Down -> the half again; Down -> bottom-right quarter
  $half=[int]($wa.Top+$wa.Height/2)
  [void][SA]::PostMessage($p,0x8000+71,[IntPtr]0x26,$a.Handle); Pump 500; [void][SA]::DwmGetWindowAttribute($a.Handle,9,[ref]$q,16)
  "Up -> top-right quarter: " + ($q.L -eq $mid -and $q.T -eq $wa.Top -and $q.R -eq $wa.Right -and $q.B -eq $half)
  [void][SA]::PostMessage($p,0x8000+71,[IntPtr]0x28,$a.Handle); Pump 500; [void][SA]::DwmGetWindowAttribute($a.Handle,9,[ref]$q,16)
  "Down -> right half again: " + ($q.L -eq $mid -and $q.T -eq $wa.Top -and $q.R -eq $wa.Right -and $q.B -eq $wa.Bottom)
  [void][SA]::PostMessage($as,0x8000+69,[IntPtr]0x1B,[IntPtr]::Zero); Pump 200   # close the offer that a half re-opens (posted to our own window)
  [void][SA]::PostMessage($p,0x8000+71,[IntPtr]0x28,$a.Handle); Pump 500; [void][SA]::DwmGetWindowAttribute($a.Handle,9,[ref]$q,16)
  "Down -> bottom-right quarter: " + ($q.L -eq $mid -and $q.T -eq $half -and $q.R -eq $wa.Right -and $q.B -eq $wa.Bottom)
}
$a.Close(); $b.Close(); Pump 300
"Snap Assist hidden at the end: " + (-not [SA]::IsWindowVisible($as))

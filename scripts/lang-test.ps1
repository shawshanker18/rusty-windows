# Win+Space over a window this script owns: the Rust input-language list shows while Win is held;
# letting go switches this window's keyboard layout. Done twice so the layout ends where it began.
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class CV { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r); public struct RECT { public int L,T,R,B; }
 [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
 public static void Down(byte k){ keybd_event(k,0,0,UIntPtr.Zero); } public static void Up(byte k){ keybd_event(k,0,2,UIntPtr.Zero); } }
'@
function Pump($ms) { $t=[Diagnostics.Stopwatch]::StartNew(); while ($t.ElapsedMilliseconds -lt $ms) { [System.Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 30 } }
$form=New-Object System.Windows.Forms.Form; $form.Text='language test'; $form.StartPosition='Manual'; $form.SetBounds(300,200,500,200)
$box=New-Object System.Windows.Forms.TextBox; $box.Dock='Fill'; $form.Controls.Add($box)
$form.Show(); $form.Activate(); $box.Focus() | Out-Null; Pump 700
[void][CV]::SetForegroundWindow($form.Handle); Pump 300
$p=[CV]::FindWindow('shellbits_lang',[IntPtr]::Zero)
if ([CV]::GetForegroundWindow() -ne $form.Handle) { 'skipped: could not bring the test window to the front'; $form.Close(); exit }
$start=[System.Windows.Forms.InputLanguage]::CurrentInputLanguage.LayoutName
"layouts installed: " + [System.Windows.Forms.InputLanguage]::InstalledInputLanguages.Count + "; this window starts on: $start"
foreach ($round in 1,2) {
  [CV]::Down(0x5B); [CV]::Down(0x20); [CV]::Up(0x20); Pump 500
  $r=New-Object CV+RECT; [void][CV]::GetWindowRect($p,[ref]$r)
  "round ${round}: list shown while Win is held: " + [CV]::IsWindowVisible($p) + " (size $($r.R-$r.L)x$($r.B-$r.T))"
  if ($round -eq 1 -and [CV]::IsWindowVisible($p)) { $b=New-Object System.Drawing.Bitmap ($r.R-$r.L),($r.B-$r.T); $g=[System.Drawing.Graphics]::FromImage($b); $g.CopyFromScreen($r.L,$r.T,0,0,$b.Size); $b.Save((Join-Path $args[0] 'snap\lang-live.png')) }
  [CV]::Up(0x5B); Pump 700
  "round ${round}: list hidden after letting go: " + (-not [CV]::IsWindowVisible($p)) + "; window now on: " + [System.Windows.Forms.InputLanguage]::CurrentInputLanguage.LayoutName
}
# clicking a row (mouse message posted to our own panel) picks that layout without letting go of Win
$names=@()
foreach ($row in 0,1) {
  [CV]::Down(0x5B); [CV]::Down(0x20); [CV]::Up(0x20); Pump 500
  $y = 8 + 56*$row + 28
  [void][CV]::PostMessage($p, 0x202, [IntPtr]::Zero, [IntPtr](($y -shl 16) -bor 100)); Pump 500
  "click on row ${row}: list closed at once: " + (-not [CV]::IsWindowVisible($p)) + "; window now on: " + [System.Windows.Forms.InputLanguage]::CurrentInputLanguage.LayoutName
  $names += [System.Windows.Forms.InputLanguage]::CurrentInputLanguage.LayoutName
  [CV]::Up(0x5B); Pump 400
}
"the two rows gave two different layouts: " + ($names[0] -ne $names[1])
# end on the layout the test started with
if ([System.Windows.Forms.InputLanguage]::CurrentInputLanguage.LayoutName -ne $start) { [CV]::Down(0x5B); [CV]::Down(0x20); [CV]::Up(0x20); Pump 400; [CV]::Up(0x5B); Pump 700 }
"back on the starting layout: " + ([System.Windows.Forms.InputLanguage]::CurrentInputLanguage.LayoutName -eq $start)
$form.Close(); $form.Dispose(); Pump 300

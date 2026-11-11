# Win+. over a text box this script owns: the Rust emoji panel opens; Enter (posted to our own panel
# (Enter is posted to our own panel window, never typed globally) pastes it into the box.
# Only the pasted LENGTH is reported, never the text. The newest item is already the clipboard's
# current content, so the clipboard is left as it was.
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
 public static void WinDot(){ keybd_event(0x5B,0,0,UIntPtr.Zero); keybd_event(0xBE,0,0,UIntPtr.Zero); keybd_event(0xBE,0,2,UIntPtr.Zero); keybd_event(0x5B,0,2,UIntPtr.Zero); } }
'@
function Pump($ms) { $t=[Diagnostics.Stopwatch]::StartNew(); while ($t.ElapsedMilliseconds -lt $ms) { [System.Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 30 } }
$form=New-Object System.Windows.Forms.Form; $form.Text='emoji test'; $form.StartPosition='Manual'; $form.SetBounds(300,200,700,300)
$box=New-Object System.Windows.Forms.TextBox; $box.Multiline=$true; $box.Dock='Fill'; $form.Controls.Add($box)
$form.Show(); $form.Activate(); $box.Focus() | Out-Null; Pump 700
[void][CV]::SetForegroundWindow($form.Handle); Pump 300
$p=[CV]::FindWindow('shellbits_emoji',[IntPtr]::Zero)
if ([CV]::GetForegroundWindow() -ne $form.Handle) { 'skipped: could not bring the test window to the front'; $form.Close(); exit }
[CV]::WinDot(); Pump 1200
$r=New-Object CV+RECT; [void][CV]::GetWindowRect($p,[ref]$r)
"Win+. opened the Rust emoji panel: " + [CV]::IsWindowVisible($p) + " (size $($r.R-$r.L)x$($r.B-$r.T))"
if ([CV]::IsWindowVisible($p)) {
  $b=New-Object System.Drawing.Bitmap ($r.R-$r.L),($r.B-$r.T); $g=[System.Drawing.Graphics]::FromImage($b); $g.CopyFromScreen($r.L,$r.T,0,0,$b.Size); $b.Save((Join-Path $args[0] 'snap\emoji-live.png'))
  [void][CV]::PostMessage($p,0x8000+66,[IntPtr]0x0D,[IntPtr]::Zero); Pump 500
  [void][CV]::PostMessage($p,0x8000+66,[IntPtr]0x27,[IntPtr]::Zero); [void][CV]::PostMessage($p,0x8000+66,[IntPtr]0x0D,[IntPtr]::Zero); Pump 600
  "typed into the text box: " + (($box.Text.ToCharArray() | ForEach-Object { '{0:X4}' -f [int]$_ }) -join ' ') + " (expect D83D DE00 D83D DE01 = the first two faces)"
  "panel still open for more: " + [CV]::IsWindowVisible($p)
  [void][CV]::PostMessage($p,0x8000+66,[IntPtr]0x1B,[IntPtr]::Zero); Pump 400
  "closed: " + (-not [CV]::IsWindowVisible($p))
}
$form.Close(); $form.Dispose(); Pump 300

# Does switching with Alt+Tab change a window's size, position or maximised state?
# Two script-owned windows (one maximised, one 800x600); real Alt+Tab presses; compare before/after.
Add-Type -AssemblyName System.Windows.Forms
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class AS {
 [DllImport("user32.dll")] public static extern void keybd_event(byte k, byte s, uint f, UIntPtr e);
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
 public static void AltTab() { keybd_event(0x12,0,0,UIntPtr.Zero); System.Threading.Thread.Sleep(60); keybd_event(0x09,0,0,UIntPtr.Zero); System.Threading.Thread.Sleep(60); keybd_event(0x09,0,2,UIntPtr.Zero); System.Threading.Thread.Sleep(250); keybd_event(0x12,0,2,UIntPtr.Zero); }
}
'@
$a = New-Object Windows.Forms.Form -Property @{ Text = 'size test A (maximised)'; WindowState = 'Maximized' }
$b = New-Object Windows.Forms.Form -Property @{ Text = 'size test B'; StartPosition = 'Manual'; Left = 100; Top = 100; Width = 800; Height = 600 }
$a.Show(); $b.Show(); [Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 500
[void][AS]::SetForegroundWindow($b.Handle); [Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 300
$state = { "A: $($a.WindowState) $($a.Bounds.X),$($a.Bounds.Y) $($a.Bounds.Width)x$($a.Bounds.Height) | B: $($b.WindowState) $($b.Bounds.X),$($b.Bounds.Y) $($b.Bounds.Width)x$($b.Bounds.Height)" }
$before = & $state; "before:  $before"
foreach ($i in 1..4) {
  [AS]::AltTab(); foreach ($k in 1..8) { [Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 100 }
  $fg = [AS]::GetForegroundWindow(); $who = if ($fg -eq $a.Handle) { 'A' } elseif ($fg -eq $b.Handle) { 'B' } else { 'other' }
  $now = & $state; "switch $i -> front: $who; same as before: $($now -eq $before)"; if ($now -ne $before) { "         $now" }
}
$a.Close(); $b.Close()

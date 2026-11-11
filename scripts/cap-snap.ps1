# Capture Windows' own Win+Z snap picker for comparison: Rust UI is stopped for a few seconds so the
# key reaches Windows, a Notepad window is the target, and closing Notepad dismisses the picker.
# Then the same with Rust UI running. No Esc/Enter is sent.
param($outDir)
Add-Type -AssemblyName System.Drawing, System.Windows.Forms
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class CS { [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int w, int hh, uint f);
 [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
 public static void WinZ(){ keybd_event(0x5B,0,0,UIntPtr.Zero); keybd_event(0x5A,0,0,UIntPtr.Zero); keybd_event(0x5A,0,2,UIntPtr.Zero); keybd_event(0x5B,0,2,UIntPtr.Zero); } }
'@
function Shot($name) {
  $b=New-Object System.Drawing.Bitmap 1000,640; $g=[System.Drawing.Graphics]::FromImage($b); $g.CopyFromScreen(200,120,0,0,$b.Size); $b.Save((Join-Path $outDir $name))
}
function Run($name) {
  # the target is a plain window this script owns (Notepad is not installed on this PC)
  $form=New-Object System.Windows.Forms.Form; $form.Text='snap test'; $form.StartPosition='Manual'; $form.SetBounds(200,120,1000,640); $form.BackColor=[System.Drawing.Color]::FromArgb(32,32,32)
  $form.Show(); $form.Activate(); [System.Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 700
  [void][CS]::SetForegroundWindow($form.Handle); [System.Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 300
  "$name target in front: " + ([CS]::GetForegroundWindow() -eq $form.Handle)
  if ([CS]::GetForegroundWindow() -eq $form.Handle) {
    [CS]::WinZ(); for ($i=0; $i -lt 12; $i++) { [System.Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 100 }
    Shot $name
  }
  $form.Close(); $form.Dispose(); [System.Windows.Forms.Application]::DoEvents(); Start-Sleep 1
}
Stop-Process -Name shellbits -Force -ErrorAction SilentlyContinue; Start-Sleep 1
Run 'snap-native.png'
Start-ScheduledTask shellbits; Start-Sleep 5
Run 'snap-rust.png'
"Rust UI running again: " + [bool](Get-Process shellbits -ErrorAction SilentlyContinue)

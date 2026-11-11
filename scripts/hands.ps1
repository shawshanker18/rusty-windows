# Use the PC the way a person does: real mouse and keyboard, then look.
#   hands.ps1 "click 960 1056; wait 600; shot start; type calc; wait 500; shot typed"
# Actions: click x y | rclick x y | dbl x y | move x y | wheel x y n | key win | key win+e | type text |
#          wait ms | shot name [x y w h]   (pictures go to shellbits\snap\hands\<name>.png, half size when full screen)
# Esc and Enter are refused on purpose (they would act on whatever has the keyboard).
param([string]$Do)
Add-Type -AssemblyName System.Drawing, System.Windows.Forms
Add-Type -ReferencedAssemblies System.Drawing @'
using System; using System.Runtime.InteropServices; using System.Threading;
public static class Hd {
 [DllImport("user32.dll")] public static extern void keybd_event(byte k, byte s, uint f, UIntPtr e);
 [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint x, uint y, int d, UIntPtr e);
 [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
 [DllImport("user32.dll")] public static extern short VkKeyScan(char c);
 [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, System.Text.StringBuilder s, int n);
 [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(System.Drawing.Point p);
 [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
 // is the window under this point one of ours (any window of a shellbits process, menus included)?
 public static bool Ours(int x, int y) { uint pid; GetWindowThreadProcessId(WindowFromPoint(new System.Drawing.Point(x, y)), out pid); try { return System.Diagnostics.Process.GetProcessById((int)pid).ProcessName.ToLower() == "shellbits"; } catch { return false; } }
 public static string Front() { var s = new System.Text.StringBuilder(128); GetClassName(GetForegroundWindow(), s, 128); return s.ToString(); }
 public static void Btn(uint down, uint up) { mouse_event(down,0,0,0,UIntPtr.Zero); Thread.Sleep(70); mouse_event(up,0,0,0,UIntPtr.Zero); }
 public static void Chord(byte[] ks) { foreach (var k in ks) { keybd_event(k,0,0,UIntPtr.Zero); Thread.Sleep(40); } for (int i=ks.Length-1;i>=0;i--) { keybd_event(ks[i],0,2,UIntPtr.Zero); Thread.Sleep(40); } }
 public static void Type(string s) { foreach (char c in s) { short v = VkKeyScan(c); byte k = (byte)(v & 0xFF); bool sh = (v & 0x100) != 0; if (sh) keybd_event(0x10,0,0,UIntPtr.Zero); keybd_event(k,0,0,UIntPtr.Zero); keybd_event(k,0,2,UIntPtr.Zero); if (sh) keybd_event(0x10,0,2,UIntPtr.Zero); Thread.Sleep(45); } }
}
'@
[void][Hd]::SetProcessDPIAware()
$out = "$PSScriptRoot\..\snap\hands"; New-Item -ItemType Directory -Force $out | Out-Null
$vk = @{ win = 0x5B; ctrl = 0x11; alt = 0x12; shift = 0x10; tab = 0x09; back = 0x08; del = 0x2E; up = 0x26; down = 0x28; left = 0x25; right = 0x27; f2 = 0x71; f5 = 0x74; space = 0x20; home = 0x24; end = 0x23 }
foreach ($a in ($Do -split ';')) {
  $p = $a.Trim() -split '\s+', 2; $cmd = $p[0]; $rest = if ($p.Count -gt 1) { $p[1] } else { '' }
  $n = $rest -split '\s+'
  switch ($cmd) {
    'click'  { if (-not [Hd]::Ours([int]$n[0], [int]$n[1])) { throw "not clicking $($n[0]),$($n[1]): that is not a Rust window" }; [void][Hd]::SetCursorPos([int]$n[0], [int]$n[1]); Start-Sleep -Milliseconds 140; [Hd]::Btn(2, 4) }
    'rclick' { if (-not [Hd]::Ours([int]$n[0], [int]$n[1])) { throw "not clicking $($n[0]),$($n[1]): that is not a Rust window" }; [void][Hd]::SetCursorPos([int]$n[0], [int]$n[1]); Start-Sleep -Milliseconds 140; [Hd]::Btn(8, 16) }
    'dbl'    { if (-not [Hd]::Ours([int]$n[0], [int]$n[1])) { throw "not clicking $($n[0]),$($n[1]): that is not a Rust window" }; [void][Hd]::SetCursorPos([int]$n[0], [int]$n[1]); Start-Sleep -Milliseconds 140; [Hd]::Btn(2, 4); Start-Sleep -Milliseconds 90; [Hd]::Btn(2, 4) }
    'move'   { [void][Hd]::SetCursorPos([int]$n[0], [int]$n[1]) }
    'wheel'  { [void][Hd]::SetCursorPos([int]$n[0], [int]$n[1]); Start-Sleep -Milliseconds 100; [Hd]::mouse_event(0x800, 0, 0, 120 * [int]$n[2], [UIntPtr]::Zero) }
    'wait'   { Start-Sleep -Milliseconds ([int]$n[0]) }
    # typing and keys only ever go to OUR windows: stop at once if something else has the keyboard
    'type'   { if ([Hd]::Front() -notlike 'shellbits_*') { throw "not typing: '$([Hd]::Front())' has the keyboard" }; [Hd]::Type($rest) }
    'expect' { if ([Hd]::Front() -notlike $n[0]) { throw "expected $($n[0]) in front, found '$([Hd]::Front())'" } }
    'front'  { "in front: $([Hd]::Front())" }
    'key'    {
      $ks = foreach ($k in ($rest.ToLower() -split '\+')) { if ($k -in 'esc', 'escape', 'enter', 'return') { throw "refused key: $k" } elseif ($vk.ContainsKey($k)) { [byte]$vk[$k] } elseif ($k.Length -eq 1) { [byte]([Hd]::VkKeyScan($k[0]) -band 0xFF) } else { throw "unknown key: $k" } }
      [Hd]::Chord([byte[]]$ks)
    }
    'shot'   {
      $b = [Windows.Forms.Screen]::PrimaryScreen.Bounds
      if ($n.Count -ge 5) { $x, $y, $w, $h = [int]$n[1], [int]$n[2], [int]$n[3], [int]$n[4]; $half = $false } else { $x, $y, $w, $h = 0, 0, $b.Width, $b.Height; $half = $true }
      $bmp = New-Object Drawing.Bitmap $w, $h; $g = [Drawing.Graphics]::FromImage($bmp); $g.CopyFromScreen($x, $y, 0, 0, $bmp.Size); $g.Dispose()
      if ($half) { $s = New-Object Drawing.Bitmap $bmp, ([int]($w / 2)), ([int]($h / 2)); $bmp.Dispose(); $bmp = $s }
      $bmp.Save("$out\$($n[0]).png"); $bmp.Dispose()
    }
    '' { }
    default { throw "unknown action: $cmd" }
  }
}
'done'

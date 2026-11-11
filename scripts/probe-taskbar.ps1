# Read-only check of Explorer's taskbar: its windows, what UI Automation can see in it, recent app errors.
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$A=[System.Windows.Automation.AutomationElement]
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class PT { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, IntPtr t);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr p, IntPtr a, string c, IntPtr t);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r); public struct RECT { public int L,T,R,B; }
 [DllImport("user32.dll")] public static extern bool IsHungAppWindow(IntPtr h); }
'@
$t=[PT]::FindWindow('Shell_TrayWnd',[IntPtr]::Zero)
$r=New-Object PT+RECT; [void][PT]::GetWindowRect($t,[ref]$r)
"taskbar window $t at $($r.L),$($r.T)-$($r.R),$($r.B) visible $([PT]::IsWindowVisible($t)) hung $([PT]::IsHungAppWindow($t))"
$x=[PT]::FindWindowEx($t,[IntPtr]::Zero,'Windows.UI.Composition.DesktopWindowContentBridge',[IntPtr]::Zero)
"content bridge $x visible $([PT]::IsWindowVisible($x))"
$all=[System.Windows.Automation.Condition]::TrueCondition
"UIA descendants of the taskbar: " + $A::FromHandle($t).FindAll([System.Windows.Automation.TreeScope]::Descendants,$all).Count
if ($x -ne [IntPtr]::Zero) { "UIA descendants of the bridge: " + $A::FromHandle($x).FindAll([System.Windows.Automation.TreeScope]::Descendants,$all).Count }
$raw=[System.Windows.Automation.TreeWalker]::RawViewWalker; $c=$raw.GetFirstChild($A::FromHandle($t)); $n=0; while ($c -and $n -lt 10) { "  raw child: $($c.Current.ClassName) | $($c.Current.Name)"; $c=$raw.GetNextSibling($c); $n++ }
Get-WinEvent -FilterHashtable @{LogName='Application'; Level=2; StartTime=(Get-Date).AddMinutes(-45)} -MaxEvents 6 -ErrorAction SilentlyContinue | ForEach-Object { "$($_.TimeCreated) $($_.ProviderName): " + ($_.Message -split "`n")[0] }
Add-Type -AssemblyName WindowsBase
foreach ($x in 780,900,1000,1700,1850) { try { $e=$A::FromPoint((New-Object System.Windows.Point $x,1056)); "at $x,1056: $($e.Current.ClassName) | $($e.Current.AutomationId) | $($e.Current.Name)" } catch { "at ${x}: $($_.Exception.Message)" } }

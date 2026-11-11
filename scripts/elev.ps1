Add-Type @'
using System; using System.Runtime.InteropServices;
public static class EL { [DllImport("kernel32.dll", SetLastError=true)] public static extern IntPtr OpenProcess(uint a, bool i, int pid);
 [DllImport("advapi32.dll", SetLastError=true)] public static extern bool OpenProcessToken(IntPtr p, uint a, out IntPtr t);
 [DllImport("advapi32.dll", SetLastError=true)] public static extern bool GetTokenInformation(IntPtr t, int c, out int v, int n, out int r);
 [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
 public static string Elevated(int pid){ IntPtr p=OpenProcess(0x1000,false,pid); if(p==IntPtr.Zero) return "cannot open (likely elevated or protected)"; IntPtr t; if(!OpenProcessToken(p,8,out t)){ CloseHandle(p); return "cannot read token (likely elevated)"; } int v,r; GetTokenInformation(t,20,out v,4,out r); CloseHandle(t); CloseHandle(p); return v!=0?"ELEVATED":"normal"; } }
'@
Get-Process | Where-Object { $_.MainWindowHandle -ne 0 -and $_.MainWindowTitle } | ForEach-Object { "{0} [{1}]: {2}" -f $_.Name, $_.MainWindowTitle.Substring(0,[Math]::Min(25,$_.MainWindowTitle.Length)), [EL]::Elevated($_.Id) }

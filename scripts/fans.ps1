# MSI Cooler Boost (fans to max) through MSI's own firmware WMI interface (what MSI Center uses).
# Needs admin, so shellbits runs it via the "shellbits-fans-on/off" scheduled tasks.
# Reads embedded-controller register 0x98 first, flips only bit 7 (Cooler Boost), reads it back,
# and writes the result to fans-state.txt next to this script.
param([ValidateSet('on','off','status')] [string]$Mode = 'status')
$ErrorActionPreference = 'Stop'
$state = Join-Path $PSScriptRoot 'fans-state.txt'
$REG = 0x98
function Pkg([byte[]]$b) { New-CimInstance -Namespace root/wmi -ClassName Package_32 -ClientOnly -Property @{ Bytes = [byte[]]($b + (,0 * (32 - $b.Length))) } }
try {
  $acpi = Get-CimInstance -Namespace root/wmi -ClassName MSI_ACPI | Select-Object -First 1
  $get = { (Invoke-CimMethod -InputObject $acpi -MethodName Get_Data -Arguments @{ Data = (Pkg @($REG)) }).Data.Bytes }
  $r = & $get
  if ($r[0] -ne 1) { throw "unexpected status from Get_Data: $($r[0..3] -join ',')" }   # 1 = success
  $cur = [int]$r[1]
  if ($Mode -ne 'status') {
    $new = if ($Mode -eq 'on') { $cur -bor 0x80 } else { $cur -band 0x7F }
    if ($new -ne $cur) {
      [void](Invoke-CimMethod -InputObject $acpi -MethodName Set_Data -Arguments @{ Data = (Pkg @($REG, [byte]$new)) })
    }
    $cur = [int](& $get)[1]
  }
  Set-Content -Path $state -Value $(if ($cur -band 0x80) { 'on' } else { 'off' }) -Encoding ascii
} catch {
  Set-Content -Path $state -Value ("error: " + $_.Exception.Message) -Encoding ascii
}

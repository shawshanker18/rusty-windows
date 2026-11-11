# How late would Rust UI notice a new notification? Starts the timing self-test, posts one test
# notification 8 s in, and prints when it was posted next to when the list / the store on disk changed.
$out = "$env:TEMP\shellbits-toast-selftest.txt"; Remove-Item $out -ErrorAction SilentlyContinue
$p = Start-Process "$PSScriptRoot\..\target\release\shellbits.exe" -ArgumentList '--toast-selftest', $out -PassThru
Start-Sleep 32
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType=WindowsRuntime] | Out-Null
$xml = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02)
$tx = $xml.GetElementsByTagName('text'); [void]$tx.Item(0).AppendChild($xml.CreateTextNode('Rust UI test')); [void]$tx.Item(1).AppendChild($xml.CreateTextNode('Timing check'))
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe').Show([Windows.UI.Notifications.ToastNotification]::new($xml))
$t = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds(); "{0}.{1:000} notification posted" -f ([math]::Floor($t/1000) % 100000), ($t % 1000)
$p.WaitForExit(40000) | Out-Null
Get-Content $out

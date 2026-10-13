# As administrator: removes a KilatX-Test installation left by an interrupted installer test,
# including a protected folder whose permissions went wrong.
$ErrorActionPreference = 'Continue'
$work = Join-Path $env:TEMP 'kilatx-install-test'
$appKey = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{02B69550-DBD6-419F-A029-44E057A0D633}_is1'
$dirs = @((Get-ItemProperty $appKey -ErrorAction SilentlyContinue).InstallLocation, "$env:ProgramFiles\KilatX-Test") + @(Get-ChildItem $work -Directory -Filter 'Prog *' -ErrorAction SilentlyContinue | ForEach-Object { $_.FullName })
foreach ($d in $dirs | Where-Object { $_ -and (Test-Path $_) }) {
    takeown /f "$d" /r /d y | Out-Null
    icacls "$d" /reset /t /c /q | Out-Null
    $u = Get-ChildItem $d -Recurse -Filter unins000.exe | Select-Object -First 1
    if ($u) { $p = Start-Process $u.FullName -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -PassThru; $p.WaitForExit(); Start-Sleep -Seconds 5 }
}
if (Test-Path $appKey) { Remove-Item $appKey -Force }
foreach ($l in "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\KilatX-Test.lnk", "$env:PUBLIC\Desktop\KilatX-Test.lnk") { if (Test-Path $l) { Remove-Item $l -Force } }
if (Test-Path $work) { takeown /f "$work" /r /d y | Out-Null; icacls "$work" /reset /t /c /q | Out-Null; cmd /c "rmdir /s /q `"$work`"" }
if (Test-Path "$env:ProgramFiles\KilatX-Test") { cmd /c "rmdir /s /q `"$env:ProgramFiles\KilatX-Test`"" }
"left: work=$(Test-Path $work) key=$(Test-Path $appKey) pf=$(Test-Path "$env:ProgramFiles\KilatX-Test")" | Set-Content "$env:TEMP\kilatx-install-reset.txt"

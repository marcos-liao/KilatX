param([string]$Work)
# Shared by installer_test.ps1 (signed-in user) and installer_test_admin.ps1 (administrator).
$env:KILATX_INSTALL_TEST = '1'          # the 0.4 per-user setup and KilatX 0.4 use "KilatX-Test"
$env:KILATX_INSTANCE = '-install-test'  # own single-instance name
$env:KILATX_DATA_DIR = "$Work\appdata"
$env:KILATX_EVENT_LOG = "$Work\events.log"
$pfDir = "$env:ProgramFiles\KilatX-Test"
$pfExe = "$pfDir\KilatX.exe"
$appKey = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{02B69550-DBD6-419F-A029-44E057A0D633}_is1'
$legacyKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\KilatX-Test'
$startLnk = "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\KilatX-Test.lnk"
$desktopLnk = "$env:PUBLIC\Desktop\KilatX-Test.lnk"
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$hostBrowsers = 'Google\Chrome', 'Microsoft\Edge', 'BraveSoftware\Brave-Browser', 'Vivaldi', 'Chromium'
$results = @()

# Tests never contact the real update server (KilatX's updater test sets its own address).
$env:KILATX_UPDATE_URL = 'http://127.0.0.1:9/no-update-server'

# Runs a program and returns its exit code. (Start-Process -Wait would also wait for the
# KilatX that setup starts at the end.)
function Run([string]$file, [string[]]$arguments = @(), [switch]$Hidden) {
    $quoted = @($arguments | ForEach-Object { if ($_ -match ' ' -and $_ -notmatch '"') { "`"$_`"" } else { $_ } })
    $opts = @{ FilePath = $file; PassThru = $true }
    if ($quoted.Count) { $opts.ArgumentList = $quoted }
    if ($Hidden) { $opts.WindowStyle = 'Hidden' }
    $p = Start-Process @opts
    $null = $p.Handle  # keeps the exit code readable
    $p.WaitForExit()
    $p.ExitCode
}
function Check($name, $ok, $detail = '') { $script:results += [pscustomobject]@{ Test = $name; Result = $(if ($ok) { 'PASS' } else { 'FAIL' }); Detail = $(if ($ok) { '' } else { "$detail" }) } }
function Procs { @(Get-Process KilatX -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$Work*" -or $_.Path -like "$pfDir*" }) }
function Wait-Until([scriptblock]$cond, [int]$sec = 15) { $t = Get-Date; while (((Get-Date) - $t).TotalSeconds -lt $sec) { if (& $cond) { return $true }; Start-Sleep -Milliseconds 250 }; return [bool](& $cond) }
function Events($kind) { @(Get-Content $env:KILATX_EVENT_LOG -ErrorAction SilentlyContinue | Where-Object { $_.StartsWith("$kind`t") }) }
function Items { @((Get-Content "$env:KILATX_DATA_DIR\state.json" -Raw -ErrorAction SilentlyContinue | ConvertFrom-Json).items) }
function LnkTarget($p) { if (Test-Path $p) { (New-Object -ComObject WScript.Shell).CreateShortcut($p).TargetPath } }
function LnkAumid($p) { $f = (New-Object -ComObject Shell.Application).NameSpace((Split-Path $p)).ParseName((Split-Path $p -Leaf)); $f.ExtendedProperty('System.AppUserModel.ID') }
function HostKeys { foreach ($k in $hostBrowsers) { (Get-ItemProperty "HKCU:\Software\$k\NativeMessagingHosts\app.kilatx.host" -ErrorAction SilentlyContinue).'(default)' } }
# Machine-wide browser registration (written by setup as administrator), both registry views
$machineKeys = foreach ($k in $hostBrowsers) { "HKLM:\SOFTWARE\$k\NativeMessagingHosts\app.kilatx.host"; "HKLM:\SOFTWARE\WOW6432Node\$k\NativeMessagingHosts\app.kilatx.host" }
function MachineHostKeys { foreach ($k in $machineKeys) { (Get-ItemProperty $k -ErrorAction SilentlyContinue).'(default)' } }
function RunEntry { (Get-ItemProperty $runKey -ErrorAction SilentlyContinue).KilatX }
function Hash($p) { (Get-FileHash $p).Hash }

# Never leaves test copies registered in Windows or running.
function Cleanup-Test {
    Get-Process KilatX, 'KilatX-Setup*', 'unins*', '_iu*' -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$Work*" -or $_.Path -like "$pfDir*" -or $_.Path -like "$env:TEMP*" } | Stop-Process -Force -ErrorAction SilentlyContinue
    foreach ($k in $hostBrowsers) {
        $key = "HKCU:\Software\$k\NativeMessagingHosts\app.kilatx.host"
        if ((Get-ItemProperty $key -ErrorAction SilentlyContinue).'(default)' -like "$Work*") { Remove-Item $key -Force }
    }
    $run = RunEntry
    if ($run -and ($run -like "*$Work*" -or $run -like "*$pfDir*")) { Remove-ItemProperty $runKey -Name KilatX }
}

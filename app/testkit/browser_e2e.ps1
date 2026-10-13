param([string]$Exe = "$PSScriptRoot\..\target\debug\KilatX.exe", [string]$Base = 'https://127.0.0.1:8780', [string]$Only = '')
# Real browsers (Chrome, Edge, Brave if installed) with the KilatX extension, end to end.
# Uses a portable KilatX copy in %TEMP% with its own single-instance name (debug build),
# registers it as the browsers' native host for the run, and removes that again at the end.
$ErrorActionPreference = 'Stop'
$work = Join-Path $env:TEMP 'kilatx-browser-e2e'
$app = "$work\app\KilatX.exe"
$data = "$work\app\data"
$env:KILATX_INSTANCE = '-browser-e2e'
$env:KILATX_EVENT_LOG = "$work\events.log"
# Debug build only: accept the test server's throwaway HTTPS certificate. HTTPS because
# another download manager, if installed, may intercept plain-HTTP downloads inside the browser process.
$env:KILATX_TEST_INSECURE_TLS = '1'

function Procs { @(Get-Process KilatX -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $app }) }
function Kill-All { Procs | Stop-Process -Force; Start-Sleep -Milliseconds 700 }

$browsers = @(
    "$env:ProgramFiles\Google\Chrome\Application\chrome.exe",
    "${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe",
    "$env:LOCALAPPDATA\BraveSoftware\Brave-Browser\Application\brave.exe",
    "$env:ProgramFiles\BraveSoftware\Brave-Browser\Application\brave.exe"
) | Where-Object { (Test-Path $_) -and ($Only -eq '' -or $_ -like "*$Only*") }

$all = @()
foreach ($browser in $browsers) {
    # Fresh KilatX data for every browser, downloads into the work folder, no dialog.
    Kill-All
    if (Test-Path $work) { Get-ChildItem $work -Recurse -Force | Sort-Object FullName -Descending | ForEach-Object { $_.Delete() } }
    New-Item -ItemType Directory -Force $data, "$work\dl" | Out-Null
    Copy-Item $Exe $app
    # A Site login only for one folder of the test server, password encrypted like KilatX does (DPAPI).
    Add-Type -AssemblyName System.Security
    $blob = [Security.Cryptography.ProtectedData]::Protect([Text.Encoding]::UTF8.GetBytes('pass'), $null, 'CurrentUser')
    $hex = ($blob | ForEach-Object { $_.ToString('x2') }) -join ''
    $login = @{ pattern = '127.0.0.1/auth/1500000/'; username = 'user'; password = "dpapi:$hex" }
    $json = @{ settings = @{ connections = 8; browser_dialog = $false; download = @{ base_folder = "$work\dl"; logins = @($login) } } } | ConvertTo-Json -Depth 6
    [IO.File]::WriteAllText("$data\state.json", $json, (New-Object Text.UTF8Encoding($false)))
    # One start registers this copy as the native host (and writes the extension folder).
    $p = Start-Process $app -ArgumentList '/tray' -PassThru; Start-Sleep -Seconds 3
    Kill-All
    $json = node "$PSScriptRoot\browser_e2e.mjs" $browser "$data\browser\extension" $Base $data "$work\events.log" | Select-Object -Last 1
    $all += $json | ConvertFrom-Json
    Kill-All
}

# Leave no registration pointing at the temporary copy.
foreach ($k in 'Google\Chrome', 'Microsoft\Edge', 'BraveSoftware\Brave-Browser', 'Vivaldi', 'Chromium') {
    Remove-Item "HKCU:\Software\$k\NativeMessagingHosts\app.kilatx.host" -Recurse -ErrorAction SilentlyContinue
}

$all | ForEach-Object { [pscustomobject]@{ Test = $_.n; Result = $(if ($_.ok) { 'PASS' } else { 'FAIL' }); Detail = $_.detail } } |
    Format-Table -AutoSize -Wrap | Out-String -Width 220
"{0}/{1} passed" -f @($all | Where-Object ok).Count, $all.Count

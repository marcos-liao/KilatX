param(
    [string]$Base = 'http://127.0.0.1:8766'
)
# KilatX Setup (Inno Setup) end to end, with the debug build. Needs the scenario server
# (testkit\scenario_server.py 8766 8767) and Inno Setup 6.
#
# Part 1 runs as the signed-in user: setup asks Windows for administrator permission (one UAC
# prompt), installs, and must start KilatX again WITHOUT administrator rights.
# Part 2 (installer_test_admin.ps1) runs as administrator (a second UAC prompt): migration from
# 0.4, updates, locked files, KilatX's own updater, refused folders, custom folder protection,
# planted DLLs, uninstall.
#
# Test mode keeps everything apart from a real installation: product "KilatX-Test" (own AppId,
# folder, shortcuts, Installed-apps entry), own single-instance name, temporary data folder.
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path "$PSScriptRoot\..\..").Path
$app = "$root\app"
$work = Join-Path $env:TEMP 'kilatx-install-test'
# A test copy left running from an earlier run would keep files in use.
Get-Process KilatX -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$work*" -or $_.Path -like "$env:ProgramFiles\KilatX-Test*" } | Stop-Process -Force
if (Test-Path $work) { cmd /c "rmdir /s /q `"$work`"" 2>&1 | Out-Null }
if (Test-Path $work) { throw "$work could not be removed (a protected folder from an interrupted run?): remove it as administrator, and uninstall KilatX-Test from Settings › Apps if it is listed." }
if (Test-Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{02B69550-DBD6-419F-A029-44E057A0D633}_is1') { throw 'KilatX-Test is still installed from an interrupted run: uninstall it from Settings › Apps first.' }
New-Item -ItemType Directory -Force $work | Out-Null
. "$PSScriptRoot\installer_test_common.ps1" -Work $work

# Test setups: 0.5.0 = the debug build, 0.5.1 = the same exe with a marker appended (a
# different file, so a replaced exe can be told apart).
$iscc = "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
New-Item -ItemType Directory -Force "$work\payload-0.5.1", "$work\setups", "$work\web" | Out-Null
Copy-Item "$app\target\debug\KilatX.exe" "$work\payload-0.5.1\KilatX.exe"
[IO.File]::AppendAllText("$work\payload-0.5.1\KilatX.exe", 'KILATX-TEST-PAYLOAD-0.5.1')
foreach ($b in @(@('0.5.0', "$app\target\debug"), @('0.5.1', "$work\payload-0.5.1"))) {
    & $iscc /Q /DTest "/DAppVersion=$($b[0])" "/DPayloadDir=$($b[1])" "/DOutDir=$work\setups" "$root\setup\KilatX.iss"
    if ($LASTEXITCODE) { throw "ISCC failed for $($b[0])" }
    & "$env:USERPROFILE\.dotnet\tools\wix.exe" build "$root\setup\KilatX.wxs" -arch x64 -d Test=1 -d "Version=$($b[0])" -d "PayloadDir=$($b[1])" -o "$work\setups\KilatX-Test-$($b[0])-x64.msi"
    if ($LASTEXITCODE) { throw "WiX failed for $($b[0])" }
}

# Update server content for KilatX's updater (signed with the real update key).
$setup051 = "$work\setups\KilatX-Setup-0.5.1-x64.exe"
Copy-Item $setup051 "$work\web\KilatX-Setup-0.5.1-x64.exe"
Set-Content "$work\web\notes.txt" "Test release`nwith two lines" -Encoding ASCII
[IO.File]::WriteAllBytes("$work\web\damaged.exe", [byte[]](1..200))
$sign = "$app\target\debug\sign-update.exe"
$key = "$app\update-signing\update.key"
& $sign sign $key 0.5.1 "$work\web\notes.txt" 'https://example.invalid/download' "$work\web\update.json" --file x64 'http://127.0.0.1:8790/KilatX-Setup-0.5.1-x64.exe' $setup051 | Out-Null
& $sign sign $key 0.5.1 "$work\web\notes.txt" 'https://example.invalid/download' "$work\web\update-damaged.json" --file x64 'http://127.0.0.1:8790/damaged.exe' $setup051 | Out-Null
(Get-Content "$work\web\update.json" -Raw).Replace('"0.5.1"', '"0.5.2"') | Set-Content "$work\web\update-forged.json" -Encoding ASCII -NoNewline

# A real installation's machine-wide browser registration is put back at the end (part 2).
$backup = @{}; foreach ($k in $machineKeys) { $v = (Get-ItemProperty $k -ErrorAction SilentlyContinue).'(default)'; if ($v) { $backup[$k] = $v } }
$backup | ConvertTo-Json | Set-Content "$work\machine-keys-backup.json"

# ---- Part 1: as the signed-in user ----
$setup = "$work\setups\KilatX-Setup-0.5.0-x64.exe"
$code = Run $setup @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/launch', "/LOG=`"$work\setup-part1.log`"")
Check 'User install (UAC): exit code 0' ($code -eq 0) "exit $code"
Check 'Installed in Program Files' (Test-Path $pfExe)
Check 'Started again after install' (Wait-Until { (Procs | Where-Object Path -eq $pfExe).Count -eq 1 } 30)
Check 'Started WITHOUT administrator rights' (Wait-Until { (Events 'start' | Where-Object { $_ -like '*elevated=0' }).Count -ge 1 } 15) ((Events 'start') -join ' | ')
Check 'Per-user setup ran as the user (browser connection)' (Wait-Until { @(HostKeys | Where-Object { $_ -like "$env:KILATX_DATA_DIR*" }).Count -eq 5 } 10) ((HostKeys) -join ' | ')
Check 'Browsers of all users connected too (machine-wide, both registry views)' (@(MachineHostKeys | Where-Object { $_ -eq "$pfDir\app.kilatx.host.json" }).Count -eq 10) ((MachineHostKeys) -join ' | ')

# ---- Part 2: as administrator ----
$results | ConvertTo-Json | Set-Content "$work\results-part1.json"
$admin = Start-Process powershell -Verb RunAs -PassThru -ArgumentList '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSScriptRoot\installer_test_admin.ps1`"", '-Work', "`"$work`"", '-Base', $Base
# While it runs: when it asks (policy-ready.flag), drive real Chrome and Edge as this user under
# the company policy it set, and hand the results back.
while (-not $admin.HasExited) {
    $phase = Get-Content "$work\policy-ready.flag" -ErrorAction SilentlyContinue
    if ($phase -and -not (Test-Path "$work\policy-results-$phase.json")) {
        $env:KILATX_TEST_INSECURE_TLS = '1'
        $policyResults = @()
        $browsersToRun = @("$env:ProgramFiles\Google\Chrome\Application\chrome.exe", "${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe") | Where-Object { Test-Path $_ }
        if ($phase -eq 'control') { $browsersToRun = @($browsersToRun | Select-Object -First 1) }
        foreach ($b in $browsersToRun) {
            $out = node "$PSScriptRoot\browser_e2e.mjs" $b "$env:KILATX_DATA_DIR\browser\extension" 'https://127.0.0.1:8780' $env:KILATX_DATA_DIR $env:KILATX_EVENT_LOG | Select-Object -Last 1
            $policyResults += [pscustomobject]@{ browser = (Split-Path $b -Leaf); results = @($out | ConvertFrom-Json | ForEach-Object { $_ }) }
            Get-Process KilatX -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $pfExe } | Stop-Process -Force -ErrorAction SilentlyContinue
        }
        Remove-Item Env:KILATX_TEST_INSECURE_TLS
        ConvertTo-Json @($policyResults) -Depth 6 | Set-Content "$work\policy-results-$phase.json"
    }
    Start-Sleep -Milliseconds 500
}
if (Test-Path "$work\results-part2.json") {
    foreach ($r in (Get-Content "$work\results-part2.json" -Raw | ConvertFrom-Json)) { $results += $r }
} else {
    Check 'Administrator part ran' $false "exit $($admin.ExitCode); see $work\admin-transcript.txt"
}
Cleanup-Test
$results | Format-Table -AutoSize -Wrap | Out-String -Width 220
"{0}/{1} passed" -f @($results | Where-Object Result -eq 'PASS').Count, $results.Count

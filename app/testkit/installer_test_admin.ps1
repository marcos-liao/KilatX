param([string]$Work, [string]$Base = 'http://127.0.0.1:8766')
# Part 2 of installer_test.ps1, as administrator. Writes $Work\results-part2.json.
$ErrorActionPreference = 'Stop'
Start-Transcript "$Work\admin-transcript.txt" | Out-Null
try {
. "$PSScriptRoot\installer_test_common.ps1" -Work $Work
Add-Type -Namespace K -Name E -MemberDefinition '[DllImport("kernel32.dll")] public static extern uint SetErrorMode(uint mode);'; [K.E]::SetErrorMode(3) | Out-Null
$app = (Resolve-Path "$PSScriptRoot\..").Path
$setup050 = "$Work\setups\KilatX-Setup-0.5.0-x64.exe"
$setup051 = "$Work\setups\KilatX-Setup-0.5.1-x64.exe"
$hash050 = Hash "$app\target\debug\KilatX.exe"
$hash051 = Hash "$Work\payload-0.5.1\KilatX.exe"
$script:logNo = 0
function Setup([string]$exe, [string[]]$more = @()) {
    $script:logNo++
    Run $exe (@('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=`"$Work\setup-admin-$($script:logNo).log`"") + $more)
}
function Uninstall([string]$dir, [string[]]$more = @()) {
    $code = Run "$dir\unins000.exe" (@('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART') + $more)
    # The uninstaller hands over to a copy of itself in %TEMP% and returns at once: wait for it.
    Wait-Until { @(Get-Process '_iu*', 'unins*' -ErrorAction SilentlyContinue).Count -eq 0 } 60 | Out-Null
    $code
}
function Version { (Get-ItemProperty $appKey -ErrorAction SilentlyContinue).DisplayVersion }
function Write-State([hashtable]$settings) {
    # Settings only (no BOM): KilatX keeps the rest at its defaults.
    $s = @{ connections = 8 } + $settings
    New-Item -ItemType Directory -Force $env:KILATX_DATA_DIR | Out-Null
    $old = Get-Content "$env:KILATX_DATA_DIR\state.json" -Raw -ErrorAction SilentlyContinue | ConvertFrom-Json
    $json = @{ settings = $s; items = @(if ($old) { $old.items }); queues = @(if ($old) { $old.queues }) } | ConvertTo-Json -Depth 20
    [IO.File]::WriteAllText("$env:KILATX_DATA_DIR\state.json", $json, (New-Object Text.UTF8Encoding $false))
}
# True if anyone besides Windows itself and administrators may change files there.
function Users-Can-Write($dir) {
    $acl = Get-Acl $dir
    $write = [Security.AccessControl.FileSystemRights]'WriteData, AppendData, Delete, ChangePermissions, TakeOwnership'
    @($acl.Access | Where-Object {
        $_.AccessControlType -eq 'Allow' -and ($_.FileSystemRights -band $write) -and
        $_.IdentityReference -notmatch 'SYSTEM$|\\Administrators$|TrustedInstaller$|APPLICATION PACKAGE' -and
        # CREATOR OWNER applies only to files created later, which needs write access first
        -not ($_.IdentityReference -match 'CREATOR OWNER' -and $_.PropagationFlags -match 'InheritOnly')
    }).Count -gt 0
}

# 1. Uninstall while the (unelevated) KilatX from part 1 runs; settings kept
Check 'Part 1 install found' (Test-Path "$pfDir\unins000.exe")
$code = Uninstall $pfDir
Check 'Uninstall: exit code 0' ($code -eq 0) "exit $code"
Check 'Uninstall: running KilatX closed' (Wait-Until { (Procs).Count -eq 0 } 15) ((Procs).Path -join ', ')
Check 'Uninstall: program folder removed' (Wait-Until { -not (Test-Path $pfDir) } 15) ((Get-ChildItem $pfDir -ErrorAction SilentlyContinue).Name -join ', ')
Check 'Uninstall: Installed apps entry removed' (-not (Test-Path $appKey))
Check 'Uninstall: Start menu shortcut removed' (-not (Test-Path $startLnk))
Check 'Uninstall: browser connection removed' (@(HostKeys | Where-Object { $_ -like "$env:KILATX_DATA_DIR*" }).Count -eq 0) ((HostKeys) -join ' | ')
Check 'Uninstall: machine-wide browser connection removed' (@(MachineHostKeys | Where-Object { $_ -like "$pfDir*" }).Count -eq 0 -and -not (Test-Path "$pfDir\app.kilatx.host.json")) ((MachineHostKeys) -join ' | ')
Check 'Uninstall: per-user cleanup ran' ((Events 'cleanup-user').Count -ge 1)
Check 'Uninstall: settings and downloads kept (not asked to remove)' (Test-Path "$env:KILATX_DATA_DIR\state.json")

# 2. Migration from KilatX 0.4 (per-user in AppData) while it runs, start with Windows on
$legacyDir = "$Work\legacy\KilatX-Test"
$code = Run "$app\target\fixtures\KilatX-Setup-0.4.0-debug.exe" @('/S', "/D=$legacyDir")
Check '0.4 per-user install prepared' ($code -eq 0 -and (Test-Path $legacyKey)) "exit $code"
Write-State @{ start_with_windows = $true }
Start-Process "$legacyDir\KilatX.exe" -ArgumentList '/tray'
Wait-Until { (Procs | Where-Object Path -like "$legacyDir*").Count -eq 1 } | Out-Null
Run "$legacyDir\KilatX.exe" @('/d', "$Base/plain/1000000/kept-across-update.zip", '/a') | Out-Null
Check '0.4 running with a download and start with Windows' ((Wait-Until { (Items | Where-Object url -like '*kept-across-update*') } 20) -and (RunEntry) -like "*$legacyDir*") (RunEntry)
$code = Setup $setup050
Check 'Migration: exit code 0' ($code -eq 0) "exit $code"
Check 'Migration: running 0.4 closed' (Wait-Until { (Procs | Where-Object Path -like "$legacyDir*").Count -eq 0 } 10)
Check 'Migration: installed in Program Files' ((Test-Path $pfExe) -and (Hash $pfExe) -eq $hash050)
Check 'Migration: 0.4 Installed-apps entry removed' (-not (Test-Path $legacyKey))
Check 'Migration: 0.4 program files removed' (Wait-Until { -not (Test-Path "$legacyDir\KilatX.exe") } 20)
Check 'Migration: downloads list kept' ([bool](Items | Where-Object url -like '*kept-across-update*'))
Check 'Migration: start with Windows now starts the new KilatX' ((RunEntry) -like "*$pfExe*") (RunEntry)
Check 'Migration: browser connection kept' (@(HostKeys | Where-Object { $_ -like "$env:KILATX_DATA_DIR*" }).Count -eq 5) ((HostKeys) -join ' | ')
$manifest = Get-Content "$env:KILATX_DATA_DIR\browser\app.kilatx.host.json" -Raw -ErrorAction SilentlyContinue | ConvertFrom-Json
Check 'Migration: browser connection points at the new KilatX' ($manifest.path -eq $pfExe) $manifest.path

# 3. Install result: entry, shortcut, protection
$u = Get-ItemProperty $appKey -ErrorAction SilentlyContinue
Check 'Installed apps entry (machine-wide): name, version, folder' ($u.DisplayName -eq 'KilatX-Test' -and $u.DisplayVersion -eq '0.5.0' -and $u.InstallLocation -eq "$pfDir\") ($u | Out-String)
Check 'Start menu shortcut for all users, with the AppUserModelID' ((LnkTarget $startLnk) -eq $pfExe -and (LnkAumid $startLnk) -eq 'KilatX.KilatX') "$(LnkTarget $startLnk) $(LnkAumid $startLnk)"
Check 'No desktop shortcut unless chosen' (-not (Test-Path $desktopLnk))
Check 'License and notices installed' ((Test-Path "$pfDir\LICENSE.txt") -and (Test-Path "$pfDir\THIRD-PARTY-NOTICES.txt"))
Check 'Program Files folder: users cannot change it' (-not (Users-Can-Write $pfDir) -and -not (Users-Can-Write $pfExe)) ((Get-Acl $pfDir).AccessToString)
$m = Get-Content "$pfDir\app.kilatx.host.json" -Raw -ErrorAction SilentlyContinue | ConvertFrom-Json
Check 'Machine-wide browser connection: both registry views, manifest next to KilatX.exe' (@(MachineHostKeys | Where-Object { $_ -eq "$pfDir\app.kilatx.host.json" }).Count -eq 10 -and $m.path -eq $pfExe -and $m.allowed_origins -contains 'chrome-extension://mbnpaegbhgmmkfddjehcbmbdofgdlkok/') "$((MachineHostKeys) -join ' | ') / $($m.path)"

# 3b. Company PCs: a browser policy that ignores per-user hosts (NativeMessagingUserLevelHosts = 0).
# Real Chrome and Edge with the extension must still hand their downloads to KilatX through the
# machine-wide registration. The policies are put back as they were right after.
$policies = 'HKLM:\SOFTWARE\Policies\Google\Chrome', 'HKLM:\SOFTWARE\Policies\Microsoft\Edge'
$savedPolicy = @{}
foreach ($pk in $policies) {
    $savedPolicy[$pk] = (Get-ItemProperty $pk -ErrorAction SilentlyContinue).NativeMessagingUserLevelHosts
    New-Item -Path $pk -Force | Out-Null
    New-ItemProperty -Path $pk -Name NativeMessagingUserLevelHosts -Value 0 -PropertyType DWord -Force | Out-Null
}
try {
    Add-Type -AssemblyName System.Security
    $blob = [Security.Cryptography.ProtectedData]::Protect([Text.Encoding]::UTF8.GetBytes('pass'), $null, 'CurrentUser')
    $hex = ($blob | ForEach-Object { $_.ToString('x2') }) -join ''
    New-Item -ItemType Directory -Force "$Work\policy-dl" | Out-Null
    $old = Get-Content "$env:KILATX_DATA_DIR\state.json" -Raw | ConvertFrom-Json
    $state = @{ settings = @{ connections = 8; browser_dialog = $false; download = @{ base_folder = "$Work\policy-dl"; logins = @(@{ pattern = '127.0.0.1/auth/1500000/'; username = 'user'; password = "dpapi:$hex" }) } }; items = @($old.items) }
    [IO.File]::WriteAllText("$env:KILATX_DATA_DIR\state.json", ($state | ConvertTo-Json -Depth 8), (New-Object Text.UTF8Encoding $false))
    # One start writes the extension folder; then no per-user registration at all.
    Start-Process $pfExe -ArgumentList '/tray'
    Wait-Until { Test-Path "$env:KILATX_DATA_DIR\browser\extension\manifest.json" } 15 | Out-Null
    Run $pfExe @('/close') | Out-Null
    foreach ($k in $hostBrowsers) { Remove-Item "HKCU:\Software\$k\NativeMessagingHosts\app.kilatx.host" -Recurse -ErrorAction SilentlyContinue }
    # Browsers can't be driven as administrator: the signed-in user's part of the test (still
    # waiting in installer_test.ps1) runs them, like on a real company PC, and hands back the results.
    function Run-Browsers([string]$phase) {
        Remove-Item "$Work\policy-results-$phase.json" -ErrorAction SilentlyContinue
        Set-Content "$Work\policy-ready.flag" $phase
        $done = Wait-Until { Test-Path "$Work\policy-results-$phase.json" } 900
        Remove-Item "$Work\policy-ready.flag" -ErrorAction SilentlyContinue
        if (-not $done) { Check "Company policy ($phase): browsers run by the signed-in user" $false 'no results within 15 minutes' }
        @(Get-Content "$Work\policy-results-$phase.json" -Raw -ErrorAction SilentlyContinue | ConvertFrom-Json | ForEach-Object { $_ })
    }
    # Control: policy on, machine-wide registration taken away -> the browser must NOT reach KilatX
    # (otherwise the policy isn't in effect and the next check would prove nothing).
    $savedKeys = @{}; foreach ($k in $machineKeys) { $savedKeys[$k] = (Get-ItemProperty $k -ErrorAction SilentlyContinue).'(default)'; Remove-Item $k -ErrorAction SilentlyContinue }
    $ctl = Run-Browsers 'control'
    foreach ($k in $savedKeys.Keys) { if ($savedKeys[$k]) { New-Item -Path $k -Force | Out-Null; Set-ItemProperty -Path $k -Name '(default)' -Value $savedKeys[$k] } }
    $ctlResults = @($ctl | ForEach-Object { $_.results | ForEach-Object { $_ } })
    Check 'Control: with the policy and no machine-wide registration, the browser cannot reach KilatX' ($ctlResults.Count -gt 1 -and @($ctlResults | Where-Object { -not $_.ok }).Count -gt 0) "$(@($ctlResults | Where-Object { -not $_.ok }).Count) of $($ctlResults.Count) checks failed (some must)"
    foreach ($b in (Run-Browsers 'machine')) {
        # Windows PowerShell 5 hands a JSON array over as one object: unroll it. The last two checks
        # of browser_e2e.mjs remove the per-user registration and expect KilatX to be unreachable;
        # here the machine-wide one (the IT department's) rightly stays, so they don't apply.
        $r = @($b.results | ForEach-Object { $_ } | Where-Object { $_.n -notmatch 'once unregistered|without KilatX the browser downloads normally' })
        # A browser that couldn't be driven ("skipped") proves nothing here: counts as failed
        $failed = @($r | Where-Object { -not $_.ok -or $_.n -like '*skipped*' })
        Check "Company policy (machine-wide hosts only), $($b.browser): $($r.Count - $failed.Count)/$($r.Count) browser checks" ($r.Count -gt 1 -and $failed.Count -eq 0) (($failed | ForEach-Object { "$($_.n): $($_.detail)" }) -join ' | ')
    }
    Get-Process KilatX -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $pfExe } | Stop-Process -Force
}
finally {
    Remove-Item Env:KILATX_TEST_INSECURE_TLS -ErrorAction SilentlyContinue
    foreach ($pk in $policies) {
        if ($null -eq $savedPolicy[$pk]) { Remove-ItemProperty -Path $pk -Name NativeMessagingUserLevelHosts -ErrorAction SilentlyContinue }
        else { Set-ItemProperty -Path $pk -Name NativeMessagingUserLevelHosts -Value $savedPolicy[$pk] }
        # Remove the policy key again if the test created it empty
        if ((Test-Path $pk) -and -not (Get-Item $pk).Property -and -not (Get-ChildItem $pk)) { Remove-Item $pk }
    }
}
Check 'Browser policies put back as they were' (@($policies | Where-Object { (Get-ItemProperty $_ -ErrorAction SilentlyContinue).NativeMessagingUserLevelHosts -ne $savedPolicy[$_] }).Count -eq 0)
# Per-user connection back (as KilatX does on every start)
Run $pfExe @('/register-user') | Out-Null

# 4. Update while KilatX runs, with /launch (what KilatX's updater uses)
Start-Process $pfExe -ArgumentList '/tray'
Check 'Installed KilatX runs' (Wait-Until { (Procs | Where-Object Path -eq $pfExe).Count -eq 1 } 20)
$pidBefore = (Procs | Where-Object Path -eq $pfExe)[0].Id
$quits = (Events 'quit').Count
$code = Setup $setup051 @('/launch')
Check 'Update: exit code 0' ($code -eq 0) "exit $code"
Check 'Update: KilatX was asked to save and exit' ((Events 'quit').Count -gt $quits -and -not (Get-Process -Id $pidBefore -ErrorAction SilentlyContinue))
Check 'Update: new exe in place, version 0.5.1' ((Hash $pfExe) -eq $hash051 -and (Version) -eq '0.5.1') (Version)
Check 'Update: KilatX started again (/launch)' (Wait-Until { (Procs | Where-Object Path -eq $pfExe).Count -eq 1 } 20)
Check 'Update: downloads list kept' ([bool](Items | Where-Object url -like '*kept-across-update*'))

# 5. Update while a browser's connection (no window, no single-instance lock) holds KilatX.exe
$psi = New-Object Diagnostics.ProcessStartInfo $pfExe, 'chrome-extension://mbnpaegbhgmmkfddjehcbmbdofgdlkok/'
$psi.UseShellExecute = $false; $psi.RedirectStandardInput = $true; $psi.RedirectStandardOutput = $true
$hostProc = [Diagnostics.Process]::Start($psi)
Start-Sleep -Seconds 1
Check 'Browser connection process holds KilatX.exe' (-not $hostProc.HasExited)
$code = Setup $setup050
Check 'Update over a busy KilatX.exe: exit code 0' ($code -eq 0) "exit $code"
Check 'Update over a busy KilatX.exe: replaced now (no restart needed)' ((Hash $pfExe) -eq $hash050 -and (Version) -eq '0.5.0') "$(Version)"
Check 'Update over a busy KilatX.exe: browser connection process ended' ($hostProc.HasExited)
Check 'No pending replace-at-restart left behind' (-not ((Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager' -ErrorAction SilentlyContinue).PendingFileRenameOperations -match 'KilatX-Test'))

# 6. KilatX's own updater (signed update.json from a local server)
$web = Start-Process python -ArgumentList '-I', '-m', 'http.server', '8790', '--bind', '127.0.0.1', '--directory', "`"$Work\web`"" -PassThru -WindowStyle Hidden
Wait-Until { try { (Invoke-WebRequest 'http://127.0.0.1:8790/update.json' -UseBasicParsing -TimeoutSec 2).StatusCode -eq 200 } catch { $false } } 15 | Out-Null
function Run-Updater([string]$json, [string]$waitFor, [int]$sec = 40) {
    Run $pfExe @('/close') | Out-Null
    Write-State @{ check_updates = $true; last_update_check = 0 }
    $env:KILATX_UPDATE_URL = "http://127.0.0.1:8790/$json"; $env:KILATX_UPDATE_AUTO = '1'
    $before = (Events $waitFor).Count
    Start-Process $pfExe -ArgumentList '/tray'
    $ok = Wait-Until { (Events $waitFor).Count -gt $before } $sec
    $env:KILATX_UPDATE_URL = 'http://127.0.0.1:9/no-update-server'; Remove-Item Env:KILATX_UPDATE_AUTO
    $ok
}
Check 'Updater: forged update information refused' (Run-Updater 'update-forged.json' 'update-check-failed') ((Events 'update-check-failed') -join ' | ')
Check 'Updater: forged one not installed' ((Version) -eq '0.5.0')
Check 'Updater: damaged download refused (checksum)' ((Run-Updater 'update-damaged.json' 'update-failed') -and @(Events 'update-failed')[-1] -like '*checksum*') ((Events 'update-failed') -join ' | ')
Check 'Updater: damaged one not installed' ((Version) -eq '0.5.0' -and (Hash $pfExe) -eq $hash050)
Check 'Updater: offers 0.5.1 and starts setup' ((Run-Updater 'update.json' 'update-started' 60) -and @(Events 'update-available')[-1] -like '*0.5.1') ((Events 'update-available') -join ' | ')
Check 'Updater: 0.5.1 installed' (Wait-Until { (Version) -eq '0.5.1' -and (Hash $pfExe) -eq $hash051 } 90) (Version)
Check 'Updater: KilatX running again after the update' (Wait-Until { (Procs | Where-Object Path -eq $pfExe).Count -eq 1 } 30)
$web | Stop-Process -Force -ErrorAction SilentlyContinue

# 7. Refused folders (nothing changes)
New-Item -ItemType Directory -Force "$Work\portable\data" | Out-Null
$code = Setup $setup050 @("/DIR=$Work\portable")
Check 'Portable folder refused' ($code -ne 0 -and -not (Test-Path "$Work\portable\KilatX.exe") -and (Version) -eq '0.5.1') "exit $code"
New-Item -ItemType Directory -Force "$Work\foreign" | Out-Null
Set-Content "$Work\foreign\report.docx" 'not ours'
$code = Setup $setup050 @("/DIR=$Work\foreign")
Check 'Folder with other files refused (files and permissions untouched)' ($code -ne 0 -and -not (Test-Path "$Work\foreign\KilatX.exe") -and (Test-Path "$Work\foreign\report.docx") -and -not (Get-Acl "$Work\foreign").AreAccessRulesProtected) "exit $code"

# 8. Uninstall, then install to a folder outside Program Files (spaces, non-ASCII letters)
$code = Uninstall $pfDir
Check 'Uninstall 0.5.1: exit code 0, folder gone' ($code -eq 0 -and (Wait-Until { -not (Test-Path $pfDir) } 15)) "exit $code"
$custom = "$Work\Prog $([char]0xDC)n$([char]0xEF) Apps\KilatX-Test"
$code = Setup $setup050 @("/DIR=$custom", '/TASKS=desktopicon')
Check 'Custom folder install: exit code 0' ($code -eq 0 -and (Test-Path "$custom\KilatX.exe")) "exit $code"
Check 'Custom folder: protected like Program Files' ((Get-Acl $custom).AreAccessRulesProtected -and -not (Users-Can-Write $custom) -and -not (Users-Can-Write "$custom\KilatX.exe")) ((Get-Acl $custom).AccessToString)
Check 'Custom folder: users can still run KilatX' (@((Get-Acl "$custom\KilatX.exe").Access | Where-Object { $_.IdentityReference -match 'Users' -and ($_.FileSystemRights -band [Security.AccessControl.FileSystemRights]::ReadAndExecute) }).Count -ge 1)
Check 'Desktop shortcut when chosen' ((LnkTarget $desktopLnk) -eq "$custom\KilatX.exe") (LnkTarget $desktopLnk)

# 9. Planted DLLs next to KilatX.exe are not loaded
$plant = "$Work\dll-plant"
New-Item -ItemType Directory -Force $plant | Out-Null
Copy-Item "$app\target\debug\KilatX.exe" "$plant\KilatX.exe"
Copy-Item "$app\target\fixtures\KilatX-0.4.0-debug.exe" "$plant\KilatX-0.4.exe"
foreach ($d in 'crypt32', 'uxtheme', 'dwmapi', 'secur32', 'dwrite', 'opengl32', 'uiautomationcore', 'bcryptprimitives', 'version', 'winhttp', 'vcruntime140', 'dbghelp', 'propsys', 'textinputframework', 'cryptbase', 'wintypes') {
    [IO.File]::WriteAllBytes("$plant\$d.dll", [byte[]](0x4D, 0x5A) + [byte[]](1..250))
}
# KilatX 0.4 loads the planted DLL and fails to start (expected). If Windows still shows a
# message for it, the stuck start is ended after a few seconds.
# Started directly (not through the shell) so it inherits SetErrorMode: no "Bad Image" message.
$psi = New-Object Diagnostics.ProcessStartInfo "$plant\KilatX-0.4.exe", '/quit'
$psi.UseShellExecute = $false; $psi.CreateNoWindow = $true
$ctl = [Diagnostics.Process]::Start($psi)
if (-not $ctl.WaitForExit(8000)) { $ctl | Stop-Process -Force; Get-Process WerFault -ErrorAction SilentlyContinue | Stop-Process -Force; $ctl.WaitForExit() }
$oldCode = $ctl.ExitCode
Check 'Control: KilatX 0.4 loads a planted DLL (fails to start)' ($oldCode -ne 0) "exit $oldCode"
$newCode = Run "$plant\KilatX.exe" @('/close') -Hidden
Check 'KilatX 0.5 ignores planted DLLs (starts normally)' ($newCode -eq 0) "exit $newCode"
$starts = (Events 'start').Count
Start-Process "$plant\KilatX.exe" -ArgumentList '/tray'
Check 'KilatX 0.5 window starts with planted DLLs next to it' (Wait-Until { (Events 'start').Count -gt $starts } 20)
Run "$plant\KilatX.exe" @('/close') | Out-Null
Check 'And it exits cleanly' (Wait-Until { (Procs | Where-Object Path -like "$plant*").Count -eq 0 } 15)

# 10. Uninstall with /removedata: settings and folder gone
$code = Uninstall $custom @('/removedata')
Check 'Uninstall /removedata: exit code 0' ($code -eq 0) "exit $code"
Check 'Uninstall /removedata: data folder removed' (Wait-Until { -not (Test-Path $env:KILATX_DATA_DIR) } 15)
Check 'Uninstall /removedata: install folder removed' (Wait-Until { -not (Test-Path $custom) } 15) ((Get-ChildItem $custom -ErrorAction SilentlyContinue).Name -join ', ')
Check 'Uninstall: desktop shortcut removed' (-not (Test-Path $desktopLnk))
Check 'Uninstall: start with Windows removed' (-not ((RunEntry) -like "*KilatX-Test*")) (RunEntry)
Check 'Nothing of the test left in Installed apps' (-not (Test-Path $appKey) -and -not (Test-Path $legacyKey))

# 11. MSI package (company deployment)
$msi050 = "$Work\setups\KilatX-Test-0.5.0-x64.msi"
$msi051 = "$Work\setups\KilatX-Test-0.5.1-x64.msi"
$msiMarker = 'HKLM:\SOFTWARE\KilatX-Test'
function MsiEntries { @(Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*' -ErrorAction SilentlyContinue | Where-Object { $_.DisplayName -eq 'KilatX-Test' -and $_.WindowsInstaller -eq 1 }) }
# Intune / SCCM / Group Policy install as the SYSTEM account: same here, through a one-time task.
function Run-AsSystem([string]$file, [string]$arguments) {
    $name = 'KilatX-Test-msi'
    $action = New-ScheduledTaskAction -Execute $file -Argument $arguments
    Register-ScheduledTask -TaskName $name -Action $action -User 'SYSTEM' -RunLevel Highest -Force | Out-Null
    Start-ScheduledTask -TaskName $name
    Start-Sleep -Seconds 2
    Wait-Until { (Get-ScheduledTask -TaskName $name).State -ne 'Running' } 300 | Out-Null
    $code = (Get-ScheduledTaskInfo -TaskName $name).LastTaskResult
    Unregister-ScheduledTask -TaskName $name -Confirm:$false
    $code
}
$code = Run-AsSystem "$env:WINDIR\System32\msiexec.exe" "/i `"$msi050`" /qn /l*v `"$Work\msi-install.log`""
Check 'MSI: installed by the SYSTEM account (like Intune/SCCM), exit 0' ($code -eq 0) "exit $code (see msi-install.log)"
Check 'MSI: KilatX.exe, license and notices in Program Files' ((Test-Path $pfExe) -and (Hash $pfExe) -eq $hash050 -and (Test-Path "$pfDir\LICENSE.txt") -and (Test-Path "$pfDir\THIRD-PARTY-NOTICES.txt"))
Check 'MSI: Start menu shortcut for all users, with the AppUserModelID' ((LnkTarget $startLnk) -eq $pfExe -and (LnkAumid $startLnk) -eq 'KilatX.KilatX') "$(LnkTarget $startLnk) $(LnkAumid $startLnk)"
Check 'MSI: one entry in Installed apps, version 0.5.0' (@(MsiEntries).Count -eq 1 -and @(MsiEntries)[0].DisplayVersion -eq '0.5.0') ((MsiEntries | ForEach-Object DisplayVersion) -join ', ')
Check 'MSI: browsers of all users connected (machine-wide)' (@(MachineHostKeys | Where-Object { $_ -eq "$pfDir\app.kilatx.host.json" }).Count -eq 10) ((MachineHostKeys) -join ' | ')
Check 'MSI: the folder is protected like Program Files' (-not (Users-Can-Write $pfDir) -and -not (Users-Can-Write $pfExe))
$code = Setup $setup050
Check 'KilatX Setup refuses to install over the MSI (nothing changed)' ($code -ne 0 -and -not (Test-Path $appKey) -and (Hash $pfExe) -eq $hash050) "exit $code"
# A user starts it: the per-user part (browser connection, notification name) is set up by KilatX itself
Start-Process $pfExe -ArgumentList '/tray'
Check 'MSI: KilatX runs for the user and connects that user''s browsers' (Wait-Until { @(HostKeys | Where-Object { $_ -like "$env:KILATX_DATA_DIR*" }).Count -eq 5 } 20) ((HostKeys) -join ' | ')
Run $pfExe @('/d', "$Base/plain/1000000/kept-across-update.zip", '/a') | Out-Null
Wait-Until { [bool](Items | Where-Object url -like '*kept-across-update*') } 20 | Out-Null
$psi = New-Object Diagnostics.ProcessStartInfo $pfExe, 'chrome-extension://mbnpaegbhgmmkfddjehcbmbdofgdlkok/'
$psi.UseShellExecute = $false; $psi.RedirectStandardInput = $true; $psi.RedirectStandardOutput = $true
$hostProc = [Diagnostics.Process]::Start($psi)
Start-Sleep -Seconds 1
$code = Run-AsSystem "$env:WINDIR\System32\msiexec.exe" "/i `"$msi051`" /qn /l*v `"$Work\msi-upgrade.log`""
Check 'MSI upgrade 0.5.0 -> 0.5.1 while KilatX and a browser connection run: exit 0, no restart' ($code -eq 0) "exit $code (3010 = restart needed; see msi-upgrade.log)"
Check 'MSI upgrade: new exe in place, still one Installed-apps entry (0.5.1)' ((Hash $pfExe) -eq $hash051 -and @(MsiEntries).Count -eq 1 -and @(MsiEntries)[0].DisplayVersion -eq '0.5.1') ((MsiEntries | ForEach-Object DisplayVersion) -join ', ')
Check 'MSI upgrade: running KilatX and the browser connection were ended' ((Procs | Where-Object Path -eq $pfExe).Count -eq 0 -and $hostProc.HasExited)
Check 'MSI upgrade: machine-wide browser connection still there' (@(MachineHostKeys | Where-Object { $_ -eq "$pfDir\app.kilatx.host.json" }).Count -eq 10)
Check 'MSI upgrade: downloads list kept' ([bool](Items | Where-Object url -like '*kept-across-update*'))
$product = @(MsiEntries)[0].PSChildName
$code = Run-AsSystem "$env:WINDIR\System32\msiexec.exe" "/x $product /qn /l*v `"$Work\msi-uninstall.log`""
Check 'MSI uninstall (SYSTEM): exit 0' ($code -eq 0) "exit $code"
Check 'MSI uninstall: folder, shortcut and Installed-apps entry gone' ((Wait-Until { -not (Test-Path $pfDir) } 15) -and -not (Test-Path $startLnk) -and @(MsiEntries).Count -eq 0 -and -not (Test-Path $msiMarker)) ((Get-ChildItem $pfDir -ErrorAction SilentlyContinue).Name -join ', ')
Check 'MSI uninstall: machine-wide browser connection removed' (@(MachineHostKeys | Where-Object { $_ -like "$pfDir*" }).Count -eq 0) ((MachineHostKeys) -join ' | ')
Check 'MSI uninstall: users'' settings and download lists kept' (Test-Path "$env:KILATX_DATA_DIR\state.json")
# The other way round: the MSI refuses while KilatX Setup's installation is there
$code = Setup $setup050
$codeMsi = Run "$env:WINDIR\System32\msiexec.exe" @('/i', "`"$msi050`"", '/qn', '/l*v', "`"$Work\msi-refused.log`"")
Check 'MSI refuses to install over KilatX Setup''s installation (nothing changed)' ($code -eq 0 -and $codeMsi -ne 0 -and @(MsiEntries).Count -eq 0 -and (Test-Path $appKey)) "setup $code, msi $codeMsi"
$code = Uninstall $pfDir
Check 'Clean again after both installers' ($code -eq 0 -and (Wait-Until { -not (Test-Path $pfDir) } 15) -and @(MsiEntries).Count -eq 0 -and -not (Test-Path $appKey))
}
catch {
    Check "Admin script error at line $($_.InvocationInfo.ScriptLineNumber)" $false $_.Exception.Message
}
finally {
    Cleanup-Test
    # Whatever test install is left (Program Files or a custom folder), and its protected folder.
    $left = (Get-ItemProperty $appKey -ErrorAction SilentlyContinue).InstallLocation
    foreach ($d in @($left, $pfDir) | Where-Object { $_ }) {
        if (Test-Path "$d\unins000.exe") { Uninstall $d.TrimEnd('\') | Out-Null }
    }
    foreach ($e in @(Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*' -ErrorAction SilentlyContinue | Where-Object { $_.DisplayName -eq 'KilatX-Test' -and $_.WindowsInstaller -eq 1 })) {
        Run "$env:WINDIR\System32\msiexec.exe" @('/x', $e.PSChildName, '/qn') | Out-Null
    }
    Unregister-ScheduledTask -TaskName 'KilatX-Test-msi' -Confirm:$false -ErrorAction SilentlyContinue
    Get-ChildItem $Work -Directory -Filter 'Prog *' -ErrorAction SilentlyContinue | ForEach-Object { cmd /c "rmdir /s /q `"$($_.FullName)`"" }
    # A real installation's machine-wide browser registration, as it was before the test
    $saved = Get-Content "$Work\machine-keys-backup.json" -Raw -ErrorAction SilentlyContinue | ConvertFrom-Json
    if ($saved) { foreach ($p in $saved.PSObject.Properties) { New-Item -Path $p.Name -Force | Out-Null; Set-ItemProperty -Path $p.Name -Name '(default)' -Value $p.Value } }
    $results | ConvertTo-Json -Depth 3 | Set-Content "$Work\results-part2.json"
    Stop-Transcript | Out-Null
}

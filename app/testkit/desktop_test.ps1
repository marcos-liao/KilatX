param([string]$Exe = "$PSScriptRoot\..\target\debug\KilatX.exe", [string]$Base = 'http://127.0.0.1:8766')
# Stage 3 desktop features, end to end. Needs scenario_server.py on $Base.
# Uses a portable copy in %TEMP% so the user's own data is never touched.
$ErrorActionPreference = 'Stop'
$work = Join-Path $env:TEMP 'kilatx-desktop-test'
Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force "$work\app\data", "$work\dl" | Out-Null
Copy-Item $Exe "$work\app\KilatX.exe"
$app = "$work\app\KilatX.exe"
# KilatX writes on-screen events (dialogs, notifications) here when this is set.
$env:KILATX_EVENT_LOG = "$work\events.log"
function Events($kind) { @(Get-Content $env:KILATX_EVENT_LOG -ErrorAction SilentlyContinue | Where-Object { $_.StartsWith("$kind`t") }) }
$state = "$work\app\data\state.json"
$results = @()
function Check($name, $ok, $detail = '') {
    $script:results += [pscustomobject]@{ Test = $name; Result = $(if ($ok) { 'PASS' } else { 'FAIL' }); Detail = $detail }
}
function Procs { @(Get-Process KilatX -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $app }) }
function Kill-All { Procs | Stop-Process -Force; Start-Sleep -Milliseconds 500 }
function Wait-Until([scriptblock]$cond, [int]$sec = 10) {
    $t = Get-Date; while (((Get-Date) - $t).TotalSeconds -lt $sec) { if (& $cond) { return $true }; Start-Sleep -Milliseconds 200 }; return $false
}
function Items { (Get-Content $state -Raw -ErrorAction SilentlyContinue | ConvertFrom-Json).items }
Add-Type @'
using System; using System.Text; using System.Runtime.InteropServices;
public class KxWin {
  delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32.dll")] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  // True if the process has a visible top-level window with exactly this title.
  public static bool Visible(uint pid, string title) {
    bool found = false;
    EnumWindows((h, l) => { uint p; GetWindowThreadProcessId(h, out p); if (p == pid && IsWindowVisible(h)) { var t = new StringBuilder(256); GetWindowText(h, t, 256); if (t.ToString() == title) found = true; } return true; }, IntPtr.Zero);
    return found;
  }
}
'@
# The main window specifically (helper windows of the tray/event loop don't count).
function Window-Visible {
    $p = Procs | Select-Object -First 1
    if (-not $p) { return $false }
    return [KxWin]::Visible([uint32]$p.Id, 'KilatX')
}
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
function Toasts { try { [int]@([Windows.UI.Notifications.ToastNotificationManager]::History.GetHistory('KilatX.KilatX')).Count } catch { 0 } }

# 1. Single instance: a second launch hands its link over and exits
Start-Process $app; Wait-Until { Window-Visible } | Out-Null
Start-Process $app -ArgumentList "/d $Base/plain/2000000/single.bin /p `"$work\dl`"" -Wait
Check 'Second launch exits, one KilatX left' ((Procs).Count -eq 1) "processes: $((Procs).Count)"
$got = Wait-Until { Test-Path "$work\dl\single.bin" } 15
Check 'Link from second launch is downloaded by the first' $got

# 2. Closing the window keeps KilatX in the tray (+ one-time hint notification)
$before = Toasts
(Procs)[0].CloseMainWindow() | Out-Null
Start-Sleep -Seconds 2
Check 'Close keeps KilatX running in the tray' ((Procs).Count -eq 1 -and -not (Window-Visible))
Check 'Hint notification "still running" shown' ((Toasts) -gt $before) "toasts $before -> $(Toasts)"

# 3. A second launch without arguments brings the window back
Start-Process $app -Wait
Check 'Second launch shows the hidden window again' (Wait-Until { Window-Visible } 5)

# 4. Notifications when a download finishes and when it fails
$before = Toasts
Start-Process $app -ArgumentList "/d $Base/plain/1000000/done.bin /p `"$work\dl`"" -Wait
Wait-Until { Test-Path "$work\dl\done.bin" } 15 | Out-Null
Check 'Notification when a download finishes' (Wait-Until { (Events 'notify' | Where-Object { $_ -match 'Complete\tdone.bin' }) } 5) "Action Center toasts $before -> $(Toasts)"
$before = Toasts
Start-Process $app -ArgumentList "/d $Base/notfound/1/missing.bin /p `"$work\dl`"" -Wait
Check 'Notification when a download fails' (Wait-Until { (Events 'notify' | Where-Object { $_ -match 'Error\tmissing.bin' }) } 15) "Action Center toasts $before -> $(Toasts)"

# 5. Clipboard: copying a link to a file opens the Add dialog (the user's clipboard is restored)
$saved = Get-Clipboard -Raw -ErrorAction SilentlyContinue
Set-Clipboard -Value "$Base/plain/1000000/from-clipboard.zip"
Start-Sleep -Seconds 2
Set-Clipboard -Value 'just some text, not a link'
Start-Sleep -Seconds 1
if ($null -ne $saved) { Set-Clipboard -Value $saved } else { Set-Clipboard -Value ' ' }
$offers = Events 'clipboard-offer'
Check 'Copied link to a file opens the Add dialog' (($offers | Where-Object { $_ -match 'from-clipboard.zip' }).Count -eq 1) ($offers -join ' | ')
Check 'Copied plain text is ignored' (-not ($offers | Where-Object { $_ -match 'just some text' }))
Check 'Copied link is only offered, not downloaded' (-not (Test-Path "$work\dl\from-clipboard.zip"))

Kill-All

# 6. /tray: starts hidden in the notification area
Start-Process $app -ArgumentList '/tray'
Start-Sleep -Seconds 3
Check '/tray starts without a window' ((Procs).Count -eq 1 -and -not (Window-Visible))
Kill-All

# 7. GPU rendering on, but the last start never got its window up: back to software rendering
$j = Get-Content $state -Raw | ConvertFrom-Json
$j.settings | Add-Member -NotePropertyName gpu_render -NotePropertyValue $true -Force
$j | ConvertTo-Json -Depth 20 | Set-Content $state -Encoding UTF8
Set-Content "$work\app\data\starting.lock" ''
Start-Process $app; Wait-Until { Window-Visible } | Out-Null
Start-Sleep -Seconds 4
Check 'After a failed start: window comes up (software renderer)' (Window-Visible)
Check 'Marker removed once the window is up' (-not (Test-Path "$work\app\data\starting.lock"))
(Procs)[0].CloseMainWindow() | Out-Null; Start-Sleep 1
Kill-All
$gpu = (Get-Content $state -Raw | ConvertFrom-Json).settings.gpu_render
Check 'GPU rendering turned off for next starts' ($gpu -eq $false)

# 8. Strictly one instance: ten launches at once leave exactly one process
$burst = 1..10 | ForEach-Object { Start-Process $app -ArgumentList '/tray' -PassThru }
Start-Sleep -Seconds 6
Check 'Ten launches at once: exactly one KilatX' ((Procs).Count -eq 1) "running: $((Procs).Count)"
$one = (Procs)[0]; $cpu0 = $one.CPU; Start-Sleep -Seconds 5; $one.Refresh()
Check 'Idle in the tray: no CPU use' (($one.CPU - $cpu0) -lt 0.5) ("{0:N2} s CPU in 5 s, {1} MB" -f ($one.CPU - $cpu0), [int]($one.WorkingSet64 / 1MB))
Kill-All

# The test copy registered itself as the browsers' host: remove that if it still points here.
foreach ($k in 'Google\Chrome', 'Microsoft\Edge', 'BraveSoftware\Brave-Browser', 'Vivaldi', 'Chromium') {
    $key = "HKCU:\Software\$k\NativeMessagingHosts\app.kilatx.host"
    $target = (Get-ItemProperty $key -ErrorAction SilentlyContinue).'(default)'
    if ($target -and $target.StartsWith($work, [StringComparison]::OrdinalIgnoreCase)) { Remove-Item $key -Recurse }
}

$results | Format-Table -AutoSize | Out-String -Width 200
"{0}/{1} passed" -f @($results | Where-Object Result -eq 'PASS').Count, $results.Count

param(
    [string]$Kilat = "$PSScriptRoot\..\target\release\KilatX.exe",
    [string[]]$Only = @(),
    [int]$TimeoutSec = 900,
    [int]$Runs = 1
)
# Real-world check: every file is downloaded by Kilat (8 connections) and by curl
# (one connection, like a browser). Hashes must match each other and, where a
# project publishes one, the official SHA-256.
$ErrorActionPreference = 'Stop'
$Curl = "$env:SystemRoot\System32\curl.exe"
$Work = Join-Path $env:TEMP 'kilat-realworld'
$KilatDir = Join-Path $Work 'kilat'
New-Item -ItemType Directory -Force "$KilatDir\data" | Out-Null
Copy-Item $Kilat "$KilatDir\KilatX.exe" -Force            # isolated portable copy
Remove-Item "$KilatDir\data\state.json" -ErrorAction SilentlyContinue
# Kilat logs its pure download time here (start to finished file, no app start-up).
$env:KILAT_TIMING_LOG = Join-Path $Work 'timing.log'
Remove-Item $env:KILAT_TIMING_LOG -ErrorAction SilentlyContinue

function Official([string]$SumsUrl, [string]$File) {
    $line = (& $Curl -s -L $SumsUrl) -split "`n" | Where-Object { $_ -match [regex]::Escape($File) + '\s*$' } | Select-Object -First 1
    if ($line) { return ($line -split '\s+')[0].ToLower() }
}

$Tests = @(
    @{ Name = 'Citrahost Indonesia (Ubuntu WSL)'; Url = 'https://mirror.citrahost.com/ubuntu-releases/24.04/ubuntu-24.04.5-wsl-amd64.wsl'
       Sums = 'https://mirror.citrahost.com/ubuntu-releases/24.04/SHA256SUMS'; File = 'ubuntu-24.04.5-wsl-amd64.wsl' }
    @{ Name = 'Hetzner Singapore'; Url = 'https://sin-speed.hetzner.com/100MB.bin'; File = '100MB.bin' }
    @{ Name = 'Hetzner Germany'; Url = 'https://nbg1-speed.hetzner.com/100MB.bin'; File = '100MB.bin' }
    @{ Name = 'Node.js (Cloudflare)'; Url = 'https://nodejs.org/dist/v22.11.0/node-v22.11.0-x64.msi'
       Sums = 'https://nodejs.org/dist/v22.11.0/SHASUMS256.txt'; File = 'node-v22.11.0-x64.msi' }
    @{ Name = 'Python (Fastly)'; Url = 'https://www.python.org/ftp/python/3.13.1/python-3.13.1-amd64.exe'; File = 'python-3.13.1-amd64.exe' }
    @{ Name = 'VC++ Redist (aka.ms redirect)'; Url = 'https://aka.ms/vs/17/release/vc_redist.x64.exe'; File = 'VC_redist.x64.exe' }
    @{ Name = 'GitHub Releases (signed redirect)'; Url = 'https://github.com/BurntSushi/ripgrep/releases/download/14.1.1/ripgrep-14.1.1-x86_64-pc-windows-msvc.zip'; File = 'ripgrep-14.1.1-x86_64-pc-windows-msvc.zip' }
    @{ Name = 'GitHub archive (no size)'; Url = 'https://github.com/BurntSushi/ripgrep/archive/refs/tags/14.1.1.zip'; File = 'ripgrep-14.1.1.zip' }
    @{ Name = 'SourceForge (mirror redirect)'; Url = 'https://downloads.sourceforge.net/project/sevenzip/7-Zip/24.09/7z2409-x64.exe'; File = '7z2409-x64.exe' }
    @{ Name = 'Tele2 (plain HTTP)'; Url = 'http://speedtest.tele2.net/10MB.zip'; File = '10MB.zip' }
)
if ($Only.Count) { $Tests = $Tests | Where-Object { $Only -contains $_.Name } }

function Run-Kilat($t, $dir) {
    New-Item -ItemType Directory -Force $dir | Out-Null
    $start = Get-Date
    $p = Start-Process "$KilatDir\KilatX.exe" -ArgumentList "/d `"$($t.Url)`" /p `"$dir`"" -PassThru
    $result = @{ Seconds = $null; Path = $null; Error = $null }
    $lastStateCheck = Get-Date
    while (((Get-Date) - $start).TotalSeconds -lt $TimeoutSec) {
        $files = @(Get-ChildItem $dir -File -ErrorAction SilentlyContinue)
        $done = @($files | Where-Object { $_.Name -notlike '*.kilatpart' })
        if ($done.Count -eq 1 -and -not ($files | Where-Object { $_.Name -like '*.kilatpart' })) {
            try { $s = [System.IO.File]::Open($done[0].FullName, 'Open', 'Read', 'None'); $s.Close()
                  $result.Path = $done[0].FullName
                  Start-Sleep -Milliseconds 300
                  $logged = Get-Content $env:KILAT_TIMING_LOG -ErrorAction SilentlyContinue | Where-Object { $_.StartsWith("$($t.Url)`t") } | Select-Object -Last 1
                  if ($logged) { $result.Seconds = [double]($logged -split "`t")[2] / 1000 } else { $result.Seconds = ((Get-Date) - $start).TotalSeconds }
                  break } catch { }
        }
        if (((Get-Date) - $lastStateCheck).TotalSeconds -gt 2) {
            $lastStateCheck = Get-Date
            $state = Get-Content "$KilatDir\data\state.json" -Raw -ErrorAction SilentlyContinue | ConvertFrom-Json -ErrorAction SilentlyContinue
            $item = $state.items | Where-Object { $_.url -eq $t.Url } | Select-Object -Last 1
            if ($item.status -eq 'Error') { $result.Error = $item.error; break }
        }
        Start-Sleep -Milliseconds 100
    }
    $p.CloseMainWindow() | Out-Null
    if (-not $p.WaitForExit(10000)) { $p.Kill() }
    Remove-Item "$KilatDir\data\state.json" -ErrorAction SilentlyContinue   # every run starts with an empty list
    if (-not $result.Seconds -and -not $result.Error) { $result.Error = 'timeout' }
    return $result
}

function Run-Curl($t, $dir) {
    New-Item -ItemType Directory -Force $dir | Out-Null
    $path = Join-Path $dir $t.File
    $start = Get-Date
    & $Curl -s -L --fail -o $path $t.Url
    if ($LASTEXITCODE -ne 0) { return @{ Seconds = $null; Path = $null; Error = "curl exit $LASTEXITCODE" } }
    return @{ Seconds = ((Get-Date) - $start).TotalSeconds; Path = $path; Error = $null }
}

$rows = @()
$i = 0
foreach ($t in $Tests) {
    $i++
    Write-Host "== $($t.Name)"
    $kDir = Join-Path $Work "k$i"; $cDir = Join-Path $Work "c$i"
    Remove-Item $kDir, $cDir -Recurse -Force -ErrorAction SilentlyContinue
    # alternate who goes first so neither always gets a warm server; keep the median
    $kTimes = @(); $cTimes = @(); $k = $null; $c = $null
    for ($r = 1; $r -le $Runs; $r++) {
        Remove-Item $kDir, $cDir -Recurse -Force -ErrorAction SilentlyContinue
        if (($i + $r) % 2) { $k = Run-Kilat $t $kDir; $c = Run-Curl $t $cDir } else { $c = Run-Curl $t $cDir; $k = Run-Kilat $t $kDir }
        if ($k.Seconds) { $kTimes += $k.Seconds }; if ($c.Seconds) { $cTimes += $c.Seconds }
        if ($Runs -gt 1) { Write-Host ("   run {0}: Kilat {1:N1}s  curl {2:N1}s" -f $r, $k.Seconds, $c.Seconds) }
    }
    function Median($xs) { if (-not $xs.Count) { return $null }; $s = $xs | Sort-Object; return $s[[int][math]::Floor(($s.Count - 1) / 2)] }
    if ($kTimes.Count) { $k.Seconds = Median $kTimes }; if ($cTimes.Count) { $c.Seconds = Median $cTimes }
    $official = if ($t.Sums) { Official $t.Sums $t.File }
    $kHash = if ($k.Path) { (Get-FileHash $k.Path -Algorithm SHA256).Hash.ToLower() }
    $cHash = if ($c.Path) { (Get-FileHash $c.Path -Algorithm SHA256).Hash.ToLower() }
    $size = if ($k.Path) { (Get-Item $k.Path).Length } elseif ($c.Path) { (Get-Item $c.Path).Length } else { 0 }
    function Mbps($s) { if ($s) { [math]::Round($size * 8 / $s / 1e6, 1) } }
    $kSec = $null; if ($k.Seconds) { $kSec = [math]::Round($k.Seconds, 1) }
    $cSec = $null; if ($c.Seconds) { $cSec = [math]::Round($c.Seconds, 1) }
    $speedup = $null; if ($k.Seconds -and $c.Seconds) { $speedup = [math]::Round($c.Seconds / $k.Seconds, 2) }
    $same = $null; if ($kHash -and $cHash) { $same = ($kHash -eq $cHash) }
    $off = 'n/a'; if ($official) { $off = ($kHash -eq $official) }
    $name = "ERROR: $($k.Error)"; $nameOk = $false
    if ($k.Path) { $name = Split-Path $k.Path -Leaf; $nameOk = ($name -eq $t.File) }
    $row = [pscustomobject]@{
        Test       = $t.Name
        SizeMB     = [math]::Round($size / 1MB, 1)
        KilatSec   = $kSec
        KilatMbps  = Mbps $k.Seconds
        CurlSec    = $cSec
        CurlMbps   = Mbps $c.Seconds
        Speedup    = $speedup
        SameAsCurl = $same
        Official   = $off
        KilatName  = $name
        NameOK     = $nameOk
    }
    $row | Format-List | Out-String | Write-Host
    $rows += $row
    Remove-Item $kDir, $cDir -Recurse -Force -ErrorAction SilentlyContinue
}
$rows | Export-Csv (Join-Path $Work 'results.csv') -NoTypeInformation
$rows | Format-Table Test, SizeMB, KilatSec, KilatMbps, CurlSec, CurlMbps, Speedup, SameAsCurl, Official, NameOK -AutoSize | Out-String -Width 200

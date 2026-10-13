param(
    [string[]]$Arch = @('x64', 'arm64'),
    [switch]$SkipCargo
)
# Builds a KilatX release into dist\<version>\:
#   x64\KilatX-Setup-<v>-x64.exe, arm64\KilatX-Setup-<v>-arm64.exe   installers (Inno Setup 6, Program Files)
#   x64\KilatX-<v>-x64.msi, arm64\KilatX-<v>-arm64.msi               MSI packages for companies (WiX 5)
#   portable\KilatX-<v>-portable-<win64|arm64>.zip                   portable copies, and the same
#   portable\KilatX-x64\, portable\KilatX-arm64\                     unzipped, ready to run
#   extension\KilatX-browser-extension-<v>.zip                       browser extension for manual install
# The version comes from app\Cargo.toml. Needs Rust (with the aarch64-pc-windows-msvc target
# for arm64), Inno Setup 6 and WiX 5 (dotnet tool install --global wix --version 5.0.2, plus
# the .NET 8 SDK).
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path "$PSScriptRoot\..").Path
$app = "$root\app"
$version = (Select-String -Path "$app\Cargo.toml" -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$out = "$root\dist\$version"
$iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw 'Inno Setup 6 not found (winget install JRSoftware.InnoSetup)' }
$wix = @("$env:USERPROFILE\.dotnet\tools\wix.exe") + @((Get-Command wix -ErrorAction SilentlyContinue).Source) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $wix) { throw 'WiX 5 not found (dotnet tool install --global wix --version 5.0.2)' }
New-Item -ItemType Directory -Force "$out\portable", "$out\extension" | Out-Null
# Zip with "/" in entry names (Windows PowerShell's Compress-Archive writes "\", which the stores'
# servers and other systems read as part of the file name). With -Folder, entries go under it.
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
function New-Zip([string]$source, [string]$zip, [string]$Folder = '') {
    if (Test-Path $zip) { Remove-Item -LiteralPath $zip }
    $root = (Resolve-Path $source).Path.TrimEnd('\')
    $archive = [IO.Compression.ZipFile]::Open($zip, 'Create')
    try {
        foreach ($file in Get-ChildItem $root -Recurse -File) {
            $name = $file.FullName.Substring($root.Length + 1).Replace('\', '/')
            if ($Folder) { $name = "$Folder/$name" }
            [void][IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $file.FullName, $name, 'Optimal')
        }
    } finally { $archive.Dispose() }
}

$staging = Join-Path $env:TEMP "kilatx-build-$version"

# Source paths end up in the exe (panic messages). Replace this PC's folders with neutral names
# so a release doesn't carry the builder's user name or folder names. Encoded flags (separated by
# 0x1F) because the paths may contain spaces; these replace .cargo\config.toml's rustflags, so
# the static C runtime is repeated here.
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { "$env:USERPROFILE\.cargo" }
$flags = @('-C', 'target-feature=+crt-static', "--remap-path-prefix=$app=kilatx", "--remap-path-prefix=$cargoHome=cargo")
$env:CARGO_ENCODED_RUSTFLAGS = $flags -join [char]0x1F
if (Test-Path $staging) { Remove-Item $staging -Recurse -Force }

foreach ($a in $Arch) {
    $target = if ($a -eq 'arm64') { 'aarch64-pc-windows-msvc' } else { 'x86_64-pc-windows-msvc' }
    $exeDir = "$app\target\$target\release"
    if (-not $SkipCargo) {
        Push-Location $app
        try {
            # Through cmd: cargo reports progress on stderr, which Windows PowerShell treats as an error.
            cmd /c "cargo build --release --bin KilatX --target $target 2>&1"
            if ($LASTEXITCODE) { throw "cargo build failed for $a" }
        } finally { Pop-Location }
    }
    & $iscc /Q "/DAppVersion=$version" "/DArch=$a" "/DPayloadDir=$exeDir" "/DOutDir=$out\$a" "$root\setup\KilatX.iss"
    if ($LASTEXITCODE) { throw "ISCC failed for $a" }
    # MSI for company deployment (Group Policy, Intune, SCCM)
    & $wix build "$root\setup\KilatX.wxs" -arch $a -d "Version=$version" -d "PayloadDir=$exeDir" -o "$out\$a\KilatX-$version-$a.msi"
    if ($LASTEXITCODE) { throw "WiX failed for $a" }
    Remove-Item "$out\$a\*.wixpdb" -ErrorAction SilentlyContinue

    # Portable: KilatX\KilatX.exe + portable.txt + license and notices
    $name = if ($a -eq 'arm64') { 'arm64' } else { 'win64' }
    $p = "$staging\portable-$a\KilatX"
    New-Item -ItemType Directory -Force $p | Out-Null
    Copy-Item "$exeDir\KilatX.exe", "$app\docs\THIRD-PARTY-NOTICES.txt" $p
    Copy-Item "$root\LICENSE" "$p\LICENSE.txt"
    Set-Content "$p\portable.txt" -Encoding ASCII -Value @'
KilatX - portable mode

Because this file sits next to KilatX.exe, the download list and settings
are stored in the "data" folder here instead of in %APPDATA%.
To install KilatX on this PC instead, run KilatX-Setup (it installs to Program Files).
'@
    $zip = "$out\portable\KilatX-$version-portable-$name.zip"
    New-Zip $p $zip -Folder 'KilatX'
    # Unzipped copy, fresh (an earlier version's data folder is not carried over)
    $ready = "$out\portable\KilatX-$a"
    if (Test-Path "$ready\data") { throw "$ready has a data folder (someone uses it): move it first" }
    if (Test-Path $ready) { Remove-Item $ready -Recurse -Force }
    Copy-Item $p $ready -Recurse
}

# Browser extension for manual install
$e = "$staging\ext\KilatX-extension"
New-Item -ItemType Directory -Force $e | Out-Null
Copy-Item "$app\extension\*" $e -Recurse
Set-Content "$e\README.txt" -Encoding ASCII -Value @"
KilatX browser extension $version (Chrome, Edge, Brave, Vivaldi, Opera, Chromium)

Needs KilatX on this PC (started at least once, Settings > Browser > "Let browsers send
their downloads to KilatX" on). Without KilatX, downloads simply stay in the browser.

Install (until it is available in the Chrome Web Store / Edge Add-ons):
1. Unzip this folder somewhere it can stay (don't delete it afterwards).
2. Open the browser's extensions page: chrome://extensions (Edge: edge://extensions).
3. Turn on "Developer mode".
4. Click "Load unpacked" and choose this KilatX-extension folder.
"@
$zip = "$out\extension\KilatX-browser-extension-$version.zip"
New-Zip $e $zip -Folder 'KilatX-extension'

# Store package (Chrome Web Store, Edge Add-ons): files at the top of the zip, no "key" (the
# stores give their copy its own ID), no README.
$s = "$staging\store"
New-Item -ItemType Directory -Force $s | Out-Null
Copy-Item "$app\extension\*" $s -Recurse
$manifest = Get-Content "$s\manifest.json" -Raw | ConvertFrom-Json
$manifest.PSObject.Properties.Remove('key')
[IO.File]::WriteAllText("$s\manifest.json", ($manifest | ConvertTo-Json -Depth 10), (New-Object Text.UTF8Encoding $false))
$zip = "$out\extension\KilatX-extension-$version-store.zip"
New-Zip $s $zip
Remove-Item $staging -Recurse -Force

# Checksums for the release page (users compare them with Get-FileHash)
$files = Get-ChildItem $out -File -Recurse | Where-Object { $_.Name -like "*$version*" }
$sums = $files | ForEach-Object { '{0}  {1}' -f (Get-FileHash $_.FullName).Hash.ToLower(), $_.Name }
Set-Content "$out\SHA256SUMS.txt" -Encoding ASCII -Value $sums
$files | ForEach-Object {
    '{0,-60} {1,8:N1} MB  SHA256 {2}' -f $_.FullName.Substring($out.Length + 1), ($_.Length / 1MB), (Get-FileHash $_.FullName).Hash
}

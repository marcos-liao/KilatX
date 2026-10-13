param([string]$Exe = "$PSScriptRoot\..\target\debug\KilatX.exe")
# Starts KilatX and lists every DLL its process really loaded. Anything not from Windows
# itself is reported (KilatX ships as a single exe and loads only Windows system DLLs).
$out = "$env:TEMP\kilatx-dllcheck"
New-Item -ItemType Directory -Force "$out\data" | Out-Null
$env:KILATX_INSTANCE = '-dll-check'; $env:KILATX_DATA_DIR = "$out\data"
$exePath = (Resolve-Path $Exe).Path
$p = Start-Process $exePath -PassThru
Start-Sleep -Seconds 6
$mods = (Get-Process -Id $p.Id).Modules
"KilatX: $exePath"
"DLLs loaded: $($mods.Count)"
$foreign = @($mods | Where-Object { $_.FileName -ne $exePath -and $_.FileName -notlike "$env:WINDIR\*" })
"Not from Windows: $($foreign.Count)"
$foreign | ForEach-Object { "  $($_.FileName)  ($($_.FileVersionInfo.CompanyName))" }
$q = Start-Process $exePath -ArgumentList '/quit' -PassThru
$q.WaitForExit()

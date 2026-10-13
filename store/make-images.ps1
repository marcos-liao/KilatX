param([string]$Exe = "$PSScriptRoot\..\app\target\debug\KilatX.exe", [string]$Base = 'http://127.0.0.1:8766')
# Makes the pictures for the Chrome Web Store / Edge Add-ons listing in store\images\:
#   screenshot-1-downloads.png, screenshot-2-settings.png (1280x800), promo-small.png (440x280)
# Uses the debug build with its own instance name and a temporary data folder; needs the
# scenario server (app\testkit\scenario_server.py 8766 8767) for the example downloads.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class Shot {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
}
'@
[Shot]::SetProcessDPIAware() | Out-Null
$images = "$PSScriptRoot\images"
New-Item -ItemType Directory -Force $images | Out-Null
$work = "$env:TEMP\kilatx-store-shots"
$exePath = (Resolve-Path $Exe).Path
$env:KILATX_INSTANCE = '-store-shots'
$env:KILATX_PREVIEW_NOSELECT = '1'  # no detail panel: it would show this PC's folders
$bg = [Drawing.Color]::FromArgb(255, 24, 24, 27)

function Capture([string]$file) {
    $p = Get-Process KilatX | Where-Object { $_.Path -eq $exePath -and $_.MainWindowHandle -ne 0 } | Select-Object -First 1
    $c = New-Object Shot+RECT; [Shot]::GetClientRect($p.MainWindowHandle, [ref]$c) | Out-Null
    $win = New-Object Drawing.Bitmap $c.R, $c.B
    $g = [Drawing.Graphics]::FromImage($win); $dc = $g.GetHdc(); [Shot]::PrintWindow($p.MainWindowHandle, $dc, 3) | Out-Null; $g.ReleaseHdc($dc); $g.Dispose()
    # 1280x800 canvas, window centred
    $canvas = New-Object Drawing.Bitmap 1280, 800
    $g = [Drawing.Graphics]::FromImage($canvas); $g.Clear($bg)
    $scale = [math]::Min(1240.0 / $win.Width, 760.0 / $win.Height)
    $w = [int]($win.Width * $scale); $h = [int]($win.Height * $scale)
    $g.InterpolationMode = 'HighQualityBicubic'
    $g.DrawImage($win, [int]((1280 - $w) / 2), [int]((800 - $h) / 2), $w, $h)
    $g.Dispose(); $win.Dispose()
    $canvas.Save("$images\$file", [Drawing.Imaging.ImageFormat]::Png); $canvas.Dispose()
}
function Start-KilatX([hashtable]$settings, [string]$page = '') {
    if (Test-Path $work) { Remove-Item $work -Recurse -Force }
    New-Item -ItemType Directory -Force "$work\data", "$work\dl" | Out-Null
    $env:KILATX_DATA_DIR = "$work\data"
    $s = @{ connections = 8; check_updates = $false; browser_capture = $false; download = @{ base_folder = "$work\dl" } } + $settings
    [IO.File]::WriteAllText("$work\data\state.json", (@{ settings = $s } | ConvertTo-Json -Depth 6), (New-Object Text.UTF8Encoding $false))
    if ($page) { $env:KILATX_PREVIEW_PAGE = $page } else { Remove-Item Env:KILATX_PREVIEW_PAGE -ErrorAction SilentlyContinue }
    Start-Process $exePath | Out-Null
    Start-Sleep -Seconds 4
}
function Stop-KilatX { $q = Start-Process $exePath -ArgumentList '/quit' -PassThru; $q.WaitForExit(); Start-Sleep -Seconds 1 }

# 1. Downloads: some finished, some running with several connections
Start-KilatX @{}
foreach ($f in 'Annual-Report-2026.pdf', 'Podcast-Episode-42.mp3', 'project-assets.zip') {
    Start-Process $exePath -ArgumentList "/d $Base/plain/3000000/$f" -Wait
}
Start-Sleep -Seconds 4
foreach ($f in 'Holiday-Video-4K.mp4', 'ubuntu-24.04-desktop-amd64.iso', 'Design-Course-Lesson-07.mkv') {
    Start-Process $exePath -ArgumentList "/d $Base/plain/4000000000/$($f)?r=600000" -Wait
}
Start-Sleep -Seconds 6
Capture 'screenshot-1-downloads.png'
Stop-KilatX

# 2. Settings (General)
Start-KilatX @{} '0'
Capture 'screenshot-2-settings.png'
Stop-KilatX
Remove-Item Env:KILATX_PREVIEW_PAGE -ErrorAction SilentlyContinue
if (Test-Path $work) { Remove-Item $work -Recurse -Force }

# 3. Small promo tile 440x280: icon, name, one line
$tile = New-Object Drawing.Bitmap 440, 280
$g = [Drawing.Graphics]::FromImage($tile)
$g.SmoothingMode = 'AntiAlias'; $g.TextRenderingHint = 'AntiAliasGridFit'; $g.InterpolationMode = 'HighQualityBicubic'
$rect = New-Object Drawing.Rectangle 0, 0, 440, 280
$grad = New-Object Drawing.Drawing2D.LinearGradientBrush $rect, ([Drawing.Color]::FromArgb(255, 30, 41, 82)), ([Drawing.Color]::FromArgb(255, 17, 17, 24)), 45
$g.FillRectangle($grad, $rect)
$icon = [Drawing.Image]::FromFile((Resolve-Path "$PSScriptRoot\..\app\ui\icons\app-256.png").Path)
$g.DrawImage($icon, 40, 76, 128, 128)
$g.DrawString('KilatX', (New-Object Drawing.Font 'Segoe UI Semibold', 34), [Drawing.Brushes]::White, 186, 86)
$g.DrawString("Free downloads with`nmany connections", (New-Object Drawing.Font 'Segoe UI', 15), (New-Object Drawing.SolidBrush ([Drawing.Color]::FromArgb(255, 190, 200, 225))), 190, 142)
$g.Dispose(); $icon.Dispose()
$tile.Save("$images\promo-small.png", [Drawing.Imaging.ImageFormat]::Png); $tile.Dispose()
Copy-Item "$PSScriptRoot\..\app\extension\icons\icon128.png" "$images\icon-128.png"
Get-ChildItem $images | Select-Object Name, Length

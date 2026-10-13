param([string]$Exe = "$PSScriptRoot\..\target\debug\KilatX.exe", [string]$Out = "$env:TEMP\kilatx-repaint")
# Windows sometimes throws away what a window shows (screen off / sleep, lock, Remote Desktop,
# display change) and asks it to paint everything again (WM_PAINT with an update region).
# This test fakes that: it paints the KilatX window magenta, invalidates it the way Windows
# does, and counts the magenta pixels left after KilatX had time to repaint. Also checks
# showing the window again from the tray. Screenshots are saved in $Out.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class W {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref RECT p);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool InvalidateRect(IntPtr h, IntPtr r, bool erase);
  [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr h);
  [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr h, IntPtr dc);
  [DllImport("user32.dll")] public static extern int FillRect(IntPtr dc, ref RECT r, IntPtr brush);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("gdi32.dll")] public static extern IntPtr CreateSolidBrush(uint color);
  [DllImport("gdi32.dll")] public static extern bool DeleteObject(IntPtr o);
}
'@
if (Test-Path $Out) { Remove-Item $Out -Recurse -Force }
New-Item -ItemType Directory -Force "$Out\data" | Out-Null
$env:KILATX_INSTANCE = '-repaint-test'; $env:KILATX_DATA_DIR = "$Out\data"; $env:KILATX_EVENT_LOG = "$Out\events.log"
$exePath = (Resolve-Path $Exe).Path
$results = @()
function Check($name, $ok, $detail = '') { $script:results += [pscustomobject]@{ Test = $name; Result = $(if ($ok) { 'PASS' } else { 'FAIL' }); Detail = $detail } }
function Hwnd { (Get-Process KilatX -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exePath -and $_.MainWindowHandle -ne 0 } | Select-Object -First 1).MainWindowHandle }

# Client area screenshot; returns the share of magenta pixels (sampled).
function Magenta($name) {
    $h = Hwnd
    $c = New-Object W+RECT; [W]::GetClientRect($h, [ref]$c) | Out-Null
    # The window's own content (PW_CLIENTONLY | PW_RENDERFULLCONTENT), even if other windows cover it
    $bmp = New-Object Drawing.Bitmap $c.R, $c.B
    $g = [Drawing.Graphics]::FromImage($bmp); $dc = $g.GetHdc()
    [W]::PrintWindow($h, $dc, 3) | Out-Null
    $g.ReleaseHdc($dc); $g.Dispose()
    $bmp.Save("$Out\$name.png")
    $hit = 0; $n = 0
    for ($y = 2; $y -lt $bmp.Height; $y += 9) { for ($x = 2; $x -lt $bmp.Width; $x += 9) {
        $p = $bmp.GetPixel($x, $y); $n++
        if ($p.R -gt 240 -and $p.G -lt 20 -and $p.B -gt 240) { $hit++ }
    } }
    $bmp.Dispose()
    [math]::Round(100.0 * $hit / [math]::Max($n, 1), 1)
}
function Smear {
    $h = Hwnd
    $c = New-Object W+RECT; [W]::GetClientRect($h, [ref]$c) | Out-Null
    $dc = [W]::GetDC($h); $brush = [W]::CreateSolidBrush(0x00FF00FF)
    [W]::FillRect($dc, [ref]$c, $brush) | Out-Null
    [W]::DeleteObject($brush) | Out-Null; [W]::ReleaseDC($h, $dc) | Out-Null
}

$p = Start-Process $exePath -PassThru
Start-Sleep -Seconds 4
[W]::SetForegroundWindow((Hwnd)) | Out-Null
Start-Sleep -Milliseconds 500
$m0 = Magenta '0-start'
Check 'Window drawn at start' ($m0 -eq 0) "$m0% magenta"

Smear
Start-Sleep -Milliseconds 300
$m1 = Magenta '1-smeared'
# Taking the picture (PrintWindow) already makes Windows ask for a repaint, so a fixed KilatX
# may be clean here. Without the fix it stays 100% magenta through every step below.
"(after smearing: $m1% magenta)"

[W]::InvalidateRect((Hwnd), [IntPtr]::Zero, $false) | Out-Null
Start-Sleep -Seconds 1
$m2 = Magenta '2-after-repaint-request'
Check 'Windows asks for a repaint: everything drawn again' ($m2 -lt 1) "$m2% magenta left"

# Hidden to the tray, then shown again by a second launch
[W]::PostMessage((Hwnd), 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
Start-Sleep -Seconds 2
Start-Process $exePath | Out-Null
Start-Sleep -Seconds 3
[W]::SetForegroundWindow((Hwnd)) | Out-Null
Start-Sleep -Milliseconds 500
$m3 = Magenta '3-shown-again'
Check 'Shown again from the tray: fully drawn' ($m3 -eq 0) "$m3% magenta"

Smear
[W]::InvalidateRect((Hwnd), [IntPtr]::Zero, $false) | Out-Null
Start-Sleep -Seconds 1
$m4 = Magenta '4-second-repaint'
Check 'Second repaint request also handled' ($m4 -lt 1) "$m4% magenta left"

$before = $p.TotalProcessorTime
Start-Sleep -Seconds 5
$p.Refresh()
$cpu = ($p.TotalProcessorTime - $before).TotalSeconds
Check 'No repaint loop (idle CPU with the window open)' ($cpu -lt 0.5) "$([math]::Round($cpu,2)) s CPU in 5 s"

Start-Process $exePath -ArgumentList '/quit' -Wait
$results | Format-Table -AutoSize | Out-String -Width 200
"{0}/{1} passed" -f @($results | Where-Object Result -eq 'PASS').Count, $results.Count

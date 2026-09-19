# Draws Burrow's icon: a small treemap in the app's own palette on a dark
# rounded square, at every size Windows asks for, plus a multi-size .ico.
#
#   powershell -ExecutionPolicy Bypass -File scripts\make-icons.ps1

Add-Type -AssemblyName System.Drawing
$ErrorActionPreference = 'Stop'
$out = Join-Path $PSScriptRoot '..\icons'
New-Item -ItemType Directory -Force $out | Out-Null

function Colour($hex) { [System.Drawing.ColorTranslator]::FromHtml($hex) }

# Tiles on a 16x16 grid: x, y, w, h, colour. Squarified by hand, largest
# first, in the dark-theme palette steps the map uses.
$tiles = @(
    @(0, 0, 9, 10, '#3987e5'),   # program
    @(9, 0, 7, 6, '#d95926'),    # video
    @(9, 6, 4, 4, '#199e70'),    # image
    @(13, 6, 3, 4, '#c98500'),   # archive
    @(0, 10, 6, 6, '#008300'),   # game
    @(6, 10, 5, 6, '#9085e9'),   # disk image
    @(11, 10, 5, 3, '#d55181'),  # system
    @(11, 13, 5, 3, '#5c5b57')   # other
)

function Draw([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap $size, $size
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.Clear([System.Drawing.Color]::Transparent)

    # Rounded background.
    $r = [Math]::Max(2, [int]($size * 0.18))
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $path.AddArc(0, 0, 2 * $r, 2 * $r, 180, 90)
    $path.AddArc($size - 2 * $r - 1, 0, 2 * $r, 2 * $r, 270, 90)
    $path.AddArc($size - 2 * $r - 1, $size - 2 * $r - 1, 2 * $r, 2 * $r, 0, 90)
    $path.AddArc(0, $size - 2 * $r - 1, 2 * $r, 2 * $r, 90, 90)
    $path.CloseFigure()
    $g.FillPath((New-Object System.Drawing.SolidBrush (Colour '#0f1117')), $path)

    # The map, inset, with a hairline gap between tiles.
    $pad = [Math]::Max(1.5, $size * 0.12)
    $cell = ($size - 2 * $pad) / 16.0
    $gap = [Math]::Max(0.6, $size / 128.0)
    foreach ($t in $tiles) {
        $x = $pad + $t[0] * $cell + $gap / 2
        $y = $pad + $t[1] * $cell + $gap / 2
        $w = $t[2] * $cell - $gap
        $h = $t[3] * $cell - $gap
        $rect = New-Object System.Drawing.RectangleF $x, $y, $w, $h
        # Lit from the top left, like the map's cushions.
        $base = Colour $t[4]
        $light = [System.Drawing.Color]::FromArgb(255, [Math]::Min(255, $base.R + 40), [Math]::Min(255, $base.G + 40), [Math]::Min(255, $base.B + 40))
        $dark = [System.Drawing.Color]::FromArgb(255, [int]($base.R * 0.78), [int]($base.G * 0.78), [int]($base.B * 0.78))
        $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rect, $light, $dark, 45.0
        $g.FillRectangle($brush, $rect)
    }
    $g.Dispose()
    return $bmp
}

$pngs = @{}
foreach ($size in 16, 32, 48, 64, 128, 256) {
    $bmp = Draw $size
    $file = Join-Path $out "${size}x${size}.png"
    $bmp.Save($file, [System.Drawing.Imaging.ImageFormat]::Png)
    $pngs[$size] = [IO.File]::ReadAllBytes($file)
    $bmp.Dispose()
}
Copy-Item (Join-Path $out '256x256.png') (Join-Path $out 'icon.png') -Force

# A .ico is a directory of PNG images.
$sizes = 16, 32, 48, 64, 128, 256
$stream = New-Object IO.MemoryStream
$w = New-Object IO.BinaryWriter $stream
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
foreach ($s in $sizes) {
    $b = if ($s -ge 256) { 0 } else { $s }
    $w.Write([byte]$b); $w.Write([byte]$b); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32)
    $w.Write([uint32]$pngs[$s].Length); $w.Write([uint32]$offset)
    $offset += $pngs[$s].Length
}
foreach ($s in $sizes) { $w.Write($pngs[$s]) }
[IO.File]::WriteAllBytes((Join-Path $out 'icon.ico'), $stream.ToArray())
'icons written to ' + (Resolve-Path $out)

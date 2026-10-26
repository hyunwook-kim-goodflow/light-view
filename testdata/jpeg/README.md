# JPEG test samples

Made for light-view's tests with Windows' own JPEG encoder (GDI+ through
`System.Drawing`, quality 90): a horizontal red and vertical green gradient over blue 128.
Baseline, JFIF, YCbCr 4:2:0 (16x16 MCUs), standard Huffman tables.

| File | Size | Used for |
|---|---|---|
| `gradient-64x32.jpg` | 64x32, whole MCUs | lossless rotation through the worker |
| `gradient-65x33.jpg` | 65x33, partial MCUs on both edges | `NotPerfect` through the worker |

Re-create in Windows PowerShell 5.1:

```powershell
Add-Type -AssemblyName System.Drawing
function Make($w, $h, $path) {
    $bmp = New-Object System.Drawing.Bitmap $w, $h
    for ($y = 0; $y -lt $h; $y++) { for ($x = 0; $x -lt $w; $x++) {
        $bmp.SetPixel($x, $y, [System.Drawing.Color]::FromArgb([int]($x * 255 / $w), [int]($y * 255 / $h), 128))
    } }
    $codec = [System.Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() | Where-Object { $_.MimeType -eq 'image/jpeg' }
    $ep = New-Object System.Drawing.Imaging.EncoderParameters 1
    $ep.Param[0] = New-Object System.Drawing.Imaging.EncoderParameter ([System.Drawing.Imaging.Encoder]::Quality), ([long]90)
    $bmp.Save($path, $codec, $ep)
    $bmp.Dispose()
}
Make 64 32 "$PWD\gradient-64x32.jpg"
Make 65 33 "$PWD\gradient-65x33.jpg"
```

The exact bytes depend on the Windows version; the tests only rely on the size and the
4:2:0 sampling.

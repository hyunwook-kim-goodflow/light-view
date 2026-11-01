+++
title = "Download"
description = "Download the light-view installer or portable zip, SHA-256 checksums and the code signing policy"
template = "download.html"

[extra]
intro = "light-view is a free photo viewer for Windows. It opens iPhone photos (HEIC, HEIF) as well as JPEG, PNG, GIF, WebP and BMP, flips through a folder with the arrow keys, rotates without loss and resizes photos for upload."
+++

## Installer: two ways to install

You choose on the installer's first page. The default is **for me only**.

| | For me only (default) | For all users |
|---|---|---|
| Administrator rights | Not needed | Needed (UAC prompt) |
| Folder | `%LOCALAPPDATA%\Programs\light-view` | `C:\Program Files\light-view` |
| HEIC thumbnails in Explorer | Yes | Yes |
| Explorer preview pane | No | Yes |
| Details tab (size, date taken, camera) | No | Yes |

Both add a Start menu shortcut and "Open with" entries and can be uninstalled. Windows 10/11 do not let a program make itself the default app, so pick light-view under **Open default apps settings** on the installer's last page.

## Portable zip

Unzip it anywhere and run `light-view.exe`. Settings go to a `data` folder next to the exe, and it normally leaves the registry alone. No Explorer thumbnails.

## If "Windows protected your PC" appears

Releases before code signing can show a SmartScreen window on first run. Check the checksum below first, then choose **More info** → **Run anyway**. If a file unpacked from the zip shows a warning, tick **Unblock** in the zip file's Properties and unpack it again.

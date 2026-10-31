+++
title = "Frequently asked questions"
description = "Common questions about light-view: HEIC, cost, safety, SmartScreen, install modes, uninstalling"
template = "faq.html"
+++

## Why won't Windows open HEIC (iPhone) photos?

HEIC uses HEVC compression, and Windows needs Microsoft's paid "HEVC Video Extensions" from the Store to decode it. light-view ships its own decoders (libheif, libde265), so it opens HEIC and HEIF without that extension.

## Is it really free?

Yes. light-view is open source (MIT or Apache-2.0) with no paid features. The app has no ads.

## Is it safe?

Photos are decoded (C code) only in a separate worker process (`lv-worker.exe`) with reduced rights: low integrity, an AppContainer where possible, no child processes and no windows. One bad photo file cannot put the app or your PC at risk that way. The app collects no personal data ([privacy policy](@/privacy/_index.en.md)).

## Windows says "Windows protected your PC".

Releases before code signing can show SmartScreen on first run. Check the SHA-256 checksum on the [download page](@/download/_index.en.md) first, then choose **More info** → **Run anyway**. From the 1.0 release on, files are code-signed through SignPath Foundation ([Code signing policy](@/code-signing/_index.en.md)).

## What is the difference between "for me only" and "for all users"?

**For me only** (the default) installs for your account without administrator rights and shows HEIC thumbnails in Explorer. **For all users** needs administrator rights and also fills Explorer's preview pane and the file Details tab (size, date taken, camera).

## How do I use the portable zip?

Unzip it anywhere and run `light-view.exe`. Settings go to a `data` folder next to the exe. Before deleting the folder, run `light-view.exe --uninstall-cleanup` once.

## How do I uninstall it?

Remove light-view under Windows Settings → Apps. Settings (`%APPDATA%\light-view`) and crash records (`%LOCALAPPDATA%\light-view\crashes`) stay; delete them yourself if you do not need them.

## Where is the official site?

Only **lightview.goodflow.dev** and **github.com/hyunwook-kim-goodflow/light-view**. Do not run files from look-alike sites or search ads.

## How do I report a problem or help?

Open a [GitHub issue](https://github.com/hyunwook-kim-goodflow/light-view/issues). Report security problems as described in the repository's [SECURITY.md](https://github.com/hyunwook-kim-goodflow/light-view/blob/main/SECURITY.md).

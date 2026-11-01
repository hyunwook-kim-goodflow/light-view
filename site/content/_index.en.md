+++
title = "light-view"
description = "A light, free Windows photo viewer that opens iPhone photos (HEIC) too"

[extra]
headline = "A light, <mark>free</mark> Windows photo viewer<br>that opens iPhone photos (HEIC) too"
lede = "Windows usually needs a paid HEVC extension to show HEIC photos. light-view opens them without it: free, fast and safe."
features_title = "What it does"
features = [
  { title = "iPhone photos for free", body = "Opens HEIC and HEIF without the paid HEVC extension, and JPEG, PNG, GIF, WebP and BMP too." },
  { title = "Flip through a folder", body = "Double-click a photo and it opens; ← and → go through the folder. Zoom with the wheel or a double-click." },
  { title = "Lossless rotation", body = "Rotate with Ctrl+← / Ctrl+→ and save with Ctrl+S. JPEGs are rotated without losing quality." },
  { title = "Delete with undo", body = "Delete sends a photo to the Recycle Bin; Ctrl+Z brings it back." },
  { title = "Resize for upload", body = "Shrink photos to a resolution, file size or scale. Location data is removed by default." },
  { title = "HEIC thumbnails in Explorer", body = "The installed copy shows HEIC thumbnails in Explorer. The all-users install also fills the preview pane and the Details tab." },
]
safety_title = "Safe by design"
safety = [
  "Photos are decoded (C code) only in a separate worker process with reduced rights, so one bad file cannot put the app at risk.",
  "The app has no ads and no tracking. Its only network use is an update check you turn on yourself.",
  "Every release comes with SHA-256 checksums. Releases are not code-signed yet.",
  "Open source (MIT or Apache-2.0). The code is on GitHub.",
]
faq_title = "FAQ"
faq = [
  { q = "Is it really free?", a = "Yes. It is an open-source program with no ads and no paid features." },
  { q = "Windows says \"Windows protected your PC\".", a = "Releases are not code-signed yet, so this can show on first run. Check the checksum, then choose More info → Run anyway." },
  { q = "Where is the official site?", a = "Only lightview.goodflow.dev and github.com/hyunwook-kim-goodflow/light-view. Do not run copies from anywhere else." },
]
+++

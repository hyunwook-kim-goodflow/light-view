+++
title = "HEIC thumbnails missing in Explorer"
description = "If iPhone photos show as blank icons in Explorer, light-view's installed copy draws their thumbnails."
weight = 4
+++

## The installed copy shows thumbnails

light-view's installer registers a handler that draws HEIC and HEIF thumbnails in Explorer. Both **For me only** and **For all users** get thumbnails.

The **For all users** install also shows HEIC photos in Explorer's preview pane and fills the **Details** tab in a file's properties with the size, date taken and camera. Location data is never exposed.

The portable copy does not touch the registry, so it adds no Explorer thumbnails.

## Still blank icons after installing

Explorer remembers thumbnails it has drawn. Photos it remembered as blank icons before light-view was installed can stay that way.

1. Run **Disk Cleanup** from the Start menu.
2. Tick **Thumbnails** and clean up.
3. Open the folder again; the thumbnails are drawn afresh.

## If you used another HEIC tool

If another program had registered a HEIC handler, light-view saves it when installing and puts it back when uninstalled. Updating or uninstalling never closes or restarts Explorer.

+++
title = "How to remove location data before sharing photos"
description = "Phone photos can carry the GPS coordinates of where they were taken. Here is how to remove them with light-view before you share."
weight = 3
+++

## What the problem is

Photo files from a phone carry EXIF data, and if location services were on, that includes latitude and longitude. When a photo is sent or uploaded as a file, the coordinates can go with it.

## How to remove it

1. Open the **Resize** screen: in the installed copy, select photos in Explorer, right-click → **Resize with light-view**; in the portable copy, drop photos on the light-view window and choose **Resize**.
2. Check that **Remove location** under **Metadata** is ticked. It is on by default.
3. To keep the size, enter `100`% on the **Scale** tab, or shrink the photos to suit where they are going.
4. Press **Save**. A new file such as `name_small.jpg` appears in the same folder; the original stays as it was.
5. Share the new `_small` file.

{{<shot name="resize" alt="The Resize screen with Remove location ticked on the Metadata row, above the Estimate list." lang={lang} />}}

## What stays and what goes

What stays is what helps you sort photos:

- the date taken and the time zone
- the camera make and model, and the lens
- exposure time, aperture, ISO and focal length

Location and everything else is left out. The photo is saved upright.

## How to check

Open the saved file in light-view and press **I** for the info card. **Show all** lists every tag, so you can see that there is no location (GPS) entry.

{{<shot name="exif" alt="The info card opened with I: the date taken, the time and camera, the exposure, the size and file size, and a Show all button below." lang={lang} />}}

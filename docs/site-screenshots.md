# Website screenshots

`site/static/img/shots/` holds the app screenshots on the home page: `viewer`, `exif` and
`resize`, each in Korean (no suffix) and English (`-en`), 1262 and 760 px wide, WebP.

## How they were made (2026-11-01)

- App: the published v0.2.0 portable release (`light-view-0.2.0-portable.zip`, checked
  against `SHA256SUMS.txt`), with `language` set to `"auto"` (Korean on this PC) or `"en"`
  in `data/settings.toml`.
- Window: 1280 px wide at 100% scaling, captured with `PrintWindow(PW_RENDERFULLCONTENT)` and
  cropped to the visible frame (`DWMWA_EXTENDED_FRAME_BOUNDS`).
- `viewer`: `frontenac.jpg` opened from a folder of the six photos below (toolbar showing).
  `exif`: `rome.jpg`, then the I key. `resize`: `light-view.exe --resize` with all six.
- Converted to WebP (quality 82) with Pillow.

## Photos in the screenshots

All CC0 (public domain dedication) from Wikimedia Commons; no attribution is required, but
the home page credits the photographers.

| File | Source | Author | License | Size |
|---|---|---|---|---|
| `bananas.jpg` | [Bunch of bananas on sale.jpg](https://commons.wikimedia.org/wiki/File:Bunch_of_bananas_on_sale.jpg) | Wilfredor | CC0 | 4165x2767 |
| `frontenac.jpg` | [Chateau Frontenac at dusk in Quebec City.jpg](https://commons.wikimedia.org/wiki/File:Chateau_Frontenac_at_dusk_in_Quebec_City.jpg) | Wilfredor | CC0 | 7541x5091 |
| `paella.jpg` | [Cooking a paella.jpg](https://commons.wikimedia.org/wiki/File:Cooking_a_paella.jpg) | Jebulon | CC0 | 3344x2224 |
| `laguna.jpg` | [Laguna de Raya 2.jpg](https://commons.wikimedia.org/wiki/File:Laguna_de_Raya_2.jpg) | Wilfredor | CC0 | 3680x2539 |
| `nymphaea.jpg` | [Nymphaea alba, Alhambra, Granada, Spain.jpg](https://commons.wikimedia.org/wiki/File:Nymphaea_alba,_Alhambra,_Granada,_Spain.jpg) | Jebulon | CC0 | 3902x2894 |
| `rome.jpg` | [Saint Peter's Basilica, Sant'Angelo bridge, by night, Rome, Italy.jpg](https://commons.wikimedia.org/wiki/File:Saint_Peter%27s_Basilica,_Sant%27Angelo_bridge,_by_night,_Rome,_Italy.jpg) | Jebulon | CC0 | 4592x3056 |

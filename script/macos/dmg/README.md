# Galaxy disk-image artwork

`background.svg` is the editable source. Its header uses the exact macOS app
artwork at `app/assets/bundled/png/galaxy.png` through a relative image reference,
without storing a second copy of that source image. The committed PNG exports
are 700 × 500
and 1400 × 1000 pixels; `dmgbuild` combines them into a Retina TIFF inside the
image. They are installer resources, not app resources.

To reproduce the exports using CairoSVG 2.9.1 and DejaVu Sans, run from the
repository root. The `--unsafe` flag lets this trusted, tracked SVG load its local
PNG reference; keep the SVG and canonical artwork in their repository locations.

```sh
cairosvg --unsafe script/macos/dmg/background.svg -o script/macos/dmg/background.png -s 1
cairosvg --unsafe script/macos/dmg/background.svg -o script/macos/dmg/background@2x.png -s 2
```

The real Finder app and Applications icons sit at (170, 250) and (530, 250),
with 112-point icons in a 700 × 500-point content area. The background deliberately
leaves those areas empty. Labels come from Finder, not the artwork.
Light caption strips preserve contrast for Finder's dark filename labels; the
layout does not rely on Finder inferring white text from a dark background.

`prepare_dmg_tools` installs exact, hash-checked wheels into a job-owned Python
environment before signing begins. `create_dmg.py` creates the layout without
Finder or AppleScript. Both official CI and the local bundle script use it.
The app is copied with `ditto`; official builds verify its signature again inside
the mounted image before the final DMG is signed, notarized, and stapled.

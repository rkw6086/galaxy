# Galaxy installer branding

The Windows app icon, launcher PNG, and both installer BMPs use the same artwork
as macOS: `app/assets/bundled/png/galaxy.png`. The three-star SVG is reserved for
buttons; it is not the application or installer branding.

To regenerate from the repository root, use a private environment with Pillow
13.0.0 and run:

```sh
python script/export_galaxy_branding.py
```

The exporter also uses the existing bundled Roboto font. It keeps Inno Setup's
58 × 58 and 202 × 385 dimensions and creates 24-bit BMPs. The ICO includes 16,
24, 32, 48, 64, 128, and 256 pixel sizes with transparency. These are committed
build resources; packaging does not install Pillow or regenerate artwork.

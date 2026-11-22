#!/usr/bin/env python3
"""Export launcher and Windows installer assets from the macOS Galaxy artwork.

Maintainer-only tool; requires Pillow 13.0.0. Nothing is generated at app startup.
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont


def main():
    root = Path(__file__).resolve().parent.parent
    source = Image.open(root / "app/assets/bundled/png/galaxy.png").convert("RGBA")
    icons = root / "app/channels/oss/icon/no-padding"
    installer = root / "script/windows/installer-images"
    source.resize((512, 512), Image.Resampling.LANCZOS).save(icons / "512x512.png", optimize=True)
    source.save(
        icons / "icon.ico",
        sizes=[(size, size) for size in (16, 24, 32, 48, 64, 128, 256)],
    )

    # Inno Setup's header uses a white background; flatten alpha for portable
    # 24-bit BMP decoding. The sidebar uses Galaxy's navy background instead.
    small = Image.new("RGBA", (58, 58), "white")
    small.alpha_composite(source.resize(small.size, Image.Resampling.LANCZOS))
    small.convert("RGB").save(installer / "galaxy-logo.bmp")

    banner = Image.new("RGBA", (202, 385), "#061329")
    banner.alpha_composite(source.resize((170, 170), Image.Resampling.LANCZOS), (16, 88))
    font = ImageFont.truetype(str(root / "app/assets/bundled/fonts/roboto/Roboto-Medium.ttf"), 22)
    ImageDraw.Draw(banner).text((101, 274), "Galaxy", font=font, fill="#d3e7ff", anchor="mt")
    banner.convert("RGB").save(installer / "galaxy-banner.bmp")


if __name__ == "__main__":
    main()

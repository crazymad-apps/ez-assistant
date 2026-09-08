"""从 120 张透明 PNG 导出网页源素材；仅重新导出时需要 Pillow，不参与普通应用构建。"""

from pathlib import Path
import sys

from PIL import Image

if len(sys.argv) != 2:
    raise SystemExit("Usage: python3 scripts/prepare-rocket-assets.py <PNG frames directory>")

source = Path(sys.argv[1])
output = Path(__file__).resolve().parents[1] / "src/features/runtime-access/WorkspaceTransition/assets"
output.mkdir(parents=True, exist_ok=True)
for sheet in range(10):
    with Image.new("RGBA", (4096, 1920)) as atlas:
        for offset in range(12):
            frame = sheet * 12 + offset + 1
            with Image.open(source / f"{frame:04d}.png") as image:
                if image.size != (1024, 640) or image.mode != "RGBA":
                    raise ValueError(f"Unexpected frame: {frame}")
                if frame in (1, 120) and image.getchannel("A").getextrema() != (0, 0):
                    raise ValueError(f"First and last frames must be transparent: {frame}")
                atlas.paste(image, ((offset % 4) * 1024, (offset // 4) * 640))
        atlas.save(output / f"rocket-{sheet + 1:02d}.webp", quality=90, method=6)

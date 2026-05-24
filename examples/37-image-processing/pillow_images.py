from __future__ import annotations
from pathlib import Path
from PIL import Image, ImageFilter, ImageOps


def thumbnail(src: Path, dest: Path, max_side: int = 256) -> None:
    img: Image.Image = Image.open(src)
    img.thumbnail((max_side, max_side))
    img.save(dest)


def grayscale_with_edges(src: Path, dest: Path) -> None:
    img: Image.Image = Image.open(src).convert("L")
    edges: Image.Image = img.filter(ImageFilter.FIND_EDGES)
    edges.save(dest)


def center_crop(src: Path, dest: Path, size: int = 224) -> None:
    img: Image.Image = Image.open(src)
    w: int = img.width
    h: int = img.height
    lo_x: int = (w - size) // 2
    lo_y: int = (h - size) // 2
    cropped: Image.Image = img.crop((lo_x, lo_y, lo_x + size, lo_y + size))
    cropped.save(dest)


def make_collage(sources: list[Path], dest: Path, tile: int = 128) -> None:
    cols: int = 3
    rows: int = (len(sources) + cols - 1) // cols
    canvas: Image.Image = Image.new("RGB", (cols * tile, rows * tile), (240, 240, 240))
    for i, p in enumerate(sources):
        img: Image.Image = ImageOps.fit(Image.open(p), (tile, tile))
        cx: int = i % cols * tile
        cy: int = i // cols * tile
        canvas.paste(img, (cx, cy))
    canvas.save(dest)


def make_sample(path: Path) -> None:
    img: Image.Image = Image.new("RGB", (512, 384), (100, 150, 220))
    img.save(path)


def main() -> None:
    out: Path = Path("/tmp/typhon-images")
    out.mkdir(parents=True, exist_ok=True)
    sample: Path = out / "sample.png"
    make_sample(sample)
    thumbnail(sample, out / "thumb.png", 128)
    grayscale_with_edges(sample, out / "edges.png")
    center_crop(sample, out / "centre.png", size=200)
    samples: list[Path] = [sample] * 6
    make_collage(samples, out / "collage.png", tile=96)
    print(f"images written to {out}/")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Generate EyesCare PNG + classic ICO 3.00 (BMP/DIB) icons. Stdlib only."""
from __future__ import annotations

import math
import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "apps" / "desktop" / "src-tauri" / "icons"


def sample(nx: float, ny: float) -> tuple[int, int, int, int]:
    """Unit-square coords in [-1, 1]. Warm amber eye on a circular badge."""
    r = math.hypot(nx, ny)
    if r > 1.0:
        return (0, 0, 0, 0)
    amber = (232, 148, 24)
    amber_dark = (196, 112, 12)
    cream = (255, 244, 228)
    iris = (92, 48, 16)
    pupil = (28, 16, 8)
    highlight = (255, 255, 255)

    # radial badge
    t = min(1.0, r)
    bg_r = int(amber[0] * (1 - 0.18 * t) + amber_dark[0] * 0.18 * t)
    bg_g = int(amber[1] * (1 - 0.18 * t) + amber_dark[1] * 0.18 * t)
    bg_b = int(amber[2] * (1 - 0.18 * t) + amber_dark[2] * 0.18 * t)
    alpha = 255
    if r > 0.92:
        alpha = int(255 * (1.0 - r) / 0.08)

    # almond eye
    eye_x = nx / 0.62
    eye_y = ny / 0.28
    eye_r = eye_x * eye_x + eye_y * eye_y
    if eye_r <= 1.05:
        edge = max(0.0, min(1.0, (1.05 - eye_r) / 0.12))
        col = mix(amber, cream, 0.15 + 0.85 * edge)
        # iris
        ir = math.hypot(nx / 0.22, (ny + 0.02) / 0.22)
        if ir <= 1.0:
            col = mix(iris, (140, 78, 28), max(0.0, 1.0 - ir))
        if ir <= 0.48:
            col = pupil
        # highlight
        if math.hypot(nx - 0.07, ny + 0.08) < 0.07:
            col = highlight
        if eye_r > 0.92:
            col = mix((bg_r, bg_g, bg_b), col, edge)
        bg_r, bg_g, bg_b = col

    return (clamp(bg_r), clamp(bg_g), clamp(bg_b), clamp(alpha))


def mix(a, b, t: float):
    t = max(0.0, min(1.0, t))
    return (
        int(a[0] + (b[0] - a[0]) * t),
        int(a[1] + (b[1] - a[1]) * t),
        int(a[2] + (b[2] - a[2]) * t),
    )


def clamp(v: int) -> int:
    return 0 if v < 0 else 255 if v > 255 else v


def raster(size: int) -> list[tuple[int, int, int, int]]:
    px = []
    for y in range(size):
        for x in range(size):
            nx = (x + 0.5) / size * 2.0 - 1.0
            ny = (y + 0.5) / size * 2.0 - 1.0
            px.append(sample(nx, ny))
    return px


def write_png(path: Path, size: int, pixels) -> None:
    raw = bytearray()
    for y in range(size):
        raw.append(0)
        for x in range(size):
            raw.extend(pixels[y * size + x])

    def chunk(tag: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    path.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


def ico_dib(size: int, pixels) -> bytes:
    xor_size = size * size * 4
    and_row = ((size + 31) // 32) * 4
    and_size = and_row * size
    header = struct.pack(
        "<IIIHHIIIIII",
        40,
        size,
        size * 2,
        1,
        32,
        0,
        xor_size + and_size,
        0,
        0,
        0,
        0,
    )
    xor = bytearray()
    mask = bytearray()
    for y in range(size - 1, -1, -1):
        row_bits = []
        for x in range(size):
            r, g, b, a = pixels[y * size + x]
            xor.extend((b, g, r, a))
            row_bits.append(1 if a < 128 else 0)
        row = bytearray()
        acc = 0
        n = 0
        for bit in row_bits:
            acc = (acc << 1) | bit
            n += 1
            if n == 8:
                row.append(acc)
                acc = 0
                n = 0
        if n:
            row.append(acc << (8 - n))
        row.extend(b"\x00" * (and_row - len(row)))
        mask.extend(row)
    return header + xor + mask


def write_ico(path: Path, sizes: list[int]) -> None:
    images = []
    for s in sizes:
        images.append(ico_dib(s, raster(s)))
    count = len(images)
    offset = 6 + 16 * count
    buf = bytearray(struct.pack("<HHH", 0, 1, count))
    for s, data in zip(sizes, images):
        w = 0 if s >= 256 else s
        buf.extend(
            struct.pack(
                "<BBBBHHII",
                w,
                w,
                0,
                0,
                1,
                32,
                len(data),
                offset,
            )
        )
        offset += len(data)
    for data in images:
        buf.extend(data)
    path.write_bytes(buf)


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    write_png(OUT / "32x32.png", 32, raster(32))
    write_png(OUT / "128x128.png", 128, raster(128))
    write_png(OUT / "icon.png", 256, raster(256))
    write_ico(OUT / "icon.ico", [16, 32, 48, 256])
    ico = (OUT / "icon.ico").read_bytes()
    reserved, kind, count = struct.unpack_from("<HHH", ico)
    assert reserved == 0 and kind == 1 and count >= 1, (reserved, kind, count)
    print(f"wrote icons to {OUT}")
    print(f"icon.ico {len(ico)} bytes, type={kind}, images={count}")


if __name__ == "__main__":
    main()

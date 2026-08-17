#!/usr/bin/env python3
"""Generate EyesCare icons: Eye of Horus (Wedjat) on a dark gold plaque."""
from __future__ import annotations

import math
import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "apps" / "desktop" / "src-tauri" / "icons"

INK = (18, 16, 13)
INK_EDGE = (10, 9, 7)
GOLD = (228, 176, 74)
GOLD_HI = (245, 214, 138)
GOLD_LO = (168, 118, 32)
CREAM = (255, 244, 224)
IRIS = (196, 118, 28)
PUPIL = (28, 18, 10)
HIGHLIGHT = (255, 252, 245)


def clamp(v: int) -> int:
    return 0 if v < 0 else 255 if v > 255 else v


def mix(a: tuple[int, int, int], b: tuple[int, int, int], t: float):
    t = max(0.0, min(1.0, t))
    return (
        int(a[0] + (b[0] - a[0]) * t),
        int(a[1] + (b[1] - a[1]) * t),
        int(a[2] + (b[2] - a[2]) * t),
    )


def sdf_round_box(x: float, y: float, hx: float, hy: float, r: float) -> float:
    ax, ay = abs(x) - hx + r, abs(y) - hy + r
    ox, oy = max(ax, 0.0), max(ay, 0.0)
    return math.hypot(ox, oy) + min(max(ax, ay), 0.0) - r


def sdf_circle(x: float, y: float, cx: float, cy: float, r: float) -> float:
    return math.hypot(x - cx, y - cy) - r


def sdf_segment(px: float, py: float, ax: float, ay: float, bx: float, by: float) -> float:
    pax, pay = px - ax, py - ay
    bax, bay = bx - ax, by - ay
    h = max(0.0, min(1.0, (pax * bax + pay * bay) / (bax * bax + bay * bay + 1e-9)))
    return math.hypot(pax - bax * h, pay - bay * h)


def sdf_polyline(px: float, py: float, pts: list[tuple[float, float]]) -> float:
    d = 1e9
    for i in range(len(pts) - 1):
        d = min(d, sdf_segment(px, py, pts[i][0], pts[i][1], pts[i + 1][0], pts[i + 1][1]))
    return d


def horus_path() -> list[tuple[float, float]]:
    """Markings under the eye: vertical falcon cheek + spiral curl."""
    return [
        (0.02, 0.18),
        (0.03, 0.42),
        (0.02, 0.58),
        (0.10, 0.70),
        (0.22, 0.68),
        (0.28, 0.56),
        (0.20, 0.48),
        (0.12, 0.54),
    ]


def sample(nx: float, ny: float, size: int) -> tuple[int, int, int, int]:
    compact = size <= 48
    w_brow = 0.09 if compact else 0.055
    w_mark = 0.09 if compact else 0.048
    plaque = sdf_round_box(nx, ny, 0.86, 0.86, 0.28 if compact else 0.22)
    if plaque > 0.08:
        return (0, 0, 0, 0)

    t = max(0.0, min(1.0, (plaque + 0.86) / 1.2))
    bg = mix(INK_EDGE, INK, 1.0 - t * 0.35)
    if not compact and abs(plaque + 0.06) < 0.018:
        bg = mix(bg, GOLD_LO, 0.55)

    col = bg
    cover = lambda d, w: max(0.0, min(1.0, 0.5 - d / max(w, 1e-4)))

    brow = sdf_polyline(nx, ny, [(-0.58, -0.38), (-0.18, -0.46), (0.22, -0.44), (0.56, -0.34)])
    k = cover(brow, w_brow)
    col = mix(col, GOLD, k)
    if not compact:
        col = mix(col, GOLD_HI, k * cover(brow, 0.018) * 0.6)

    upper = sdf_circle(nx, ny, 0.0, 0.22, 0.58)
    lower = sdf_circle(nx, ny, 0.0, -0.34, 0.62)
    almond = max(upper, lower)
    eye = cover(almond, 0.04 if compact else 0.03)
    col = mix(col, mix(GOLD_LO, CREAM, 0.88), eye)
    col = mix(col, GOLD, cover(abs(almond) - 0.01, 0.036 if compact else 0.028) * eye)

    iris_r = 0.24 if compact else 0.20
    ir = sdf_circle(nx, ny, -0.02, -0.02, iris_r)
    col = mix(col, mix(IRIS, GOLD, 0.25), cover(ir, 0.03) * eye)
    col = mix(col, PUPIL, cover(sdf_circle(nx, ny, -0.02, -0.02, 0.10 if compact else 0.09), 0.02) * eye)
    if not compact:
        col = mix(col, HIGHLIGHT, cover(sdf_circle(nx, ny, 0.05, -0.08, 0.045), 0.02) * eye)

    path = [(-0.00, 0.20), (0.04, 0.58), (0.22, 0.66), (0.18, 0.50)] if compact else horus_path()
    mark = sdf_polyline(nx, ny, path)
    mk = cover(mark, w_mark)
    col = mix(col, GOLD, mk)

    flare = sdf_segment(nx, ny, 0.48, 0.02, 0.64, 0.12)
    col = mix(col, GOLD, cover(flare, 0.055 if compact else 0.04))

    alpha = 255
    if plaque > 0.0:
        alpha = int(255 * max(0.0, 1.0 - plaque / 0.08))
    return (clamp(col[0]), clamp(col[1]), clamp(col[2]), clamp(alpha))


def raster(size: int) -> list[tuple[int, int, int, int]]:
    px = []
    for y in range(size):
        for x in range(size):
            nx = (x + 0.5) / size * 2.0 - 1.0
            ny = (y + 0.5) / size * 2.0 - 1.0
            px.append(sample(nx, ny, size))
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
    images = [ico_dib(s, raster(s)) for s in sizes]
    count = len(images)
    offset = 6 + 16 * count
    buf = bytearray(struct.pack("<HHH", 0, 1, count))
    for s, data in zip(sizes, images):
        w = 0 if s >= 256 else s
        buf.extend(struct.pack("<BBBBHHII", w, w, 0, 0, 1, 32, len(data), offset))
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
    print(f"wrote icons to {OUT}")


if __name__ == "__main__":
    main()

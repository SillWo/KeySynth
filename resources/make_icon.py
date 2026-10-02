"""Build the committed multi-resolution ICO from the matching KeySynth SVG artwork.

Uses only Python's standard library. Run from the repository root:
    python resources/make_icon.py
"""

import struct
import zlib
import binascii
from pathlib import Path


ROOT = Path(__file__).resolve().parent
SIZES = (16, 32, 48, 256)


def rounded_rect(x, y, left, top, right, bottom, radius):
    cx = min(max(x, left + radius), right - radius)
    cy = min(max(y, top + radius), bottom - radius)
    return (x - cx) ** 2 + (y - cy) ** 2 <= radius**2


def blend(base, top, alpha):
    return tuple(round(a * (1 - alpha) + b * alpha) for a, b in zip(base, top))


def pixel(x, y, size):
    scale = size / 256.0
    px, py = x / scale, y / scale
    if not rounded_rect(px, py, 8, 8, 248, 248, 54):
        return (0, 0, 0, 0)

    t = min(1.0, max(0.0, py / 256.0))
    color = tuple(round(a * (1 - t) + b * t) for a, b in zip((49, 136, 245), (20, 85, 199)))

    # Golden eighth note: curved note head, stem and flag.
    head_x, head_y = px - 138, py - 122
    if (head_x / 15) ** 2 + (head_y / 10) ** 2 < 1:
        color = blend(color, (255, 226, 122), 1)
    if 151 <= px <= 158 and 45 <= py <= 118:
        color = blend(color, (255, 226, 122), 1)
    if 152 <= px <= 190 and 40 <= py <= 48:
        color = blend(color, (255, 226, 122), 1)
    if 183 <= px <= 190 and 40 <= py <= 83:
        color = blend(color, (255, 226, 122), 1)
    if 151 <= px < 190 and 45 <= py <= 51 + (190 - px) * 0.5:
        color = blend(color, (255, 226, 122), 1)

    if rounded_rect(px, py, 34, 137, 222, 215, 12):
        color = (249, 251, 255)
        for line in (61, 88, 115, 142, 169, 196):
            if abs(px - line) <= 2:
                color = (184, 203, 234)
        for left in (53, 80, 134, 161, 188):
            if left <= px <= left + 16 and 137 <= py <= 174:
                color = (22, 51, 95)
    return (*color, 255)


def render(size):
    # Four coverage samples in each dimension give clean edges at small sizes.
    pixels = bytearray(size * size * 4)
    samples = 4
    for y in range(size):
        for x in range(size):
            sums = [0, 0, 0, 0]
            for sy in range(samples):
                for sx in range(samples):
                    rgba = pixel(x + (sx + 0.5) / samples, y + (sy + 0.5) / samples, size)
                    for channel in range(4):
                        sums[channel] += rgba[channel]
            offset = (y * size + x) * 4
            pixels[offset : offset + 4] = bytes(round(value / (samples * samples)) for value in sums)
    return pixels


def dib_image(size, rgba):
    row_bytes = size * 4
    xor = bytearray()
    mask_row_bytes = ((size + 31) // 32) * 4
    mask = bytearray(mask_row_bytes * size)
    for y in range(size - 1, -1, -1):
        for x in range(size):
            red, green, blue, alpha = rgba[(y * size + x) * 4 : (y * size + x + 1) * 4]
            xor.extend((blue, green, red, alpha))
            if alpha < 128:
                mask[(size - 1 - y) * mask_row_bytes + x // 8] |= 0x80 >> (x % 8)
    header = struct.pack(
        "<IiiHHIIiiII",
        40,
        size,
        size * 2,
        1,
        32,
        0,
        row_bytes * size,
        0,
        0,
        0,
        0,
    )
    return header + xor + mask


def png_chunk(kind, data):
    body = kind + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", binascii.crc32(body) & 0xFFFFFFFF)


def png_image(size, rgba):
    scanlines = bytearray()
    row_size = size * 4
    for y in range(size):
        scanlines.append(0)  # PNG filter type None.
        start = y * row_size
        scanlines.extend(rgba[start : start + row_size])
    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", header)
        + png_chunk(b"IDAT", zlib.compress(scanlines, level=9))
        + png_chunk(b"IEND", b"")
    )


def main():
    images = []
    for size in SIZES:
        rgba = render(size)
        data = png_image(size, rgba) if size == 256 else dib_image(size, rgba)
        images.append((size, data))
    header = struct.pack("<HHH", 0, 1, len(images))
    entries = bytearray()
    offset = 6 + 16 * len(images)
    for size, data in images:
        entries.extend(
            struct.pack(
                "<BBBBHHII",
                0 if size == 256 else size,
                0 if size == 256 else size,
                0,
                0,
                1,
                32,
                len(data),
                offset,
            )
        )
        offset += len(data)
    output = ROOT / "keysynth.ico"
    output.write_bytes(header + entries + b"".join(data for _, data in images))
    print(f"Wrote {output} ({output.stat().st_size} bytes)")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Writes a Windows .ico file from PNGs: make_ico.py out.ico 16.png 32.png ... 256.png

An .ico file is a 6-byte header (reserved 0, type 1, image count), one
16-byte entry per image (width, height, colour count, reserved, planes,
bits per pixel, data size, data offset), then the image data. Windows
Vista and newer read PNG data directly, so each image is stored as is.
A width or height of 256 is written as 0.
"""
import struct
import sys


def png_size(data: bytes) -> int:
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit("not a PNG file")
    width, height = struct.unpack(">II", data[16:24])
    if width != height:
        raise SystemExit(f"icon must be square, got {width}x{height}")
    if width > 256:
        raise SystemExit(f"icon must be at most 256 px, got {width}")
    return width


def main() -> None:
    if len(sys.argv) < 3:
        raise SystemExit("usage: make_ico.py out.ico 16.png [32.png ...]")
    images = []
    for path in sys.argv[2:]:
        with open(path, "rb") as f:
            data = f.read()
        images.append((png_size(data), data))
    images.sort(key=lambda i: i[0])

    header = struct.pack("<HHH", 0, 1, len(images))
    offset = len(header) + 16 * len(images)
    entries = b""
    for size, data in images:
        side = 0 if size == 256 else size
        entries += struct.pack("<BBBBHHII", side, side, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    with open(sys.argv[1], "wb") as out:
        out.write(header + entries + b"".join(d for _, d in images))


if __name__ == "__main__":
    main()

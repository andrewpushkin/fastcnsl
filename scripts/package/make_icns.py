#!/usr/bin/env python3
"""Writes a macOS .icns file from PNGs: make_icns.py out.icns 128.png 256.png 512.png

An .icns file is the 'icns' magic, the total length, then one chunk per size
(4-byte type, 4-byte length including the header, PNG data).
"""
import struct
import sys

TYPES = {128: b"ic07", 256: b"ic08", 512: b"ic09", 1024: b"ic10"}


def png_size(data: bytes) -> int:
    width, height = struct.unpack(">II", data[16:24])
    if width != height:
        raise SystemExit(f"icon must be square, got {width}x{height}")
    return width


def main() -> None:
    out, *pngs = sys.argv[1:]
    chunks = b""
    for path in pngs:
        with open(path, "rb") as f:
            data = f.read()
        size = png_size(data)
        if size not in TYPES:
            raise SystemExit(f"unsupported icon size {size}")
        chunks += TYPES[size] + struct.pack(">I", len(data) + 8) + data
    with open(out, "wb") as f:
        f.write(b"icns" + struct.pack(">I", len(chunks) + 8) + chunks)


if __name__ == "__main__":
    main()

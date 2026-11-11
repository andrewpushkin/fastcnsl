#!/usr/bin/env python3
"""Writes the benchmark inputs, about SIZE MiB each, into DIR.

    gen.py DIR [SIZE]

ascii    printable ASCII, 76-column lines (like base64 output)
color    words in 256-colour SGR escapes, ~70 visible columns per line
unicode  Cyrillic, Greek, box drawing and wide CJK, ~60 columns per line
lines    `seq` output: very many short lines, so mostly scrolling
"""
import os
import random
import sys

out = sys.argv[1]
size = int(sys.argv[2]) * 1024 * 1024 if len(sys.argv) > 2 else 32 * 1024 * 1024
os.makedirs(out, exist_ok=True)
rnd = random.Random(1)


def write(name, line_fn):
    path = os.path.join(out, name + ".txt")
    if os.path.exists(path) and os.path.getsize(path) >= size:
        return
    chunks, n = [], 0
    while n < size:
        line = line_fn()
        chunks.append(line)
        n += len(line.encode())
    with open(path, "w", encoding="utf-8") as f:
        f.write("".join(chunks))


PRINTABLE = "".join(chr(c) for c in range(33, 127))


def ascii_line():
    return "".join(rnd.choices(PRINTABLE, k=76)) + "\n"


WORDS = ["error", "warning", "info", "debug", "trace", "request", "response",
         "fn", "let", "match", "Some", "None", "0x7ffd", "=>", "{", "}"]


def color_line():
    parts, cols = [], 0
    while cols < 70:
        w = rnd.choice(WORDS)
        parts.append(f"\x1b[38;5;{rnd.randrange(256)}m{w}\x1b[0m ")
        cols += len(w) + 1
    return "".join(parts) + "\n"


UNI = ("абвгдеёжзийклмнопрстуфхцчшщъыьэюя"
       "αβγδεζηθικλμνξοπρστυφχψω"
       "─│┌┐└┘├┤┬┴┼═║╔╗╚╝"
       "漢字テスト中文日本語한국어")


def unicode_line():
    return "".join(rnd.choices(UNI, k=50)) + "\n"


write("ascii", ascii_line)
write("color", color_line)
write("unicode", unicode_line)

lines = os.path.join(out, "lines.txt")
if not (os.path.exists(lines) and os.path.getsize(lines) >= size):
    with open(lines, "w") as f:
        i, n = 1, 0
        buf = []
        while n < size:
            s = f"{i}\n"
            buf.append(s)
            n += len(s)
            i += 1
        f.write("".join(buf))

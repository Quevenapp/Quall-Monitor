#!/usr/bin/env python3
"""Generate Quall Monitor's vector source and native icons from the Quall geometry.

Requires Pillow; iconutil generates the macOS ICNS on a Mac. No original app files
are changed. The white ring and red light preserve the existing Quall mark.
"""
from __future__ import annotations

import io
import math
from pathlib import Path
import struct
import subprocess
import sys
import tempfile

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / 'assets'
WINDOWS_SIZES = (16, 20, 24, 32, 40, 48, 64, 96, 128, 256)
R_OUT, R_IN, R_CUT = 12.3, 8.7, 6.1
MARK_SCALE, MARK_X, MARK_Y = 11.4, 357.5, 270.0


def mark_path():
    def point(r, a):
        return 15 + r * math.cos(a), 15 + r * math.sin(a)

    def angles(r):
        distance = math.hypot(10.2, 10.2)
        half = math.acos((r * r + distance * distance - R_CUT * R_CUT) / (2 * r * distance))
        return math.pi / 4 - half, math.pi / 4 + half

    def p(x, y):
        return f'{MARK_X + (x - 2.7) * MARK_SCALE:.3f},{MARK_Y + (y - 2.7) * MARK_SCALE:.3f}'

    a1, a2 = angles(R_OUT)
    b1, b2 = angles(R_IN)
    outer, inner, cut = (r * MARK_SCALE for r in (R_OUT, R_IN, R_CUT))
    return (f'M{p(*point(R_OUT, a2))} A{outer:.3f},{outer:.3f} 0 1 1 {p(*point(R_OUT, a1))} '
            f'A{cut:.3f},{cut:.3f} 0 0 0 {p(*point(R_IN, b1))} '
            f'A{inner:.3f},{inner:.3f} 0 1 0 {p(*point(R_IN, b2))} '
            f'A{cut:.3f},{cut:.3f} 0 0 0 {p(*point(R_OUT, a2))} Z')


def vector():
    light_x = MARK_X + (25.2 - 2.7) * MARK_SCALE
    light_y = MARK_Y + (25.2 - 2.7) * MARK_SCALE
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024" role="img" aria-label="Quall Monitor">
<defs>
  <linearGradient id="metal" x2="0" y2="1"><stop stop-color="#f9fbff"/><stop offset="1" stop-color="#aeb9cb"/></linearGradient>
  <linearGradient id="screen" x2="0" y2="1"><stop stop-color="#152652"/><stop offset="1" stop-color="#0a1433"/></linearGradient>
</defs>
<path d="M452 689H572L594 821H430Z" fill="url(#metal)"/>
<rect x="317" y="805" width="390" height="56" rx="27" fill="url(#metal)"/>
<rect x="104" y="154" width="816" height="559" rx="48" fill="url(#metal)" stroke="#8493aa" stroke-width="8"/>
<rect x="139" y="188" width="746" height="455" rx="22" fill="url(#screen)"/>
<path d="{mark_path()}" fill="#fff"/>
<circle cx="{light_x:.3f}" cy="{light_y:.3f}" r="{4.6 * MARK_SCALE:.3f}" fill="#ff453a"/>
<circle cx="512" cy="680" r="7" fill="#74849b"/>
</svg>
'''


def render(size):
    sample = 4
    side = size * sample
    scale = side / 1024
    image = Image.new('RGBA', (side, side))
    draw = ImageDraw.Draw(image)

    def box(values):
        return tuple(round(v * scale) for v in values)

    def rounded(values, radius, color, outline=None, width=0):
        draw.rounded_rectangle(box(values), radius=round(radius * scale), fill=color,
                               outline=outline, width=max(1, round(width * scale)))

    draw.polygon([box(p) for p in [(452, 689), (572, 689), (594, 821), (430, 821)]], fill='#bbc6d5')
    rounded((317, 805, 707, 861), 27, '#c9d2df')
    rounded((104, 154, 920, 713), 48, '#dbe1eb', '#8493aa', 8)
    rounded((111, 160, 913, 706), 43, '#e4e9f1')
    mask = Image.new('L', image.size)
    ImageDraw.Draw(mask).rounded_rectangle(box((139, 188, 885, 643)), radius=round(22 * scale), fill=255)
    gradient = Image.new('RGBA', image.size)
    gd = ImageDraw.Draw(gradient)
    for y in range(round(188 * scale), round(643 * scale) + 1):
        ratio = (y / scale - 188) / 455
        color = tuple(round(a + (b - a) * ratio) for a, b in zip((21, 38, 82), (10, 20, 51)))
        gd.line((0, y, side, y), fill=color + (255,))
    image.paste(gradient, (0, 0), mask)
    ring = Image.new('L', image.size)
    rd = ImageDraw.Draw(ring)

    def mark_point(x, y):
        return ((MARK_X + (x - 2.7) * MARK_SCALE) * scale,
                (MARK_Y + (y - 2.7) * MARK_SCALE) * scale)

    def circle(target, center, radius, fill):
        x, y = mark_point(*center)
        r = radius * MARK_SCALE * scale
        target.ellipse((x - r, y - r, x + r, y + r), fill=fill)

    circle(rd, (15, 15), R_OUT, 255)
    circle(rd, (15, 15), R_IN, 0)
    circle(rd, (25.2, 25.2), R_CUT, 0)
    image.paste((255, 255, 255, 255), (0, 0), ring)
    draw = ImageDraw.Draw(image)
    circle(draw, (25.2, 25.2), 4.6, '#ff453a')
    draw.ellipse(box((505, 673, 519, 687)), fill='#74849b')
    return image.resize((size, size), Image.Resampling.LANCZOS)


def ico(images):
    encoded = []
    for image in images:
        stream = io.BytesIO()
        image.save(stream, format='PNG')
        encoded.append(stream.getvalue())
    header = struct.pack('<HHH', 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries = []
    for image, data in zip(images, encoded):
        width, height = image.size
        entries.append(struct.pack('<BBBBHHII', width % 256, height % 256, 0, 0, 1, 32, len(data), offset))
        offset += len(data)
    return header + b''.join(entries) + b''.join(encoded)


def main():
    ASSETS.mkdir(exist_ok=True)
    (ASSETS / 'QuallMonitor.svg').write_text(vector())
    render(1024).save(ASSETS / 'QuallMonitor-1024.png', optimize=True)
    render(256).save(ROOT / 'site/quall-monitor/icon.png', optimize=True)
    (ROOT / 'apps/windows/quall.ico').write_bytes(ico([render(s) for s in WINDOWS_SIZES]))
    if sys.platform == 'darwin':
        with tempfile.TemporaryDirectory() as directory:
            iconset = Path(directory) / 'QuallMonitor.iconset'
            iconset.mkdir()
            for size in (16, 32, 128, 256, 512):
                render(size).save(iconset / f'icon_{size}x{size}.png')
                render(size * 2).save(iconset / f'icon_{size}x{size}@2x.png')
            subprocess.run(['iconutil', '-c', 'icns', str(iconset), '-o',
                            str(ROOT / 'apps/macos/Empacotar/QuallMonitor.icns')], check=True)
    print('Quall Monitor SVG, PNG, ICO and native macOS ICNS generated.')


if __name__ == '__main__':
    main()

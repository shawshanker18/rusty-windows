# Find which glyph of Windows' icon fonts each Settings card icon is, by comparing against native captures.
import sys
from PIL import Image, ImageDraw, ImageFont, ImageChops
from fontTools.ttLib import TTFont

FONTS = {
    'S': r'C:\Windows\SystemResources\Windows.UI.SettingsAppThreshold\SystemSettings\Assets\Fonts\SettingsIcons.ttf',
    'F': r'C:\Windows\Fonts\SegoeIcons.ttf',
}
SIZE = 20
glyphs = []  # (font key, codepoint, 32x32 L image)
for key, path in FONTS.items():
    cps = sorted(c for c in TTFont(path).getBestCmap() if c >= 0xE000)
    font = ImageFont.truetype(path, SIZE)
    for cp in cps:
        im = Image.new('L', (32, 32), 0x2B)
        d = ImageDraw.Draw(im)
        box = d.textbbox((0, 0), chr(cp), font=font)
        w, h = box[2] - box[0], box[3] - box[1]
        if w <= 0 or h <= 0:
            continue
        d.text((16 - w / 2 - box[0], 16 - h / 2 - box[1]), chr(cp), font=font, fill=255)
        glyphs.append((key, cp, im))

def best(crop):
    out = []
    for key, cp, im in glyphs:
        low = None
        for dx in (-2, -1, 0, 1, 2):
            for dy in (-2, -1, 0, 1, 2):
                diff = ImageChops.difference(crop, ImageChops.offset(im, dx, dy)).crop((3, 3, 29, 29))
                s = sum(i * v for i, v in enumerate(diff.histogram()))
                if low is None or s < low:
                    low = s
        out.append((low, key, cp))
    out.sort()
    return out[:3]

PAGES = {
    'top-apps': [117 + 72 * i for i in range(9)],
    'top-gaming': [117 + 72 * i for i in range(4)],
    'top-timelanguage': [209 + 72 * i for i in range(4)],
    'top-accessibility': [144 + 72 * i for i in range(9)],
    'top-privacysecurity': [144, 216, 328, 400, 472, 544, 616],
    'top-windowsupdate': [350, 423, 495, 567, 639],
    'network': [231, 304, 376, 448, 521, 594, 666],
    'devices': [388, 477, 549, 621, 693],
    'personalization': [355 + 72 * i for i in range(6)],
}
for page, tops in PAGES.items():
    a = Image.open(f'shellbits/snap/native-settings-{page}.png').convert('L')
    for i, top in enumerate(tops):
        cx, cy = 8 + 325 + 28, 8 + top + 34
        crop = a.crop((cx - 16, cy - 16, cx + 16, cy + 16))
        print(page, i, ' '.join(f'{k}:{cp:04X}({s})' for s, k, cp in best(crop)[:2]), flush=True)

"""Pixel match of the Rust Claude window against a capture of the real app (same 1920x1032 frame)."""
from PIL import Image, ImageChops
n = Image.open('snap/claude-native.png').convert('RGB').crop((9, 9, 1929, 1041))
r = Image.open('snap/r-claude.bmp').convert('RGB')

def match(a, b, tol=16):
    d = ImageChops.difference(a, b).convert('L').point(lambda v: 255 if v > tol else 0)
    return 100 - 100 * sum(1 for v in d.getdata() if v) / (a.width * a.height)

print('whole %.2f' % match(n, r))
for name, box in [('sidebar', (0, 0, 266, 1032)), ('titlebar', (266, 0, 1920, 36)), ('conversation', (267, 36, 1600, 940)),
                  ('composer', (540, 940, 1330, 1032)), ('right', (1600, 36, 1920, 1032))]:
    print(name, '%.2f' % match(n.crop(box), r.crop(box)))
o = Image.new('RGB', (1920 * 2 + 10, 1032), (255, 0, 255))
o.paste(n, (0, 0))
o.paste(r, (1930, 0))
o.resize((1925, 516)).save('snap/cmp-claude-small.png')

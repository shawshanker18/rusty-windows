# Pixel match of the Rust Calculator snapshot against Windows' own (tolerance 16/255 per channel).
# usage: python cmp-calc.py [native.png] [rust.bmp]   (run from shellbits/)
# The outer 1px (the window border Windows draws itself) is left out.
import sys
from PIL import Image, ImageChops
a = Image.open(sys.argv[1] if len(sys.argv) > 1 else 'snap/native-calc.png').convert('RGB')
b = Image.open(sys.argv[2] if len(sys.argv) > 2 else 'snap/rs-calc.bmp').convert('RGB')
assert a.size == b.size, (a.size, b.size)
W, H = a.size
bad = ImageChops.difference(a, b).point(lambda v: 255 if v > 16 else 0).convert('L').point(lambda v: 255 if v else 0)
def pct(box):
    c = bad.crop(box); n = c.width * c.height
    return 100.0 * (n - sum(1 for v in c.getdata() if v)) / n
print('whole window: %.2f%%' % pct((1, 1, W - 1, H - 1)))
for name, box in [('title bar', (1, 1, W - 1, 32)), ('header', (1, 32, 880, 90)), ('display', (1, 90, 880, 270)), ('memory row', (1, 270, 880, 325)),
                  ('keypad', (1, 325, 880, H - 1)), ('side panel', (880, 32, W - 1, H - 1))]:
    print('  %-11s %.2f%%' % (name, pct(box)))
bad.save('snap/calc-diff.png')

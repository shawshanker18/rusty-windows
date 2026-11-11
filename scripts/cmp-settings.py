import sys
from PIL import Image, ImageChops
n,m=sys.argv[1],sys.argv[2]
a=Image.open(f'shellbits/snap/native-settings-{n}.png').convert('RGB'); b=Image.open(f'shellbits/snap/rs-settings-{m}.bmp').convert('RGB')
w,h=b.size; a=a.crop((8,8,8+w,8+h))
d=ImageChops.difference(a,b).convert('L').point(lambda v:255 if v>16 else 0)
f=lambda c:100*(1-c.histogram()[255]/(c.size[0]*c.size[1]))
print(n,f"whole {f(d):.2f}%  nav {f(d.crop((0,0,320,h))):.2f}%  content {f(d.crop((320,0,w,h))):.2f}%")
s=Image.new('RGB',(w*2+4,h),(255,0,255)); s.paste(a,(0,0)); s.paste(b,(w+4,0)); s.save(f'shellbits/snap/cmp-settings-{n}.png')
d.save(f'shellbits/snap/diff-settings-{n}.png')

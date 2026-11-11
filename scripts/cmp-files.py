from PIL import Image, ImageChops
a0=Image.open('shellbits/snap/native-explorer.png').convert('RGB'); b=Image.open('shellbits/snap/files-live.png').convert('RGB')
w,h=b.size; a=a0.crop((8,0,8+w,h))
d=ImageChops.difference(a,b).convert('L').point(lambda v:255 if v>16 else 0)
f=lambda c:100*(1-c.histogram()[255]/(c.size[0]*c.size[1]))
print(f"whole {f(d):.2f}%")
for name,box in [('title+toolbar',(0,0,w,88)),('command bar',(0,88,w,136)),('nav',(0,136,158,h-24)),('list',(158,136,w,h-24)),('status',(0,h-24,w,h))]:
    print(name, f"{f(d.crop(box)):.2f}%")
s=Image.new('RGB',(w,2*h)); s.paste(a,(0,0)); s.paste(b,(0,h)); s.save('shellbits/snap/files-cmp.png')

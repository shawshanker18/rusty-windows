"""Turn Lucide SVG icons (ISC licence) into src/icons.rs: each icon becomes absolute move/line/cubic/close
commands on Lucide's 24x24 grid, so Rust can stroke them with Direct2D exactly like the Claude app draws them.
Usage: python -I gen_icons.py <lucide package/icons dir> <out.rs>"""
import math, re, sys, xml.etree.ElementTree as ET

NAMES = ['align-left', 'panel-left', 'chevron-left', 'chevron-right', 'chevron-down', 'message-circle', 'code', 'plus', 'search',
         'copy', 'clock', 'briefcase', 'list', 'terminal', 'diff', 'globe', 'folder', 'ellipsis', 'laptop', 'monitor', 'server',
         'square', 'mic', 'settings', 'arrow-up', 'x', 'minus']

def arc_to_cubics(x1, y1, rx, ry, phi, fa, fs, x2, y2):
    """SVG elliptical arc -> list of cubic segments (standard endpoint-to-centre conversion)."""
    if rx == 0 or ry == 0:
        return [('L', x2, y2)]
    phi = math.radians(phi)
    cp, sp = math.cos(phi), math.sin(phi)
    dx, dy = (x1 - x2) / 2, (y1 - y2) / 2
    x1p, y1p = cp * dx + sp * dy, -sp * dx + cp * dy
    rx, ry = abs(rx), abs(ry)
    lam = x1p ** 2 / rx ** 2 + y1p ** 2 / ry ** 2
    if lam > 1:
        rx, ry = rx * math.sqrt(lam), ry * math.sqrt(lam)
    num = rx ** 2 * ry ** 2 - rx ** 2 * y1p ** 2 - ry ** 2 * x1p ** 2
    den = rx ** 2 * y1p ** 2 + ry ** 2 * x1p ** 2
    co = math.sqrt(max(0, num / den)) * (-1 if fa == fs else 1)
    cxp, cyp = co * rx * y1p / ry, -co * ry * x1p / rx
    cx = cp * cxp - sp * cyp + (x1 + x2) / 2
    cy = sp * cxp + cp * cyp + (y1 + y2) / 2
    def ang(ux, uy, vx, vy):
        a = math.atan2(ux * vy - uy * vx, ux * vx + uy * vy)
        return a
    t1 = ang(1, 0, (x1p - cxp) / rx, (y1p - cyp) / ry)
    dt = ang((x1p - cxp) / rx, (y1p - cyp) / ry, (-x1p - cxp) / rx, (-y1p - cyp) / ry)
    if not fs and dt > 0:
        dt -= 2 * math.pi
    elif fs and dt < 0:
        dt += 2 * math.pi
    n = max(1, int(math.ceil(abs(dt) / (math.pi / 2))))
    d = dt / n
    k = 4 / 3 * math.tan(d / 4)
    out = []
    for i in range(n):
        a1, a2 = t1 + i * d, t1 + (i + 1) * d
        p1 = (math.cos(a1) - k * math.sin(a1), math.sin(a1) + k * math.cos(a1))
        p2 = (math.cos(a2) + k * math.sin(a2), math.sin(a2) - k * math.cos(a2))
        p3 = (math.cos(a2), math.sin(a2))
        def tr(p):
            x, y = p[0] * rx, p[1] * ry
            return (cp * x - sp * y + cx, sp * x + cp * y + cy)
        q1, q2, q3 = tr(p1), tr(p2), tr(p3)
        out.append(('C', q1[0], q1[1], q2[0], q2[1], q3[0], q3[1]))
    return out

def path_cmds(d):
    toks = re.findall(r'[MmLlHhVvCcSsQqTtAaZz]|-?(?:\d+\.?\d*|\.\d+)(?:e-?\d+)?', d)
    out, i, cmd = [], 0, None
    x = y = sx = sy = 0.0
    lc = None  # last control point for S/T
    def num():
        nonlocal i
        v = float(toks[i]); i += 1; return v
    while i < len(toks):
        if re.match(r'[A-Za-z]', toks[i]):
            cmd = toks[i]; i += 1
            if cmd in 'Zz':
                out.append(('Z',)); x, y = sx, sy; lc = None
                continue
        rel = cmd.islower(); C = cmd.upper()
        ox, oy = (x, y) if rel else (0, 0)
        if C == 'M':
            x, y = num() + ox, num() + oy; sx, sy = x, y; out.append(('M', x, y)); cmd = 'l' if rel else 'L'; lc = None
        elif C == 'L':
            x, y = num() + ox, num() + oy; out.append(('L', x, y)); lc = None
        elif C == 'H':
            x = num() + (x if rel else 0); out.append(('L', x, y)); lc = None
        elif C == 'V':
            y = num() + (y if rel else 0); out.append(('L', x, y)); lc = None
        elif C == 'C':
            a, b, c2, d2, e, f = [num() for _ in range(6)]
            a, b, c2, d2, e, f = a + ox, b + oy, c2 + ox, d2 + oy, e + ox, f + oy
            out.append(('C', a, b, c2, d2, e, f)); lc = (c2, d2); x, y = e, f
        elif C == 'S':
            c2, d2, e, f = [num() for _ in range(4)]
            c2, d2, e, f = c2 + ox, d2 + oy, e + ox, f + oy
            a, b = (2 * x - lc[0], 2 * y - lc[1]) if lc else (x, y)
            out.append(('C', a, b, c2, d2, e, f)); lc = (c2, d2); x, y = e, f
        elif C == 'Q':
            qx, qy, e, f = [num() for _ in range(4)]
            qx, qy, e, f = qx + ox, qy + oy, e + ox, f + oy
            out.append(('C', x + 2 / 3 * (qx - x), y + 2 / 3 * (qy - y), e + 2 / 3 * (qx - e), f + 2 / 3 * (qy - f), e, f)); lc = (qx, qy); x, y = e, f
        elif C == 'A':
            rx, ry, phi, fa, fs, e, f = [num() for _ in range(7)]
            e, f = e + ox, f + oy
            out += arc_to_cubics(x, y, rx, ry, phi, int(fa), int(fs), e, f); x, y = e, f; lc = None
        else:
            raise ValueError(cmd)
    return out

def circle(cx, cy, r):
    return [('M', cx + r, cy)] + arc_to_cubics(cx + r, cy, r, r, 0, 0, 1, cx - r, cy) + arc_to_cubics(cx - r, cy, r, r, 0, 0, 1, cx + r, cy) + [('Z',)]

def rect_cmds(x, y, w, h, rx):
    if not rx:
        return [('M', x, y), ('L', x + w, y), ('L', x + w, y + h), ('L', x, y + h), ('Z',)]
    r = rx
    return path_cmds(f'M{x+r} {y}H{x+w-r}A{r} {r} 0 0 1 {x+w} {y+r}V{y+h-r}A{r} {r} 0 0 1 {x+w-r} {y+h}H{x+r}A{r} {r} 0 0 1 {x} {y+h-r}V{y+r}A{r} {r} 0 0 1 {x+r} {y}Z')

def icon(path):
    root = ET.parse(path).getroot()
    out = []
    for el in root.iter():
        tag = el.tag.split('}')[-1]
        a = lambda k, d=0: float(el.get(k, d))
        if tag == 'path':
            out += path_cmds(el.get('d'))
        elif tag == 'circle':
            out += circle(a('cx'), a('cy'), a('r'))
        elif tag == 'rect':
            out += rect_cmds(a('x'), a('y'), a('width'), a('height'), a('rx'))
        elif tag == 'line':
            out += [('M', a('x1'), a('y1')), ('L', a('x2'), a('y2'))]
        elif tag in ('polyline', 'polygon'):
            pts = list(map(float, el.get('points').replace(',', ' ').split()))
            out.append(('M', pts[0], pts[1]))
            out += [('L', pts[j], pts[j + 1]) for j in range(2, len(pts), 2)]
            if tag == 'polygon':
                out.append(('Z',))
    return out

src, dst = sys.argv[1], sys.argv[2]
code = {'M': 0, 'L': 1, 'C': 2, 'Z': 3}
lines = ['//! Lucide icons (ISC licence, lucide.dev) as stroke commands on a 24x24 grid, generated by gen_icons.py.',
         '//! Ops: 0 = move x y, 1 = line x y, 2 = cubic x1 y1 x2 y2 x y, 3 = close.', '',
         'pub const ICONS: &[(&str, &[f32])] = &[']
for n in NAMES:
    cmds = icon(f'{src}/{n}.svg')
    flat = []
    for c in cmds:
        flat.append(code[c[0]])
        flat += [round(v, 3) for v in c[1:]]
    lines.append(f'    ("{n}", &[{", ".join(repr(float(v)) for v in flat)}]),')
lines.append('];')
open(dst, 'w', encoding='utf-8').write('\n'.join(lines) + '\n')
print('wrote', len(NAMES), 'icons')

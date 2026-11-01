"""Isometric expert-grid logo for FLE Link: 3x3 experts on a plane, the three active ones raised as lit blocks."""
import math, sys

COS, SIN = math.cos(math.radians(30)), math.sin(math.radians(30))
S = 10.0          # tile edge in grid units
GAP = 2.6         # gap between tiles
ACTIVE = {(0, 0): 9.0, (2, 1): 13.0, (1, 2): 6.5}   # (col,row) -> height: the three active experts, different heights


def iso(x, y, z=0.0):
    return ((x - y) * COS, (x + y) * SIN - z)


def poly(pts, **a):
    d = " ".join(f"{x:.2f},{y:.2f}" for x, y in pts)
    attrs = " ".join(f'{k.replace("_", "-")}="{v}"' for k, v in a.items())
    return f'<polygon points="{d}" {attrs}/>'


def block(c, r, h):
    x0, y0 = c * (S + GAP), r * (S + GAP)
    x1, y1 = x0 + S, y0 + S
    top = [iso(x0, y0, h), iso(x1, y0, h), iso(x1, y1, h), iso(x0, y1, h)]
    out = []
    if h > 0:
        left = [iso(x0, y1, h), iso(x1, y1, h), iso(x1, y1, 0), iso(x0, y1, 0)]
        right = [iso(x1, y0, h), iso(x1, y1, h), iso(x1, y1, 0), iso(x1, y0, 0)]
        out.append(poly(left, fill="url(#side-l)"))
        out.append(poly(right, fill="url(#side-r)"))
        out.append(poly(top, fill="url(#top)"))
        out.append(poly(top, fill="none", stroke="#ffffff", stroke_opacity="0.35", stroke_width="0.5", stroke_linejoin="round"))
    else:
        out.append(poly(top, fill="#94a0b2", fill_opacity="0.16", stroke="#94a0b2", stroke_opacity="0.32", stroke_width="0.5"))
    return out


def centre(c, r, z):
    x0, y0 = c * (S + GAP), r * (S + GAP)
    return iso(x0 + S / 2, y0 + S / 2, z)


def build():
    shapes = []
    # light the active experts throw on the floor (drawn first, under everything)
    for (c, r), h in ACTIVE.items():
        cx, cy = centre(c, r, 0)
        shapes.append(f'<ellipse cx="{cx:.2f}" cy="{cy + 1.5:.2f}" rx="{S * 1.25:.2f}" ry="{S * 0.72:.2f}" fill="url(#spill)"/>')
    # paint back to front: smaller c+r first
    order = sorted(((c, r) for c in range(3) for r in range(3)), key=lambda t: (t[0] + t[1], t[0]))
    for c, r in order:
        shapes += block(c, r, ACTIVE.get((c, r), 0.0))
    # the link: the token's path through its three experts, top to top
    path = [(0, 0), (2, 1), (1, 2)]
    tops = [centre(c, r, ACTIVE[(c, r)]) for c, r in path]
    d = " ".join(f"{x:.2f},{y:.2f}" for x, y in tops)
    shapes.append(f'<polyline points="{d}" fill="none" stroke="url(#link)" stroke-width="1.1" stroke-linecap="round" stroke-linejoin="round" opacity="0.95"/>')
    for x, y in tops:
        shapes.append(f'<circle cx="{x:.2f}" cy="{y:.2f}" r="1.35" fill="#ffffff"/>')
    pts = []
    for c, r in order:
        x0, y0 = c * (S + GAP), r * (S + GAP)
        h = ACTIVE.get((c, r), 0.0)
        for x, y in ((x0, y0), (x0 + S, y0), (x0 + S, y0 + S), (x0, y0 + S)):
            pts += [iso(x, y, h), iso(x, y, 0)]
    xs, ys = [p[0] for p in pts], [p[1] for p in pts]
    pad = 4
    vb = (min(xs) - pad, min(ys) - pad, max(xs) - min(xs) + 2 * pad, max(ys) - min(ys) + 2 * pad)
    return vb, shapes


def svg(bg: bool):
    (x, y, w, h), shapes = build()
    side = max(w, h)
    x -= (side - w) / 2
    y -= (side - h) / 2
    defs = """<defs>
    <linearGradient id="top" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#5eeaf9"/><stop offset="0.55" stop-color="#a78bfa"/><stop offset="1" stop-color="#f0abfc"/></linearGradient>
    <linearGradient id="side-l" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#6d28d9"/><stop offset="1" stop-color="#3b1a78"/></linearGradient>
    <linearGradient id="side-r" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#0e7490"/><stop offset="1" stop-color="#123a52"/></linearGradient>
    <radialGradient id="spill"><stop offset="0" stop-color="#a78bfa" stop-opacity="0.42"/><stop offset="1" stop-color="#a78bfa" stop-opacity="0"/></radialGradient>
    <linearGradient id="link" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#ffffff"/><stop offset="1" stop-color="#f0abfc"/></linearGradient>
    <radialGradient id="glow" cx="0.5" cy="0.62" r="0.55"><stop offset="0" stop-color="#8b5cf6" stop-opacity="0.38"/><stop offset="1" stop-color="#8b5cf6" stop-opacity="0"/></radialGradient>
  </defs>"""
    back = f'<rect x="{x:.2f}" y="{y:.2f}" width="{side:.2f}" height="{side:.2f}" rx="{side*0.22:.2f}" fill="#0b0e14"/>' if bg else ""
    glow = f'<rect x="{x:.2f}" y="{y:.2f}" width="{side:.2f}" height="{side:.2f}" fill="url(#glow)"/>' if bg else ""
    body = "\n  ".join(shapes)
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{x:.2f} {y:.2f} {side:.2f} {side:.2f}" role="img" aria-label="FLE Link">\n'
            f'  <title>FLE Link</title>\n  <!-- nine experts on a plane; the three active for this token rise and light up -->\n  {defs}\n  {back}\n  {glow}\n  {body}\n</svg>\n')


if __name__ == "__main__":
    out = sys.argv[1]
    open(f"{out}/logo.svg", "w").write(svg(True))
    open(f"{out}/logo-mark.svg", "w").write(svg(False))
    print("written")

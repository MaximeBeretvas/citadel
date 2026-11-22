import math, sys, os
NAVY, BLUE, LBLUE, WHITE = "#0f172a", "#2563eb", "#60a5fa", "#ffffff"
f = lambda v: f"{v:.2f}".rstrip("0").rstrip(".")

def circle(cx, cy, r):  # closed circle subpath
    return f"M{f(cx)},{f(cy-r)} A{f(r)},{f(r)} 0 1 0 {f(cx)},{f(cy+r)} A{f(r)},{f(r)} 0 1 0 {f(cx)},{f(cy-r)} Z"

def symbol(R=92, r=68, horizon=140, merlons=((80,100),(118,138),(156,176)), mtop=118, beacon=(128,90,13)):
    """ring + walled skyline (wall bottom follows a circle 2 units larger than the inner edge, so it fuses with the ring) + beacon"""
    c = 128
    ring = circle(c, c, R) + " " + circle(c, c, r)
    rw = r + 2
    dx = math.sqrt(rw*rw - (horizon-c)**2)
    d = f"M{f(c-dx)},{horizon}"
    for a, b in merlons:
        d += f" H{f(a)} V{f(mtop)} H{f(b)} V{f(horizon)}"
    d += f" H{f(c+dx)} A{f(rw)},{f(rw)} 0 0 1 {f(c-dx)},{horizon} Z"
    bx, by, br = beacon
    return ring, d, circle(bx, by, br)

def svg_symbol(title, parts, ring_col, beacon_col, extra=""):
    ring, wall, beacon = parts
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" width="256" height="256" role="img" aria-label="{title}">
  <title>{title}</title>{extra}
  <path fill="{ring_col}" fill-rule="evenodd" d="{ring}"/>
  <path fill="{ring_col}" d="{wall}"/>
  <path fill="{beacon_col}" d="{beacon}"/>
</svg>
'''

MASTER = symbol()
SMALL = symbol(R=112, r=72, horizon=148, merlons=((82,112),(144,174)), mtop=120, beacon=(128,88,17))
REVERSED = symbol(R=92, r=69, horizon=140, merlons=((80.5,99.5),(118.5,137.5),(156.5,175.5)), mtop=118.5, beacon=(128,90,12.5))

# ---- wordmark "citadel": geometric lowercase, stroke 24, x-height 80..200, ascender 32, rings r62/38 (2-unit overshoot)
def wordmark_paths(ox=0.0):
    R, r, cy = 62, 38, 140
    c40, s40 = math.cos(math.radians(40)), math.sin(math.radians(40))
    P = []
    def X(v): return f(v + ox)
    # c (opening right, radial terminals at +-40deg)
    cx = 62
    P.append(f"M{X(cx+R*c40)},{f(cy-R*s40)} A{R},{R} 0 1 0 {X(cx+R*c40)},{f(cy+R*s40)} L{X(cx+r*c40)},{f(cy+r*s40)} A{r},{r} 0 1 1 {X(cx+r*c40)},{f(cy-r*s40)} Z")
    # i
    P.append(f"M{X(128)},80 H{X(152)} V200 H{X(128)} Z"); P.append(circle(140+ox, 50, 15))
    # t
    P.append(f"M{X(178)},44 H{X(202)} V80 H{X(226)} V102 H{X(202)} V200 H{X(178)} V102 H{X(164)} V80 H{X(178)} Z")
    # a, d: annulus + stem (separate paths so evenodd does not punch the overlap)
    rings = []
    for cx, top in ((302, 80), (444, 32)):
        rings.append(circle(cx+ox, cy, R) + " " + circle(cx+ox, cy, r))
        P.append(f"M{X(cx+r)},{top} H{X(cx+R)} V200 H{X(cx+r)} Z")
    # e: ring open lower-right below the bar, plus bar
    cx = 586; h = 11
    ox_, ix_ = math.sqrt(R*R-h*h), math.sqrt(r*r-h*h)
    P.append(f"M{X(cx+ox_)},{cy+h} A{R},{R} 0 1 0 {X(cx+R*c40)},{f(cy+R*s40)} L{X(cx+r*c40)},{f(cy+r*s40)} A{r},{r} 0 1 1 {X(cx+ix_)},{cy+h} Z")
    P.append(f"M{X(cx-r-2)},{cy-h} H{X(cx+ox_)} V{cy+h} H{X(cx-r-2)} Z")
    # l
    P.append(f"M{X(666)},32 H{X(690)} V200 H{X(666)} Z")
    return P, rings

def wm_group(col, tx, ty, s):
    P, rings = wordmark_paths()
    body = "\n".join(f'    <path d="{d}"/>' for d in P) + "\n" + "\n".join(f'    <path fill-rule="evenodd" d="{d}"/>' for d in rings)
    return f'  <g fill="{col}" transform="translate({f(tx)},{f(ty)}) scale({s})">\n{body}\n  </g>'

def sym_group(parts, ring_col, beacon_col, tx=0, ty=0, s=1):
    ring, wall, beacon = parts
    return f'''  <g transform="translate({f(tx)},{f(ty)}) scale({s})">
    <path fill="{ring_col}" fill-rule="evenodd" d="{ring}"/>
    <path fill="{ring_col}" d="{wall}"/>
    <path fill="{beacon_col}" d="{beacon}"/>
  </g>'''

def lockup(kind, ring_col, beacon_col, word_col, parts=MASTER, title="Citadel"):
    s = 0.75
    if kind == "horizontal":
        # symbol visible 36..220; wordmark x-height centre (140*s) aligned to symbol centre 128; gap 36
        body = sym_group(parts, ring_col, beacon_col) + "\n" + wm_group(word_col, 256, 128 - 140*s, s)
        vb = f"20 0 {f(256 + 690*s + 16 - 20)} 256"
    else:
        W = 690*s
        body = sym_group(parts, ring_col, beacon_col, tx=(W-256)/2 + 16, ty=0) + "\n" + wm_group(word_col, 16, 264 - 32*s, s)
        vb = f"0 20 {f(W+32)} {f(264 + 170*s + 16 - 20)}"
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="{vb}" role="img" aria-label="{title}">
  <title>{title}</title>
{body}
</svg>
'''

def wordmark(col):
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="-12 20 714 196" role="img" aria-label="Citadel">
  <title>Citadel</title>
{wm_group(col, 0, 0, 1)}
</svg>
'''

out = sys.argv[1]
w = lambda name, s: open(os.path.join(out, name), "w").write(s)
w("citadel-symbol.svg", svg_symbol("Citadel — Porthole symbol", MASTER, NAVY, BLUE))
w("citadel-symbol-small.svg", svg_symbol("Citadel — Porthole symbol, small-size cut (≤32 px)", SMALL, NAVY, BLUE))
w("citadel-symbol-reversed.svg", svg_symbol("Citadel — Porthole symbol, reversed (thinned for dark backgrounds)", REVERSED, WHITE, LBLUE))
w("citadel-symbol-small-reversed.svg", svg_symbol("Citadel — Porthole small-size cut, reversed", SMALL, WHITE, LBLUE))
w("citadel-horizontal.svg", lockup("horizontal", NAVY, BLUE, NAVY))
w("citadel-horizontal-reversed.svg", lockup("horizontal", WHITE, LBLUE, WHITE, parts=REVERSED))
w("citadel-stacked.svg", lockup("stacked", NAVY, BLUE, NAVY))
w("citadel-stacked-reversed.svg", lockup("stacked", WHITE, LBLUE, WHITE, parts=REVERSED))
w("citadel-wordmark.svg", wordmark(NAVY))
w("citadel-wordmark-reversed.svg", wordmark(WHITE))
# app icon: navy tile, white mark, light-blue beacon (keeps the beacon colour, unlike a one-colour tile)
tile = '\n  <rect width="256" height="256" rx="56" fill="#0f172a"/>'
def icon(parts, title):
    ring, wall, beacon = parts
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" width="256" height="256" role="img" aria-label="{title}">
  <title>{title}</title>{tile}
  <g transform="translate(128,126) scale(0.74) translate(-128,-128)">
    <path fill="#ffffff" fill-rule="evenodd" d="{ring}"/>
    <path fill="#ffffff" d="{wall}"/>
    <path fill="#60a5fa" d="{beacon}"/>
  </g>
</svg>
'''
w("citadel-app-icon.svg", icon(REVERSED, "Citadel app icon"))
w("citadel-app-icon-small.svg", icon(SMALL, "Citadel app icon, small-size cut"))

#!/usr/bin/env python3
"""Generate MailView's symbolic icon set.

The icons follow the visual language of Apple Mail on iOS: a rounded tray for
mailboxes, an even 1.3px stroke on a 16px grid, generous corner radii.

Everything is emitted as *filled* paths, never strokes. GTK recolours symbolic
icons by injecting `rect, circle, path { fill: <colour> !important }` into the
SVG, and it never touches `stroke`, so a stroked icon would keep whatever
colour the file declared and disappear against a dark background. Outlines are
therefore built by offsetting each polyline to both sides and filling the band
between them, with mitred joins so right angles stay sharp.

Run:  python3 tools/make-icons.py
"""

import math
import os
from pathlib import Path

STROKE = 1.3
OUT_DIR = Path(__file__).resolve().parent.parent / "data" / "icons" / "scalable" / "actions"

# ----------------------------------------------------------------- geometry


def _normal(p, q):
    """Unit left-hand normal of the segment p->q."""
    dx, dy = q[0] - p[0], q[1] - p[1]
    length = math.hypot(dx, dy)
    if length == 0:
        raise ValueError(f"degenerate segment {p}->{q}")
    return (-dy / length, dx / length)


def _intersect(a1, a2, b1, b2):
    """Intersection of the infinite lines a1a2 and b1b2, or None if parallel."""
    x1, y1 = a1
    x2, y2 = a2
    x3, y3 = b1
    x4, y4 = b2
    denominator = (x1 - x2) * (y3 - y4) - (y1 - y2) * (x3 - x4)
    if abs(denominator) < 1e-9:
        return None
    a = x1 * y2 - y1 * x2
    b = x3 * y4 - y3 * x4
    return ((a * (x3 - x4) - (x1 - x2) * b) / denominator,
            (a * (y3 - y4) - (y1 - y2) * b) / denominator)


def _offset_open(points, distance):
    """Offset an open polyline by `distance` along its left normal."""
    segments = []
    for p, q in zip(points, points[1:]):
        nx, ny = _normal(p, q)
        segments.append(((p[0] + nx * distance, p[1] + ny * distance),
                         (q[0] + nx * distance, q[1] + ny * distance)))

    result = [segments[0][0]]
    for current, following in zip(segments, segments[1:]):
        joint = _intersect(current[0], current[1], following[0], following[1])
        result.append(joint if joint else current[1])
    result.append(segments[-1][1])
    return result


def _offset_closed(points, distance):
    """Offset a closed polyline by `distance` along its left normal."""
    count = len(points)
    segments = []
    for index in range(count):
        p, q = points[index], points[(index + 1) % count]
        nx, ny = _normal(p, q)
        segments.append(((p[0] + nx * distance, p[1] + ny * distance),
                         (q[0] + nx * distance, q[1] + ny * distance)))

    result = []
    for index in range(count):
        previous = segments[index - 1]
        current = segments[index]
        joint = _intersect(previous[0], previous[1], current[0], current[1])
        result.append(joint if joint else current[0])
    return result


def _fmt(points, close=True):
    out = [f"M{points[0][0]:.2f} {points[0][1]:.2f}"]
    out += [f"L{x:.2f} {y:.2f}" for x, y in points[1:]]
    if close:
        out.append("Z")
    return "".join(out)


# ------------------------------------------------------------------- shapes


def stroke_line(points, width=STROKE):
    """An open polyline rendered as a filled band."""
    left = _offset_open(points, width / 2)
    right = _offset_open(points, -width / 2)
    return _fmt(left + right[::-1])


def stroke_shape(points, width=STROKE):
    """A closed polyline rendered as a filled outline."""
    outer = _offset_closed(points, width / 2)
    inner = _offset_closed(points, -width / 2)
    return _fmt(outer) + _fmt(inner[::-1])


def rounded_rect(x, y, w, h, r):
    """Path data for a rounded rectangle, drawn clockwise."""
    r = min(r, w / 2, h / 2)
    return (
        f"M{x + r:.2f} {y:.2f}"
        f"H{x + w - r:.2f}A{r:.2f} {r:.2f} 0 0 1 {x + w:.2f} {y + r:.2f}"
        f"V{y + h - r:.2f}A{r:.2f} {r:.2f} 0 0 1 {x + w - r:.2f} {y + h:.2f}"
        f"H{x + r:.2f}A{r:.2f} {r:.2f} 0 0 1 {x:.2f} {y + h - r:.2f}"
        f"V{y + r:.2f}A{r:.2f} {r:.2f} 0 0 1 {x + r:.2f} {y:.2f}Z"
    )


def rounded_rect_outline(x, y, w, h, r, width=STROKE):
    """An outlined rounded rectangle. The rect given is the stroke centreline."""
    half = width / 2
    outer = rounded_rect(x - half, y - half, w + width, h + width, r + half)
    inner = rounded_rect(x + half, y + half, w - width, h - width, max(r - half, 0.01))
    return outer + inner


def filled_pill(x, y, w, h):
    return rounded_rect(x, y, w, h, h / 2)


def star(cx, cy, outer_r, inner_r, points=5):
    coords = []
    for index in range(points * 2):
        radius = outer_r if index % 2 == 0 else inner_r
        angle = -math.pi / 2 + index * math.pi / points
        coords.append((cx + radius * math.cos(angle), cy + radius * math.sin(angle)))
    return _fmt(coords)


# --------------------------------------------------------- reply/forward arrows
#
# Traced from a reference glyph: a thin hooked stroke (an arc plus a short
# barb forming the arrowhead notch), not a filled shape like the rest of the
# set. Built from straight-line segments like everything else here, so it
# still needs its own `fill-rule="nonzero"` in ICON_RULES below — the arc and
# the barb are two separate bands that deliberately overlap where they meet,
# and `evenodd` (the default) would punch that overlap into a hole.


def _polar(cx, cy, r, deg):
    rad = math.radians(deg)
    return (cx + r * math.cos(rad), cy + r * math.sin(rad))


def _arc_points(cx, cy, r, start_deg, end_deg, n=9):
    return [_polar(cx, cy, r, start_deg + (end_deg - start_deg) * i / (n - 1)) for i in range(n)]


def reply_hook(cx, cy, r, width, start_deg, end_deg, barb_len, barb_angle, barb_at_end=True):
    """A hooked arrow: an arc from `start_deg` to `end_deg`, with a short
    barb stroke at one tip (the `barb_at_end` one) forming the arrowhead."""
    arc = _arc_points(cx, cy, r, start_deg, end_deg)
    main = stroke_line(arc, width)

    tip, prev = (arc[-1], arc[-2]) if barb_at_end else (arc[0], arc[1])
    nx, ny = _normal(prev, tip) if barb_at_end else _normal(tip, prev)
    ang = math.degrees(math.atan2(ny, nx)) + barb_angle
    barb_end = _polar(tip[0], tip[1], barb_len, ang)
    barb = stroke_line([tip, barb_end], width)

    return main + barb


def compose_glyph(x, y, w, h, r, width, pencil):
    """A rounded square with a pencil stroke crossing its top-right corner."""
    square = rounded_rect_outline(x, y, w, h, r, width)
    return square + stroke_line(pencil, width)


# ------------------------------------------------------------------- icons

# The mailbox tray: a rounded box whose front panel dips in the middle, which
# is the single most recognisable shape in the reference screenshots.
def tray_lip(dip_y, width=STROKE, depth=1.5):
    """The dipped front panel of a mailbox tray."""
    return stroke_line(
        [
            (2.0, dip_y),
            (5.0, dip_y),
            (6.2, dip_y + depth),
            (9.8, dip_y + depth),
            (11.0, dip_y),
            (14.0, dip_y),
        ],
        width,
    )


def tray(top, bottom, dip_y, radius, width=STROKE):
    body = rounded_rect_outline(2.0, top, 12.0, bottom - top, radius, width)
    return body + tray_lip(dip_y, width)


ICONS = {
    # A single mailbox.
    "mailview-inbox-symbolic": tray(top=3.0, bottom=13.0, dip_y=9.1, radius=2.8),

    # Every mailbox at once: the same tray, with sheets stacking into it.
    "mailview-inbox-all-symbolic": (
        filled_pill(5.1, 1.4, 5.8, 1.25)
        + filled_pill(3.6, 3.7, 8.8, 1.25)
        + rounded_rect_outline(2.0, 6.2, 12.0, 7.6, 2.2)
        + tray_lip(10.4, depth=1.35)
    ),

    # A dog-eared page.
    "mailview-drafts-symbolic": (
        stroke_shape([(3.7, 2.1), (9.3, 2.1), (12.3, 5.1), (12.3, 13.9), (3.7, 13.9)])
        + stroke_line([(9.3, 2.1), (9.3, 5.1), (12.3, 5.1)])
    ),

    # A paper plane. Outlined like the rest of the set; a solid dart at this
    # size reads as a mouse cursor instead.
    "mailview-sent-symbolic": stroke_shape(
        [(14.8, 1.2), (1.2, 7.6), (6.6, 9.4), (8.4, 14.8)]
    ),

    # Junk: the tray, crossed out.
    "mailview-junk-symbolic": (
        rounded_rect_outline(2.0, 3.0, 12.0, 10.0, 2.8)
        + stroke_line([(5.7, 5.7), (10.3, 10.3)])
        + stroke_line([(10.3, 5.7), (5.7, 10.3)])
    ),

    "mailview-trash-symbolic": (
        filled_pill(2.4, 4.05, 11.2, 1.3)
        + stroke_line([(6.2, 4.05), (6.2, 2.3), (9.8, 2.3), (9.8, 4.05)], 1.2)
        + stroke_line([(3.7, 5.35), (4.5, 13.6), (11.5, 13.6), (12.3, 5.35)])
    ),

    # A storage box: solid lid, open body, small pull handle.
    "mailview-archive-symbolic": (
        rounded_rect(2.2, 2.7, 11.6, 2.6, 0.9)
        + stroke_line([(3.0, 6.2), (3.0, 13.3), (13.0, 13.3), (13.0, 6.2)])
        + stroke_line([(6.6, 9.5), (9.4, 9.5)], 1.2)
    ),

    "mailview-folder-symbolic": stroke_shape(
        [(2.0, 13.2), (2.0, 3.7), (5.9, 3.7), (7.3, 5.7), (14.0, 5.7), (14.0, 13.2)]
    ),

    # Flagged / VIP.
    "mailview-flagged-symbolic": star(8.0, 8.4, 6.5, 2.75),

    # Unread: a plain dot, matching the list's unread marker.
    "mailview-unread-symbolic": rounded_rect(4.2, 4.2, 7.6, 7.6, 3.8),

    # New message: a rounded square with a pencil crossing its corner.
    "mailview-compose-symbolic": compose_glyph(
        x=1.7, y=3.6, w=10.2, h=10.2, r=2.4, width=1.35,
        pencil=[(7.0, 10.2), (12.6, 4.6), (14.3, 4.1)],
    ),

    # Reply: a single hooked arrow curving back and to the left.
    "mailview-reply-symbolic": reply_hook(
        cx=9.2, cy=9.3, r=4.3, width=1.7, start_deg=-15, end_deg=-155, barb_len=3.6, barb_angle=60
    ),

    # Reply all: the same hook, twice, side by side.
    "mailview-reply-all-symbolic": (
        reply_hook(cx=5.6, cy=9.6, r=2.55, width=1.25, start_deg=-15, end_deg=-155, barb_len=2.3, barb_angle=60)
        + reply_hook(cx=9.9, cy=9.6, r=2.55, width=1.25, start_deg=-15, end_deg=-155, barb_len=2.3, barb_angle=60)
    ),

    # Forward: the same hook again, its barb on the other end so it points
    # right instead of left.
    "mailview-forward-symbolic": reply_hook(
        cx=9.2, cy=9.3, r=4.3, width=1.7, start_deg=-15, end_deg=-155, barb_len=3.6, barb_angle=-60,
        barb_at_end=False,
    ),
}

# Icons whose sub-paths are deliberately overlapping bands (a hook's arc and
# its barb) rather than the outer-minus-inner outline shape the rest of the
# set uses — evenodd would turn that overlap into a hole, so these need the
# other fill rule instead.
NONZERO_ICONS = {
    "mailview-reply-symbolic",
    "mailview-reply-all-symbolic",
    "mailview-forward-symbolic",
}

TEMPLATE = """<?xml version="1.0" encoding="UTF-8"?>
<!-- Generated by tools/make-icons.py — do not edit by hand. -->
<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16">
  <path fill="#222222" fill-rule="{rule}" d="{path}"/>
</svg>
"""


def main():
    os.makedirs(OUT_DIR, exist_ok=True)
    for name, path_data in ICONS.items():
        rule = "nonzero" if name in NONZERO_ICONS else "evenodd"
        target = OUT_DIR / f"{name}.svg"
        target.write_text(TEMPLATE.format(path=path_data, rule=rule))
        print(f"wrote {target.relative_to(OUT_DIR.parents[3])}")
    print(f"\n{len(ICONS)} icons in {OUT_DIR}")


if __name__ == "__main__":
    main()

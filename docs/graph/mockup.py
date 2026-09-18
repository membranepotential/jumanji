"""Generate docs/graph/mockup.html: a static reference of the target graph design.

Run from the repo root:  uv run --with pillow docs/graph/mockup.py
Needs Source Serif 4 installed (pacman -S adobe-source-serif-fonts): labels
are cut to width with real font metrics, as the overlay does.
"""
from pathlib import Path
from html import escape
from PIL import ImageFont

OUT = Path(__file__).with_name("mockup.html")
SERIF = "/usr/share/fonts/adobe-source-serif/SourceSerif4-Regular.otf"
SERIF_B = "/usr/share/fonts/adobe-source-serif/SourceSerif4-Semibold.otf"
_fonts = {}


def width(text, size, bold=False):
    key = (size, bold)
    if key not in _fonts:
        _fonts[key] = ImageFont.truetype(SERIF_B if bold else SERIF, size)
    # 6 % headroom for fallback faces (Georgia runs wider than Source Serif).
    return _fonts[key].getlength(text) * 1.06


def cut(text, size, maxw, bold=False):
    if width(text, size, bold) <= maxw:
        return text
    while text and width(text.rstrip() + "…", size, bold) > maxw:
        text = text[:-1]
    return text.rstrip(" —:,") + "…"


# Theme (src/core/assets/style.css, html.dark) and derived fills (graph.css).
BG, FG, MUTED, HEAD, ACCENT = "#1a1a1a", "#d6d6d6", "#9aa0a6", "#f0f0f0", "#6cb6ff"
BORDER, STRONG = "#333333", "#454545"
PILL, PILL_HOVER, PILL_CURRENT = "#252525", "#2f2f2f", "#2c3c4c"  # fg 6 %, fg 11 %, accent 22 %
NOTE = "#e8c766"  # annotation colour: the theme's note yellow, never used by the graph

WIN_W, WIN_H, STATUS_H = 1200, 720, 22

# Titles as jumanji derives them (frontmatter title, else H1), per file.
T = {
    "README.md": "Lighthouse",
    "getting-started.md": "Getting started",
    "installation.md": "Installation",
    "configuration.md": "Configuring a Lighthouse site",
    "architecture.md": "Architecture",
    "guides/writing-content.md": "Writing content: pages, collections and front matter",
    "guides/theming.md": "Themes and layouts",
    "guides/deploying.md": "Deploying a site",
    "contributing.md": "Contributing",
    "changelog.md": "Changelog",
    "faq.md": "Frequently asked questions",
    "reference/config-keys.md": "Configuration keys",
    "reference/cli.md": "Command-line interface",
    "reference/templates.md": "Template functions",
    "roadmap.md": "Roadmap",
    "architecture/pipeline.md": "The build pipeline",
    "architecture/content-model.md": "The content model",
    "architecture/storage.md": "Storage — sources, metadata and build output",
    "architecture/cache.md": "The build cache",
    "architecture/plugins.md": "Plugins",
    "architecture/incremental-builds.md": "Incremental builds and dependency tracking",
    "architecture/rendering.md": "Rendering",
    "guides/shortcodes.md": "Shortcodes",
    "guides/images.md": "Images",
    "reference/plugin-hooks.md": "Plugin hooks — the lifecycle events emitted during a build",
}
LINKS = {  # outgoing links to documents, per file (demo/graph)
    "README.md": 14, "getting-started.md": 4, "installation.md": 2, "configuration.md": 3,
    "architecture.md": 8, "guides/writing-content.md": 4, "guides/theming.md": 2,
    "guides/deploying.md": 1, "contributing.md": 2, "changelog.md": 2, "faq.md": 2,
    "reference/config-keys.md": 2, "architecture/pipeline.md": 4,
    "architecture/content-model.md": 2, "architecture/storage.md": 6,
    "architecture/cache.md": 2, "architecture/plugins.md": 2, "guides/images.md": 1,
}
ROUTE = ["README.md", "architecture.md", "architecture/storage.md"]
README_LINKS = [
    "getting-started.md", "installation.md", "configuration.md", "architecture.md",
    "guides/writing-content.md", "guides/theming.md", "guides/deploying.md",
    "contributing.md", "changelog.md", "faq.md", "reference/config-keys.md",
    "reference/cli.md", "reference/templates.md", "roadmap.md",
]
ARCH_LINKS = [
    "architecture/pipeline.md", "architecture/content-model.md", "architecture/storage.md",
    "architecture/cache.md", "architecture/plugins.md", "architecture/incremental-builds.md",
    "architecture/rendering.md", "README.md",
]
STORAGE_LINKS = [
    "architecture/cache.md", "architecture/content-model.md",
    "architecture/incremental-builds.md", "reference/config-keys.md",
    "architecture.md", "README.md",
]


def basename(path):
    return path.rsplit("/", 1)[-1]


class Item:
    def __init__(self, path, col, row, parent=None, handle="auto", **flags):
        self.path, self.col, self.row, self.parent = path, col, row, parent
        self.flags = flags
        n = LINKS.get(path, 0)
        # Links view default: route and current unfolded, the rest folded.
        if handle == "auto":
            handle = ("−" if path in ROUTE and flags.get("spine") else f"+{n}") if n else None
        self.handle = handle


def links_items():
    """The links view with its defaults: route unfolded, siblings, current's fan."""
    items = [Item("README.md", 0, 0, spine=True, root=True)]
    arch_i = README_LINKS.index("architecture.md")
    for i, p in enumerate(README_LINKS):
        items.append(Item(p, 1, i - arch_i, parent=0, spine=(p == "architecture.md")))
    arch = next(i for i, it in enumerate(items) if it.path == "architecture.md")
    st_i = ARCH_LINKS.index("architecture/storage.md")
    for i, p in enumerate(ARCH_LINKS):
        items.append(Item(p, 2, i - st_i, parent=arch, spine=(p == "architecture/storage.md")))
    cur = next(i for i, it in enumerate(items) if it.path == "architecture/storage.md")
    items[cur].flags["current"] = True
    for i, p in enumerate(STORAGE_LINKS):
        items.append(Item(p, 3, i - 2, parent=cur))
    return items


def tree_items():
    """The tree view: the spanning tree, every node once, all unfolded."""
    rows = [
        ("README.md", 0, 0, None),
        *[(p, 1, i - 3, "README.md") for i, p in enumerate(README_LINKS)],
        ("architecture/pipeline.md", 2, -2, "architecture.md"),
        ("architecture/content-model.md", 2, -1, "architecture.md"),
        ("architecture/storage.md", 2, 0, "architecture.md"),
        ("architecture/cache.md", 2, 1, "architecture.md"),
        ("architecture/plugins.md", 2, 2, "architecture.md"),
        ("architecture/incremental-builds.md", 2, 3, "architecture.md"),
        ("architecture/rendering.md", 2, 4, "architecture.md"),
        ("guides/shortcodes.md", 2, 5, "guides/writing-content.md"),
        ("guides/images.md", 2, 6, "guides/writing-content.md"),
        ("reference/plugin-hooks.md", 3, 2, "architecture/plugins.md"),
    ]
    index = {p: i for i, (p, *_rest) in enumerate(rows)}
    inner = {parent for *_r, parent in rows if parent}
    items = []
    for p, col, row, parent in rows:
        items.append(Item(
            p, col, row, parent=index.get(parent),
            handle="−" if p in inner else None,
            spine=p in ROUTE, root=p == "README.md",
            current=p == "architecture/storage.md",
        ))
    return items


def elbow(x1, y1, x2, y2, r=9):
    mx = (x1 + x2) / 2
    dy = y2 - y1
    if abs(dy) < 0.5:
        return f"M{x1:.1f} {y1:.1f}H{x2:.1f}"
    r = min(abs(dy) / 2, r)
    s = 1 if dy > 0 else -1
    return (f"M{x1:.1f} {y1:.1f}H{mx - r:.1f}Q{mx:.1f} {y1:.1f} {mx:.1f} {y1 + s * r:.1f}"
            f"V{y2 - s * r:.1f}Q{mx:.1f} {y2:.1f} {mx + r:.1f} {y2:.1f}H{x2:.1f}")


class Near:
    """Near zoom geometry: the scene's units (graph/svg.rs) times k."""
    k = 0.9
    col, row, w, h = 300 * k, 54 * k, 232 * k, 42 * k

    def __init__(self, x0, y0):
        self.x0, self.y0 = x0, y0

    def at(self, it):
        return self.x0 + it.col * self.col, self.y0 + it.row * self.row


def pill(x, y, it, g, *, sel=False, hover=False, dim=False, linked=False, handle=True):
    """One node: pill, route dot, title, file name, handle at the right edge."""
    w, h = g.w, g.h
    top = y - h / 2
    route = it.path in ROUTE
    current = it.flags.get("current")
    fill = PILL_CURRENT if current else PILL_HOVER if hover else PILL
    stroke = FG if sel else ACCENT if linked else "none"
    hd = it.handle if handle else None
    title_w = w - 16 - (40 if hd else 12)
    title = cut(T[it.path], 14, title_w, bold=bool(current))
    name = cut(basename(it.path), 12, title_w)
    out = [f'<g class="node{" dim" if dim else ""}" transform="translate({x:.1f} {top:.1f})">']
    out.append(f'<rect width="{w:.1f}" height="{h:.1f}" rx="7" fill="{fill}" stroke="{stroke}" stroke-width="1.5"/>')
    r = 5 if it.flags.get("root") or it.path == "README.md" else 3.2
    out.append(f'<circle cy="{h / 2:.1f}" r="{r}" fill="{ACCENT if route else STRONG}"/>')
    tcls = "t cur" if current else "t"
    out.append(f'<text class="{tcls}" x="14" y="{h / 2 - 2:.1f}">{escape(title)}</text>')
    out.append(f'<text class="f" x="14" y="{h / 2 + 12:.1f}">{escape(name)}</text>')
    if hd:
        out.append(f'<line x1="{w - 34:.1f}" y1="8" x2="{w - 34:.1f}" y2="{h - 8:.1f}" stroke="{STRONG}"/>')
        out.append(f'<text class="hd" x="{w - 17:.1f}" y="{h / 2:.1f}">{hd}</text>')
    out.append("</g>")
    return "".join(out)


def scene(items, g, *, sel=None, hover=None, dim=None, linked=(), skip=()):
    """Edges, spine and pills for a near-zoom scene."""
    edges, spine, nodes = [], [], []
    for i, it in enumerate(items):
        if i in skip:
            continue
        x, y = g.at(it)
        if it.parent is not None:
            px, py = g.at(items[it.parent])
            d_dim = dim is not None and dim(i)
            if it.flags.get("spine") and items[it.parent].flags.get("spine"):
                spine.append(f'<path class="spine" d="M{px + g.w:.1f} {py:.1f}H{x:.1f}"/>')
            else:
                cls = "edge dim" if d_dim else "edge"
                edges.append(f'<path class="{cls}" d="{elbow(px + g.w, py, x, y)}"/>')
        nodes.append(pill(x, y, it, g, sel=i == sel, hover=i == hover,
                          dim=dim is not None and dim(i), linked=i in linked))
    return "".join(edges) + "".join(spine) + "".join(nodes)


def callout(n, x, y):
    return (f'<g class="co" transform="translate({x:.1f} {y:.1f})"><circle r="10"/>'
            f'<text y="0.5">{n}</text></g>')


def panel(title, path, crumbs):
    """The bottom-left panel: selection's title and path, the route as breadcrumb,
    wrapped so the panel stays clear of the second column."""
    maxw = 272
    lines, line = [], ""
    for i, c in enumerate(crumbs):
        piece = c if i == 0 else f"{line} › {c}" if line else c
        if line and width(piece + " ›", 13) > maxw:
            lines.append(line + " ›")
            line = c
        else:
            line = piece
    lines.append(line)
    lines = [cut(l, 13, maxw) for l in lines]
    w = max([width(title, 16, True), width(path, 12) * 1.1] + [width(l, 13) for l in lines]) + 30
    h = 64 + 18 * len(lines)
    x0, y0 = 20, WIN_H - STATUS_H - 18 - h
    crumb = "".join(f'<text class="pr" x="14" y="{68 + 18 * i}">{escape(l)}</text>'
                    for i, l in enumerate(lines))
    return (f'<g class="panel" transform="translate({x0} {y0})">'
            f'<rect width="{w:.1f}" height="{h}" fill="{BG}" stroke="{BORDER}"/>'
            f'<rect width="2" height="{h}" fill="{ACCENT}"/>'
            f'<text class="pt" x="14" y="27">{escape(title)}</text>'
            f'<text class="pp" x="14" y="46">{escape(path)}</text>{crumb}</g>')


HELP = [("Enter", "open"), ("hjkl", "move"), ("Space", "fold"), ("v", "view"),
        ("Ctrl+wheel", "zoom"), ("Esc", "close")]


def help_line():
    parts, x = [], 8.0
    for i, (k, v) in enumerate(HELP):
        if i:
            x += 16
        parts.append(f'<text class="k" x="{x:.1f}" y="16">{k}</text>')
        x += width(k + " ", 12)
        parts.append(f'<text x="{x:.1f}" y="16">{v}</text>')
        x += width(v, 12)
    w = x + 8
    x0 = WIN_W - 20 - w
    y0 = WIN_H - STATUS_H - 18 - 24
    return (f'<g class="help" transform="translate({x0:.1f} {y0})">'
            f'<rect width="{w:.1f}" height="24" fill="{BG}"/>{"".join(parts)}</g>')


def window(body, status, label):
    return (
        f'<svg class="win" viewBox="0 0 {WIN_W} {WIN_H}" role="img" aria-label="{escape(label)}">'
        f'<rect width="{WIN_W}" height="{WIN_H}" fill="{BG}"/>'
        f'<g clip-path="url(#graph-area)">{body}</g>'
        f'<rect y="{WIN_H - STATUS_H}" width="{WIN_W}" height="{STATUS_H}" class="sb"/>'
        f'<text class="st" x="8" y="{WIN_H - 6}">{escape(status)}</text>'
        "</svg>"
    )


CRUMBS = ["Lighthouse", "Architecture", "Storage — sources, metadata and build output"]


def frame_links(peek=False):
    g = Near(60, 178)
    items = links_items()
    sel = next(i for i, it in enumerate(items) if it.col == 3 and it.path == "architecture/cache.md")
    plugins = next(i for i, it in enumerate(items) if it.col == 2 and it.path == "architecture/plugins.md")
    body = [scene(items, g, sel=sel, hover=plugins if peek else None)]
    cos = []
    if peek:
        # The floating layer: plugins' children as a fan, over a theme backdrop.
        px, py = g.at(items[plugins])
        kids = [Item("reference/plugin-hooks.md", 3, items[plugins].row),
                Item("architecture/pipeline.md", 3, items[plugins].row + 1)]
        bx0 = px + g.w + 6
        bx1 = g.x0 + 3 * g.col + g.w + 14
        by0 = py - g.row / 2 - 4
        by1 = py + g.row * 1.5 + 4
        layer = [f'<rect class="backdrop" x="{bx0:.1f}" y="{by0:.1f}" width="{bx1 - bx0:.1f}" height="{by1 - by0:.1f}" rx="10"/>']
        for kid in kids:
            kx, ky = g.at(kid)
            layer.append(f'<path class="edge peek" d="{elbow(px + g.w, py, kx, ky)}"/>')
            layer.append(pill(kx, ky, kid, g))
        body.append(f'<g class="peek-layer">{"".join(layer)}</g>')
        # The pointer, resting on the hovered pill.
        cx, cy = px + g.w * 0.55, py + 4
        body.append(f'<path class="cursor" transform="translate({cx:.1f} {cy:.1f})" d="M0 0V17L4.5 12.8L7.6 19.6L10.4 18.4L7.4 11.7H13.2Z"/>')
        cos += [callout(1, px - 16, py - 16), callout(2, bx1 + 16, by0 + 14),
                callout(3, g.x0 + 3 * g.col + g.w + 20, g.y0 + g.row)]
    else:
        x0, y0 = g.at(items[0])
        a_x, a_y = g.at(items[4])
        s_x, s_y = g.at(next(it for it in items if it.flags.get("current")))
        sel_x, sel_y = g.at(items[sel])
        c1x = g.x0 + g.col
        cos += [
            callout(1, (x0 + g.w + a_x) / 2, y0 - 14),
            callout(2, c1x + g.w + 18, g.y0 - 2 * g.row),
            callout(2, c1x + g.w + 18, g.y0 + 2 * g.row),
            callout(3, sel_x + g.w + 20, s_y),
            callout(4, s_x + g.w + 18, s_y + 5 * g.row),
            callout(5, sel_x + g.w + 20, sel_y),
            callout(6, sel_x + g.w + 20, sel_y + 5 * g.row),
        ]
    body.append(panel("The build cache", "architecture/cache.md", CRUMBS))
    body.append(help_line())
    body.append("".join(cos))
    return window("".join(body), "Graph: links", "Links view at near zoom")


def frame_tree():
    g = Near(60, 178)
    items = tree_items()
    sel = next(i for i, it in enumerate(items) if it.path == "reference/config-keys.md")
    targets = {"configuration.md", "architecture/cache.md"}
    linked = {i for i, it in enumerate(items) if it.path in targets}
    keep = linked | {sel}

    def dim(i):
        return not (items[i].path in ROUTE or i in keep)

    body = [scene(items, g, sel=sel, dim=dim, linked=linked)]
    sx, sy = g.at(items[sel])
    cx, cy = g.at(next(it for it in items if it.path == "architecture/cache.md"))
    fx, fy = g.at(next(it for it in items if it.path == "configuration.md"))
    rx, ry = g.at(next(it for it in items if it.path == "guides/shortcodes.md"))
    body.append(panel("Configuration keys", "reference/config-keys.md", CRUMBS))
    body.append(help_line())
    body.append(callout(1, sx + g.w + 16, sy))
    body.append(callout(2, cx + g.w + 16, cy))
    body.append(callout(2, fx + g.w + 16, fy))
    body.append(callout(3, rx + g.w + 16, ry))
    body.append(callout(4, (g.x0 + g.w + g.x0 + g.col) / 2, g.y0 - 14))
    return window("".join(body), "Graph: tree", "Tree view with a selection")


def frame_far():
    k, kx = 0.36, 0.55
    col, row, w, h = 300 * kx, 54 * k, 232 * kx, 42 * k
    x0, y0 = 290, 262
    items = links_items()
    sel = next(i for i, it in enumerate(items) if it.col == 3 and it.path == "architecture/cache.md")

    def at(it):
        return x0 + it.col * col, y0 + it.row * row

    # Leaf runs: consecutive leaf siblings of one parent, two or more.
    runs, cur = [], []
    for i, it in enumerate(items):
        leaf = it.handle is None and not it.flags.get("spine")
        if leaf and cur and items[cur[-1]].parent == it.parent and items[cur[-1]].row == it.row - 1:
            cur.append(i)
            continue
        if len(cur) >= 2:
            runs.append(cur)
        cur = [i] if leaf else []
    if len(cur) >= 2:
        runs.append(cur)
    bundled = {i for r in runs for i in r}

    edges, spine, labels, bars = [], [], [], []
    for i, it in enumerate(items):
        x, y = at(it)
        if it.parent is not None and i not in bundled:
            px, py = at(items[it.parent])
            if it.flags.get("spine") and items[it.parent].flags.get("spine"):
                spine.append(f'<path class="spine" d="M{px + w:.1f} {py:.1f}H{x:.1f}"/>')
            else:
                edges.append(f'<path class="edge" d="{elbow(px + w, py, x, y, 6)}"/>')
        if i in bundled:
            continue
        cls = "far"
        if it.flags.get("current"):
            cls += " cur"
            labels.append(f'<rect x="{x:.1f}" y="{y - 12:.1f}" width="{w:.1f}" height="24" rx="6" fill="{PILL_CURRENT}"/>')
        if i == sel:
            labels.append(f'<rect x="{x:.1f}" y="{y - 12:.1f}" width="{w:.1f}" height="24" rx="6" fill="{PILL}" stroke="{FG}" stroke-width="1.5"/>')
        route = it.path in ROUTE
        r = 4.5 if it.path == "README.md" else 2.8
        labels.append(f'<circle cx="{x:.1f}" cy="{y:.1f}" r="{r}" fill="{ACCENT if route else STRONG}"/>')
        text = cut(T[it.path], 15, w - 14, bold=bool(it.flags.get("current")))
        labels.append(f'<text class="{cls}" x="{x + 9:.1f}" y="{y:.1f}">{escape(text)}</text>')
    for run in runs:
        first, last = items[run[0]], items[run[-1]]
        x, y1 = at(first)
        _, y2 = at(last)
        top, bottom = y1 - h / 2, y2 + h / 2
        mid = (top + bottom) / 2
        px, py = at(items[first.parent])
        bars.append(f'<path class="edge" d="{elbow(px + w, py, x, mid, 6)}"/>')
        bars.append(f'<rect x="{x:.1f}" y="{top:.1f}" width="5" height="{bottom - top:.1f}" rx="2.5" fill="{STRONG}"/>')
        bars.append(f'<text class="far bundle" x="{x + 13:.1f}" y="{mid:.1f}">{len(run)} nodes</text>')
    body = "".join(edges) + "".join(bars) + "".join(spine) + "".join(labels)
    cos = []
    r0 = runs[0]
    bx, by = at(items[r0[0]])
    cos.append(callout(1, bx + 13 + width("3 nodes", 15) + 20, by + row))
    kx_, ky_ = at(next(it for it in items if it.path == "reference/config-keys.md" and it.col == 3))
    cos.append(callout(2, kx_ + w + 30, ky_))
    sx, sy = at(items[0])
    cos.append(callout(3, sx + w - 6, sy - 18))
    body += panel("The build cache", "architecture/cache.md", CRUMBS) + help_line() + "".join(cos)
    return window(body, "Graph: links", "Far zoom")


def legend():
    """Every cue of interaction.md's visual vocabulary, drawn, with its one meaning."""
    g = Near(0, 0)

    def mini(path, **flags):
        it = Item(path, 0, 0, **flags)
        return it

    def cell(svg, cue, meaning):
        return (f'<div class="cue"><svg viewBox="0 0 250 64" aria-hidden="true">{svg}</svg>'
                f'<div><div class="cue-name">{cue}</div><div class="cue-meaning">{meaning}</div></div></div>')

    def at(it, x=14, y=32, **kw):
        return f'<g transform="translate({x} 0)">{pill(0, y, it, g, **kw)}</g>'

    cells = [
        cell(f'<path class="spine" d="M14 32H236"/>', "Accent line", "The route."),
        cell(at(mini("architecture.md"), handle=False), "Accent dot on a node",
             "The node is on the route, also where it appears again."),
        cell(at(mini("architecture/storage.md", current=True), handle=False), "Accent tint fill", "The current node."),
        cell(at(mini("README.md", root=True), handle=False), "Larger dot", "The root."),
        cell(at(mini("architecture/cache.md"), sel=True, handle=False), "Light outline", "The selection."),
        cell(at(mini("architecture/plugins.md"), hover=True, handle=False), "Brighter fill", "Hover."),
        cell(at(mini("guides/theming.md")), "Handle +n", "Folded, with n children."),
        cell(at(mini("guides/writing-content.md", handle="−")), "Handle −", "Unfolded."),
        cell(at(mini("reference/cli.md")), "No handle", "No links."),
        cell(f'<circle cx="16" cy="32" r="3.2" fill="{ACCENT}"/><path class="spine jump" d="M24 32H236"/>',
             "Dashed line", "A step no link explains (a jump). Nothing else is dashed."),
        cell(f'<rect x="16" y="10" width="5" height="44" rx="2.5" fill="{STRONG}"/>'
             f'<text class="far bundle" x="30" y="32">3 nodes</text>',
             "Bar reading “N nodes”", "Zoomed out: N nodes with no links of their own. Click to zoom in."),
        cell(at(mini("configuration.md"), linked=True, handle=False),
             "Accent outline, rest dimmed", "Tree view: the selected node’s link targets."),
        cell('<rect x="14" y="8" width="222" height="48" fill="#1a1a1a" stroke="#333"/>'
             f'<rect x="14" y="8" width="2" height="48" fill="{ACCENT}"/>'
             '<text class="pt" x="26" y="29" style="font-size:14px">The build cache</text>'
             '<text class="pp" x="26" y="46">architecture/cache.md</text>',
             "Panel, bottom left", "The selection’s title and path; the route as a breadcrumb."),
        cell('<rect x="0" y="22" width="250" height="22" class="sb"/>'
             '<text class="st" x="8" y="38">Graph: tree</text>',
             "Status line", "The view: <code>Graph: links</code> or <code>Graph: tree</code>."),
        cell('<g class="help"><text class="k" x="14" y="37">Enter</text><text x="52" y="37">open</text>'
             '<text class="k" x="96" y="37">Space</text><text x="134" y="37">fold</text></g>',
             "Help line, bottom right", "The keys."),
    ]
    return f'<div class="legend">{"".join(cells)}</div>'


CSS = f"""
:root {{
  --bg: {BG}; --fg: {FG}; --fg-muted: {MUTED}; --heading: {HEAD}; --accent: {ACCENT};
  --border: {BORDER}; --border-strong: {STRONG}; --note: {NOTE};
  --page: #141414;
  --serif: "Source Serif 4", "Iowan Old Style", Palatino, Georgia, serif;
  --mono: "JetBrains Mono", "Fira Code", "SFMono-Regular", Consolas, "Liberation Mono", monospace;
  color-scheme: dark;
}}
* {{ box-sizing: border-box; }}
html, body {{ margin: 0; background: var(--page); color: var(--fg); }}
body {{ font: 17px/1.55 var(--serif); padding: 56px 16px 96px; }}
main {{ max-width: 1240px; margin: 0 auto; }}
a {{ color: var(--accent); text-decoration-thickness: 1px; text-underline-offset: 2px; }}
a:focus-visible {{ outline: 2px solid var(--fg); outline-offset: 2px; }}
header {{ max-width: 70ch; margin-bottom: 56px; }}
h1 {{ color: var(--heading); font-weight: 600; font-size: 34px; line-height: 1.15; margin: 0 0 14px; letter-spacing: -0.01em; }}
header p {{ margin: 0 0 8px; color: var(--fg-muted); }}
header p strong {{ color: var(--fg); font-weight: 600; }}
code {{ font-family: var(--mono); font-size: 0.86em; color: var(--fg); }}
section {{ margin: 0 0 72px; }}
.head {{ display: grid; grid-template-columns: 2.2em 1fr; align-items: baseline; max-width: 80ch; }}
.head .letter {{ color: var(--fg-muted); font-size: 22px; }}
h2 {{ color: var(--heading); font-size: 22px; font-weight: 600; margin: 0; }}
.head p {{ grid-column: 2; margin: 4px 0 18px; color: var(--fg-muted); }}
.win {{ display: block; width: 100%; height: auto; border: 1px solid var(--border); border-radius: 6px;
       font-family: var(--serif); }}
ol.keys {{ list-style: none; padding: 0; margin: 16px 0 0 2.2em; max-width: 90ch;
          columns: 2 36ch; column-gap: 40px; font-size: 15px; }}
ol.keys li {{ display: grid; grid-template-columns: 30px 1fr; margin: 0 0 6px; break-inside: avoid; }}
ol.keys li span {{ display: inline-grid; place-items: center; width: 20px; height: 20px; border-radius: 50%;
                  background: var(--note); color: #1a1a1a; font: 600 12px/1 var(--serif); margin-top: 2px; }}
footer {{ color: var(--fg-muted); font-size: 15px; max-width: 80ch; border-top: 1px solid var(--border); padding-top: 18px; }}

/* The scene: the overlay's own styling (src/controller/assets/graph.css). */
.edge {{ fill: none; stroke: var(--border-strong); stroke-width: 1.25; }}
.edge.peek {{ stroke: var(--fg-muted); }}
.spine {{ fill: none; stroke: var(--accent); stroke-width: 2.5; stroke-linecap: round; }}
.spine.jump {{ stroke-dasharray: 2 7; }}
.dim {{ opacity: 0.35; }}
.t {{ fill: var(--fg); font-size: 14px; }}
.t.cur {{ fill: var(--heading); font-weight: 600; }}
.f {{ fill: var(--fg-muted); font-size: 12px; }}
.hd {{ fill: var(--fg-muted); font-size: 13px; font-weight: 600; text-anchor: middle; dominant-baseline: central; }}
.far {{ fill: var(--fg); font-size: 15px; dominant-baseline: central;
        paint-order: stroke; stroke: var(--bg); stroke-width: 4px; stroke-linejoin: round; }}
.far.cur {{ fill: var(--heading); font-weight: 600; stroke: none; }}
.far.bundle {{ fill: var(--fg-muted); }}
.backdrop {{ fill: var(--bg); stroke: var(--border); }}
.cursor {{ fill: #fff; stroke: #000; stroke-width: 1; stroke-linejoin: round; }}
.panel .pt {{ fill: var(--heading); font-size: 16px; font-weight: 600; }}
.pp {{ fill: var(--fg-muted); font: 12px var(--mono); }}
.panel .pr {{ fill: var(--fg-muted); font-size: 13px; white-space: pre; }}
.crumb.sel {{ fill: var(--accent); }}
.help {{ fill: var(--fg-muted); font-size: 12px; }}
.help .k {{ fill: var(--fg); }}
.sb {{ fill: #202020; }}
.st {{ fill: var(--fg); font: 13px var(--mono); }}
.co circle {{ fill: var(--note); }}
.co text {{ fill: #1a1a1a; font-size: 12px; font-weight: 700; text-anchor: middle; dominant-baseline: central; }}

.legend {{ display: grid; grid-template-columns: repeat(auto-fill, minmax(460px, 1fr)); gap: 1px;
          background: var(--border); border: 1px solid var(--border); border-radius: 6px; overflow: hidden; }}
.cue {{ display: grid; grid-template-columns: 250px 1fr; gap: 18px; align-items: center;
       background: var(--bg); padding: 10px 18px 10px 6px; }}
.cue svg {{ width: 250px; height: 64px; display: block; font-family: var(--serif); }}
.cue-name {{ color: var(--heading); font-weight: 600; font-size: 15px; }}
.cue-meaning {{ color: var(--fg-muted); font-size: 15px; line-height: 1.4; }}
@media (max-width: 620px) {{
  body {{ padding-top: 32px; }}
  h1 {{ font-size: 27px; }}
  .legend {{ grid-template-columns: 1fr; }}
  .cue {{ grid-template-columns: 1fr; }}
  .cue svg {{ width: 100%; max-width: 250px; }}
  ol.keys {{ margin-left: 0; }}
}}
"""


def section(letter, title, caption, body, keys=()):
    ks = "".join(f"<li><span>{i}</span><div>{k}</div></li>" for i, k in enumerate(keys, 1))
    ol = f'<ol class="keys">{ks}</ol>' if keys else ""
    return (f'<section id="frame-{letter}"><div class="head"><span class="letter">{letter}</span>'
            f"<h2>{title}</h2><p>{caption}</p></div>{body}{ol}</section>")


SPEC = "interaction.md"


def page():
    frames = [
        section("a", "Links view, near zoom",
                f'The graph as it opens on <code>architecture/storage.md</code>, reached from '
                f'<code>README.md</code> through <code>architecture.md</code>: the defaults of '
                f'<a href="{SPEC}#the-model">The model</a>.',
                frame_links(),
                ["The route: one straight accent line from the root to the current node.",
                 "Siblings of a route step: its parent’s other links, in the same column, above (listed before it) and below (after it).",
                 "The current node’s links fan out to its right.",
                 "Handles: <code>+n</code> on a folded node, <code>−</code> on an unfolded one; none without links.",
                 "The selection: a light outline; the panel names it and shows the route.",
                 "A link back to a route node is a child like any other; its accent dot says it is on the route."]),
        section("b", "Hover peek",
                f'The pointer rests on the folded node <em>Plugins</em>: its children appear as a '
                f'temporary fan in a floating layer, and nothing else moves (<a href="{SPEC}#gestures">Gestures</a>, peek).',
                frame_links(peek=True),
                ["The hovered node gets the brighter fill.",
                 "The floating layer: a theme-background backdrop and the fan the unfold would show.",
                 "The scene under the layer is unchanged; leaving the node removes the layer."]),
        section("c", "Tree view with a selection",
                f'<code>v</code> shows the spanning tree, every node once; the selection’s link targets '
                f'are outlined instead of drawn as lines (<a href="{SPEC}#visual-vocabulary">Visual vocabulary</a>).',
                frame_tree(),
                ["The selection, <em>Configuration keys</em>.",
                 "Its link targets, wherever the tree placed them: an accent outline.",
                 "Everything else off the route is dimmed; no lines are drawn across the tree.",
                 "The route stays straight and at full strength."]),
        section("d", "Far zoom",
                'Zoomed out, the pills give way to 15 px labels cut to their column, leaf runs '
                'collapse into bundles, and the route and current node stay legible (P6).',
                frame_far(),
                ["A bundle bar: three nodes without links of their own. Click it to zoom in on them.",
                 "Labels stay 15 px on screen and end in an ellipsis at their column’s width.",
                 "The route line and the current node’s tint survive every zoom level."]),
        section("e", "Visual vocabulary",
                f'Every cue from <a href="{SPEC}#visual-vocabulary">Visual vocabulary</a>, drawn, '
                'with its one meaning (P3).',
                legend()),
    ]
    return f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Document graph — visual reference</title>
<style>{CSS}</style>
</head>
<body>
<svg width="0" height="0" style="position:absolute" aria-hidden="true">
  <clipPath id="graph-area"><rect width="{WIN_W}" height="{WIN_H - STATUS_H}"/></clipPath>
</svg>
<main>
<header>
<h1>Document graph — visual reference</h1>
<p>The target design of the document graph (<code>t</code>), drawn from its spec. Where this sheet and the spec disagree, the spec wins.</p>
<p><strong>Read with:</strong> <a href="README.md">the feature doc</a>, <a href="{SPEC}">interaction.md</a> (the interaction model), and <a href="../DESIGN.md#d14-the-document-graph--a-route-spine-with-its-links-fanned-out-2026-09-18">DESIGN.md D14</a> (how it is built).</p>
<p>Sample content: the fictional <em>Lighthouse</em> docs in <code>demo/graph/</code>. Colours are jumanji’s dark theme; yellow circles are annotations, not part of the interface.</p>
</header>
{''.join(frames)}
<footer>Static reference sheet; no scripts. Frames are 1200 × 720 windows at near zoom <code>k = 0.9</code> and far zoom <code>k = 0.36</code>, with the geometry of <code>src/core/graph/svg.rs</code>.</footer>
</main>
</body>
</html>
"""


with open(OUT, "w") as f:
    f.write(page())
print("wrote", OUT)

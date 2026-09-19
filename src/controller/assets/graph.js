// The document-graph overlay (DESIGN D14; the interaction is specified in
// docs/graph/interaction.md). Viewport glue in the D12 sense: the scene
// arrives as finished SVG from `core::graph`; this only puts it on screen,
// pans and zooms it, swaps level-of-detail classes, draws a peek, and reports
// clicks. The controller owns the selection, the folds and the layout: a click
// on a pill calls `post.select(key)`, a click on a fold handle
// `post.fold(key)`, a double-click `post.open(key)` (all built in `scripts.rs`
// over the shell's post seam, which stamp each post with the walk's
// generation; an item's key survives re-layouts, its index does not), and the
// controller answers through `window.__jmnj_graph.select(i)`,
// or `update(svg, i, anchor)` after a re-layout.
//
// Called as `(GRAPH_JS)(svgMarkup, css, selected, post)`.
(function (svgMarkup, css, selected, post) {
  if (window.__jmnj_graph) window.__jmnj_graph.close();

  const root = document.createElement('div');
  root.id = '__jmnj_graph';
  root.innerHTML =
    '<style>' + css + '</style>' +
    '<div class="jg-stage"></div>' +
    '<div class="jg-info"><div class="jg-info-title"></div>' +
    '<div class="jg-info-path"></div><div class="jg-info-cut"></div>' +
    '<div class="jg-info-route"></div></div>' +
    '<div class="jg-help"><div class="jg-view"><b></b> <span></span></div>' +
    '<div class="jg-keys"><kbd>Enter</kbd> open &nbsp; <kbd>hjkl</kbd> move &nbsp; ' +
    '<kbd>Space</kbd> fold &nbsp; <kbd>v</kbd> view &nbsp; ' +
    '<kbd>Ctrl</kbd>+wheel zoom &nbsp; <kbd>Esc</kbd> close</div></div>';
  document.documentElement.appendChild(root);

  const stage = root.querySelector('.jg-stage');
  // Above the help line: the view on screen, and what it shows, in one
  // sentence (docs/graph/README.md, "Links view or tree view").
  const VIEWS = {
    links: ['Links view', 'Each document branches into every document it links to, so one linked from several places appears in each.'],
    tree: ['Tree view', 'Each document appears once, where the walk from the root first reached it; the selection’s links are outlined.'],
  };
  const viewLine = root.querySelector('.jg-view');
  const SVG_NS = 'http://www.w3.org/2000/svg';
  // The peek's layer: screen space, over the scene and outside its camera, so
  // a peek is the same size at every zoom level.
  const peekLayer = document.createElementNS(SVG_NS, 'svg');
  peekLayer.setAttribute('class', 'jg-peek-layer');
  root.insertBefore(peekLayer, stage.nextSibling);
  const info = {
    box: root.querySelector('.jg-info'),
    title: root.querySelector('.jg-info-title'),
    path: root.querySelector('.jg-info-path'),
    cut: root.querySelector('.jg-info-cut'),
    route: root.querySelector('.jg-info-route'),
  };
  // Level-of-detail thresholds (DESIGN D14): near ≥ 0.7 > mid ≥ 0.4 > far.
  const NEAR = 0.7, FAR = 0.4;
  // Zoomed out, columns shrink less than rows: the horizontal scale stops at
  // KX_MIN, so a far label has room for more of its title. Text is
  // counter-scaled horizontally (`--sx` in graph.css), so glyphs never stretch.
  const KX_MIN = 0.55;
  // The zoom range. In, twice the drawn size is plenty. Out, the scale at
  // which the whole graph's height fits the window is all there is to see —
  // at most 1:1, and never below K_FLOOR, where a huge graph's rows would be
  // a few px apart and its far labels would all cull one another.
  const K_MAX = 2, K_FLOOR = 0.15;
  // Room kept around what the camera frames.
  const MARGIN = 48;
  // Clicking a bundle zooms in to this scale: mid, where titles show.
  const BUNDLE_ZOOM = 0.55;
  // How long the pointer rests on a folded node before its peek appears, so a
  // passing pointer does not flash fans.
  const PEEK_DELAY = 250;
  // The least distance between a peek's rows on screen, in CSS px: a far
  // label is 15 px tall.
  const PEEK_PITCH = 22;
  // Scene units of a pill's padding: the text starts 16 in, and keeps 10 clear
  // of the right edge — or of the fold handle, the pill's last 38.
  const PAD_LEFT = 16, PAD_RIGHT = 10, HANDLE_W = 38;
  // A pill's vertical middle, in scene units.
  const NODE_MID = 21;
  let svg = null;
  // The camera: a translation, and a vertical and horizontal scale.
  let tx = 0, ty = 0, k = 1;
  const kx = () => Math.max(k, KX_MIN);
  // Where the pointer last was, in CSS px: `Ctrl`+wheel zooms about it. The
  // page tracks it itself — the shell's pointer is in toolkit px, which need
  // not be CSS px (page.rs `GraphZoomAt::Pointer`).
  const pointer = { x: innerWidth / 2, y: innerHeight / 2 };

  function apply() {
    svg.style.transform =
      'translate(' + tx + 'px,' + ty + 'px) scale(' + kx() + ',' + k + ')';
    root.style.setProperty('--k', String(k));
    root.style.setProperty('--sx', String(k / kx()));
    const lod = k >= NEAR ? 'near' : k >= FAR ? 'mid' : 'far';
    if (root.dataset.lod !== lod) {
      root.classList.remove('lod-near', 'lod-mid', 'lod-far');
      root.classList.add('lod-' + lod);
      root.dataset.lod = lod;
    }
    hidePeek();
    declutter();
  }

  // Labels keep a minimum size on screen at every level (graph.css), so a
  // zoomed-out label is larger than its pill was drawn for. Once per frame
  // after a change: cut each label to what fits — its pill near and mid, one
  // column far out — at the size it is actually drawn, and then, far out,
  // keep labels in priority order and hide (fade) any that would overlap one
  // already kept. The panel and hover still show the whole title.
  // A pan moves every label alike, so only a new scale, scene or selection
  // (which outranks other labels) redoes it: `clutterK` is the scale it was
  // last done at, `null` when it must be redone.
  let declutterFrame = 0;
  let clutterK = null;
  function declutter() {
    if (k === clutterK) return;
    clutterK = k;
    cancelAnimationFrame(declutterFrame);
    declutterFrame = requestAnimationFrame(() => {
      const lod = root.dataset.lod;
      const column = Number(svg.dataset.column);
      for (const c of svg.querySelectorAll('.culled')) c.classList.remove('culled');
      for (const g of svg.querySelectorAll('.jg-nodes .jg-node')) fitLabels(g, lod, column);
      if (lod !== 'far') return;
      const labels = svg.querySelectorAll('.jg-nodes .t, .jg-bundle .t');
      const kept = [];
      const hits = (b) => kept.some((o) =>
        b.left < o.right && o.left < b.right && b.top < o.bottom && o.top < b.bottom);
      // A label's box includes its handle, which stands beside it.
      for (const t of [...labels].sort((a, b) => rank(a) - rank(b))) {
        const g = t.closest('.jg-node');
        if (g && g.classList.contains('bundled') && !g.classList.contains('sel')) continue;
        const handle = g && g.querySelector('.jg-handle text');
        const parts = handle ? [t, handle] : [t];
        const rects = parts.map((p) => p.getBoundingClientRect());
        const b = {
          left: Math.min(...rects.map((r) => r.left)),
          right: Math.max(...rects.map((r) => r.right)),
          top: Math.min(...rects.map((r) => r.top)),
          bottom: Math.max(...rects.map((r) => r.bottom)),
        };
        if (hits(b)) for (const p of parts) p.classList.add('culled');
        else kept.push(b);
      }
    });
  }

  // Cut one node's labels to what fits at level `lod` — its pill near and
  // mid, one `column` far out, beside its handle — at the size it is drawn.
  // The scene's nodes and the peek's are both fitted here.
  function fitLabels(g, lod, column) {
    const pill = g.querySelector('rect').width.baseVal.value;
    const title = g.querySelector('.t');
    const handle = g.querySelector('.jg-handle > g');
    if (handle && handle.dataset.home === undefined) {
      handle.dataset.home = handle.getAttribute('transform');
    }
    if (lod === 'far') {
      // Far out the handle stands beside the label, which leaves room
      // for it within the column.
      const sx = k / kx();
      const gap = 8 / kx();
      const hw = handle ? handle.querySelector('text').getComputedTextLength() * sx : 0;
      const room = column - PAD_LEFT - PAD_RIGHT - (handle ? hw + gap : 0);
      fit(title, g.dataset.title, room);
      if (handle) {
        const end = PAD_LEFT + title.getComputedTextLength() * sx + gap + hw / 2;
        handle.setAttribute('transform', 'translate(' + end + ' ' + NODE_MID + ')');
      }
    } else {
      const room = pill - (handle ? HANDLE_W : 0) - PAD_LEFT - PAD_RIGHT;
      fit(title, g.dataset.title, room);
      if (handle) handle.setAttribute('transform', handle.dataset.home);
    }
    const name = g.querySelector('.f');
    if (lod === 'near') {
      if (name.dataset.full === undefined) name.dataset.full = name.textContent;
      fit(name, name.dataset.full, pill - (handle ? HANDLE_W : 0) - PAD_LEFT - PAD_RIGHT);
    }
  }

  // Set `t` to `full`, cut with an ellipsis to `room` units of its pill's
  // width. In the scene, text is counter-scaled horizontally (--sx = k / kx),
  // so a unit of its own length covers `sx` units of the pill; in the peek's
  // screen-space layer, `sx` is 1.
  function fit(t, full, room, sx = k / kx()) {
    t.textContent = full;
    const len = () => t.getComputedTextLength() * sx;
    if (len() <= room) return;
    let n = Math.max(1, Math.floor(full.length * room / len()));
    t.textContent = full.slice(0, n) + '…';
    while (n > 1 && len() > room) {
      n -= 1;
      t.textContent = full.slice(0, n) + '…';
    }
  }

  // Which labels win a collision: current, route, selected, bundles, nodes
  // with children in this view, then leaves.
  function rank(t) {
    const g = t.closest('.jg-node');
    if (!g) return 3;
    const c = g.classList;
    return c.contains('current') ? 0 : c.contains('spine') ? 1 : c.contains('sel') ? 2
      : c.contains('folded') || c.contains('unfolded') ? 4 : 5;
  }

  // Put the scene on stage, and the route into the panel's breadcrumb.
  function mount(next) {
    clutterK = null;
    hidePeek();
    stage.replaceChildren(next);
    svg = next;
    info.route.textContent = '';
    const route = (svg.dataset.route || '').split(' ').filter(Boolean);
    // A one-node route would only repeat the title.
    info.route.hidden = route.length < 2;
    const [name, says] = VIEWS[svg.dataset.view] || ['', ''];
    viewLine.querySelector('b').textContent = name;
    viewLine.querySelector('span').textContent = says;
    route.forEach((i, n) => {
      if (n > 0) {
        const sep = document.createElement('span');
        sep.className = 'jg-sep';
        sep.textContent = ' › ';
        info.route.appendChild(sep);
      }
      const seg = document.createElement('span');
      seg.className = 'jg-crumb';
      seg.dataset.i = i;
      seg.dataset.key = nodeEl(i).dataset.key;
      seg.textContent = nodeEl(i).dataset.title;
      info.route.appendChild(seg);
    });
    apply();
  }

  function parse(markup) {
    const holder = document.createElement('div');
    holder.innerHTML = markup;
    return holder.querySelector('svg');
  }

  // A keyboard-driven move glides; a pointer-driven one follows the hand.
  let glideTimer = 0;
  function glide() {
    stage.classList.add('glide');
    clearTimeout(glideTimer);
    glideTimer = setTimeout(() => stage.classList.remove('glide'), 220);
  }

  function nodeEl(i, within) {
    return (within || svg).querySelector('.jg-nodes .jg-node[data-i="' + i + '"]');
  }

  // The item's pill in layout (SVG user) coordinates.
  function layoutBox(el) {
    const m = /translate\(([-\d.]+)[ ,]+([-\d.]+)\)/.exec(el.getAttribute('transform'));
    const r = el.querySelector('rect');
    return { x: parseFloat(m[1]), y: parseFloat(m[2]), w: r.width.baseVal.value, h: r.height.baseVal.value };
  }

  // The item's pill in stage (CSS px) coordinates.
  function box(el) {
    const b = layoutBox(el);
    const sx = kx();
    return { x: b.x * sx + tx, y: b.y * k + ty, w: b.w * sx, h: b.h * k };
  }

  // Put `el` a third of the way across — the scene grows to the right, so
  // that is where what lies beyond an item stays in view.
  function centre(el) {
    const b = box(el);
    tx += innerWidth / 3 - (b.x + b.w / 2);
    ty += innerHeight / 2 - (b.y + b.h / 2);
    apply();
  }

  // The tree view draws each node once, so where its links go is not a line
  // of the tree: selecting a node outlines the nodes it links to and dims
  // everything that is neither those, the selection, nor the route. The links
  // view emits no `data-to` — its children already are the links.
  function highlightLinks(el) {
    for (const n of svg.querySelectorAll('.linked, .dim')) n.classList.remove('linked', 'dim');
    if (svg.dataset.view !== 'tree') return;
    const keep = new Set([el.dataset.i]);
    for (const t of (el.dataset.to || '').split(' ').filter(Boolean)) {
      const n = nodeEl(t);
      if (!n) continue;
      n.classList.add('linked');
      keep.add(t);
    }
    for (const n of svg.querySelectorAll('.jg-nodes .jg-node:not(.spine)')) {
      if (!keep.has(n.dataset.i)) n.classList.add('dim');
    }
    for (const e of svg.querySelectorAll('.jg-edge')) {
      if (!keep.has(e.dataset.b)) e.classList.add('dim');
    }
    for (const b of svg.querySelectorAll('.jg-bundle')) b.classList.add('dim');
  }

  function select(i, reveal) {
    const prev = svg.querySelector('.jg-node.sel');
    if (prev) prev.classList.remove('sel');
    const el = nodeEl(i);
    if (!el) return;
    el.classList.add('sel');
    highlightLinks(el);
    info.title.textContent = el.dataset.title;
    info.path.textContent = el.dataset.path;
    info.path.hidden = !el.dataset.path;
    info.cut.textContent = el.dataset.cut || '';
    info.cut.hidden = !el.dataset.cut;
    for (const c of info.route.querySelectorAll('.jg-crumb')) {
      c.classList.toggle('sel', c.dataset.i === String(i));
    }
    clutterK = null;
    declutter();
    if (!reveal) return;
    const b = box(el);
    const margin = 64;
    const bottom = Math.min(innerHeight, info.box.getBoundingClientRect().top) - margin / 2;
    if (b.x < margin || b.x + b.w > innerWidth - margin || b.y < margin || b.y + b.h > bottom) {
      glide();
      centre(el);
    }
  }

  // A re-layout: swap the scene, and move the camera so item `anchor` — the
  // one the reader acted on — is where it was on screen (found by its key, or
  // else where the old selection was). Then select item `i`, revealing it if
  // it is not the anchor and landed off screen.
  function update(markup, i, anchor) {
    const next = parse(markup);
    const el = nodeEl(anchor, next);
    if (!el) return;
    const old = svg.querySelector('.jg-nodes .jg-node[data-key="' + el.dataset.key + '"]') ||
      svg.querySelector('.jg-node.sel');
    const before = old && box(old);
    mount(next);
    if (before) {
      const after = box(el);
      tx += before.x - after.x;
      ty += before.y - after.y;
      apply();
    }
    unhover();
    select(i, i !== anchor);
  }

  function kMin() {
    const h = svg.querySelector('.jg-nodes').getBBox().height;
    const fit = h > 0 ? (innerHeight - 2 * MARGIN) / h : 1;
    return Math.min(1, Math.max(K_FLOOR, fit));
  }

  // Zoom about a stage point: the world point under it stays put, on each
  // axis at that axis's own scale. Clamped to [kMin, K_MAX]; a scale already
  // below kMin (the graph shrank by a fold) may stay, but not go lower.
  function zoom(factor, cx, cy) {
    const beforeX = kx(), beforeY = k;
    k = Math.min(K_MAX, Math.max(Math.min(kMin(), k), k * factor));
    tx = cx - (cx - tx) * (kx() / beforeX);
    ty = cy - (cy - ty) * (k / beforeY);
    apply();
  }

  // The keys zoom about the selection (where the keyboard's attention is).
  function selectionCentre() {
    const el = svg.querySelector('.jg-node.sel');
    if (!el) return { x: innerWidth / 2, y: innerHeight / 2 };
    const b = box(el);
    return { x: b.x + b.w / 2, y: b.y + b.h / 2 };
  }

  // A far-out bundle is a handle too: clicking it zooms in until its nodes
  // are readable, centred on it.
  function zoomIntoBundle(bundle) {
    const r = bundle.querySelector('rect').getBoundingClientRect();
    const cx = r.left + r.width / 2, cy = r.top + r.height / 2;
    glide();
    zoom(Math.max(1, BUNDLE_ZOOM / k), cx, cy);
    tx += innerWidth / 2 - cx;
    ty += innerHeight / 2 - cy;
    apply();
  }

  // The wheel pans (Shift+wheel sideways). Ctrl+wheel never arrives here:
  // GTK takes it first and the controller routes it to `zoom` at the pointer
  // (DESIGN D4).
  root.addEventListener('wheel', (e) => {
    e.preventDefault();
    track(e);
    const unit = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? innerHeight : 1;
    const dx = e.shiftKey && !e.deltaX ? e.deltaY : e.deltaX;
    const dy = e.shiftKey && !e.deltaX ? 0 : e.deltaY;
    tx -= dx * unit;
    ty -= dy * unit;
    apply();
  }, { passive: false });

  // The peek: resting the pointer on a folded node draws its children as a
  // temporary fan next to it — scene nodes under the scene's camera, so they
  // look as the nodes around them do at every level — in their own layer
  // over the scene, laid out as unfolding
  // would lay them out (one column right, centred on the node, one row apart,
  // an elbow trunk), over a backdrop so nothing beneath shows through, and
  // clamped to the viewport. Nothing else moves. It stays while the pointer
  // is on the node or the fan, and goes on leave, zoom, pan, resize or
  // re-layout. Its data is the node's `data-peek`, a JSON array of
  // `{title, file}`.
  let peekTimer = 0;
  // The key of the node a peek is pending or shown for: moving between the
  // parts of one node must not restart the delay.
  let peekKey = null;
  let ghost = null;
  function hidePeek() {
    clearTimeout(peekTimer);
    if (ghost) ghost.remove();
    ghost = null;
    peekKey = null;
  }
  function schedulePeek(el) {
    if (peekKey === el.dataset.key) return;
    hidePeek();
    peekKey = el.dataset.key;
    peekTimer = setTimeout(() => showPeek(el), PEEK_DELAY);
  }
  function svgEl(tag, attrs, parent) {
    const e = document.createElementNS(SVG_NS, tag);
    for (const [name, value] of Object.entries(attrs)) e.setAttribute(name, value);
    parent.appendChild(e);
    return e;
  }
  function elbowPath(x1, y1, x2, y2) {
    const mx = (x1 + x2) / 2;
    return 'M' + x1 + ' ' + y1 + 'H' + mx + 'V' + y2 + 'H' + x2;
  }
  function showPeek(el) {
    let entries = [];
    try {
      entries = JSON.parse(el.dataset.peek || '[]');
    } catch (_) {
      return;
    }
    const more = Number(el.dataset.peekMore || 0);
    if (more > 0) entries.push({ title: 'and ' + more + ' more', file: '' });
    if (!entries.length) return;
    // The fan is scene markup under the scene's own camera, so every rule
    // that styles a node at this zoom level styles these alike. Only the
    // pitch differs far out, where the scene's bare labels sit a few px
    // apart and are culled against each other, which a fan cannot be: there
    // its rows keep a readable distance.
    const lod = root.dataset.lod;
    const pill = layoutBox(el);
    const w = pill.w, h = pill.h;
    const column = Number(svg.dataset.column);
    const row = Math.max(Number(svg.dataset.row), PEEK_PITCH / k);
    const from = pill.x + w, midY = pill.y + h / 2;
    const n = entries.length;
    const pad = 8 / k;
    let x = from + (column - w);
    let first = midY - ((n - 1) / 2) * row;
    // Clamp to the viewport, in scene units.
    const view = { top: -ty / k, bottom: (innerHeight - ty) / k, right: (innerWidth - tx) / kx() };
    const top = first - h / 2, bottom = first + (n - 1) * row + h / 2;
    if (bottom - top < view.bottom - view.top - 2 * pad) {
      if (top < view.top + pad) first += view.top + pad - top;
      else if (bottom > view.bottom - pad) first -= bottom - view.bottom + pad;
    }
    // At the right edge the fan slides left, but never over its own node.
    const padX = 8 / kx();
    if (x + w + padX > view.right) x = Math.max(from + 2 * padX, view.right - w - padX);
    ghost = svgEl('g', {
      class: 'jg-ghost',
      transform: 'translate(' + tx + ' ' + ty + ') scale(' + kx() + ' ' + k + ')',
    }, peekLayer);
    svgEl('rect', {
      class: 'backdrop', x: from + 2 / kx(), y: first - h / 2 - pad,
      width: x + w + padX - from - 2 / kx(), height: (n - 1) * row + h + 2 * pad, rx: 8,
    }, ghost);
    const edges = svgEl('g', { class: 'jg-edges' }, ghost);
    const nodes = svgEl('g', { class: 'jg-nodes' }, ghost);
    entries.forEach(({ title, file }, j) => {
      const y = first + j * row;
      svgEl('path', { class: 'jg-edge', d: elbowPath(from, midY, x, y) }, edges);
      const g = svgEl('g', {
        class: 'jg-node', 'data-title': title,
        transform: 'translate(' + x + ' ' + (y - h / 2) + ')',
      }, nodes);
      svgEl('rect', { width: w, height: h, rx: 8 }, g);
      svgEl('circle', { class: 'dot', cy: h / 2, r: 3.5 }, g);
      svgEl('text', { class: 't', x: PAD_LEFT, y: 18 }, g).textContent = title;
      svgEl('text', { class: 'f', x: PAD_LEFT, y: 33 }, g).textContent = file;
      fitLabels(g, lod, column);
    });
  }
  // The pointer may cross from the node into its fan and back.
  peekLayer.addEventListener('pointerleave', (e) => {
    const into = e.relatedTarget && e.relatedTarget.closest && e.relatedTarget.closest('.jg-node');
    if (!into || into.dataset.key !== peekKey) hidePeek();
  });

  // Hover lights up the item and the edges that meet it, and a folded one
  // peeks once the pointer rests on it.
  let hovered = null;
  // The page learns where the pointer is from every pointer event it gets,
  // not only from motion: `Ctrl`+wheel may come before the pointer moves.
  function track(e) {
    pointer.x = e.clientX;
    pointer.y = e.clientY;
  }
  root.addEventListener('pointerover', track);
  root.addEventListener('pointerdown', track);
  function unhover() {
    if (!hovered) return;
    for (const h of root.querySelectorAll('.hover')) h.classList.remove('hover');
    hovered = null;
  }
  stage.addEventListener('pointerover', (e) => {
    const el = e.target.closest('.jg-nodes .jg-node');
    if (el !== hovered) {
      unhover();
      if (el) {
        hovered = el;
        el.classList.add('hover');
        const i = el.dataset.i;
        for (const p of svg.querySelectorAll('path[data-a="' + i + '"], path[data-b="' + i + '"]')) {
          p.classList.add('hover');
        }
      }
    }
    if (el && el.classList.contains('folded')) schedulePeek(el);
    else hidePeek();
  });
  stage.addEventListener('pointerleave', (e) => {
    unhover();
    if (!(e.relatedTarget && peekLayer.contains(e.relatedTarget))) hidePeek();
  });

  let drag = null;
  // The item a click just selected, so the controller's echo does not pan.
  let clicked = null;
  stage.addEventListener('pointerdown', (e) => {
    if (e.button !== 0) return;
    drag = { x: e.clientX, y: e.clientY, tx, ty, moved: false };
  });
  // On the window, not the stage: a drag may leave the stage, and pointer
  // capture would retarget the `click` that ends it away from the item.
  function onMove(e) {
    track(e);
    if (!drag) return;
    // A release outside the window never reaches `pointerup` here.
    if (!(e.buttons & 1)) { onUp(); return; }
    const dx = e.clientX - drag.x, dy = e.clientY - drag.y;
    if (!drag.moved && Math.hypot(dx, dy) < 4) return;
    drag.moved = true;
    root.classList.add('dragging');
    tx = drag.tx + dx;
    ty = drag.ty + dy;
    apply();
  }
  function onUp() {
    if (!drag) return;
    root.classList.remove('dragging');
    // Let the click that follows this release see whether it was a drag.
    setTimeout(() => { drag = null; }, 0);
  }
  window.addEventListener('pointermove', onMove);
  window.addEventListener('pointerup', onUp);
  // A resized window keeps the world point at its centre where it was.
  let size = { w: innerWidth, h: innerHeight };
  function onResize() {
    tx += (innerWidth - size.w) / 2;
    ty += (innerHeight - size.h) / 2;
    size = { w: innerWidth, h: innerHeight };
    clutterK = null;
    apply();
  }
  window.addEventListener('resize', onResize);
  stage.addEventListener('click', (e) => {
    if (drag && drag.moved) return;
    const bundle = e.target.closest('.jg-bundle');
    if (bundle) {
      zoomIntoBundle(bundle);
      return;
    }
    const el = e.target.closest('.jg-nodes .jg-node');
    if (!el) return;
    // The item is under the pointer already; panning it away would move a
    // double-click's second press onto a different item.
    clicked = el.dataset.key;
    if (e.target.closest('.jg-handle')) post.fold(el.dataset.key);
    else post.select(el.dataset.key);
  });
  stage.addEventListener('dblclick', (e) => {
    const el = e.target.closest('.jg-nodes .jg-node');
    if (el && !e.target.closest('.jg-handle')) post.open(el.dataset.key);
  });
  // A breadcrumb segment selects that route node — which may be off screen,
  // so this selection is revealed.
  info.route.addEventListener('click', (e) => {
    const seg = e.target.closest('.jg-crumb');
    if (seg) post.select(seg.dataset.key);
  });

  // The opening frame, route first: the whole route and the current node's
  // children, centred, when they fit the window at 1:1; else the whole route,
  // the root at the left margin and the children running off the right; only
  // a route wider than the window puts the current node a third across. The
  // spine sits on the vertical middle.
  function frame() {
    const cur = svg.querySelector('.jg-node.current');
    const first = nodeEl(0);
    if (!cur || !first) return;
    const c = layoutBox(cur);
    const left = layoutBox(first).x;
    const routeRight = c.x + c.w;
    let right = routeRight;
    for (const child of svg.querySelectorAll('.jg-node[data-parent="' + cur.dataset.i + '"]')) {
      const b = layoutBox(child);
      right = Math.max(right, b.x + b.w);
    }
    const room = innerWidth - 2 * MARGIN;
    if (routeRight - left > room) {
      centre(cur);
      return;
    }
    tx = right - left <= room ? (innerWidth - (right - left)) / 2 - left : MARGIN - left;
    ty = innerHeight / 2 - (c.y + c.h / 2);
    apply();
  }

  window.__jmnj_graph = {
    select: (i) => {
      const el = nodeEl(i);
      select(i, !el || clicked !== el.dataset.key);
      clicked = null;
    },
    update: (markup, i, anchor) => {
      update(markup, i, anchor);
      clicked = null;
    },
    // `Ctrl`+wheel zooms about the pointer, `+` / `-` about the selection.
    zoom: (factor, at) => {
      const p = at === 'pointer' ? pointer : selectionCentre();
      zoom(factor, p.x, p.y);
    },
    // `=`: back to 1:1 on the current node.
    reset: () => {
      k = 1;
      apply();
      const el = svg.querySelector('.jg-node.current');
      if (el) { glide(); centre(el); }
    },
    close: () => {
      hidePeek();
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('resize', onResize);
      root.remove();
      delete window.__jmnj_graph;
    },
  };

  mount(parse(svgMarkup));
  frame();
  select(selected, false);
})

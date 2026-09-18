// The document-graph overlay (DESIGN D14). Viewport glue in the D12 sense:
// the scene arrives as finished SVG from `core::graph`; this only puts it on
// screen, pans and zooms it, swaps level-of-detail classes, and reports
// clicks. The controller owns the selection and the layout: a click calls
// `post.select(key)`, a click on a cluster or a `+n` badge `post.expand(key)`,
// a double-click `post.open(key)` (all built in `scripts.rs` over the shell's
// post seam; an item's key survives re-layouts, its index does not), and the
// controller answers through `window.__jmnj_graph.select(i)`, or
// `update(svg, i)` after a re-layout.
//
// Called as `(GRAPH_JS)(svgMarkup, css, selected, post)`.
(function (svgMarkup, css, selected, post) {
  if (window.__jmnj_graph) window.__jmnj_graph.close();

  const root = document.createElement('div');
  root.id = '__jmnj_graph';
  root.innerHTML =
    '<style>' + css + '</style>' +
    '<div class="jg-stage"></div>' +
    '<div class="jg-peek" hidden></div>' +
    '<div class="jg-info"><div class="jg-info-title"></div>' +
    '<div class="jg-info-path"></div><div class="jg-info-route"></div></div>' +
    '<div class="jg-keys"><kbd>Enter</kbd> open &nbsp; <kbd>hjkl</kbd> move &nbsp; ' +
    '<kbd>v</kbd> view &nbsp; wheel pan &nbsp; <kbd>Ctrl</kbd>+wheel zoom &nbsp; ' +
    '<kbd>Esc</kbd> close</div>';
  document.documentElement.appendChild(root);

  const stage = root.querySelector('.jg-stage');
  const peek = root.querySelector('.jg-peek');
  const info = {
    box: root.querySelector('.jg-info'),
    title: root.querySelector('.jg-info-title'),
    path: root.querySelector('.jg-info-path'),
    route: root.querySelector('.jg-info-route'),
  };
  // Level-of-detail thresholds (DESIGN D14): near ≥ 0.7 > mid ≥ 0.4 > far.
  const NEAR = 0.7, FAR = 0.4;
  // Zoomed out, columns shrink less than rows: the horizontal scale stops at
  // KX_MIN, so a far label has room for more of its title. Text is
  // counter-scaled horizontally (`--sx` in graph.css), so glyphs never stretch.
  const KX_MIN = 0.55;
  // The far level's label size on screen, in px (the far `font-size` in graph.css).
  const FAR_FONT = 15;
  let svg = null;
  // The camera: a translation, and a vertical and horizontal scale.
  let tx = 0, ty = 0, k = 1;
  const kx = () => Math.max(k, KX_MIN);

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

  // Far out, labels are drawn at a fixed screen size over a shrinking scene,
  // so they would run into each other. Once per frame after a change: cut
  // each label to what fits one column pitch on screen, then keep labels in
  // priority order and hide (fade) any that would overlap one already kept.
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
      const far = root.dataset.lod === 'far';
      const labels = svg.querySelectorAll('.jg-nodes .t, .jg-bundle .t');
      const perChar = FAR_FONT * 0.55;
      const chars = Math.max(4, Math.floor((svg.dataset.column * kx() * 0.85) / perChar));
      for (const t of labels) {
        if (t.dataset.full === undefined) t.dataset.full = t.textContent;
        const full = t.dataset.full;
        t.textContent = far && full.length > chars ? full.slice(0, chars - 1) + '…' : full;
        t.classList.remove('culled');
      }
      if (!far) return;
      const kept = [];
      const hits = (b) => kept.some((o) =>
        b.left < o.right && o.left < b.right && b.top < o.bottom && o.top < b.bottom);
      for (const t of [...labels].sort((a, b) => rank(a) - rank(b))) {
        const g = t.closest('.jg-node');
        if (g && g.classList.contains('bundled') && !g.classList.contains('sel')) continue;
        const b = t.getBoundingClientRect();
        if (hits(b)) t.classList.add('culled');
        else kept.push(b);
      }
    });
  }

  // Which labels win a collision: current, spine, selected, clusters and
  // bundles, notes with something under them, then leaves.
  function rank(t) {
    const g = t.closest('.jg-node');
    if (!g) return 3;
    const c = g.classList;
    return c.contains('current') ? 0 : c.contains('spine') ? 1 : c.contains('sel') ? 2
      : c.contains('cluster') ? 3 : c.contains('inner') || c.contains('collapsed') ? 4 : 5;
  }

  // Put the scene on stage, and the route into the panel's breadcrumb.
  function mount(next) {
    clutterK = null;
    stage.replaceChildren(next);
    svg = next;
    info.route.textContent = '';
    const route = (svg.dataset.route || '').split(' ').filter(Boolean);
    // A one-note route would only repeat the title.
    info.route.hidden = route.length < 2;
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
    return (within || svg).querySelector('.jg-node[data-i="' + i + '"]');
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

  // The tree view draws each note once, so where its links go is not a line
  // of the tree: selecting a note outlines the notes it links to and dims
  // everything that is neither those, the selection, nor the route. The links
  // view emits no `data-to` — its fans already are the links.
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
    for (const n of svg.querySelectorAll('.jg-node:not(.spine)')) {
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

  // A re-layout: swap the scene, and move the camera so the selected item is
  // where it was on screen — found by its key, or else where the old
  // selection was.
  function update(markup, i) {
    const next = parse(markup);
    const el = nodeEl(i, next);
    if (!el) return;
    const old = svg.querySelector('.jg-node[data-key="' + el.dataset.key + '"]') ||
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
    select(i, false);
  }

  // Zoom about a stage point: the world point under it stays put, on each
  // axis at that axis's own scale.
  function zoom(factor, cx, cy) {
    const beforeX = kx(), beforeY = k;
    k = Math.min(3, Math.max(0.08, k * factor));
    tx = cx - (cx - tx) * (kx() / beforeX);
    ty = cy - (cy - ty) * (k / beforeY);
    apply();
  }

  // The wheel pans (Shift+wheel sideways). Ctrl+wheel never arrives here:
  // GTK takes it first and the controller routes it to `zoom` at the cursor
  // (DESIGN D4).
  root.addEventListener('wheel', (e) => {
    e.preventDefault();
    const unit = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? innerHeight : 1;
    const dx = e.shiftKey && !e.deltaX ? e.deltaY : e.deltaX;
    const dy = e.shiftKey && !e.deltaX ? 0 : e.deltaY;
    tx -= dx * unit;
    ty -= dy * unit;
    apply();
  }, { passive: false });

  // Hovering a collapsed cluster or a `+n` badge previews the titles it holds
  // (`data-members`, from core), next to the item.
  function showPeek(el) {
    const titles = (el.dataset.members || '').split('\n').filter(Boolean);
    if (!titles.length) return hidePeek();
    peek.textContent = '';
    for (const t of titles) {
      const line = document.createElement('div');
      line.textContent = t;
      if (/^and \d+ more$/.test(t) && t === titles[titles.length - 1]) line.className = 'more';
      peek.appendChild(line);
    }
    const b = el.getBoundingClientRect();
    peek.hidden = false;
    const p = peek.getBoundingClientRect();
    const left = b.right + 10 + p.width < innerWidth ? b.right + 10 : Math.max(8, b.left - 10 - p.width);
    peek.style.left = left + 'px';
    peek.style.top = Math.max(8, Math.min(innerHeight - p.height - 8, b.top)) + 'px';
    peek.classList.add('shown');
  }
  function hidePeek() {
    peek.classList.remove('shown');
    peek.hidden = true;
  }

  // Hover lights up the item and the edges that meet it.
  let hovered = null;
  function unhover() {
    hidePeek();
    if (!hovered) return;
    for (const h of root.querySelectorAll('.hover')) h.classList.remove('hover');
    hovered = null;
  }
  stage.addEventListener('pointerover', (e) => {
    const el = e.target.closest('.jg-node');
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
    // A cluster previews from anywhere on it; a note only from its badge.
    const previewed = el && el.classList.contains('collapsed') &&
      (el.classList.contains('cluster') || e.target.closest('.jg-badge'));
    if (!previewed) hidePeek();
    else if (peek.hidden) showPeek(el);
  });
  stage.addEventListener('pointerleave', unhover);

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
  stage.addEventListener('click', (e) => {
    if (drag && drag.moved) return;
    const el = e.target.closest('.jg-node');
    if (!el) return;
    // The item is under the pointer already; panning it away would move a
    // double-click's second press onto a different item.
    clicked = el.dataset.key;
    const expand = e.target.closest('.jg-badge') ||
      (el.classList.contains('cluster') && el.classList.contains('collapsed'));
    if (expand) post.expand(el.dataset.key);
    else post.select(el.dataset.key);
  });
  stage.addEventListener('dblclick', (e) => {
    const el = e.target.closest('.jg-node');
    if (el) post.open(el.dataset.key);
  });
  // A breadcrumb segment selects that spine note — which may be off screen,
  // so this selection is revealed.
  info.route.addEventListener('click', (e) => {
    const seg = e.target.closest('.jg-crumb');
    if (seg) post.select(seg.dataset.key);
  });

  // The opening frame: the whole route and the current note's first column
  // of links when that fits the window at 1:1, centred, the spine on the
  // vertical middle; otherwise the current note a third of the way across.
  function frame() {
    const cur = svg.querySelector('.jg-node.current');
    const first = svg.querySelector('.jg-node[data-i="0"]');
    if (!cur || !first) return;
    let right = layoutBox(cur);
    right = right.x + right.w;
    for (const c of svg.querySelectorAll('.jg-node[data-parent="' + cur.dataset.i + '"]')) {
      const b = layoutBox(c);
      right = Math.max(right, b.x + b.w);
    }
    const left = layoutBox(first).x;
    const margin = 48;
    if (right - left > innerWidth - 2 * margin) {
      centre(cur);
      return;
    }
    const c = layoutBox(cur);
    tx = (innerWidth - (right - left)) / 2 - left;
    ty = innerHeight / 2 - (c.y + c.h / 2);
    apply();
  }

  window.__jmnj_graph = {
    select: (i) => {
      const el = nodeEl(i);
      select(i, !el || clicked !== el.dataset.key);
      clicked = null;
    },
    update: (markup, i) => {
      update(markup, i);
      clicked = null;
    },
    zoom,
    // `=`: back to 1:1 on the current note.
    reset: () => {
      k = 1;
      apply();
      const el = svg.querySelector('.jg-node.current');
      if (el) { glide(); centre(el); }
    },
    close: () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      root.remove();
      delete window.__jmnj_graph;
    },
  };

  mount(parse(svgMarkup));
  frame();
  select(selected, false);
})

// Find in page: the `/` search, `n`/`N`, and clearing on `Esc`. Viewport glue
// in the D12 sense, identical on every toolkit: the controller drives it
// through `window.__jmnj_search` and reads the result from `post`.
//
// Matches are painted with the CSS Custom Highlight API, never with the DOM
// selection, so a search leaves the selection and every clipboard alone.
// Every match is in the `all` highlight, except the current one, which is in
// `active` only: the stylesheet colours the two (`highlight-color`,
// `highlight-active-color`), and one colour must not blend into the other.
//
// The text searched is the document as rendered, not the raw DOM text: the
// scan walks `main.markdown-body`, skips what is not rendered, collapses
// whitespace runs to one space where CSS collapses them (the `\n` comrak
// emits for a soft line break, the indentation of an HTML block), and puts a
// separator between blocks, so a match may cross inline elements
// (`foo **bar**`) but never a block boundary. Case-insensitive; `n`/`N` wrap.
//
// Called as `(SEARCH_JS)(names, post)` at document start: `names` are the
// highlight names the stylesheet styles (`{all, active}`), and
// `post(id, count, active)` reports a search's match count and current match
// (0-based, -1 when there is none) under the id the controller gave it.
(function (names, post) {
  // Between blocks: a query cannot contain it, so no match spans it.
  const SEP = '\u0000';
  const WS = /[ \t\n\r\f]/;
  // SVG elements whose text is never painted, even inside `<text>`.
  const SVG_UNPAINTED = new Set(['style', 'script', 'title', 'desc', 'metadata']);

  let search = null; // { id, ranges: StaticRange[], active, all, current }

  // One text node's share of the scan string: it starts at scan offset
  // `start`, and `offs` maps each of its scan characters back to an offset in
  // the node (`null` when the mapping is the identity).
  const scan = (root) => {
    const parts = [];
    const segs = [];
    let at = 0;
    // Whether the scan string ends in a (collapsed) space or a separator:
    // whitespace after either adds nothing.
    let quiet = true;
    const separate = () => {
      if (at > 0 && parts[parts.length - 1] !== SEP) {
        parts.push(SEP);
        at += 1;
      }
      quiet = true;
    };
    const text = (node, collapse) => {
      const raw = node.data;
      if (raw.length === 0) return;
      const lower = raw.toLowerCase();
      // A character whose lower case is longer (`İ`) would shift every
      // offset after it; fold per character only then.
      const low = lower.length === raw.length
        ? lower
        : Array.from(raw, (c) => (c.toLowerCase().length === 1 ? c.toLowerCase() : c)).join('');
      let out = '';
      let offs = null;
      for (let i = 0; i < low.length; i++) {
        const c = low[i];
        if (collapse && WS.test(c)) {
          if (quiet) {
            if (!offs) offs = Array.from({ length: out.length }, (_, k) => k);
            continue;
          }
          if (offs) offs.push(i);
          out += ' ';
          quiet = true;
          continue;
        }
        if (offs) offs.push(i);
        out += c === '\u00a0' ? ' ' : c;
        quiet = false;
      }
      if (out.length === 0) return;
      // A collapsed run keeps its first character, so `out` still lines up
      // with the node wherever nothing was dropped.
      if (offs && offs.every((o, k) => o === k)) offs = null;
      segs.push({ node, start: at, offs });
      parts.push(out);
      at += out.length;
    };
    // `collapse`: whether CSS collapses whitespace in `el`; `painted`:
    // whether its own text is painted. `visibility` is inherited, but a child
    // may turn it back on, so the walk goes on below a hidden element. Inside
    // an SVG only `<text>` and `<foreignObject>` paint text: a diagram's
    // `<style>` or `<title>` does not, yet computes to `display: inline`.
    const walk = (el, collapse, painted) => {
      for (let n = el.firstChild; n; n = n.nextSibling) {
        if (n.nodeType === 3) {
          if (painted) text(n, collapse);
          continue;
        }
        if (n.nodeType !== 1) continue;
        const cs = getComputedStyle(n);
        if (cs.display === 'none') continue;
        const tag = n.localName;
        const svg = n instanceof SVGElement;
        const visible = cs.visibility === 'visible';
        const inner = !svg ? visible && (painted || !(el instanceof SVGElement))
          : tag === 'text' || tag === 'foreignObject' ? visible
          : SVG_UNPAINTED.has(tag) || tag === 'svg' ? false
          : visible && painted;
        // A hard line break ends a line as a block does.
        const block = tag === 'br' || (svg
          ? tag === 'text' || tag === 'svg'
          : !(cs.display.startsWith('inline') || cs.display === 'contents'));
        const ws = cs.whiteSpaceCollapse || cs.whiteSpace;
        const collapseInner = ws === 'collapse' || ws === 'normal' || ws === 'nowrap';
        if (block) separate();
        if (tag === 'details' && !n.open) {
          // Only the summary of a closed <details> is on screen.
          const summary = n.querySelector(':scope > summary');
          if (summary) {
            separate();
            walk(summary, collapseInner, getComputedStyle(summary).visibility === 'visible');
            separate();
          }
        } else {
          walk(n, collapseInner, inner);
        }
        if (block) separate();
      }
    };
    walk(root, true, getComputedStyle(root).visibility === 'visible');
    return { string: parts.join(''), segs };
  };

  // The segment holding scan offset `p` (the last one starting at or before it).
  const segAt = (segs, p) => {
    let lo = 0, hi = segs.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (segs[mid].start <= p) lo = mid; else hi = mid - 1;
    }
    return segs[lo];
  };
  const nodeOffset = (seg, p) => {
    const k = p - seg.start;
    return seg.offs ? seg.offs[k] : k;
  };

  // A live `Range` over a match, to measure it.
  const liveRange = (r) => {
    const live = document.createRange();
    live.setStart(r.startContainer, r.startOffset);
    live.setEnd(r.endContainer, r.endOffset);
    return live;
  };

  // Scroll `r` into view if any of it is outside: inside each scrolling
  // ancestor first (a wide code block, a zoomed diagram), then the page,
  // centring on each axis that has to move.
  const reveal = (r) => {
    const live = liveRange(r);
    let box = live.getBoundingClientRect();
    const d = document.documentElement;
    for (let el = r.startContainer.parentElement; el && el !== document.body && el !== d; el = el.parentElement) {
      const cs = getComputedStyle(el);
      const sx = /auto|scroll/.test(cs.overflowX) && el.scrollWidth > el.clientWidth;
      const sy = /auto|scroll/.test(cs.overflowY) && el.scrollHeight > el.clientHeight;
      if (!sx && !sy) continue;
      const b = el.getBoundingClientRect();
      const left = b.left + el.clientLeft, top = b.top + el.clientTop;
      if (sx && (box.left < left || box.right > left + el.clientWidth)) {
        el.scrollLeft += box.left + box.width / 2 - (left + el.clientWidth / 2);
      }
      if (sy && (box.top < top || box.bottom > top + el.clientHeight)) {
        el.scrollTop += box.top + box.height / 2 - (top + el.clientHeight / 2);
      }
      box = live.getBoundingClientRect();
    }
    if (box.top < 0 || box.bottom > innerHeight) {
      window.scrollBy({ top: box.top + box.height / 2 - innerHeight / 2, behavior: 'instant' });
    }
  };

  const unregister = () => {
    CSS.highlights.delete(names.all);
    CSS.highlights.delete(names.active);
  };

  // Make match `i` the current one: out of `all`, into `active`, revealed.
  const activate = (i) => {
    const s = search;
    if (s.active >= 0) s.all.add(s.ranges[s.active]);
    s.active = i;
    const r = s.ranges[i];
    s.all.delete(r);
    s.current.clear();
    s.current.add(r);
    reveal(r);
  };

  const report = () => post(search.id, search.ranges.length, search.active);

  window.__jmnj_search = {
    find(query, id) {
      unregister();
      search = { id, ranges: [], active: -1, all: new Highlight(), current: new Highlight() };
      const needle = query.toLowerCase().replace(/[ \t\n\r\f]+/g, ' ').replace(/\u00a0/g, ' ');
      const main = document.querySelector('main.markdown-body');
      if (needle.length > 0 && main) {
        const { string, segs } = scan(main);
        for (let p = string.indexOf(needle); p >= 0; p = string.indexOf(needle, p + needle.length)) {
          const a = segAt(segs, p), b = segAt(segs, p + needle.length - 1);
          search.ranges.push(new StaticRange({
            startContainer: a.node,
            startOffset: nodeOffset(a, p),
            endContainer: b.node,
            endOffset: nodeOffset(b, p + needle.length - 1) + 1,
          }));
        }
      }
      const ranges = search.ranges;
      if (ranges.length > 0) {
        // `new Highlight(...ranges)` throws past some tens of thousands of
        // arguments; adding one at a time does not.
        for (const r of ranges) search.all.add(r);
        CSS.highlights.set(names.all, search.all);
        CSS.highlights.set(names.active, search.current);
        // The first match at or below the top of the viewport, as vim
        // searches forward from the cursor; wrapping to the first. Matches
        // are in document order, so their tops rise with the index.
        let lo = 0, hi = ranges.length;
        while (lo < hi) {
          const mid = (lo + hi) >> 1;
          if (liveRange(ranges[mid]).getBoundingClientRect().top >= 0) hi = mid; else lo = mid + 1;
        }
        activate(lo % ranges.length);
      }
      report();
    },
    // Move the current match `delta` matches on (negative: back), wrapping.
    step(delta) {
      if (!search || search.ranges.length === 0) return;
      const n = search.ranges.length;
      activate((((search.active + delta) % n) + n) % n);
      report();
    },
    clear() {
      unregister();
      search = null;
    },
  };
})

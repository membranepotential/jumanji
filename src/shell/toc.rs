//! Native table-of-contents view — zathura's "index" mode.
//!
//! The document's flat `Vec<Heading>` (each carrying a level) is turned into a
//! collapsible tree and shown in a `gtk::ListBox` inside a scroller. This is the
//! page the shell swaps in (via a `gtk::Stack`) when TOC mode is entered; the
//! keymap's `Mode::Toc` bindings drive selection, expand/collapse, and select.
//!
//! The tree shape and the visible-row set are pure model state (`Model`); the
//! ListBox is rebuilt from that model on every structural change. A TOC is at
//! most a few dozen rows, so a full rebuild is cheaper to reason about than
//! diffing.
//!
//! The **selection** is not duplicated in the model — the ListBox's own selected
//! row is the single source of truth (Out-of-the-Tar-Pit: no derivable state).
//! `j`/`k`/expand/collapse move that selection, mouse clicks move it for free,
//! and `Enter`/activate read it back. A visible-row index maps to a tree node
//! via `Model::visible()`, so no synchronising glue is needed between the two.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{
    Align, CssProvider, Label, ListBox, ListBoxRow, PolicyType, ScrolledWindow, SelectionMode,
};

use crate::core::Heading;

/// One outline entry in the arena-allocated tree.
struct Node {
    text: String,
    anchor: String,
    /// Depth for indentation (1 = top-level `#`).
    level: u8,
    /// Index into the original `Vec<Heading>` (used to sync `section`).
    heading_index: usize,
    parent: Option<usize>,
    children: Vec<usize>,
    expanded: bool,
}

/// Pure tree state. Node indices are stable. The current selection lives in the
/// ListBox, not here (see the module header).
#[derive(Default)]
struct Model {
    nodes: Vec<Node>,
    roots: Vec<usize>,
}

impl Model {
    /// The nodes visible in reading order, descending only into expanded ones.
    fn visible(&self) -> Vec<usize> {
        let mut out = Vec::new();
        for &r in &self.roots {
            self.push_visible(r, &mut out);
        }
        out
    }

    fn push_visible(&self, node: usize, out: &mut Vec<usize>) {
        out.push(node);
        if self.nodes[node].expanded {
            for &c in &self.nodes[node].children {
                self.push_visible(c, out);
            }
        }
    }
}

/// Callback the shell installs to jump when a row is activated (double-click or
/// `Enter` while the ListBox holds keyboard focus).
type ActivateHandler = Rc<RefCell<Option<Box<dyn Fn()>>>>;

/// The TOC page widget plus its backing model.
pub struct TocView {
    scroller: ScrolledWindow,
    list: ListBox,
    model: Rc<RefCell<Model>>,
    on_activate: ActivateHandler,
}

impl TocView {
    pub fn new() -> Self {
        install_css();
        let list = ListBox::new();
        list.set_selection_mode(SelectionMode::Single);
        list.add_css_class("jmnj-toc");

        let scroller = ScrolledWindow::new();
        scroller.set_policy(PolicyType::Never, PolicyType::Automatic);
        scroller.set_vexpand(true);
        scroller.set_hexpand(true);
        scroller.set_child(Some(&list));

        // Double-clicking (or activating) a row selects it and jumps, matching
        // GTK convention. The activated row becomes the selection first, so the
        // handler's `selected()` read sees the right entry.
        let on_activate: ActivateHandler = Rc::new(RefCell::new(None));
        {
            let list_for_select = list.clone();
            let on_activate = on_activate.clone();
            list.connect_row_activated(move |_, row| {
                list_for_select.select_row(Some(row));
                if let Some(cb) = on_activate.borrow().as_ref() {
                    cb();
                }
            });
        }

        Self {
            scroller,
            list,
            model: Rc::new(RefCell::new(Model::default())),
            on_activate,
        }
    }

    pub fn widget(&self) -> &ScrolledWindow {
        &self.scroller
    }

    /// Install the jump-on-activate handler (double-click / Enter-in-listview).
    pub fn set_activate_handler(&self, handler: impl Fn() + 'static) {
        *self.on_activate.borrow_mut() = Some(Box::new(handler));
    }

    /// Rebuild the tree from `headings` (all nodes expanded), select the entry
    /// for `section`, and paint. Call every time TOC mode is (re)entered so the
    /// tree reflects the current document.
    pub fn rebuild(&self, headings: &[Heading], section: usize, dark: bool) {
        let mut model = Model::default();
        // Stack of (node_index, level) for the current ancestry chain.
        let mut stack: Vec<usize> = Vec::new();
        for (i, h) in headings.iter().enumerate() {
            // Pop ancestors that are not shallower than this heading.
            while let Some(&top) = stack.last() {
                if model.nodes[top].level >= h.level {
                    stack.pop();
                } else {
                    break;
                }
            }
            let parent = stack.last().copied();
            let idx = model.nodes.len();
            model.nodes.push(Node {
                text: h.text.clone(),
                anchor: h.anchor.clone(),
                level: h.level,
                heading_index: i,
                parent,
                children: Vec::new(),
                expanded: true,
            });
            match parent {
                Some(p) => model.nodes[p].children.push(idx),
                None => model.roots.push(idx),
            }
            stack.push(idx);
        }
        // Node for the requested section (or the first node) is the initial
        // selection painted by `refresh`.
        let initial = model
            .nodes
            .iter()
            .position(|n| n.heading_index == section)
            .unwrap_or(0);
        *self.model.borrow_mut() = model;
        self.set_dark(dark);
        self.refresh(initial);
    }

    pub fn set_dark(&self, dark: bool) {
        if dark {
            self.list.add_css_class("dark");
        } else {
            self.list.remove_css_class("dark");
        }
    }

    /// The tree node backing the currently-selected ListBox row, if any.
    fn selected_node(&self) -> Option<usize> {
        let pos = self.list.selected_row()?.index() as usize;
        self.model.borrow().visible().get(pos).copied()
    }

    /// Move the selection by `delta` visible rows (clamped). Positive is down.
    pub fn move_selection(&self, delta: i32) {
        let len = self.model.borrow().visible().len() as i32;
        if len == 0 {
            return;
        }
        let cur = self.list.selected_row().map(|r| r.index()).unwrap_or(0);
        let next = (cur + delta).clamp(0, len - 1) as usize;
        self.select_row(next);
    }

    /// Expand the selected node's children (no-op if it is a leaf).
    pub fn expand_selected(&self) {
        let Some(sel) = self.selected_node() else {
            return;
        };
        let changed = {
            let mut model = self.model.borrow_mut();
            if model.nodes[sel].children.is_empty() || model.nodes[sel].expanded {
                false
            } else {
                model.nodes[sel].expanded = true;
                true
            }
        };
        if changed {
            self.refresh(sel);
        }
    }

    /// Collapse the selected node; if it is already collapsed (or a leaf), move
    /// the selection up to its parent instead (zathura `h` behaviour).
    pub fn collapse_selected(&self) {
        let Some(sel) = self.selected_node() else {
            return;
        };
        let target = {
            let mut model = self.model.borrow_mut();
            let has_open_children =
                !model.nodes[sel].children.is_empty() && model.nodes[sel].expanded;
            if has_open_children {
                model.nodes[sel].expanded = false;
                Some(sel)
            } else {
                // A root leaf has no parent: nothing to do.
                model.nodes[sel].parent
            }
        };
        if let Some(target) = target {
            self.refresh(target);
        }
    }

    /// The selected entry's anchor and heading index, for a jump-and-leave.
    pub fn selected(&self) -> Option<(String, usize)> {
        let sel = self.selected_node()?;
        let model = self.model.borrow();
        model
            .nodes
            .get(sel)
            .map(|n| (n.anchor.clone(), n.heading_index))
    }

    /// Rebuild the ListBox rows from the model and select the row for
    /// `select_node`, scrolling it into view.
    fn refresh(&self, select_node: usize) {
        self.list.remove_all();
        let model = self.model.borrow();
        let visible = model.visible();
        let mut selected_pos = 0;
        for (pos, &node) in visible.iter().enumerate() {
            let n = &model.nodes[node];
            let label = Label::new(Some(&n.text));
            label.set_halign(Align::Start);
            label.set_xalign(0.0);
            label.set_margin_start(6 + (n.level.saturating_sub(1) as i32) * 18);
            // A collapsed node with children gets a marker so the tree structure
            // is legible without a disclosure triangle widget.
            if !n.children.is_empty() && !n.expanded {
                label.set_text(&format!("{}  …", n.text));
            }
            let row = ListBoxRow::new();
            row.add_css_class("jmnj-toc-row");
            row.set_child(Some(&label));
            self.list.append(&row);
            if node == select_node {
                selected_pos = pos;
            }
        }
        drop(model);
        self.select_row(selected_pos);
    }

    /// Highlight the row at visible index `pos` and scroll it into view.
    fn select_row(&self, pos: usize) {
        if let Some(row) = self.list.row_at_index(pos as i32) {
            self.list.select_row(Some(&row));
            // Focusing the row makes the ScrolledWindow bring it into view.
            row.grab_focus();
        }
    }
}

fn install_css() {
    let provider = CssProvider::new();
    provider.load_from_string(
        "\
        .jmnj-toc { background:#ffffff; }\n\
        .jmnj-toc row.jmnj-toc-row { padding:2px 8px; color:#1f2328; font-size:11pt; }\n\
        .jmnj-toc row.jmnj-toc-row:selected { background:#cfe8ff; color:#0b1b2b; }\n\
        .jmnj-toc.dark { background:#1a1a1a; }\n\
        .jmnj-toc.dark row.jmnj-toc-row { color:#d6d6d6; }\n\
        .jmnj-toc.dark row.jmnj-toc-row:selected { background:#2c5480; color:#ffffff; }\n\
        ",
    );
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

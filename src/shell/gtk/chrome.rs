//! The GTK implementation of [`Chrome`]: the girara-style bottom bar, the
//! table-of-contents page, and the stack that swaps between the document and
//! the TOC.
//!
//! Pure delegation — [`Bar`] and [`TocView`] hold the widget logic; this is the
//! one type the controller talks to, so a toolkit without widgets can render
//! the same three concerns as in-page overlays instead.

use gtk::prelude::*;
use gtk::{Box as GtkBox, Entry, Orientation, Stack, Widget};

use crate::controller::toolkit::{Chrome, Prompt};
use crate::core::Heading;

use super::bar::Bar;
use super::toc::TocView;

/// Stack page names: the document, and the table of contents.
const PAGE_CONTENT: &str = "content";
const PAGE_TOC: &str = "toc";

/// The reader's chrome: status/input bar plus the content↔TOC stack.
#[derive(Clone)]
pub struct GtkChrome {
    bar: Bar,
    toc: TocView,
    stack: Stack,
    /// The whole column — stack above, bar below — for the window to adopt.
    layout: GtkBox,
}

impl GtkChrome {
    /// Build the chrome around `content`, the document view's widget.
    pub fn new(content: &impl IsA<Widget>) -> Self {
        let bar = Bar::new();
        let toc = TocView::new();

        let stack = Stack::new();
        stack.set_vexpand(true);
        stack.set_hexpand(true);
        stack.add_named(content, Some(PAGE_CONTENT));
        stack.add_named(toc.widget(), Some(PAGE_TOC));
        stack.set_visible_child_name(PAGE_CONTENT);

        let layout = GtkBox::new(Orientation::Vertical, 0);
        layout.append(&stack);
        layout.append(bar.widget());

        Self {
            bar,
            toc,
            stack,
            layout,
        }
    }

    /// The chrome's whole widget column, for the window to adopt as its child.
    pub fn widget(&self) -> &GtkBox {
        &self.layout
    }

    /// The input bar's entry, so the shell can wire its `activate` signal.
    pub fn entry(&self) -> &Entry {
        self.bar.entry()
    }

    /// Install the jump-on-activate handler for a TOC row (double-click, or
    /// `Enter` while the list has keyboard focus).
    pub fn set_toc_activate_handler(&self, handler: impl Fn() + 'static) {
        self.toc.set_activate_handler(handler);
    }
}

impl Chrome for GtkChrome {
    fn set_trail(&self, segments: Vec<String>) {
        self.bar.set_trail(segments);
    }

    fn refit_trail(&self) {
        self.bar.refit_trail();
    }

    fn status_columns(&self) -> usize {
        self.bar.status_columns()
    }

    fn set_status_right(&self, percent: u32, pending: &str, zoom: &str) {
        self.bar.set_status_right(percent, pending, zoom);
    }

    fn set_message(&self, msg: &str) {
        self.bar.set_message(msg);
    }

    fn open_input(&self, prompt: Prompt) {
        self.bar.open_input(prompt);
    }

    fn close_input(&self) {
        self.bar.close_input();
    }

    fn prompt(&self) -> Option<Prompt> {
        self.bar.prompt()
    }

    fn input_query(&self) -> String {
        self.bar.input_query()
    }

    fn set_input_query(&self, query: &str) {
        self.bar.set_input_query(query);
    }

    fn show_toc(&self, headings: &[Heading], section: usize, dark: bool) {
        self.toc.rebuild(headings, section, dark);
        self.stack.set_visible_child_name(PAGE_TOC);
    }

    fn hide_toc(&self) {
        self.stack.set_visible_child_name(PAGE_CONTENT);
    }

    fn toc_move(&self, delta: i32) {
        self.toc.move_selection(delta);
    }

    fn toc_expand(&self) {
        self.toc.expand_selected();
    }

    fn toc_collapse(&self) {
        self.toc.collapse_selected();
    }

    fn toc_selected(&self) -> Option<(String, usize)> {
        self.toc.selected()
    }

    fn set_dark(&self, dark: bool) {
        self.toc.set_dark(dark);
    }
}

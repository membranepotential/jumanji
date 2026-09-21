//! The GTK implementation of [`Chrome`]: the girara-style bottom bar, and the
//! table of contents laid over the document.
//!
//! Pure delegation — [`Bar`] and [`TocView`] hold the widget logic; this is the
//! one type the controller talks to, so a toolkit without widgets can render
//! the same three concerns as in-page overlays instead.

use gtk::prelude::*;
use gtk::{Box as GtkBox, Entry, Orientation, Overlay, Widget};

use crate::controller::toolkit::{Chrome, Prompt};
use crate::core::Heading;

use super::bar::Bar;
use super::toc::TocView;

/// The reader's chrome: status/input bar, and the TOC over the document.
#[derive(Clone)]
pub struct GtkChrome {
    bar: Bar,
    toc: TocView,
    /// The whole column — document and TOC above, bar below — for the window.
    layout: GtkBox,
}

impl GtkChrome {
    /// Build the chrome around `content`, the document view's widget.
    pub fn new(content: &impl IsA<Widget>) -> Self {
        let bar = Bar::new();
        let toc = TocView::new();

        // An overlay, not a stack: the document stays on screen under the
        // TOC. A web view taken off screen keeps its last frame, and shows it
        // again on return until it paints anew; that flashed the graph after
        // `t`, Tab, Tab, since the graph closed while the view was away.
        let overlay = Overlay::new();
        overlay.set_vexpand(true);
        overlay.set_hexpand(true);
        overlay.set_child(Some(content));
        overlay.add_overlay(toc.widget());
        toc.widget().set_visible(false);

        let layout = GtkBox::new(Orientation::Vertical, 0);
        layout.append(&overlay);
        layout.append(bar.widget());

        Self { bar, toc, layout }
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

    fn set_status_right(&self, percent: u32, pending: &str, search: &str, zoom: &str) {
        self.bar.set_status_right(percent, pending, search, zoom);
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
        // Visible first: the rebuild focuses the selected row, and GTK
        // focuses no widget that is not shown.
        self.toc.widget().set_visible(true);
        self.toc.rebuild(headings, section, dark);
    }

    fn hide_toc(&self) {
        self.toc.widget().set_visible(false);
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

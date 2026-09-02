//! Girara-style bottom chrome: a status line plus an input entry that appears
//! for `/` search and `:` commands. Flat, minimal, monospace — no toolbars.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::pango::EllipsizeMode;
use gtk::prelude::*;
use gtk::{Align, Box as GtkBox, CssProvider, Entry, Label, Orientation};

use crate::controller::toolkit::Prompt;
use crate::core::jumplist::breadcrumb;

/// The bottom bar: `[ status_left ............ status_right ]` with a hidden
/// single-line [`Entry`] stacked above it for search/command input.
#[derive(Clone)]
pub struct Bar {
    container: GtkBox,
    status_left: Label,
    status_right: Label,
    entry: Entry,
    /// The prompt kind while the input bar is open; `None` when it is hidden.
    prompt: Rc<Cell<Option<Prompt>>>,
    /// The jumplist breadcrumb the left label shows at rest, as display names
    /// (oldest first, current document last). Kept whole so the line can be
    /// re-fitted whenever the available width changes.
    trail: Rc<RefCell<Vec<String>>>,
    /// Whether the left label currently holds a transient message rather than
    /// the breadcrumb — a re-fit must not clobber it.
    message_shown: Rc<Cell<bool>>,
}

impl Bar {
    pub fn new() -> Self {
        let status_left = Label::new(None);
        // Fill (not Start) so the label is *allocated* the free space rather
        // than shrinking to its text: its width is the fitting budget, and a
        // Start-aligned label would report only what it already shows — a
        // budget that shrinks with every re-fit. `xalign` keeps the text left.
        status_left.set_halign(Align::Fill);
        status_left.set_hexpand(true);
        status_left.set_xalign(0.0);
        // Cut from the left, so the current document stays readable when the
        // breadcrumb outgrows the bar between re-fits (and keeps a long trail
        // from forcing a window minimum width).
        status_left.set_ellipsize(EllipsizeMode::Start);
        status_left.add_css_class("status-left");

        let status_right = Label::new(None);
        status_right.set_halign(Align::End);
        status_right.set_xalign(1.0);
        status_right.add_css_class("status-right");

        let statusbar = GtkBox::new(Orientation::Horizontal, 0);
        statusbar.add_css_class("statusbar");
        statusbar.append(&status_left);
        statusbar.append(&status_right);

        let entry = Entry::new();
        entry.add_css_class("inputbar");
        entry.set_visible(false);

        let container = GtkBox::new(Orientation::Vertical, 0);
        container.append(&entry);
        container.append(&statusbar);

        install_css();

        Self {
            container,
            status_left,
            status_right,
            entry,
            prompt: Rc::new(Cell::new(None)),
            trail: Rc::new(RefCell::new(Vec::new())),
            message_shown: Rc::new(Cell::new(false)),
        }
    }

    pub fn widget(&self) -> &GtkBox {
        &self.container
    }

    pub fn entry(&self) -> &Entry {
        &self.entry
    }

    /// Show the jumplist breadcrumb — the route to the current document, e.g.
    /// `index.md > topic.md > note.md` — on the left, fitted to the bar.
    pub fn set_trail(&self, segments: Vec<String>) {
        *self.trail.borrow_mut() = segments;
        self.message_shown.set(false);
        self.refit_trail();
    }

    /// Re-render the breadcrumb for the current width. Cheap and idempotent, so
    /// the status refresh can call it unconditionally; a transient message wins
    /// until something replaces it.
    pub fn refit_trail(&self) {
        if self.message_shown.get() {
            return;
        }
        let text = breadcrumb(&self.trail.borrow(), self.status_columns());
        if self.status_left.text() != text {
            self.status_left.set_text(&text);
        }
    }

    /// How many monospace columns the left status label currently spans — the
    /// budget for anything laid out to fit it. [`usize::MAX`] before the first
    /// allocation: nothing is known not to fit, and the ellipsize keeps that
    /// honest until the next re-fit.
    pub fn status_columns(&self) -> usize {
        let width = self.status_left.width();
        if width <= 0 {
            return usize::MAX;
        }
        // Monospace (see the CSS below): one glyph's advance is every glyph's.
        // Measured over a run of them, so the per-column rounding averages out.
        const SAMPLE: &str = "0000000000";
        let run = self
            .status_left
            .create_pango_layout(Some(SAMPLE))
            .pixel_size()
            .0;
        if run <= 0 {
            return usize::MAX;
        }
        (width as usize * SAMPLE.len()) / run as usize
    }

    /// Right-hand status: any pending count/key indicator, a zoom indicator when
    /// either zoom axis is off 100% (e.g. `150%/120%T`), and the scroll percent.
    pub fn set_status_right(&self, percent: u32, pending: &str, zoom: &str) {
        let mut text = String::new();
        if !pending.is_empty() {
            text.push_str(pending);
            text.push_str("   ");
        }
        if !zoom.is_empty() {
            text.push_str(zoom);
            text.push_str("   ");
        }
        text.push_str(&format!("{percent}%"));
        self.status_right.set_text(&text);
    }

    /// Transient hint shown on the left (e.g. "not implemented", errors). It
    /// holds the label until the breadcrumb is set again.
    pub fn set_message(&self, msg: &str) {
        self.message_shown.set(true);
        self.status_left.set_text(msg);
    }

    /// Open the input bar for `prompt`, seeded with its prefix, and focus it.
    pub fn open_input(&self, prompt: Prompt) {
        self.prompt.set(Some(prompt));
        self.entry.set_text(prompt.prefix());
        self.entry.set_visible(true);
        self.entry.grab_focus();
        self.entry.set_position(-1);
    }

    /// Hide and clear the input bar.
    pub fn close_input(&self) {
        self.prompt.set(None);
        self.entry.set_text("");
        self.entry.set_visible(false);
    }

    /// The active prompt kind, or `None` when the input bar is hidden.
    pub fn prompt(&self) -> Option<Prompt> {
        self.prompt.get()
    }

    /// The current input text with the leading prefix character removed.
    pub fn input_query(&self) -> String {
        let text = self.entry.text();
        let mut chars = text.chars();
        chars.next();
        chars.as_str().to_string()
    }

    /// Replace the input text, preserving the leading prompt prefix, and put the
    /// cursor at the end. Used by tab completion.
    pub fn set_input_query(&self, query: &str) {
        let prefix = self.prompt.get().map(Prompt::prefix).unwrap_or("");
        self.entry.set_text(&format!("{prefix}{query}"));
        self.entry.set_position(-1);
    }
}

fn install_css() {
    let provider = CssProvider::new();
    provider.load_from_string(
        "\
        .statusbar { padding: 1px 6px; font-family: monospace; font-size: 10pt; }\n\
        .status-left, .status-right { color: @theme_fg_color; }\n\
        .inputbar { font-family: monospace; font-size: 10pt; border-radius: 0; }\n\
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

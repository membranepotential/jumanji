//! Girara-style bottom chrome: a status line plus an input entry that appears
//! for `/` search and `:` commands. Flat, minimal, monospace — no toolbars.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{Align, Box as GtkBox, CssProvider, Entry, Label, Orientation};

/// Which kind of input the bar is currently collecting. The prompt character
/// and how `Enter` is interpreted (search vs command) both follow from this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    /// `/` incremental find.
    Search,
    /// `:` command line.
    Command,
}

impl Prompt {
    fn prefix(self) -> &'static str {
        match self {
            Prompt::Search => "/",
            Prompt::Command => ":",
        }
    }
}

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
}

impl Bar {
    pub fn new() -> Self {
        let status_left = Label::new(None);
        status_left.set_halign(Align::Start);
        status_left.set_hexpand(true);
        status_left.set_xalign(0.0);
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
        }
    }

    pub fn widget(&self) -> &GtkBox {
        &self.container
    }

    pub fn entry(&self) -> &Entry {
        &self.entry
    }

    pub fn set_filename(&self, name: &str) {
        self.status_left.set_text(name);
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

    /// Transient hint shown on the left (e.g. "not implemented", errors).
    pub fn set_message(&self, msg: &str) {
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

    pub fn is_input_visible(&self) -> bool {
        WidgetExt::is_visible(&self.entry)
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

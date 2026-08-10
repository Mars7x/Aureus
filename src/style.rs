use std::cell::RefCell;

use gtk::gdk;

// Layout CSS is always active. The Aureus palette is a separate provider so
// Preferences can unload it cleanly and return to the stock system style.
const AUREUS_THEME_CSS: &str = include_str!("aureus-theme.css");

std::thread_local! {
    static THEME_PROVIDER: RefCell<Option<gtk::CssProvider>> = RefCell::new(None);
}

const LAYOUT_CSS: &str = r#"
.metric-card {
  padding: 10px 12px;
  min-width: 108px;
}

.detail-hero {
  padding: 18px 20px;
}

.chart-card {
  padding: 10px 12px 12px;
}

.upcoming-card {
  padding: 12px 14px;
}

.range-toggle {
  min-width: 0;
  padding-left: 6px;
  padding-right: 6px;
}

/* Keep the narrow view switcher visually continuous with the page.
 * This uses libadwaita's own window surface variable rather than a custom color. */
.mobile-bottom-nav {
  background-color: var(--window-bg-color);
}

progressbar.shortcut-refresh {
  min-height: 2px;
  padding: 0;
}

progressbar.shortcut-refresh > trough {
  min-height: 2px;
  border-radius: 0;
  background: transparent;
}

progressbar.shortcut-refresh > trough > progress {
  min-height: 2px;
  border-radius: 0;
}

/* Transactions are grouped as one boxed card per date. The outer list and
 * section rows are structural only, so date headings sit cleanly above cards. */
.transactions-list,
.transactions-list > row,
.transactions-list > row:hover,
.transactions-list > row:selected,
row.transaction-date-section,
row.transaction-date-section:hover,
row.transaction-date-section:selected {
  background: transparent;
  box-shadow: none;
  border: none;
  outline: none;
  padding: 0;
}

.transaction-day-list {
  margin: 0;
}

.allocation-legend-row {
  padding: 5px 6px;
  border-radius: 8px;
}

.allocation-legend-row:hover {
  background-color: alpha(@window_fg_color, 0.06);
}

row.search-keyboard-selected {
  background-color: alpha(@accent_bg_color, 0.16);
  outline: none;
}

/* Stock pictures intentionally are not GtkButtons: native button chrome can
 * extend beyond a circular avatar. The control is constrained to the avatar
 * footprint, and only pointer hover adds a tint over the image itself. */
.stock-picture-control,
.stock-picture-control:hover,
.stock-picture-control:focus,
.stock-picture-control:focus-visible {
  min-width: 0;
  min-height: 0;
  padding: 0;
  margin: 0;
  border: none;
  border-radius: 9999px;
  background: transparent;
  background-image: none;
  box-shadow: none;
  outline: none;
}

.stock-picture-hover-tint {
  border-radius: 9999px;
  background-color: rgba(255, 255, 255, 0);
  transition: background-color 120ms ease;
}

.stock-picture-control:hover .stock-picture-hover-tint {
  background-color: rgba(255, 255, 255, 0.10);
}
"#;

pub fn install() {
    let Some(display) = gdk::Display::default() else {
        return;
    };

    let layout_provider = gtk::CssProvider::new();
    layout_provider.load_from_string(LAYOUT_CSS);
    gtk::style_context_add_provider_for_display(
        &display,
        &layout_provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let theme_provider = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(
        &display,
        &theme_provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
    );
    THEME_PROVIDER.with(|slot| {
        *slot.borrow_mut() = Some(theme_provider);
    });
}

pub fn set_aureus_theme(enabled: bool) {
    THEME_PROVIDER.with(|slot| {
        let slot = slot.borrow();
        let Some(provider) = slot.as_ref() else {
            return;
        };
        provider.load_from_string(if enabled { AUREUS_THEME_CSS } else { "" });
    });
}

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use adw::prelude::*;
use adw::{
    AboutDialog, ActionRow, AlertDialog, Application, ApplicationWindow, Avatar, Breakpoint,
    BreakpointCondition, ComboRow, Dialog, EntryRow, HeaderBar, NavigationPage,
    NavigationSplitView, NavigationView, PreferencesGroup, SidebarMode, StatusPage,
    SwitchRow, Toast, ToastOverlay, ToolbarView, ViewStack, ViewSwitcherBar, ViewSwitcherSidebar, WrapBox,
};
use gtk::gio;
use gtk::glib;
use gtk::{
    Align, Box as GtkBox, Button, DropDown, EventControllerKey, FileDialog, GestureDrag, Image, Label, ListBox,
    ListBoxRow, MenuButton, Orientation, Overlay, PolicyType, ProgressBar, Revealer, SearchEntry, SelectionMode, Spinner, Stack,
    StringList, ToggleButton,
};

use crate::allocation_ring::{allocation_color, AllocationRing, AllocationSlice};
use crate::chart::PriceChart;
use crate::database::Database;
use crate::dividend_chart::DividendChart;
use crate::fx::{self, FxQuote};
use crate::market_data::{self, DividendHistory, History, HistoryRange, Quote, SearchResult};
use crate::sparkline::Sparkline;
use crate::model::{
    convert_currency, Account, CashEntry, DividendEvent, FxRate, NewAccount, NewTransaction, NewWatchlistItem,
    Position, PricePoint, SplitEvent, Transaction, WatchlistItem,
};

const BASE_CURRENCY_KEY: &str = "base-currency";
const LAST_ACCOUNT_ID_KEY: &str = "last-account-id";
const AUREUS_THEME_KEY: &str = "use-aureus-theme";
const USD_CAD_PAIR: &str = "USDCAD";
const QUOTE_CACHE_SECONDS: i64 = 15 * 60;
const FX_CACHE_SECONDS: i64 = 12 * 60 * 60;
const DIVIDEND_CACHE_SECONDS: i64 = 24 * 60 * 60;

#[derive(Clone)]
struct AppState {
    database: Rc<Database>,
}

#[derive(Clone)]
struct ToastManager {
    overlay: ToastOverlay,
    active: Rc<RefCell<HashMap<String, Toast>>>,
}

impl ToastManager {
    fn new(overlay: &ToastOverlay) -> Self {
        Self {
            overlay: overlay.clone(),
            active: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    fn add_toast(&self, toast: Toast) {
        let Some(title) = toast.title().map(|title| title.to_string()) else {
            self.overlay.add_toast(toast);
            return;
        };

        if let Some(existing) = self.active.borrow().get(&title).cloned() {
            // Re-adding the same AdwToast resets its timeout if it is already
            // visible, or bumps it forward if it is queued. This reinforces
            // one notification instead of building up duplicate toasts.
            self.overlay.add_toast(existing);
            return;
        }

        let active = self.active.clone();
        let key = title.clone();
        toast.connect_dismissed(move |_| {
            active.borrow_mut().remove(&key);
        });
        self.active.borrow_mut().insert(title, toast.clone());
        self.overlay.add_toast(toast);
    }
}

#[derive(Clone)]
struct UiRefs {
    state: AppState,
    navigation: NavigationView,
    toast_overlay: ToastManager,
    total_value: Label,
    total_gain: Label,
    realized_gain: Label,
    investment_return: Label,
    cost_basis: Label,
    quote_note: Label,
    portfolio_history_chart: PriceChart,
    portfolio_history_range: Rc<Cell<HistoryRange>>,
    overview_list: ListBox,
    allocation_ring: AllocationRing,
    allocation_legend: GtkBox,
    overview_holdings_layout: WrapBox,
    upcoming_box: GtkBox,
    accounts_list: ListBox,
    watchlist_list: ListBox,
    dividend_list: ListBox,
    dividend_recent_heading: Label,
    dividend_income: Label,
    dividend_yield: Label,
    dividend_status: Label,
    dividend_chart: DividendChart,
    dividend_period: DropDown,
    dividend_period_options: Rc<RefCell<Vec<DividendPeriod>>>,
    dividend_period_updating: Rc<Cell<bool>>,
    overview_stack: Stack,
    accounts_stack: Stack,
    dividends_stack: Stack,
    watchlist_stack: Stack,
    search_entry: SearchEntry,
    search_top_slot: GtkBox,
    search_bottom_slot: GtkBox,
    current_page: Rc<RefCell<String>>,
    market_refresh_generation: Rc<Cell<u64>>,
    dividend_refresh_generation: Rc<Cell<u64>>,
    watchlist_refresh_generation: Rc<Cell<u64>>,
    portfolio_history_generation: Rc<Cell<u64>>,
    pull_refresh_revealer: Revealer,
    pull_refresh_visual_revealer: Revealer,
    pull_refresh_spinner: Spinner,
    pull_refresh_icon: Image,
    pull_refresh_active: Rc<Cell<bool>>,
    shortcut_refresh_bar: ProgressBar,
    shortcut_refresh_active: Rc<Cell<bool>>,
    shortcut_refresh_generation: Rc<Cell<u64>>,
    detail_refresh: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    page_scroll_adjustments: Rc<RefCell<HashMap<String, gtk::Adjustment>>>,
}



fn stock_avatar(provider_symbol: &str, fallback_text: &str, size: i32) -> Avatar {
    let avatar = Avatar::new(size, Some(fallback_text), true);
    avatar.set_widget_name(&format!(
        "aureus-stock-picture-{}",
        crate::stock_image::picture_key(provider_symbol)
    ));
    apply_saved_stock_image(&avatar, provider_symbol);
    avatar
}

fn apply_saved_stock_image(avatar: &Avatar, provider_symbol: &str) {
    let Ok(Some(data)) = crate::stock_image::load_stock_image(provider_symbol) else {
        avatar.set_custom_image(None::<&gtk::gdk::Paintable>);
        return;
    };
    let bytes = glib::Bytes::from_owned(data.bytes);
    if let Ok(texture) = gtk::gdk::Texture::from_bytes(&bytes) {
        avatar.set_custom_image(Some(&texture));
    } else {
        avatar.set_custom_image(None::<&gtk::gdk::Paintable>);
    }
}

fn stock_image_colors(provider_symbol: &str) -> Vec<(f64, f64, f64)> {
    crate::stock_image::load_stock_image(provider_symbol)
        .ok()
        .flatten()
        .map(|image| image.colors)
        .unwrap_or_default()
}

fn refresh_stock_picture_widgets(root: &gtk::Widget, provider_symbol: &str) {
    let target = format!(
        "aureus-stock-picture-{}",
        crate::stock_image::picture_key(provider_symbol)
    );
    let mut widgets = vec![root.clone()];
    while let Some(widget) = widgets.pop() {
        if widget.widget_name().to_string() == target {
            if let Ok(avatar) = widget.clone().downcast::<Avatar>() {
                apply_saved_stock_image(&avatar, provider_symbol);
            }
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            widgets.push(current);
        }
    }
}

fn present_choose_stock_picture(
    parent: &ApplicationWindow,
    refs: UiRefs,
    provider_symbol: String,
) {
    let parent = parent.clone();
    glib::MainContext::default().spawn_local(async move {
        let dialog = FileDialog::new();
        dialog.set_title("Choose Stock Picture");
        dialog.set_accept_label(Some("Choose"));

        // Build the chooser filter from the image loaders that are actually
        // configured in Glycin at runtime. Unsupported file types therefore
        // never appear as selectable stock pictures.
        let supported_mime_types = glycin::Loader::supported_mime_types().await;
        if supported_mime_types.is_empty() {
            refs.toast_overlay
                .add_toast(Toast::new("No supported image loaders are available"));
            return;
        }

        let image_filter = gtk::FileFilter::new();
        image_filter.set_name(Some("Supported Images"));
        for mime_type in supported_mime_types {
            image_filter.add_mime_type(mime_type.as_str());
        }
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&image_filter);
        dialog.set_filters(Some(&filters));
        dialog.set_default_filter(Some(&image_filter));

        let Ok(file) = dialog.open_future(Some(&parent)).await else {
            return;
        };
        let Some(path) = file.path() else {
            refs.toast_overlay
                .add_toast(Toast::new("The selected picture could not be opened"));
            return;
        };

        match crate::stock_image::save_stock_image(&provider_symbol, &path).await {
            Ok(data) => {
                let bytes = glib::Bytes::from_owned(data.bytes);
                match gtk::gdk::Texture::from_bytes(&bytes) {
                    Ok(_) => {
                        let root: gtk::Widget = parent.clone().upcast();
                        refresh_stock_picture_widgets(&root, &provider_symbol);
                        refs.refresh();
                        refs.toast_overlay
                            .add_toast(Toast::new("Stock picture updated"));
                    }
                    Err(error) => refs.toast_overlay.add_toast(Toast::new(
                        &format!("Could not display stock picture: {error}"),
                    )),
                }
            }
            Err(error) => refs.toast_overlay.add_toast(Toast::new(&format!(
                "Could not use stock picture: {error}"
            ))),
        }
    });
}

fn present_stock_picture_actions(
    parent: &ApplicationWindow,
    refs: UiRefs,
    provider_symbol: String,
) {
    let has_picture = crate::stock_image::has_stock_image(&provider_symbol);
    let dialog = AlertDialog::builder()
        .heading("Stock Picture")
        .body(if has_picture {
            "Select a replacement picture or delete the current one"
        } else {
            "Select a picture for this stock"
        })
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("select", "Select Picture");
    if has_picture {
        dialog.add_response("delete", "Delete Picture");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    }
    dialog.set_default_response(Some("select"));
    dialog.set_close_response("cancel");

    {
        let parent_weak = parent.downgrade();
        let refs = refs.clone();
        let provider_symbol = provider_symbol.clone();
        dialog.connect_response(Some("select"), move |_, _| {
            if let Some(parent) = parent_weak.upgrade() {
                present_choose_stock_picture(&parent, refs.clone(), provider_symbol.clone());
            }
        });
    }

    if has_picture {
        let parent_weak = parent.downgrade();
        let refs = refs.clone();
        let provider_symbol = provider_symbol.clone();
        dialog.connect_response(Some("delete"), move |_, _| {
            match crate::stock_image::remove_stock_image(&provider_symbol) {
                Ok(_) => {
                    if let Some(parent) = parent_weak.upgrade() {
                        let root: gtk::Widget = parent.upcast();
                        refresh_stock_picture_widgets(&root, &provider_symbol);
                    }
                    refs.refresh();
                    refs.toast_overlay.add_toast(Toast::new("Stock picture deleted"));
                }
                Err(error) => refs.toast_overlay.add_toast(Toast::new(&format!(
                    "Could not delete stock picture: {error}"
                ))),
            }
        });
    }

    dialog.present(Some(parent));
}

fn stock_picture_control(avatar: &Avatar, size: i32) -> Overlay {
    avatar.add_css_class("stock-picture-avatar");
    avatar.set_size_request(size, size);

    let hover_tint = GtkBox::new(Orientation::Horizontal, 0);
    hover_tint.set_can_target(false);
    hover_tint.set_size_request(size, size);
    hover_tint.set_halign(Align::Center);
    hover_tint.set_valign(Align::Center);
    hover_tint.add_css_class("stock-picture-hover-tint");

    // GtkBox stretches children across its cross-axis by default. The previous
    // Overlay therefore grew to the full hero-card height, and the focus tint
    // exposed that stretched allocation as the tall gray oval around the avatar.
    // Keep the interactive overlay physically locked to the avatar dimensions;
    // hover feedback can then never paint outside the circular picture.
    let overlay = Overlay::new();
    overlay.set_child(Some(avatar));
    overlay.add_overlay(&hover_tint);
    overlay.set_size_request(size, size);
    overlay.set_halign(Align::Start);
    overlay.set_valign(Align::Center);
    overlay.set_hexpand(false);
    overlay.set_vexpand(false);
    overlay.set_tooltip_text(Some("Change stock picture"));
    overlay.set_focusable(true);
    overlay.set_overflow(gtk::Overflow::Hidden);
    overlay.add_css_class("stock-picture-control");
    overlay
}

fn connect_stock_picture_control(control: &Overlay, refs: UiRefs, provider_symbol: String) {
    let click = gtk::GestureClick::new();
    {
        let control = control.clone();
        let refs = refs.clone();
        let provider_symbol = provider_symbol.clone();
        click.connect_released(move |_, _, _, _| {
            let Some(root) = control.root() else {
                return;
            };
            let Ok(window) = root.downcast::<ApplicationWindow>() else {
                return;
            };
            present_stock_picture_actions(&window, refs.clone(), provider_symbol.clone());
        });
    }
    control.add_controller(click);

    let keys = EventControllerKey::new();
    {
        let control = control.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key != gtk::gdk::Key::Return && key != gtk::gdk::Key::KP_Enter {
                return glib::Propagation::Proceed;
            }
            let Some(root) = control.root() else {
                return glib::Propagation::Stop;
            };
            let Ok(window) = root.downcast::<ApplicationWindow>() else {
                return glib::Propagation::Stop;
            };
            present_stock_picture_actions(&window, refs.clone(), provider_symbol.clone());
            glib::Propagation::Stop
        });
    }
    control.add_controller(keys);
}

fn allocation_colors_collide(a: (f64, f64, f64), b: (f64, f64, f64)) -> bool {
    let dr = a.0 - b.0;
    let dg = a.1 - b.1;
    let db = a.2 - b.2;
    (dr * dr + dg * dg + db * db).sqrt() < 0.13
}

#[derive(Clone)]
struct DetailRefs {
    app: UiRefs,
    position_id: i64,
    provider_symbol: String,
    currency: String,
    chart: PriceChart,
    current_price: Label,
    day_change: Label,
    quote_status: Label,
    market_value: Label,
    total_gain: Label,
    base_currency: String,
    usd_cad: Option<f64>,
    range_return: Label,
    range_high_low: Label,
    history_status: Label,
    active_range: Rc<Cell<HistoryRange>>,
    pull_refresh: DetailPullRefresh,
    shortcut_refresh: DetailShortcutRefresh,
    generation: Rc<Cell<u64>>,
}

struct HistoryLoadResult {
    generation: u64,
    range: HistoryRange,
    result: Result<History, String>,
    announce: bool,
}

#[derive(Clone)]
struct WatchDetailRefs {
    app: UiRefs,
    provider_symbol: String,
    currency: String,
    chart: PriceChart,
    current_price: Label,
    day_change: Label,
    quote_status: Label,
    quote_refresh_box: GtkBox,
    quote_refresh_spinner: Spinner,
    quote_refresh_status: Label,
    range_return: Label,
    range_high_low: Label,
    history_status: Label,
    active_range: Rc<Cell<HistoryRange>>,
    pull_refresh: DetailPullRefresh,
    shortcut_refresh: DetailShortcutRefresh,
    generation: Rc<Cell<u64>>,
}

struct WatchHistoryLoadResult {
    generation: u64,
    range: HistoryRange,
    result: Result<History, String>,
    announce: bool,
}

struct WatchQuoteLoadResult {
    result: Result<Quote, String>,
}

#[derive(Clone)]
struct DividendDetailRefs {
    app: UiRefs,
    position_id: i64,
    provider_symbol: String,
    currency: String,
    annual_income: Label,
    per_share: Label,
    yield_label: Label,
    status: Label,
    list: ListBox,
    pull_refresh: DetailPullRefresh,
    shortcut_refresh: DetailShortcutRefresh,
}

struct DividendDetailLoadResult {
    result: Result<DividendHistory, String>,
    announce: bool,
}

#[derive(Clone)]
struct DetailPullRefresh {
    revealer: Revealer,
    visual_revealer: Revealer,
    spinner: Spinner,
    icon: Image,
    adjustment: gtk::Adjustment,
    pending: Rc<Cell<u8>>,
}

impl DetailPullRefresh {
    fn begin(&self, pending: u8) {
        self.pending.set(pending.max(1));
        self.icon.set_visible(false);
        self.spinner.set_visible(true);
        self.spinner.start();
        self.revealer.set_reveal_child(true);
        self.visual_revealer.set_reveal_child(true);
        reset_adjustment_to_top(&self.adjustment);
    }

    fn complete(&self) {
        let pending = self.pending.get();
        if pending > 1 {
            self.pending.set(pending - 1);
            return;
        }
        self.pending.set(0);
        self.finish();
    }

    fn cancel(&self) {
        self.pending.set(0);
        self.finish();
    }

    fn finish(&self) {
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.icon.set_visible(true);
        self.icon.set_opacity(1.0);
        reset_adjustment_to_top(&self.adjustment);
        self.revealer.set_reveal_child(false);
        self.visual_revealer.set_reveal_child(false);
        let adjustment = self.adjustment.clone();
        glib::timeout_add_local_once(Duration::from_millis(170), move || {
            reset_adjustment_to_top(&adjustment);
        });
    }
}


#[derive(Clone)]
struct DetailShortcutRefresh {
    bar: ProgressBar,
    active: Rc<Cell<bool>>,
    generation: Rc<Cell<u64>>,
    pending: Rc<Cell<u8>>,
}

impl DetailShortcutRefresh {
    fn new() -> Self {
        let bar = ProgressBar::new();
        bar.set_visible(false);
        bar.set_show_text(false);
        bar.set_hexpand(true);
        bar.set_halign(Align::Fill);
        bar.set_valign(Align::End);
        bar.add_css_class("shortcut-refresh");
        bar.set_can_target(false);
        bar.set_size_request(-1, 2);
        Self {
            bar,
            active: Rc::new(Cell::new(false)),
            generation: Rc::new(Cell::new(0)),
            pending: Rc::new(Cell::new(0)),
        }
    }

    fn begin(&self, pending: u8) -> bool {
        if self.active.get() {
            return false;
        }
        self.pending.set(pending.max(1));
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.active.set(true);
        self.bar.set_opacity(1.0);
        self.bar.set_fraction(0.015);
        self.bar.set_visible(true);

        let bar = self.bar.clone();
        let active = self.active.clone();
        let current_generation = self.generation.clone();
        glib::timeout_add_local(Duration::from_millis(16), move || {
            if !active.get() || current_generation.get() != generation {
                return glib::ControlFlow::Break;
            }
            let current = bar.fraction();
            let remaining = (0.94 - current).max(0.0);
            let step = (remaining * 0.035).max(0.0007);
            bar.set_fraction((current + step).min(0.94));
            glib::ControlFlow::Continue
        });
        true
    }

    fn complete(&self) {
        let pending = self.pending.get();
        if pending > 1 {
            self.pending.set(pending - 1);
            return;
        }
        self.pending.set(0);
        if !self.active.replace(false) {
            return;
        }

        let generation = self.generation.get();
        let bar = self.bar.clone();
        let active = self.active.clone();
        let current_generation = self.generation.clone();
        bar.set_fraction(1.0);
        bar.set_opacity(1.0);
        glib::timeout_add_local_once(Duration::from_millis(110), move || {
            if active.get() || current_generation.get() != generation {
                return;
            }
            let bar = bar.clone();
            let active = active.clone();
            let current_generation = current_generation.clone();
            let fade_started = std::time::Instant::now();
            glib::timeout_add_local(Duration::from_millis(16), move || {
                if active.get() || current_generation.get() != generation {
                    return glib::ControlFlow::Break;
                }
                let progress = (fade_started.elapsed().as_secs_f64() / 0.16).clamp(0.0, 1.0);
                bar.set_opacity(1.0 - progress);
                if progress >= 1.0 {
                    bar.set_visible(false);
                    bar.set_fraction(0.0);
                    bar.set_opacity(1.0);
                    glib::ControlFlow::Break
                } else {
                    glib::ControlFlow::Continue
                }
            });
        });
    }
}

fn complete_detail_refresh(pull: &DetailPullRefresh, shortcut: &DetailShortcutRefresh) {
    if pull.pending.get() > 0 {
        pull.complete();
    } else {
        shortcut.complete();
    }
}

fn transaction_kind_from_index(index: u32) -> &'static str {
    match index {
        1 => "SELL",
        2 => "OPEN",
        _ => "BUY",
    }
}

fn transaction_kind_index(kind: &str) -> u32 {
    match kind {
        "SELL" => 1,
        "OPEN" => 2,
        _ => 0,
    }
}

fn activity_sort_priority(kind: &str) -> u8 {
    match kind {
        "SPLIT" => 0,
        "OPEN" => 1,
        "BUY" => 2,
        "TRANSFER_IN" => 3,
        "SELL" => 4,
        "TRANSFER_OUT" => 5,
        _ => 6,
    }
}

fn validate_transaction_change(
    refs: &UiRefs,
    account_id: i64,
    provider_symbol: &str,
    editing_id: Option<i64>,
    kind: &str,
    timestamp: i64,
    shares: f64,
) -> Result<(), &'static str> {
    if shares <= 0.0 {
        return Err("Shares must be greater than zero");
    }

    let symbol = provider_symbol.to_ascii_uppercase();
    let transactions = refs
        .state
        .database
        .load_transactions()
        .unwrap_or_default()
        .into_iter()
        .filter(|transaction| {
            transaction.account_id == account_id
                && transaction.provider_symbol.eq_ignore_ascii_case(&symbol)
                && Some(transaction.id) != editing_id
        })
        .collect::<Vec<_>>();

    let opening_count = transactions
        .iter()
        .filter(|transaction| transaction.transaction_type == "OPEN")
        .count()
        + if kind == "OPEN" { 1 } else { 0 };
    if opening_count > 1 {
        return Err("Only one opening position can be recorded for a holding");
    }

    if kind == "OPEN" {
        if transactions.iter().any(|transaction| transaction.timestamp < timestamp) {
            return Err("Opening position must be dated before the other activity");
        }
    } else if let Some(opening) = transactions
        .iter()
        .find(|transaction| transaction.transaction_type == "OPEN")
    {
        if timestamp < opening.timestamp {
            return Err("Activity cannot be dated before the opening position");
        }
    }

    let mut events = transactions
        .into_iter()
        .map(|transaction| {
            (
                transaction.timestamp,
                transaction.id,
                transaction.transaction_type,
                transaction.shares,
            )
        })
        .collect::<Vec<_>>();
    for split in refs
        .state
        .database
        .split_events(&symbol)
        .unwrap_or_default()
    {
        events.push((split.timestamp, i64::MIN, "SPLIT".to_string(), split.ratio));
    }
    events.push((timestamp, editing_id.unwrap_or(i64::MAX), kind.to_string(), shares));
    events.sort_by_key(|event| (event.0, activity_sort_priority(&event.2), event.1));

    let mut held = 0.0;
    for (_, _, event_kind, event_shares) in events {
        match event_kind.as_str() {
            "SELL" | "TRANSFER_OUT" => {
                held -= event_shares;
                if held < -0.0005 {
                    return Err("Activity exceeds the shares held on that date");
                }
            }
            "BUY" | "OPEN" | "TRANSFER_IN" => held += event_shares,
            "SPLIT" => held *= event_shares,
            _ => {}
        }
    }
    Ok(())
}

impl UiRefs {
    fn refresh(&self) {
        let positions = match self.state.database.load_positions() {
            Ok(positions) => positions,
            Err(error) => {
                self.toast_overlay
                    .add_toast(Toast::new(&format!("Could not load portfolio: {error}")));
                return;
            }
        };
        let accounts = match self.state.database.load_accounts() {
            Ok(accounts) => accounts,
            Err(error) => {
                self.toast_overlay
                    .add_toast(Toast::new(&format!("Could not load accounts: {error}")));
                return;
            }
        };
        let watchlist = match self.state.database.load_watchlist() {
            Ok(items) => items,
            Err(error) => {
                self.toast_overlay
                    .add_toast(Toast::new(&format!("Could not load watchlist: {error}")));
                return;
            }
        };

        let base = base_currency(&self.state);
        let fx_rate = self.state.database.fx_rate(USD_CAD_PAIR).ok().flatten();
        let usd_cad = fx_rate.as_ref().map(|rate| rate.rate);
        let transactions = match self.state.database.load_transactions() {
            Ok(transactions) => transactions,
            Err(error) => {
                self.toast_overlay
                    .add_toast(Toast::new(&format!("Could not load transactions: {error}")));
                return;
            }
        };
        let has_positions = !positions.is_empty();
        let has_transactions = !transactions.is_empty();
        let has_cash = accounts.iter().any(|account| account.cash.abs() > 0.0000001);
        let has_portfolio = has_positions || has_transactions || has_cash;
        let has_watchlist = !watchlist.is_empty();
        let current_page = self.current_page.borrow().clone();
        self.overview_stack
            .set_visible_child_name(if has_portfolio { "portfolio" } else { "empty" });
        self.accounts_stack
            .set_visible_child_name(if accounts.is_empty() { "empty" } else { "accounts" });
        self.watchlist_stack
            .set_visible_child_name(if has_watchlist { "watchlist" } else { "empty" });

        // Only rebuild the visible page. Hidden pages are refreshed when they
        // become visible, which avoids repeatedly destroying and recreating
        // every row in the application after a single quote or cache update.
        match current_page.as_str() {
            "overview" => {
                rebuild_overview_list(&self.overview_list, &positions, &accounts, &base, usd_cad);
                rebuild_allocation(self, &positions, &accounts, &base, usd_cad);
                rebuild_upcoming_actions(self, &positions);
                update_portfolio_history_from_cache(self);
            }
            "accounts" => rebuild_accounts_list(
                &self.accounts_list,
                &accounts,
                &positions,
                &base,
                usd_cad,
            ),
            "watchlist" => rebuild_watchlist_list(self, &watchlist),
            "dividends" => rebuild_dividend_page(self, &positions, &base, usd_cad),
            _ => {}
        }

        let basis = if has_positions {
            sum_converted(
                positions
                    .iter()
                    .map(|position| (position.cost_basis(), position.currency.as_str())),
                &base,
                usd_cad,
            )
        } else {
            Some(0.0)
        };
        let market = if has_positions {
            sum_optional_converted(
                positions
                    .iter()
                    .map(|position| (position.market_value(), position.currency.as_str())),
                &base,
                usd_cad,
            )
        } else {
            Some(0.0)
        };

        let cash_total = sum_converted(
            accounts
                .iter()
                .map(|account| (account.cash, account.currency.as_str())),
            &base,
            usd_cad,
        );
        let portfolio_value = match (market, cash_total) {
            (Some(market), Some(cash)) => Some(market + cash),
            _ => None,
        };
        match portfolio_value {
            Some(value) => self.total_value.set_label(&format_currency(value, &base)),
            None => self.total_value.set_label("—"),
        }
        match basis {
            Some(value) => self.cost_basis.set_label(&format_currency(value, &base)),
            None => self.cost_basis.set_label("—"),
        }

        match (market, basis) {
            (Some(market), Some(basis)) => {
                let gain = market - basis;
                self.total_gain
                    .set_label(&format_signed_currency(gain, &base));
                set_gain_class(&self.total_gain, gain);
            }
            _ => {
                self.total_gain.set_label("—");
                set_gain_class(&self.total_gain, 0.0);
            }
        }

        let split_events = match self.state.database.all_split_events() {
            Ok(events) => events,
            Err(error) => {
                self.toast_overlay
                    .add_toast(Toast::new(&format!("Could not load stock splits: {error}")));
                return;
            }
        };
        let realized = if has_transactions {
            realized_gain_from_transactions(&transactions, &split_events, &base, usd_cad)
        } else {
            None
        };
        match realized {
            Some(value) => {
                self.realized_gain
                    .set_label(&format_signed_currency(value, &base));
                set_gain_class(&self.realized_gain, value);
            }
            None => {
                self.realized_gain.set_label("—");
                set_gain_class(&self.realized_gain, 0.0);
            }
        }

        let quote_note = if has_positions {
            market_status_text(&positions, &base, fx_rate.as_ref())
        } else if has_cash {
            "Cash balance".into()
        } else if has_transactions {
            "No current holdings".into()
        } else {
            "No holdings yet".into()
        };
        self.quote_note.set_label(&quote_note);
    }

    // Build hidden destinations from local/cache data before the user can switch
    // to them. Overview was already built by refresh(), so avoid rebuilding its
    // rows, allocation, upcoming actions, and chart twice during startup.
    fn prime_hidden_pages(&self) {
        let positions = self.state.database.load_positions().unwrap_or_default();
        let accounts = self.state.database.load_accounts().unwrap_or_default();
        let watchlist = self.state.database.load_watchlist().unwrap_or_default();
        let base = base_currency(&self.state);
        let usd_cad = self
            .state
            .database
            .fx_rate(USD_CAD_PAIR)
            .ok()
            .flatten()
            .map(|rate| rate.rate);

        rebuild_accounts_list(&self.accounts_list, &accounts, &positions, &base, usd_cad);
        rebuild_dividend_page(self, &positions, &base, usd_cad);
        rebuild_watchlist_list(self, &watchlist);
    }
}

fn main_header_content(
    title_text: &str,
    add_tooltip: Option<&str>,
) -> (adw::WindowTitle, GtkBox, GtkBox, MenuButton) {
    let start = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(6)
        .build();
    if let Some(tooltip) = add_tooltip {
        let add = Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text(tooltip)
            .build();
        add.set_action_name(Some("win.add-account"));
        start.append(&add);
    }

    let title = adw::WindowTitle::new(title_text, "");
    let end = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(6)
        .build();
    let menu = main_menu_button();
    menu.set_visible(false);
    end.append(&menu);

    (title, start, end, menu)
}

// The pull spacer is a ToolbarView top bar so it can move page content down.
// The visible glyph is a full-width Overlay child instead, because secondary
// ToolbarView bars may be allocated without the vertical-scrollbar gutter.
// Positioning the visual revealer directly below the HeaderBar gives both the
// title and refresh glyph the exact same horizontal allocation and centerline.
fn position_pull_refresh_visual(header: &HeaderBar, visual_revealer: &Revealer) {
    visual_revealer.set_margin_top(header.height().max(0));
}

pub fn build_window(app: &Application) -> Result<ApplicationWindow, String> {
    let database = Database::open_default().map_err(|error| error.to_string())?;
    // Re-derive positions on every launch so a previously cached split becomes
    // effective automatically as soon as its timestamp is reached. Treat a
    // failed synchronization as a startup error instead of showing stale data.
    database
        .sync_positions_from_activity()
        .map_err(|error| format!("Could not synchronize portfolio positions: {error}"))?;
    database
        .sync_paid_dividends_to_cash()
        .map_err(|error| format!("Could not synchronize dividend cash: {error}"))?;
    let state = AppState {
        database: Rc::new(database),
    };
    apply_appearance(&state);
    let navigation = NavigationView::new();
    navigation.set_animate_transitions(true);

    let toast_overlay = ToastOverlay::new();
    let total_value = Label::builder()
        .label("C$0.00")
        .halign(Align::Start)
        .css_classes(["title-1"])
        .build();
    let total_gain = metric_value_label();
    let realized_gain = metric_value_label();
    let investment_return = metric_value_label();
    let cost_basis = metric_value_label();
    let quote_note = Label::builder()
        .label("No holdings yet")
        .halign(Align::Start)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .build();

    let portfolio_history_chart = PriceChart::new_portfolio();
    let portfolio_history_range = Rc::new(Cell::new(HistoryRange::OneYear));
    let overview_list = positions_list();
    let allocation_ring = AllocationRing::new();
    let allocation_legend = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(8)
        .build();
    // Wrap based on the width of the Overview content itself, not the outer
    // window. This avoids the allocation card setting an oversized minimum
    // width when the sidebar is visible or the window is resized.
    let overview_holdings_layout = WrapBox::new();
    overview_holdings_layout.set_child_spacing(18);
    overview_holdings_layout.set_line_spacing(18);
    overview_holdings_layout.set_natural_line_length(760);
    overview_holdings_layout.set_line_homogeneous(false);
    overview_holdings_layout.set_wrap_policy(adw::WrapPolicy::Natural);
    let upcoming_box = GtkBox::builder().orientation(Orientation::Horizontal).spacing(10).build();
    let accounts_list = positions_list();
    let watchlist_list = positions_list();
    let dividend_list = positions_list();
    // A dedicated class provides a small fallback for the populated history
    // surface in case a runtime/theme does not paint Adwaita's boxed-list class.
    // The fallback only applies while boxed-list is present, so the empty state
    // remains directly on the page background.
    dividend_list.add_css_class("dividend-history-list");
    // Keep the empty state on the bare page surface. Once real distributions
    // exist, rebuild_dividend_page restores libadwaita's native boxed-list
    // treatment so the history reads like a standard GNOME list group.
    dividend_list.remove_css_class("boxed-list");
    dividend_list.set_show_separators(false);
    let dividend_recent_heading = section_heading("Recent Distributions");
    // The empty placeholder already says "No Recent Distributions". Hide the
    // section heading until real rows exist so the empty state does not repeat
    // the same idea twice.
    dividend_recent_heading.set_visible(false);
    let dividend_income = Label::builder()
        .label("—")
        .halign(Align::Center)
        // Match Overview's native libadwaita title metric exactly. Avoid a
        // custom point-size override so currency glyphs render consistently.
        .css_classes(["title-1"])
        .build();
    let dividend_yield = Label::builder()
        .label("—")
        .halign(Align::Center)
        .css_classes(["heading", "dim-label"])
        .build();
    let dividend_status = Label::builder()
        .label("Loading dividend history")
        .halign(Align::Center)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .build();
    let dividend_chart = DividendChart::new();
    let dividend_period_model = StringList::new(&[]);
    dividend_period_model.append("Annual");
    let dividend_period = DropDown::builder()
        .model(&dividend_period_model)
        .selected(0)
        .build();
    dividend_period.set_halign(Align::Center);
    dividend_period.set_size_request(138, -1);
    dividend_period.set_tooltip_text(Some("Dividend chart period"));
    // Annual is the current calendar year. Historical years are added only when
    // they actually exist, so a brand-new portfolio does not show a redundant
    // current-year option or an unnecessary period selector.
    dividend_period.set_visible(false);
    let dividend_period_options = Rc::new(RefCell::new(vec![DividendPeriod::Annual]));
    let dividend_period_updating = Rc::new(Cell::new(false));
    let current_page = Rc::new(RefCell::new("overview".to_string()));
    let market_refresh_generation = Rc::new(Cell::new(0));
    let dividend_refresh_generation = Rc::new(Cell::new(0));
    let watchlist_refresh_generation = Rc::new(Cell::new(0));
    let portfolio_history_generation = Rc::new(Cell::new(0));

    let pull_refresh_spinner = Spinner::new();
    pull_refresh_spinner.set_visible(false);
    pull_refresh_spinner.set_size_request(18, 18);
    let pull_refresh_icon = Image::from_icon_name("view-refresh-symbolic");
    pull_refresh_icon.set_pixel_size(18);
    let pull_refresh_active = Rc::new(Cell::new(false));
    let shortcut_refresh_bar = ProgressBar::new();
    shortcut_refresh_bar.set_visible(false);
    shortcut_refresh_bar.set_show_text(false);
    shortcut_refresh_bar.set_hexpand(true);
    shortcut_refresh_bar.set_halign(Align::Fill);
    shortcut_refresh_bar.set_valign(Align::End);
    shortcut_refresh_bar.add_css_class("shortcut-refresh");
    shortcut_refresh_bar.set_can_target(false);
    shortcut_refresh_bar.set_size_request(-1, 2);
    let shortcut_refresh_active = Rc::new(Cell::new(false));
    let shortcut_refresh_generation = Rc::new(Cell::new(0_u64));
    // Keep the visible glyph centered *inside* its 38 px indicator surface.
    // A horizontal GtkBox places its only visible child at the start edge; that
    // made an 18 px icon inside a 38 px box appear exactly 10 px left of the
    // true page center even though the box itself was centered correctly.
    let pull_refresh_indicator = Overlay::new();
    pull_refresh_indicator.set_halign(Align::Center);
    pull_refresh_indicator.set_valign(Align::Center);
    pull_refresh_indicator.set_size_request(38, 38);
    pull_refresh_indicator.set_can_target(false);
    pull_refresh_icon.set_halign(Align::Center);
    pull_refresh_icon.set_valign(Align::Center);
    pull_refresh_spinner.set_halign(Align::Center);
    pull_refresh_spinner.set_valign(Align::Center);
    pull_refresh_indicator.set_child(Some(&pull_refresh_icon));
    pull_refresh_indicator.add_overlay(&pull_refresh_spinner);
    pull_refresh_indicator.set_margin_top(6);
    pull_refresh_indicator.set_margin_bottom(6);
    let page_scroll_adjustments: Rc<RefCell<HashMap<String, gtk::Adjustment>>> =
        Rc::new(RefCell::new(HashMap::new()));
    let pull_refresh_spacer = GtkBox::builder()
        .height_request(50)
        .hexpand(true)
        .build();
    let pull_refresh_revealer = Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(140)
        .reveal_child(false)
        .hexpand(true)
        .child(&pull_refresh_spacer)
        .build();
    let pull_refresh_visual_revealer = Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(140)
        .reveal_child(false)
        .halign(Align::Fill)
        .valign(Align::Start)
        .hexpand(true)
        .child(&pull_refresh_indicator)
        .build();
    pull_refresh_visual_revealer.set_can_target(false);

    let (overview_title, overview_header_start, overview_header_end, overview_mobile_menu) =
        main_header_content("Overview", None);
    let (accounts_title, accounts_header_start, accounts_header_end, accounts_mobile_menu) =
        main_header_content("Accounts", Some("Add Account"));
    let (dividends_title, dividends_header_start, dividends_header_end, dividends_mobile_menu) =
        main_header_content("Dividends", None);
    let (search_title, search_header_start, search_header_end, search_mobile_menu) =
        main_header_content("Search", None);
    let (watchlist_title, watchlist_header_start, watchlist_header_end, watchlist_mobile_menu) =
        main_header_content("Watchlist", None);
    let mobile_menu_buttons = vec![
        overview_mobile_menu,
        dividends_mobile_menu,
        search_mobile_menu,
        watchlist_mobile_menu,
        accounts_mobile_menu,
    ];

    // Keep the title in the HeaderBar's real title slot and page actions in its
    // native start/end slots. Header content crossfades independently while the
    // page itself uses directional motion; native window controls stay fixed.
    let header_title_pages = Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(160)
        .vhomogeneous(true)
        .hhomogeneous(true)
        .build();
    header_title_pages.add_named(&overview_title, Some("overview"));
    header_title_pages.add_named(&accounts_title, Some("accounts"));
    header_title_pages.add_named(&dividends_title, Some("dividends"));
    header_title_pages.add_named(&search_title, Some("search"));
    header_title_pages.add_named(&watchlist_title, Some("watchlist"));
    header_title_pages.set_visible_child_name("overview");

    let header_start_pages = Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(160)
        .vhomogeneous(true)
        .hhomogeneous(true)
        .build();
    header_start_pages.add_named(&overview_header_start, Some("overview"));
    header_start_pages.add_named(&accounts_header_start, Some("accounts"));
    header_start_pages.add_named(&dividends_header_start, Some("dividends"));
    header_start_pages.add_named(&search_header_start, Some("search"));
    header_start_pages.add_named(&watchlist_header_start, Some("watchlist"));
    header_start_pages.set_visible_child_name("overview");

    let header_end_pages = Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(160)
        .vhomogeneous(true)
        .hhomogeneous(true)
        .build();
    header_end_pages.add_named(&overview_header_end, Some("overview"));
    header_end_pages.add_named(&accounts_header_end, Some("accounts"));
    header_end_pages.add_named(&dividends_header_end, Some("dividends"));
    header_end_pages.add_named(&search_header_end, Some("search"));
    header_end_pages.add_named(&watchlist_header_end, Some("watchlist"));
    header_end_pages.set_visible_child_name("overview");

    let overview_stack = page_stack();
    let accounts_stack = page_stack();
    let dividends_stack = page_stack();
    let watchlist_stack = page_stack();
    let search_entry = SearchEntry::builder()
        .placeholder_text("Search ticker or company name")
        .hexpand(true)
        .build();
    search_entry.set_search_delay(300);
    let search_top_slot = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .margin_top(18)
        .margin_start(14)
        .margin_end(14)
        .build();
    let search_bottom_slot = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .visible(false)
        .margin_top(8)
        .margin_bottom(10)
        .margin_start(14)
        .margin_end(14)
        .build();

    let detail_refresh: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let mobile_layout = Rc::new(Cell::new(false));

    let refs = UiRefs {
        state: state.clone(),
        navigation: navigation.clone(),
        toast_overlay: ToastManager::new(&toast_overlay),
        total_value: total_value.clone(),
        total_gain: total_gain.clone(),
        realized_gain: realized_gain.clone(),
        investment_return: investment_return.clone(),
        cost_basis: cost_basis.clone(),
        quote_note: quote_note.clone(),
        portfolio_history_chart: portfolio_history_chart.clone(),
        portfolio_history_range: portfolio_history_range.clone(),
        overview_list: overview_list.clone(),
        allocation_ring: allocation_ring.clone(),
        allocation_legend: allocation_legend.clone(),
        overview_holdings_layout: overview_holdings_layout.clone(),
        upcoming_box: upcoming_box.clone(),
        accounts_list: accounts_list.clone(),
        watchlist_list: watchlist_list.clone(),
        dividend_list: dividend_list.clone(),
        dividend_recent_heading: dividend_recent_heading.clone(),
        dividend_income: dividend_income.clone(),
        dividend_yield: dividend_yield.clone(),
        dividend_status: dividend_status.clone(),
        dividend_chart: dividend_chart.clone(),
        dividend_period: dividend_period.clone(),
        dividend_period_options: dividend_period_options.clone(),
        dividend_period_updating: dividend_period_updating.clone(),
        overview_stack: overview_stack.clone(),
        accounts_stack: accounts_stack.clone(),
        dividends_stack: dividends_stack.clone(),
        watchlist_stack: watchlist_stack.clone(),
        search_entry: search_entry.clone(),
        search_top_slot: search_top_slot.clone(),
        search_bottom_slot: search_bottom_slot.clone(),
        current_page: current_page.clone(),
        market_refresh_generation: market_refresh_generation.clone(),
        dividend_refresh_generation: dividend_refresh_generation.clone(),
        watchlist_refresh_generation: watchlist_refresh_generation.clone(),
        portfolio_history_generation: portfolio_history_generation.clone(),
        pull_refresh_revealer: pull_refresh_revealer.clone(),
        pull_refresh_visual_revealer: pull_refresh_visual_revealer.clone(),
        pull_refresh_spinner: pull_refresh_spinner.clone(),
        pull_refresh_icon: pull_refresh_icon.clone(),
        pull_refresh_active: pull_refresh_active.clone(),
        shortcut_refresh_bar: shortcut_refresh_bar.clone(),
        shortcut_refresh_active: shortcut_refresh_active.clone(),
        shortcut_refresh_generation: shortcut_refresh_generation.clone(),
        detail_refresh: detail_refresh.clone(),
        page_scroll_adjustments: page_scroll_adjustments.clone(),
    };


    // The libadwaita ViewStack remains the navigation model for the sidebar and
    // bottom switcher. The visible content lives in a GTK Stack so we can use
    // directional transitions rather than ViewStack's crossfade.
    let pages = ViewStack::builder()
        .enable_transitions(false)
        .transition_duration(0)
        .vhomogeneous(false)
        .hhomogeneous(false)
        .build();
    let content_pages = Stack::builder()
        .transition_type(gtk::StackTransitionType::None)
        .transition_duration(190)
        .vhomogeneous(false)
        .hhomogeneous(false)
        .build();
    let visible_page_index = Rc::new(Cell::new(0_i32));
    let navigation_refresh_generation = Rc::new(Cell::new(0_u64));

    let overview_page = build_overview_page(&refs);
    let accounts_page = build_accounts_page(&refs);
    let dividends_page = build_dividends_page(&refs);
    let search_page = build_search_page(&refs);
    let watchlist_page = build_watchlist_page(&refs);

    {
        let refs = refs.clone();
        let dividend_period = refs.dividend_period.clone();
        dividend_period.connect_selected_notify(move |_| {
            if refs.dividend_period_updating.get() {
                return;
            }
            let positions = refs.state.database.load_positions().unwrap_or_default();
            let base = base_currency(&refs.state);
            let usd_cad = refs
                .state
                .database
                .fx_rate(USD_CAD_PAIR)
                .ok()
                .flatten()
                .map(|rate| rate.rate);

            // Period changes are a visual data transition, not navigation. Fade the
            // complete dividend result set out, rebuild it for the newly selected
            // year, then fade it back in so Annual <-> historical switches match
            // Aureus's other loaded-value transitions instead of snapping.
            let widgets = vec![
                refs.dividend_income.clone().upcast::<gtk::Widget>(),
                refs.dividend_yield.clone().upcast::<gtk::Widget>(),
                refs.dividend_chart.widget().clone().upcast::<gtk::Widget>(),
                refs.dividend_status.clone().upcast::<gtk::Widget>(),
                refs.dividend_recent_heading.clone().upcast::<gtk::Widget>(),
                refs.dividend_list.clone().upcast::<gtk::Widget>(),
            ];
            let refs_for_update = refs.clone();
            crossfade_loaded_widgets(widgets, move || {
                rebuild_dividend_page(&refs_for_update, &positions, &base, usd_cad);
            });
        });
    }

    content_pages.add_named(&overview_page, Some("overview"));
    content_pages.add_named(&dividends_page, Some("dividends"));
    content_pages.add_named(&search_page, Some("search"));
    content_pages.add_named(&watchlist_page, Some("watchlist"));
    content_pages.add_named(&accounts_page, Some("accounts"));
    content_pages.set_visible_child_name("overview");

    // ViewSwitcher only needs a ViewStack model. These lightweight placeholders
    // are never shown directly; the matching real page is displayed above.
    pages.add_titled_with_icon(
        &GtkBox::new(Orientation::Vertical, 0),
        Some("overview"),
        "Overview",
        "view-grid-symbolic",
    );
    pages.add_titled_with_icon(
        &GtkBox::new(Orientation::Vertical, 0),
        Some("dividends"),
        "Dividends",
        "weather-showers-scattered-symbolic",
    );
    pages.add_titled_with_icon(
        &GtkBox::new(Orientation::Vertical, 0),
        Some("search"),
        "Search",
        "system-search-symbolic",
    );
    pages.add_titled_with_icon(
        &GtkBox::new(Orientation::Vertical, 0),
        Some("watchlist"),
        "Watchlist",
        "starred-symbolic",
    );
    pages.add_titled_with_icon(
        &GtkBox::new(Orientation::Vertical, 0),
        Some("accounts"),
        "Accounts",
        accounts_icon_name(),
    );

    let sidebar = ViewSwitcherSidebar::builder()
        .stack(&pages)
        .mode(SidebarMode::Sidebar)
        .build();

    let menu_button = main_menu_button();
    let sidebar_header = HeaderBar::new();
    sidebar_header.set_show_end_title_buttons(false);
    sidebar_header.set_title_widget(Some(&adw::WindowTitle::new("Aureus", "")));
    sidebar_header.pack_end(&menu_button);

    let sidebar_toolbar = ToolbarView::new();
    sidebar_toolbar.add_top_bar(&sidebar_header);
    sidebar_toolbar.set_content(Some(&sidebar));
    let sidebar_page = NavigationPage::builder()
        .title("Aureus")
        .child(&sidebar_toolbar)
        .build();

    let content_header = HeaderBar::new();
    // Let libadwaita own title geometry. Strict centering keeps the title on the
    // exact content-pane centerline regardless of window controls or page
    // actions, and using the real title slot gives the crossfade stack the full
    // HeaderBar height so its text cannot be clipped during transitions.
    content_header.set_centering_policy(adw::CenteringPolicy::Strict);
    content_header.pack_start(&header_start_pages);
    header_title_pages.set_halign(Align::Center);
    // Fill the HeaderBar vertically so GtkStack's transition snapshot always
    // has enough height for the complete glyphs, while Strict centering owns
    // the horizontal position.
    header_title_pages.set_valign(Align::Fill);
    header_title_pages.set_can_target(false);
    content_header.set_title_widget(Some(&header_title_pages));
    content_header.pack_end(&header_end_pages);

    let mobile_switcher = ViewSwitcherBar::builder()
        .stack(&pages)
        .reveal(false)
        .build();
    mobile_switcher.add_css_class("mobile-bottom-nav");

    let content_header_overlay = Overlay::new();
    content_header_overlay.set_child(Some(&content_header));
    content_header_overlay.add_overlay(&shortcut_refresh_bar);

    let content_toolbar = ToolbarView::new();
    content_toolbar.set_bottom_bar_style(adw::ToolbarStyle::Flat);
    content_toolbar.add_top_bar(&content_header_overlay);
    // This top-bar revealer is only a spacer: it keeps the natural
    // pull-down motion. The visible refresh glyph lives in the full-width
    // Overlay below so it shares the HeaderBar centerline exactly.
    content_toolbar.add_top_bar(&pull_refresh_revealer);
    content_toolbar.add_bottom_bar(&mobile_switcher);
    content_pages.set_vexpand(true);
    content_toolbar.set_content(Some(&content_pages));
    let content_overlay = Overlay::new();
    content_overlay.set_child(Some(&content_toolbar));
    content_overlay.add_overlay(&pull_refresh_visual_revealer);
    let content_page = NavigationPage::builder()
        .title("Aureus")
        .tag("portfolio-root")
        .can_pop(false)
        .child(&content_overlay)
        .build();
    navigation.add(&content_page);
    let content_navigation_page = NavigationPage::builder()
        .title("Aureus")
        .child(&navigation)
        .build();

    let split_view = NavigationSplitView::builder()
        .sidebar(&sidebar_page)
        .content(&content_navigation_page)
        .min_sidebar_width(220.0)
        .max_sidebar_width(300.0)
        .build();

    // Setup and the real application shell are pre-built in one Stack. The
    // first-run transition is a lightweight crossfade, while all normal app
    // navigation continues to use the directional transitions below.
    let portfolio_shell_page = NavigationPage::builder()
        .title("Aureus")
        .tag("app-shell")
        .can_pop(false)
        .child(&split_view)
        .build();
    let root_stack = Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(220)
        .hhomogeneous(true)
        .vhomogeneous(true)
        .build();
    root_stack.add_named(&portfolio_shell_page, Some("app"));

    let first_launch = state
        .database
        .load_accounts()
        .map_err(|error| error.to_string())?
        .is_empty();

    if first_launch {
        let setup_page = build_setup_page(refs.clone(), root_stack.clone());
        root_stack.add_named(&setup_page, Some("setup"));
        root_stack.set_visible_child_name("setup");
    } else {
        root_stack.set_visible_child_name("app");
    }

    toast_overlay.set_child(Some(&root_stack));

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Aureus")
        .default_width(1060)
        .default_height(720)
        .content(&toast_overlay)
        .build();

    install_window_actions(&window, &state, &refs, &pages);
    app.set_accels_for_action("win.refresh-current", &["<Primary>r"]);
    app.set_accels_for_action("win.search", &["<Primary>f"]);
    app.set_accels_for_action("win.close", &["<Primary>w"]);

    let narrow = Breakpoint::new(
        BreakpointCondition::parse("max-width: 700sp")
            .expect("valid narrow-window breakpoint condition"),
    );
    {
        let split_view = split_view.clone();
        let sidebar = sidebar.clone();
        let mobile_switcher = mobile_switcher.clone();
        let mobile_menu_buttons = mobile_menu_buttons.clone();
        let mobile_layout = mobile_layout.clone();
        let refs_for_mobile_search = refs.clone();
        narrow.connect_apply(move |_| {
            mobile_layout.set(true);
            split_view.set_collapsed(true);
            split_view.set_show_content(true);
            sidebar.set_mode(SidebarMode::Page);
            mobile_switcher.set_reveal(true);
            let search_entry = refs_for_mobile_search.search_entry.clone();
            let had_focus = search_entry.has_focus();
            if let Some(parent) = search_entry.parent() {
                if let Ok(parent) = parent.downcast::<GtkBox>() {
                    parent.remove(&search_entry);
                }
            }
            refs_for_mobile_search.search_top_slot.set_visible(false);
            refs_for_mobile_search.search_bottom_slot.set_visible(true);
            refs_for_mobile_search.search_bottom_slot.append(&search_entry);
            if had_focus {
                search_entry.grab_focus();
            }
            for button in &mobile_menu_buttons {
                button.set_visible(true);
            }
        });
    }
    {
        let split_view = split_view.clone();
        let sidebar = sidebar.clone();
        let mobile_switcher = mobile_switcher.clone();
        let mobile_menu_buttons = mobile_menu_buttons.clone();
        let mobile_layout = mobile_layout.clone();
        let refs_for_desktop_search = refs.clone();
        narrow.connect_unapply(move |_| {
            mobile_layout.set(false);
            mobile_switcher.set_reveal(false);
            for button in &mobile_menu_buttons {
                button.set_visible(false);
            }
            split_view.set_collapsed(false);
            split_view.set_show_content(true);
            sidebar.set_mode(SidebarMode::Sidebar);
            let search_entry = refs_for_desktop_search.search_entry.clone();
            let had_focus = search_entry.has_focus();
            if let Some(parent) = search_entry.parent() {
                if let Ok(parent) = parent.downcast::<GtkBox>() {
                    parent.remove(&search_entry);
                }
            }
            refs_for_desktop_search.search_bottom_slot.set_visible(false);
            refs_for_desktop_search.search_top_slot.set_visible(true);
            refs_for_desktop_search.search_top_slot.append(&search_entry);
            if had_focus {
                search_entry.grab_focus();
            }
        });
    }
    window.add_breakpoint(narrow);

    // Pull-to-refresh is available at every window size. The gesture can begin
    // anywhere over the current page, but it only arms while that page's main
    // vertical scroller is already at the top. Feedback is intentionally
    // icon-only: the refresh glyph appears while pulling, then becomes a spinner
    // until the page's real refresh callback completes.
    {
        let gesture = GestureDrag::new();
        // Observe the drag before the scroller does. Once a downward pull is
        // recognized we claim that sequence so GTK cannot turn the same gesture
        // into kinetic scrolling after the refresh UI collapses.
        gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
        let can_pull = Rc::new(Cell::new(false));
        let pulling = Rc::new(Cell::new(false));
        let armed = Rc::new(Cell::new(false));
        {
            let refs = refs.clone();
            let can_pull = can_pull.clone();
            let pulling = pulling.clone();
            let armed = armed.clone();
            let pull_refresh_revealer = pull_refresh_revealer.clone();
            let pull_refresh_spinner = pull_refresh_spinner.clone();
            let pull_refresh_icon = pull_refresh_icon.clone();
            let pull_refresh_visual_revealer = pull_refresh_visual_revealer.clone();
            let content_header = content_header.clone();
            gesture.connect_drag_begin(move |_, _, _| {
                position_pull_refresh_visual(&content_header, &pull_refresh_visual_revealer);
                if refs.pull_refresh_active.get() || refs.shortcut_refresh_active.get() {
                    can_pull.set(false);
                    pulling.set(false);
                    armed.set(false);
                    return;
                }
                let page = refs.current_page.borrow().clone();
                let at_top = refs
                    .page_scroll_adjustments
                    .borrow()
                    .get(&page)
                    .map(|adjustment| adjustment.value() <= adjustment.lower() + 0.5)
                    .unwrap_or(true);
                can_pull.set(at_top);
                pulling.set(false);
                armed.set(false);
                pull_refresh_spinner.stop();
                pull_refresh_spinner.set_visible(false);
                pull_refresh_icon.set_visible(true);
                pull_refresh_icon.set_opacity(0.28);
                pull_refresh_revealer.set_reveal_child(false);
                pull_refresh_visual_revealer.set_reveal_child(false);
            });
        }
        {
            let can_pull = can_pull.clone();
            let pulling = pulling.clone();
            let armed = armed.clone();
            let refs = refs.clone();
            let pull_refresh_revealer = pull_refresh_revealer.clone();
            let pull_refresh_icon = pull_refresh_icon.clone();
            let pull_refresh_visual_revealer = pull_refresh_visual_revealer.clone();
            let content_header = content_header.clone();
            gesture.connect_drag_update(move |gesture, offset_x, offset_y| {
                position_pull_refresh_visual(&content_header, &pull_refresh_visual_revealer);
                if !can_pull.get() {
                    return;
                }

                // Decide once whether this sequence is a vertical pull. After it
                // is claimed, keep it claimed until release instead of repeatedly
                // entering/leaving pull mode as the pointer jitters near the
                // activation boundary.
                if !pulling.get() {
                    if offset_y <= 8.0 || offset_y <= offset_x.abs() {
                        armed.set(false);
                        pull_refresh_icon.set_opacity(0.28);
                        pull_refresh_visual_revealer.set_reveal_child(false);
                        return;
                    }
                    let _ = gesture.set_state(gtk::EventSequenceState::Claimed);
                    pulling.set(true);
                    // Open the pull spacer once, while the drag is active, so the
                    // page keeps the original pull-down motion. Because `pulling`
                    // is latched for the rest of this gesture, layout movement can
                    // no longer make us immediately hide/reveal it again.
                    pull_refresh_revealer.set_reveal_child(true);
                    pull_refresh_visual_revealer.set_reveal_child(true);
                }

                let page = refs.current_page.borrow().clone();
                if let Some(adjustment) = refs.page_scroll_adjustments.borrow().get(&page) {
                    reset_adjustment_to_top(adjustment);
                }

                let progress = (offset_y / 84.0).clamp(0.0, 1.0);
                pull_refresh_icon.set_opacity(0.28 + progress * 0.72);
                armed.set(offset_y >= 84.0);
            });
        }
        {
            let can_pull = can_pull.clone();
            let pulling = pulling.clone();
            let armed = armed.clone();
            let window = window.clone();
            let refs = refs.clone();
            let pull_refresh_revealer = pull_refresh_revealer.clone();
            let pull_refresh_spinner = pull_refresh_spinner.clone();
            let pull_refresh_icon = pull_refresh_icon.clone();
            let pull_refresh_visual_revealer = pull_refresh_visual_revealer.clone();
            gesture.connect_drag_end(move |_, _, _| {
                pulling.set(false);
                if !can_pull.replace(false) {
                    armed.set(false);
                    return;
                }
                if armed.replace(false) {
                    refs.pull_refresh_active.set(true);
                    restore_current_page_top(&refs);
                    pull_refresh_icon.set_visible(false);
                    pull_refresh_spinner.set_visible(true);
                    pull_refresh_spinner.start();
                    // The spacer is already open from the pull gesture; keep it
                    // open while the refresh spinner is active.
                    pull_refresh_revealer.set_reveal_child(true);
                    pull_refresh_visual_revealer.set_reveal_child(true);
                    let _ = gtk::prelude::WidgetExt::activate_action(
                        &window,
                        "win.refresh-current",
                        None,
                    );
                } else {
                    pull_refresh_icon.set_opacity(1.0);
                    pull_refresh_revealer.set_reveal_child(false);
                    pull_refresh_visual_revealer.set_reveal_child(false);
                    restore_current_page_top(&refs);
                }
            });
        }
        content_pages.add_controller(gesture);
    }

    {
        let split_view = split_view.clone();
        let navigation = navigation.clone();
        sidebar.connect_activated(move |_| {
            let _ = navigation.pop_to_tag("portfolio-root");
            if split_view.is_collapsed() {
                split_view.set_show_content(true);
            }
        });
    }

    {
        let refs = refs.clone();
        let window_weak = window.downgrade();
        overview_list.connect_row_activated(move |_, row| {
            let key = row.widget_name();
            if let Some(id) = key.strip_prefix("position-").and_then(|value| value.parse::<i64>().ok()) {
                present_position_detail(id, refs.clone());
                return;
            }
            let Some(account_id) = key
                .strip_prefix("cash-")
                .and_then(|value| value.parse::<i64>().ok())
            else {
                return;
            };
            let Some(parent) = window_weak.upgrade() else {
                return;
            };
            let account = refs
                .state
                .database
                .load_accounts()
                .ok()
                .and_then(|accounts| accounts.into_iter().find(|account| account.id == account_id));
            if let Some(account) = account {
                present_manage_cash_dialog(&parent, refs.clone(), account);
            }
        });
    }

    {
        let refs = refs.clone();
        accounts_list.connect_row_activated(move |_, row| {
            let key = row.widget_name();
            let Some(account_id) = key
                .strip_prefix("account-")
                .and_then(|value| value.parse::<i64>().ok())
            else {
                return;
            };
            present_account_detail(account_id, refs.clone());
        });
    }

    {
        let refs = refs.clone();
        watchlist_list.connect_row_activated(move |_, row| {
            let Ok(items) = refs.state.database.load_watchlist() else {
                return;
            };
            let Some(item) = items.get(row.index().max(0) as usize) else {
                return;
            };
            present_watchlist_detail(item.id, refs.clone());
        });
    }

    {
        let refs = refs.clone();
        let content_pages = content_pages.clone();
        let header_title_pages = header_title_pages.clone();
        let header_start_pages = header_start_pages.clone();
        let header_end_pages = header_end_pages.clone();
        let mobile_layout = mobile_layout.clone();
        let visible_page_index = visible_page_index.clone();
        let navigation_refresh_generation = navigation_refresh_generation.clone();
        pages.connect_visible_child_name_notify(move |stack| {
            let page_name = stack
                .visible_child_name()
                .map(|name| name.to_string())
                .unwrap_or_else(|| "overview".to_string());
            let new_index = match page_name.as_str() {
                "dividends" => 1,
                "search" => 2,
                "watchlist" => 3,
                "accounts" => 4,
                _ => 0,
            };
            let old_index = visible_page_index.get();
            if new_index != old_index {
                let transition = if mobile_layout.get() {
                    if new_index > old_index {
                        gtk::StackTransitionType::SlideLeft
                    } else {
                        gtk::StackTransitionType::SlideRight
                    }
                } else if new_index > old_index {
                    gtk::StackTransitionType::SlideUp
                } else {
                    gtk::StackTransitionType::SlideDown
                };
                content_pages.set_transition_type(transition);
                header_title_pages.set_visible_child_name(&page_name);
                header_start_pages.set_visible_child_name(&page_name);
                header_end_pages.set_visible_child_name(&page_name);
                content_pages.set_visible_child_name(&page_name);
                visible_page_index.set(new_index);
            }
            *refs.current_page.borrow_mut() = page_name;

            // Let the 190 ms page slide and 160 ms header crossfade finish before
            // rebuilding the destination's dynamic rows. Only the newest pending
            // navigation refresh is allowed to run if the user switches quickly.
            let generation = navigation_refresh_generation.get().wrapping_add(1);
            navigation_refresh_generation.set(generation);
            let refs = refs.clone();
            let navigation_refresh_generation = navigation_refresh_generation.clone();
            glib::timeout_add_local_once(Duration::from_millis(210), move || {
                if navigation_refresh_generation.get() == generation {
                    refs.refresh();
                }
            });
        });
    }

    {
        let refs = refs.clone();
        let dividends_checked = Rc::new(Cell::new(false));
        let watchlist_checked = Rc::new(Cell::new(false));
        pages.connect_visible_child_name_notify(move |stack| {
            match stack.visible_child_name().as_deref() {
                Some("dividends") if !dividends_checked.replace(true) => {
                    let positions = refs.state.database.load_positions().unwrap_or_default();
                    let stale = refs
                        .state
                        .database
                        .dividends_needing_refresh(&positions, DIVIDEND_CACHE_SECONDS)
                        .unwrap_or_default();
                    if !stale.is_empty() {
                        refresh_dividends_async(refs.clone(), stale, false);
                    }
                }
                Some("watchlist") if !watchlist_checked.replace(true) => {
                    let stale = refs
                        .state
                        .database
                        .watchlist_needing_refresh(QUOTE_CACHE_SECONDS)
                        .unwrap_or_default();
                    if !stale.is_empty() {
                        refresh_watchlist_async(refs.clone(), stale, false);
                    }
                }
                _ => {}
            }
        });
    }

    refs.refresh();
    // Prime hidden destinations before the window is presented. This keeps the
    // very first sidebar/bottom-tab transition as complete as later switches.
    refs.prime_hidden_pages();
    if !state.database.load_transactions().unwrap_or_default().is_empty()
        || !state.database.load_cash_entries().unwrap_or_default().is_empty()
    {
        refresh_portfolio_history_async(refs.clone(), false);
    }

    let positions = state.database.load_positions().unwrap_or_default();
    let stale_quotes = state
        .database
        .positions_needing_refresh(QUOTE_CACHE_SECONDS)
        .unwrap_or_default();
    let fetch_fx = portfolio_needs_fx_with_cash(&state, &positions, &base_currency(&state))
        && state
            .database
            .fx_rate_needs_refresh(USD_CAD_PAIR, FX_CACHE_SECONDS)
            .unwrap_or(true);
    if !stale_quotes.is_empty() || fetch_fx {
        refresh_market_async(refs.clone(), stale_quotes, fetch_fx, false);
    }
    let stale_actions = state
        .database
        .dividends_needing_refresh(&positions, DIVIDEND_CACHE_SECONDS)
        .unwrap_or_default();
    if !stale_actions.is_empty() {
        refresh_dividends_async(refs.clone(), stale_actions, false);
    }

    // If Aureus remains open across a split's effective time, apply cached
    // corporate actions without waiting for a relaunch or manual refresh.
    {
        let refs = refs.clone();
        glib::timeout_add_local(Duration::from_secs(15 * 60), move || {
            if let Err(error) = refs.state.database.sync_positions_from_activity() {
                refs.toast_overlay.add_toast(Toast::new(&format!(
                    "Could not synchronize portfolio positions: {error}"
                )));
                return glib::ControlFlow::Continue;
            }
            if let Err(error) = refs.state.database.sync_paid_dividends_to_cash() {
                refs.toast_overlay.add_toast(Toast::new(&format!(
                    "Could not synchronize dividend cash: {error}"
                )));
                return glib::ControlFlow::Continue;
            }
            refs.refresh();
            glib::ControlFlow::Continue
        });
    }

    Ok(window)
}

fn build_setup_page(
    refs: UiRefs,
    root_stack: Stack,
) -> NavigationPage {
    let name = EntryRow::new();
    name.set_title("Account name");

    let currency = ComboRow::new();
    currency.set_title("Currency");
    let currency_model = string_model(&["CAD", "USD"]);
    currency.set_model(Some(&currency_model));
    currency.set_selected(u32::MAX);

    let account_group = PreferencesGroup::builder()
        .title("First Account")
        .build();
    account_group.add(&name);
    account_group.add(&currency);

    let create = Button::builder()
        .label("Create Account")
        .css_classes(["suggested-action", "pill"])
        .halign(Align::Fill)
        .hexpand(true)
        .sensitive(false)
        .build();
    let restore = Button::builder()
        .label("Restore Backup")
        .css_classes(["flat"])
        .halign(Align::Center)
        .tooltip_text("Restore a previously exported Aureus backup")
        .build();
    let setup_actions = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(4)
        .halign(Align::Center)
        .build();
    setup_actions.append(&create);
    setup_actions.append(&restore);

    let form = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(20)
        .build();
    form.append(&account_group);
    form.append(&setup_actions);

    let clamp = adw::Clamp::builder()
        .maximum_size(520)
        .tightening_threshold(360)
        .child(&form)
        .build();

    let status = StatusPage::builder()
        .icon_name("aureus-trend-symbolic")
        .title("Welcome to Aureus")
        .description("Create an account to start tracking your portfolio")
        .child(&clamp)
        .build();

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .child(&status)
        .build();

    let header = HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Setup", "")));
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scroller));

    let page = NavigationPage::builder()
        .title("Setup")
        .tag("setup")
        .can_pop(false)
        .child(&toolbar)
        .build();

    {
        let create_for_callback = create.clone();
        let name_for_callback = name.clone();
        let currency_for_callback = currency.clone();
        name.connect_changed(move |_| {
            sync_setup_create_button(
                &create_for_callback,
                &name_for_callback,
                &currency_for_callback,
            );
        });
    }
    {
        let create_for_callback = create.clone();
        let name_for_callback = name.clone();
        let currency_for_callback = currency.clone();
        currency.connect_selected_notify(move |_| {
            sync_setup_create_button(
                &create_for_callback,
                &name_for_callback,
                &currency_for_callback,
            );
        });
    }
    {
        let refs = refs.clone();
        let root_stack = root_stack.clone();
        let name = name.clone();
        let currency = currency.clone();
        let create_for_callback = create.clone();
        create.connect_clicked(move |_| {
            let account_name = name.text().trim().to_string();
            if account_name.is_empty() || currency.selected() > 1 {
                return;
            }

            let account = NewAccount {
                name: account_name.clone(),
                currency: currency_at(currency.selected()).into(),
            };

            match refs.state.database.add_account(&account) {
                Ok(account_id) => {
                    let _ = refs
                        .state
                        .database
                        .set_setting(LAST_ACCOUNT_ID_KEY, &account_id.to_string());
                    let _ = refs
                        .state
                        .database
                        .set_setting(BASE_CURRENCY_KEY, &account.currency);
                    create_for_callback.set_sensitive(false);

                    // Populate every destination from local data before starting the
                    // setup -> app crossfade. This work happens while Setup is still the
                    // visible page, so the first tab switch cannot expose an unbuilt page.
                    refs.refresh();
                    refs.prime_hidden_pages();
                    root_stack.set_visible_child_name("app");
                }
                Err(error) => refs
                    .toast_overlay
                    .add_toast(Toast::new(&format!("Could not create account: {error}"))),
            }
        });
    }
    {
        let refs = refs.clone();
        let root_stack = root_stack.clone();
        restore.connect_clicked(move |button| {
            let Some(root) = button.root() else {
                return;
            };
            let Ok(window) = root.downcast::<ApplicationWindow>() else {
                return;
            };
            let root_stack = root_stack.clone();
            let after_import: Rc<dyn Fn()> = Rc::new(move || {
                root_stack.set_visible_child_name("app");
            });
            present_import_backup_with_success(&window, refs.clone(), Some(after_import));
        });
    }
    {
        let name = name.clone();
        page.connect_map(move |_| {
            name.grab_focus();
        });
    }

    page
}

fn sync_setup_create_button(create: &Button, name: &EntryRow, currency: &ComboRow) {
    create.set_sensitive(!name.text().trim().is_empty() && currency.selected() <= 1);
}

pub fn build_error_window(app: &Application, error: &str) -> ApplicationWindow {
    let page = StatusPage::builder()
        .icon_name("dialog-error-symbolic")
        .title("Aureus Could Not Start")
        .description(error)
        .build();
    ApplicationWindow::builder()
        .application(app)
        .title("Aureus")
        .default_width(520)
        .default_height(360)
        .content(&page)
        .build()
}

fn page_stack() -> Stack {
    Stack::builder()
        .transition_type(gtk::StackTransitionType::None)
        .build()
}

fn build_overview_page(refs: &UiRefs) -> gtk::Widget {
    let content = page_content_box();

    // Keep the portfolio summary as the single visual anchor above history.
    // Activity lives in the application menu instead of competing with the
    // centered value and chart.
    let portfolio_summary = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(2)
        .halign(Align::Center)
        .build();
    refs.total_value.set_halign(Align::Center);
    refs.investment_return.set_halign(Align::Center);
    refs.investment_return.set_tooltip_text(Some(
        "Investment return over the selected history range",
    ));
    portfolio_summary.append(&refs.total_value);
    portfolio_summary.append(&refs.investment_return);
    content.append(&portfolio_summary);

    let history_ranges = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(6)
        .homogeneous(true)
        .build();
    let mut first_history_range: Option<ToggleButton> = None;
    for range in [
        HistoryRange::OneDay,
        HistoryRange::OneWeek,
        HistoryRange::OneMonth,
        HistoryRange::ThreeMonths,
        HistoryRange::OneYear,
        HistoryRange::FiveYears,
        HistoryRange::All,
    ] {
        let button = ToggleButton::builder()
            .label(range.label())
            .css_classes(["pill", "range-toggle"])
            .build();
        if let Some(first) = first_history_range.as_ref() {
            button.set_group(Some(first));
        } else {
            first_history_range = Some(button.clone());
        }
        if range == refs.portfolio_history_range.get() {
            button.set_active(true);
        }
        {
            let refs = refs.clone();
            button.connect_toggled(move |button| {
                if button.is_active() {
                    refs.portfolio_history_range.set(range);
                    update_portfolio_history_from_cache(&refs);
                    refresh_portfolio_history_async(refs.clone(), false);
                }
            });
        }
        history_ranges.append(&button);
    }
    content.append(refs.portfolio_history_chart.widget());
    content.append(&history_ranges);
    content.append(&section_heading("Upcoming"));
    let upcoming_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Automatic)
        .vscrollbar_policy(PolicyType::Never)
        .min_content_height(118)
        .child(&refs.upcoming_box)
        .build();
    content.append(&upcoming_scroller);

    let holdings_column = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(10)
        .hexpand(true)
        .halign(Align::Fill)
        .valign(Align::Start)
        .build();
    holdings_column.append(&section_heading("Holdings"));
    holdings_column.append(&refs.overview_list);

    let allocation_column = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(10)
        .hexpand(true)
        .halign(Align::Fill)
        .valign(Align::Start)
        .build();
    allocation_column.append(&section_heading("Allocation"));
    let allocation_card = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .build();
    allocation_card.add_css_class("card");
    let allocation_card_content = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(14)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    allocation_card_content.append(refs.allocation_ring.widget());
    allocation_card_content.append(&refs.allocation_legend);
    allocation_card.append(&allocation_card_content);
    allocation_column.append(&allocation_card);

    refs.overview_holdings_layout.append(&holdings_column);
    refs.overview_holdings_layout.append(&allocation_column);
    content.append(&refs.overview_holdings_layout);

    let scroller = page_scroller(&content, 900);
    refs.page_scroll_adjustments
        .borrow_mut()
        .insert("overview".to_string(), scroller.vadjustment());

    let add_first = Button::builder()
        .label("Add Your First Holding")
        .css_classes(["suggested-action", "pill"])
        .halign(Align::Center)
        .build();
    let empty = StatusPage::builder()
        .title("Your Portfolio Is Empty")
        .description("Record a buy or opening position to start tracking your portfolio")
        .child(&add_first)
        .build();

    refs.overview_stack.add_named(&empty, Some("empty"));
    refs.overview_stack.add_named(&scroller, Some("portfolio"));

    let refs_clone = refs.clone();
    add_first.connect_clicked(move |button| {
        let Some(root) = button.root() else {
            return;
        };
        let Ok(window) = root.downcast::<ApplicationWindow>() else {
            return;
        };
        present_add_activity_dialog(&window, refs_clone.clone());
    });

    refs.overview_stack.clone().upcast()
}

fn crossfade_loaded_widgets(widgets: Vec<gtk::Widget>, update: impl FnOnce() + 'static) {
    if widgets.is_empty() || widgets.iter().all(|widget| !widget.is_mapped()) {
        update();
        return;
    }

    let update: Rc<RefCell<Option<Box<dyn FnOnce()>>>> =
        Rc::new(RefCell::new(Some(Box::new(update))));
    let phase = Rc::new(Cell::new(0_u8));
    let started = Rc::new(RefCell::new(Instant::now()));
    glib::timeout_add_local(Duration::from_millis(16), move || {
        let elapsed = started.borrow().elapsed().as_secs_f64();
        if phase.get() == 0 {
            let progress = (elapsed / 0.075).clamp(0.0, 1.0);
            for widget in &widgets {
                widget.set_opacity(1.0 - progress);
            }
            if progress >= 1.0 {
                if let Some(update) = update.borrow_mut().take() {
                    update();
                }
                phase.set(1);
                *started.borrow_mut() = Instant::now();
            }
            glib::ControlFlow::Continue
        } else {
            let progress = (elapsed / 0.105).clamp(0.0, 1.0);
            for widget in &widgets {
                widget.set_opacity(progress);
            }
            if progress >= 1.0 {
                for widget in &widgets {
                    widget.set_opacity(1.0);
                }
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        }
    });
}

fn crossfade_loaded_label(label: &Label, text: impl Into<String>) {
    let text = text.into();
    if label.label().as_str() == text {
        return;
    }
    let label_for_update = label.clone();
    crossfade_loaded_widgets(vec![label.clone().upcast()], move || {
        label_for_update.set_label(&text);
    });
}

fn crossfade_loaded_labels(
    targets: Vec<(Label, String)>,
    update: impl FnOnce() + 'static,
) {
    let widgets = targets
        .iter()
        .filter(|(label, text)| label.label().as_str() != text.as_str())
        .map(|(label, _)| label.clone().upcast::<gtk::Widget>())
        .collect::<Vec<_>>();
    if widgets.is_empty() {
        update();
    } else {
        crossfade_loaded_widgets(widgets, update);
    }
}

fn refresh_with_loaded_crossfade(refs: UiRefs) {
    // Refresh local/cache-backed UI first, compare the resulting text, then
    // animate only values that actually changed. Stable weekend prices and
    // portfolio totals therefore stay visually still.
    let labels = vec![
        refs.total_value.clone(),
        refs.total_gain.clone(),
        refs.realized_gain.clone(),
        refs.investment_return.clone(),
        refs.cost_basis.clone(),
        refs.quote_note.clone(),
        refs.dividend_income.clone(),
        refs.dividend_yield.clone(),
        refs.dividend_status.clone(),
    ];
    let before = labels
        .iter()
        .map(|label| label.label().to_string())
        .collect::<Vec<_>>();

    refs.refresh();

    let mut changed = Vec::<(Label, String)>::new();
    for (label, old_text) in labels.into_iter().zip(before) {
        let new_text = label.label().to_string();
        if new_text != old_text {
            label.set_label(&old_text);
            changed.push((label, new_text));
        }
    }
    let widgets = changed
        .iter()
        .map(|(label, _)| label.clone().upcast::<gtk::Widget>())
        .collect::<Vec<_>>();
    crossfade_loaded_widgets(widgets, move || {
        for (label, text) in changed {
            label.set_label(&text);
        }
    });
}

fn clear_box(container: &GtkBox) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn upcoming_action_card(title: &str, subtitle: &str, detail: &str) -> GtkBox {
    let card = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(4)
        .width_request(190)
        .margin_top(2)
        .margin_bottom(8)
        .build();
    card.add_css_class("card");
    card.add_css_class("upcoming-card");
    card.append(
        &Label::builder()
            .label(title)
            .halign(Align::Start)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build(),
    );
    card.append(
        &Label::builder()
            .label(subtitle)
            .halign(Align::Start)
            .css_classes(["dim-label", "caption"])
            .build(),
    );
    card.append(
        &Label::builder()
            .label(detail)
            .halign(Align::Start)
            .wrap(true)
            .build(),
    );
    card
}

fn split_ratio_text(ratio: f64) -> String {
    if ratio >= 1.0 {
        format!("{}-for-1", trim_number(ratio))
    } else {
        format!("1-for-{}", trim_number(1.0 / ratio))
    }
}

fn rebuild_upcoming_actions(refs: &UiRefs, positions: &[Position]) {
    clear_box(&refs.upcoming_box);
    let now = current_unix_timestamp();
    let horizon = now.saturating_add(366 * 24 * 60 * 60);
    let mut items = Vec::<(i64, String, String, String)>::new();
    let mut seen_symbols = HashSet::<String>::new();

    for position in positions {
        let symbol = position.provider_symbol.to_ascii_uppercase();
        if !seen_symbols.insert(symbol.clone()) {
            continue;
        }

        let mut dividends = refs
            .state
            .database
            .dividend_events(&symbol)
            .unwrap_or_default();
        dividends.sort_by_key(|event| event.timestamp);

        let calendar = refs
            .state
            .database
            .dividend_calendar(&symbol)
            .ok()
            .flatten();
        let calendar_ex = calendar.as_ref().and_then(|(ex, _)| *ex);
        let calendar_payment = calendar.as_ref().and_then(|(_, payment)| *payment);

        let fallback_amount = dividends
            .iter()
            .rev()
            .find(|event| event.timestamp <= now)
            .or_else(|| dividends.first())
            .map(|event| event.amount)
            .unwrap_or(0.0);

        // Keep every announced ex-dividend event inside the full one-year
        // horizon, then extend the security's established cadence with clearly
        // labelled estimates when later quarters/months have not been announced.
        let mut future_ex = dividends
            .iter()
            .filter(|event| event.timestamp > now && event.timestamp <= horizon)
            .map(|event| (event.timestamp, event.amount, false))
            .collect::<Vec<_>>();

        if let Some(ex_date) = calendar_ex.filter(|date| *date > now && *date <= horizon) {
            let duplicate = future_ex
                .iter()
                .any(|(timestamp, _, _)| (*timestamp - ex_date).abs() <= 2 * 24 * 60 * 60);
            if !duplicate {
                future_ex.push((ex_date, fallback_amount, false));
            }
        }
        future_ex.sort_by_key(|event| event.0);

        let past = dividends
            .iter()
            .filter(|event| event.timestamp <= now)
            .collect::<Vec<_>>();
        let cadence = if past.len() >= 2 {
            let last = past[past.len() - 1].timestamp;
            let previous = past[past.len() - 2].timestamp;
            let gap = last.saturating_sub(previous);
            (gap >= 20 * 24 * 60 * 60 && gap <= 400 * 24 * 60 * 60).then_some(gap)
        } else {
            None
        };

        if let Some(cadence) = cadence {
            let mut anchor = future_ex
                .last()
                .map(|event| event.0)
                .or(calendar_ex)
                .or_else(|| past.last().map(|event| event.timestamp));
            while let Some(current) = anchor {
                let next = current.saturating_add(cadence);
                if next <= now {
                    anchor = Some(next);
                    continue;
                }
                if next > horizon {
                    break;
                }
                let duplicate = future_ex
                    .iter()
                    .any(|(timestamp, _, _)| (*timestamp - next).abs() <= 7 * 24 * 60 * 60);
                if !duplicate {
                    let amount = dividends
                        .iter()
                        .rev()
                        .find(|event| event.timestamp <= current)
                        .map(|event| event.amount)
                        .unwrap_or(fallback_amount);
                    future_ex.push((next, amount, true));
                    future_ex.sort_by_key(|event| event.0);
                }
                anchor = Some(next);
            }
        }

        let payment_lag = match (calendar_ex, calendar_payment) {
            (Some(ex), Some(payment)) if payment >= ex => {
                let lag = payment.saturating_sub(ex);
                (lag <= 90 * 24 * 60 * 60).then_some(lag)
            }
            _ => None,
        };

        for (timestamp, amount, estimated) in &future_ex {
            items.push((
                *timestamp,
                format!("{} · Ex-dividend", position.code),
                if *estimated {
                    format!("Estimated ex-dividend · {}", format_distribution_date(*timestamp))
                } else {
                    format!("Ex-dividend date · {}", format_distribution_date(*timestamp))
                },
                if *amount > 0.0 {
                    format!("{} per share", format_currency(*amount, &position.currency))
                } else {
                    "Dividend amount not announced".into()
                },
            ));
        }

        let mut payment_dates = HashSet::<i64>::new();
        if let Some(payment) = calendar_payment.filter(|date| *date > now && *date <= horizon) {
            payment_dates.insert(payment);
            items.push((
                payment,
                format!("{} · Dividend payment", position.code),
                format!("Payment date · {}", format_distribution_date(payment)),
                if fallback_amount > 0.0 {
                    format!("{} per share", format_currency(fallback_amount, &position.currency))
                } else {
                    "Dividend amount not announced".into()
                },
            ));
        }

        if let Some(lag) = payment_lag {
            for (ex_date, amount, _) in &future_ex {
                let payment = ex_date.saturating_add(lag);
                if payment <= now || payment > horizon {
                    continue;
                }
                if calendar_payment
                    .map(|exact| (exact - payment).abs() <= 2 * 24 * 60 * 60)
                    .unwrap_or(false)
                    || !payment_dates.insert(payment)
                {
                    continue;
                }
                items.push((
                    payment,
                    format!("{} · Dividend payment", position.code),
                    format!("Estimated payment · {}", format_distribution_date(payment)),
                    if *amount > 0.0 {
                        format!("{} per share", format_currency(*amount, &position.currency))
                    } else {
                        "Dividend amount not announced".into()
                    },
                ));
            }
        }

        for split in refs
            .state
            .database
            .split_events(&symbol)
            .unwrap_or_default()
            .into_iter()
            .filter(|split| split.timestamp > now && split.timestamp <= horizon)
        {
            let kind = if split.ratio < 1.0 { "Reverse Split" } else { "Split" };
            items.push((
                split.timestamp,
                format!("{} · {kind}", position.code),
                format_distribution_date(split.timestamp),
                split_ratio_text(split.ratio),
            ));
        }
    }

    items.sort_by_key(|item| item.0);
    if items.is_empty() {
        refs.upcoming_box.append(&upcoming_action_card(
            "Nothing announced",
            "Next 12 months",
            "Upcoming ex-dividend dates, dividend payments, and splits will appear here",
        ));
        return;
    }
    for (_, title, subtitle, detail) in items {
        refs.upcoming_box
            .append(&upcoming_action_card(&title, &subtitle, &detail));
    }
}

fn build_accounts_page(refs: &UiRefs) -> gtk::Widget {
    let content = page_content_box();
    content.append(&refs.accounts_list);

    let scroller = page_scroller(&content, 900);
    refs.page_scroll_adjustments
        .borrow_mut()
        .insert("accounts".to_string(), scroller.vadjustment());
    let empty = StatusPage::builder()
        .icon_name(accounts_icon_name())
        .title("No Accounts")
        .description("Create an account to track cash and activity")
        .build();
    refs.accounts_stack.add_named(&empty, Some("empty"));
    refs.accounts_stack.add_named(&scroller, Some("accounts"));
    refs.accounts_stack.clone().upcast()
}

fn build_search_page(refs: &UiRefs) -> gtk::Widget {
    let results: Rc<RefCell<Vec<SearchResult>>> = Rc::new(RefCell::new(Vec::new()));
    let keyboard_index: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));

    let search = refs.search_entry.clone();

    let spinner = Spinner::new();
    let search_status = Label::builder()
        .label("")
        .halign(Align::Start)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .build();
    let feedback = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .visible(false)
        .build();
    feedback.append(&spinner);
    feedback.append(&search_status);

    let result_list = ListBox::builder()
        .selection_mode(SelectionMode::None)
        .activate_on_single_click(true)
        .css_classes(["boxed-list"])
        .visible(false)
        .build();

    let content = page_content_box();
    content.set_margin_top(8);
    content.append(&feedback);
    content.append(&result_list);

    let scroller = page_scroller(&content, 900);
    scroller.set_vexpand(true);
    refs.page_scroll_adjustments
        .borrow_mut()
        .insert("search".to_string(), scroller.vadjustment());

    refs.search_top_slot.append(&search);
    let page = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .build();
    page.append(&refs.search_top_slot);
    page.append(&scroller);
    page.append(&refs.search_bottom_slot);

    enum SearchMessage {
        Complete(String, Result<Vec<SearchResult>, String>),
    }
    let (sender, receiver) = mpsc::channel::<SearchMessage>();
    let receiver = Rc::new(RefCell::new(receiver));

    {
        let result_list = result_list.clone();
        let spinner = spinner.clone();
        let search_status = search_status.clone();
        let feedback = feedback.clone();
        let results_store = results.clone();
        let keyboard_index = keyboard_index.clone();
        let search_entry = search.clone();

        glib::timeout_add_local(Duration::from_millis(50), move || {
            for message in receiver.borrow().try_iter() {
                match message {
                    SearchMessage::Complete(query, result) => {
                        if search_entry.text().trim() != query {
                            continue;
                        }
                        spinner.set_spinning(false);
                        match result {
                            Ok(items) => {
                                *results_store.borrow_mut() = items.clone();
                                keyboard_index.set(None);
                                rebuild_search_results(&result_list, &items);
                                clear_search_keyboard_highlight(&result_list);
                                result_list.set_visible(!items.is_empty());
                                if items.is_empty() {
                                    search_status.set_label("No matching stocks or ETFs");
                                    feedback.set_visible(true);
                                } else {
                                    feedback.set_visible(false);
                                }
                            }
                            Err(error) => {
                                results_store.borrow_mut().clear();
                                keyboard_index.set(None);
                                clear_list(&result_list);
                                result_list.set_visible(false);
                                search_status.set_label(&error);
                                feedback.set_visible(true);
                            }
                        }
                    }
                }
            }
            glib::ControlFlow::Continue
        });
    }

    {
        let sender = sender.clone();
        let spinner = spinner.clone();
        let search_status = search_status.clone();
        let feedback = feedback.clone();
        let result_list = result_list.clone();
        let results_store = results.clone();
        let keyboard_index = keyboard_index.clone();
        search.connect_search_changed(move |entry| {
            results_store.borrow_mut().clear();
            keyboard_index.set(None);
            clear_search_keyboard_highlight(&result_list);
            let query = entry.text().trim().to_string();
            if query.is_empty() {
                spinner.set_spinning(false);
                clear_list(&result_list);
                result_list.set_visible(false);
                search_status.set_label("");
                feedback.set_visible(false);
                return;
            }

            spinner.set_spinning(true);
            search_status.set_label("Searching");
            feedback.set_visible(true);

            let sender = sender.clone();
            std::thread::spawn(move || {
                let result = market_data::search(&query).map_err(|error| error.to_string());
                let _ = sender.send(SearchMessage::Complete(query, result));
            });
        });
    }

    {
        let controller = EventControllerKey::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let result_list = result_list.clone();
        let results = results.clone();
        let keyboard_index = keyboard_index.clone();
        let refs = refs.clone();
        controller.connect_key_pressed(move |_, key, _, _| {
            let count = results.borrow().len();
            if count == 0 {
                return glib::Propagation::Proceed;
            }

            if key == gtk::gdk::Key::Down {
                let next = match keyboard_index.get() {
                    Some(index) => (index + 1).min(count - 1),
                    None => 0,
                };
                keyboard_index.set(Some(next));
                set_search_keyboard_highlight(&result_list, Some(next));
                return glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::Up {
                let next = match keyboard_index.get() {
                    Some(index) => index.saturating_sub(1),
                    None => count - 1,
                };
                keyboard_index.set(Some(next));
                set_search_keyboard_highlight(&result_list, Some(next));
                return glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::Return || key == gtk::gdk::Key::KP_Enter {
                let index = keyboard_index.get().unwrap_or(0);
                let Some(asset) = results.borrow().get(index).cloned() else {
                    return glib::Propagation::Proceed;
                };
                keyboard_index.set(None);
                clear_search_keyboard_highlight(&result_list);
                present_search_result_detail(asset, refs.clone());
                return glib::Propagation::Stop;
            }

            glib::Propagation::Proceed
        });
        search.add_controller(controller);
    }

    {
        let results = results.clone();
        let keyboard_index = keyboard_index.clone();
        let refs = refs.clone();
        result_list.connect_row_activated(move |list, row| {
            let index = row.index();
            if index < 0 {
                return;
            }
            let Some(asset) = results.borrow().get(index as usize).cloned() else {
                return;
            };
            keyboard_index.set(None);
            clear_search_keyboard_highlight(list);
            present_search_result_detail(asset, refs.clone());
        });
    }

    page.upcast()
}

fn clear_search_keyboard_highlight(list: &ListBox) {
    let mut index = 0;
    while let Some(row) = list.row_at_index(index) {
        row.remove_css_class("search-keyboard-selected");
        index += 1;
    }
}

fn set_search_keyboard_highlight(list: &ListBox, selected: Option<usize>) {
    let mut index = 0;
    while let Some(row) = list.row_at_index(index) {
        if selected == Some(index as usize) {
            row.add_css_class("search-keyboard-selected");
        } else {
            row.remove_css_class("search-keyboard-selected");
        }
        index += 1;
    }
}

fn build_watchlist_page(refs: &UiRefs) -> gtk::Widget {
    let content = page_content_box();
    content.append(&refs.watchlist_list);

    let scroller = page_scroller(&content, 900);
    refs.page_scroll_adjustments
        .borrow_mut()
        .insert("watchlist".to_string(), scroller.vadjustment());

    let empty = StatusPage::builder()
        .icon_name("starred-symbolic")
        .title("Your Watchlist Is Empty")
        .description("Star a stock from Search to add it to your watchlist")
        .build();

    refs.watchlist_stack.add_named(&empty, Some("empty"));
    refs.watchlist_stack.add_named(&scroller, Some("watchlist"));

    refs.watchlist_stack.clone().upcast()
}

fn dividend_recent_empty_placeholder() -> GtkBox {
    // Match Aureus's full-page empty states without turning this section into a
    // card of its own. The ListBox drops its boxed background while empty, so the
    // clock icon, title, and explanation sit directly on the page surface. Keep
    // the geometry compact enough for phones and omit an action button because
    // there is no useful direct action to take from an empty dividend history.
    let icon = Image::from_icon_name("document-open-recent-symbolic");
    icon.set_pixel_size(40);
    icon.add_css_class("dim-label");

    let title = Label::builder()
        .label("No Recent Distributions")
        .halign(Align::Center)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["title-3"])
        .build();
    let description = Label::builder()
        .label("No dividend distributions were received in this period")
        .halign(Align::Center)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["dim-label"])
        .build();

    let empty = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(8)
        .hexpand(true)
        .halign(Align::Fill)
        .valign(Align::Center)
        .margin_top(26)
        .margin_bottom(26)
        .margin_start(18)
        .margin_end(18)
        .build();
    empty.append(&icon);
    empty.append(&title);
    empty.append(&description);
    empty
}

fn build_dividends_page(refs: &UiRefs) -> gtk::Widget {
    let content = page_content_box();

    // Mirror Overview's hierarchy: one strong summary value, one compact
    // secondary line, then the chart and its period control.
    let dividend_summary = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(2)
        .halign(Align::Center)
        .build();
    dividend_summary.append(&refs.dividend_income);
    dividend_summary.append(&refs.dividend_yield);
    content.append(&dividend_summary);

    // Keep the dividend plot on the page surface. The old card background and
    // horizontal guide rules made this chart visually heavier than Overview.
    content.append(refs.dividend_chart.widget());

    let period_row = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .halign(Align::Center)
        .build();
    period_row.append(&refs.dividend_period);
    content.append(&period_row);
    content.append(&refs.dividend_status);

    content.append(&refs.dividend_recent_heading);
    let recent_empty = dividend_recent_empty_placeholder();
    refs.dividend_list.remove_css_class("boxed-list");
    refs.dividend_list.set_placeholder(Some(&recent_empty));
    content.append(&refs.dividend_list);

    let scroller = page_scroller(&content, 900);
    refs.page_scroll_adjustments
        .borrow_mut()
        .insert("dividends".to_string(), scroller.vadjustment());
    let empty = StatusPage::builder()
        .icon_name("weather-showers-scattered-symbolic")
        .title("No Holdings to Analyze")
        .description("Add a holding to see dividend estimates")
        .build();

    refs.dividends_stack.add_named(&empty, Some("empty"));
    refs.dividends_stack.add_named(&scroller, Some("portfolio"));
    refs.dividends_stack.clone().upcast()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DividendPeriod {
    Annual,
    Year(i32),
}

#[derive(Clone)]
struct RecentDistribution {
    timestamp: i64,
    code: String,
    name: String,
    amount_per_share: f64,
    native_currency: String,
    estimated_base_value: Option<f64>,
}

fn selected_dividend_period(refs: &UiRefs) -> DividendPeriod {
    refs.dividend_period_options
        .borrow()
        .get(refs.dividend_period.selected() as usize)
        .copied()
        .unwrap_or(DividendPeriod::Annual)
}

fn sync_dividend_period_options(refs: &UiRefs, oldest_year: i32, current_year: i32) {
    let oldest_year = oldest_year.min(current_year);
    // "Annual" is the current calendar year. Only older years receive explicit
    // labels, which avoids showing both "Annual" and "2026" for the same data.
    let mut next = vec![DividendPeriod::Annual];
    if oldest_year < current_year {
        for year in (oldest_year..current_year).rev() {
            next.push(DividendPeriod::Year(year));
        }
    }

    let previous = selected_dividend_period(refs);
    let selected = next
        .iter()
        .position(|period| *period == previous)
        .unwrap_or(0);

    if *refs.dividend_period_options.borrow() != next {
        let labels = next
            .iter()
            .map(|period| match period {
                DividendPeriod::Annual => "Annual".to_string(),
                DividendPeriod::Year(year) => year.to_string(),
            })
            .collect::<Vec<_>>();
        let model = StringList::new(&[]);
        for label in &labels {
            model.append(label);
        }

        refs.dividend_period_updating.set(true);
        *refs.dividend_period_options.borrow_mut() = next.clone();
        refs.dividend_period.set_model(Some(&model));
        refs.dividend_period.set_selected(selected as u32);
        refs.dividend_period_updating.set(false);
    }

    // With no historical year to choose, the selector has no useful action and
    // is hidden entirely, matching the rest of Aureus's adaptive controls.
    refs.dividend_period.set_visible(next.len() > 1);
}

fn dividend_shares_held_at(
    transactions: &[Transaction],
    splits: &[SplitEvent],
    provider_symbol: &str,
    timestamp: i64,
) -> f64 {
    let mut timeline = Vec::<(i64, u8, i64, f64, Option<f64>)>::new();
    for transaction in transactions.iter().filter(|transaction| {
        transaction.provider_symbol.eq_ignore_ascii_case(provider_symbol)
            && transaction.timestamp <= timestamp
    }) {
        let priority = match transaction.transaction_type.as_str() {
            "OPEN" => 1,
            "BUY" => 2,
            "TRANSFER_IN" => 3,
            "SELL" => 4,
            "TRANSFER_OUT" => 5,
            _ => 6,
        };
        let delta = match transaction.transaction_type.as_str() {
            "BUY" | "OPEN" | "TRANSFER_IN" => transaction.shares,
            "SELL" | "TRANSFER_OUT" => -transaction.shares,
            _ => 0.0,
        };
        timeline.push((
            transaction.timestamp,
            priority,
            transaction.id,
            delta,
            None,
        ));
    }
    for split in splits
        .iter()
        .filter(|split| split.timestamp <= timestamp)
    {
        timeline.push((split.timestamp, 0, 0, 0.0, Some(split.ratio)));
    }
    timeline.sort_by(|left, right| (left.0, left.1, left.2).cmp(&(right.0, right.1, right.2)));

    let mut shares = 0.0;
    for (_, _, _, delta, split_ratio) in timeline {
        if let Some(ratio) = split_ratio {
            shares *= ratio;
        } else {
            shares += delta;
        }
    }
    shares.max(0.0)
}

fn estimated_future_dividend_events(
    refs: &UiRefs,
    provider_symbol: &str,
    events: &[DividendEvent],
    holding_currency: &str,
    now: i64,
    horizon: i64,
) -> Vec<(i64, f64, String)> {
    if horizon <= now {
        return Vec::new();
    }

    let mut sorted = events.to_vec();
    sorted.sort_by_key(|event| event.timestamp);
    let fallback = sorted
        .iter()
        .rev()
        .find(|event| event.timestamp <= now)
        .or_else(|| sorted.last());
    let fallback_amount = fallback.map(|event| event.amount).unwrap_or(0.0);
    let fallback_currency = fallback
        .map(|event| {
            if event.currency.trim().is_empty() {
                holding_currency.to_string()
            } else {
                event.currency.clone()
            }
        })
        .unwrap_or_else(|| holding_currency.to_string());

    let mut future = sorted
        .iter()
        .filter(|event| event.timestamp > now && event.timestamp <= horizon)
        .map(|event| {
            (
                event.timestamp,
                event.amount,
                if event.currency.trim().is_empty() {
                    holding_currency.to_string()
                } else {
                    event.currency.clone()
                },
            )
        })
        .collect::<Vec<_>>();

    let calendar_ex = refs
        .state
        .database
        .dividend_calendar(provider_symbol)
        .ok()
        .flatten()
        .and_then(|(ex, _)| ex);
    if let Some(ex_date) = calendar_ex.filter(|date| *date > now && *date <= horizon) {
        let duplicate = future
            .iter()
            .any(|(timestamp, _, _)| (*timestamp - ex_date).abs() <= 2 * 24 * 60 * 60);
        if !duplicate && fallback_amount > 0.0 {
            future.push((ex_date, fallback_amount, fallback_currency.clone()));
        }
    }
    future.sort_by_key(|event| event.0);

    let past = sorted
        .iter()
        .filter(|event| event.timestamp <= now)
        .collect::<Vec<_>>();
    let cadence = if past.len() >= 2 {
        let last = past[past.len() - 1].timestamp;
        let previous = past[past.len() - 2].timestamp;
        let gap = last.saturating_sub(previous);
        (gap >= 20 * 24 * 60 * 60 && gap <= 400 * 24 * 60 * 60).then_some(gap)
    } else {
        None
    };

    if let Some(cadence) = cadence {
        let mut anchor = future
            .last()
            .map(|event| event.0)
            .or(calendar_ex)
            .or_else(|| past.last().map(|event| event.timestamp));
        while let Some(current) = anchor {
            let next = current.saturating_add(cadence);
            if next <= now {
                anchor = Some(next);
                continue;
            }
            if next > horizon {
                break;
            }
            let duplicate = future
                .iter()
                .any(|(timestamp, _, _)| (*timestamp - next).abs() <= 7 * 24 * 60 * 60);
            if !duplicate && fallback_amount > 0.0 {
                let amount = sorted
                    .iter()
                    .rev()
                    .find(|event| event.timestamp <= current)
                    .map(|event| event.amount)
                    .unwrap_or(fallback_amount);
                future.push((next, amount, fallback_currency.clone()));
                future.sort_by_key(|event| event.0);
            }
            anchor = Some(next);
        }
    }

    future
}

fn rebuild_dividend_page(refs: &UiRefs, positions: &[Position], base: &str, usd_cad: Option<f64>) {
    clear_list(&refs.dividend_list);
    refs.dividend_recent_heading.set_visible(false);
    // `clear_list()` also removes GtkListBox's placeholder widget because the
    // placeholder is a child of the list. Reinstall it after every rebuild. While
    // empty, remove the boxed-list class so the placeholder uses the same plain
    // page surface as Aureus's Overview empty state instead of a gray card.
    refs.dividend_list.remove_css_class("boxed-list");
    let recent_empty = dividend_recent_empty_placeholder();
    refs.dividend_list.set_placeholder(Some(&recent_empty));

    if positions.is_empty() {
        refs.dividends_stack.set_visible_child_name("empty");
        set_dividend_income_text(&refs.dividend_income, "—");
        refs.dividend_yield.set_label("—");
        refs.dividend_status.set_label("No holdings yet");
        refs.dividend_chart
            .set_message("Add a holding to see dividend estimates");
        return;
    }
    refs.dividends_stack.set_visible_child_name("portfolio");

    #[derive(Clone)]
    struct GroupedHolding {
        code: String,
        name: String,
        currency: String,
        shares: f64,
    }

    let mut holdings = HashMap::<String, GroupedHolding>::new();
    for position in positions {
        let key = position.provider_symbol.to_ascii_uppercase();
        holdings
            .entry(key)
            .and_modify(|holding| holding.shares += position.shares)
            .or_insert_with(|| GroupedHolding {
                code: position.code.clone(),
                name: position.name.clone(),
                currency: position.currency.clone(),
                shares: position.shares,
            });
    }

    let now = current_unix_timestamp();
    let current_year = local_date_parts().0;
    let transactions = refs.state.database.load_transactions().unwrap_or_default();
    let active_symbols = holdings.keys().cloned().collect::<HashSet<_>>();
    let oldest_year = transactions
        .iter()
        .filter(|transaction| {
            active_symbols.contains(&transaction.provider_symbol.to_ascii_uppercase())
                && matches!(
                    transaction.transaction_type.as_str(),
                    "BUY" | "OPEN" | "TRANSFER_IN"
                )
        })
        .filter_map(|transaction| timestamp_year_month(transaction.timestamp).map(|(year, _)| year))
        .min()
        .unwrap_or(current_year);
    sync_dividend_period_options(refs, oldest_year, current_year);
    let selected_period = selected_dividend_period(refs);
    let selected_year = match selected_period {
        DividendPeriod::Annual => current_year,
        DividendPeriod::Year(year) => year,
    };

    let mut splits = HashMap::<String, Vec<SplitEvent>>::new();
    for symbol in holdings.keys() {
        splits.insert(
            symbol.clone(),
            refs.state.database.split_events(symbol).unwrap_or_default(),
        );
    }

    let mut fetched_symbols = 0usize;
    let mut found_any = false;
    let mut recent = Vec::<RecentDistribution>::new();
    let mut by_month = HashMap::<(i32, u32), f64>::new();
    let mut estimated_months = HashSet::<(i32, u32)>::new();
    let mut incomplete_years = HashSet::<i32>::new();

    for (symbol, holding) in &holdings {
        if refs
            .state
            .database
            .dividends_fetched_at(symbol)
            .ok()
            .flatten()
            .is_some()
        {
            fetched_symbols += 1;
        }

        let mut events = refs.state.database.dividend_events(symbol).unwrap_or_default();
        events.sort_by_key(|event| event.timestamp);
        if events.is_empty() {
            continue;
        }
        found_any = true;

        let symbol_splits = splits.get(symbol).map(Vec::as_slice).unwrap_or(&[]);
        for event in events.iter().filter(|event| event.timestamp <= now) {
            let shares = dividend_shares_held_at(
                &transactions,
                symbol_splits,
                symbol,
                event.timestamp,
            );
            // Provider history can predate the user's ownership by years. Those
            // distributions are not part of this portfolio and must not appear
            // in Recent Distributions or historical chart totals.
            if shares <= 0.0000001 {
                continue;
            }

            let event_currency = if event.currency.trim().is_empty() {
                holding.currency.as_str()
            } else {
                event.currency.as_str()
            };
            let native_value = event.amount * shares;
            let base_value = convert_currency(native_value, event_currency, base, usd_cad);
            if let Some((year, month)) = timestamp_year_month(event.timestamp) {
                match base_value {
                    Some(value) => *by_month.entry((year, month)).or_insert(0.0) += value,
                    None => {
                        incomplete_years.insert(year);
                    }
                }
            }

            recent.push(RecentDistribution {
                timestamp: event.timestamp,
                code: holding.code.clone(),
                name: holding.name.clone(),
                amount_per_share: event.amount,
                native_currency: event_currency.to_string(),
                estimated_base_value: base_value,
            });
        }

        // The current Annual view is a Jan–Dec calendar-year forecast. Keep
        // already-paid months tied to the shares actually held at each event,
        // then fill the remaining months with announced/cadence estimates using
        // today's share count. Historical years never receive forecast values.
        let year_end = parse_trade_date(&format!("{current_year}-12-31"))
            .unwrap_or(now)
            .saturating_add(24 * 60 * 60 - 1);
        for (timestamp, amount, currency) in estimated_future_dividend_events(
            refs,
            symbol,
            &events,
            &holding.currency,
            now,
            year_end,
        ) {
            let Some((year, month)) = timestamp_year_month(timestamp) else {
                continue;
            };
            if year != current_year {
                continue;
            }
            estimated_months.insert((year, month));
            match convert_currency(amount * holding.shares, &currency, base, usd_cad) {
                Some(value) => *by_month.entry((year, month)).or_insert(0.0) += value,
                None => {
                    incomplete_years.insert(year);
                }
            }
        }
    }

    // Every period is a Jan–Dec view. "Annual" is simply the current year;
    // explicit year options are historical years only.
    let chart_values = (1..=12)
        .map(|month| {
            (
                month_name(month).to_string(),
                by_month
                    .get(&(selected_year, month))
                    .copied()
                    .unwrap_or(0.0),
                estimated_months.contains(&(selected_year, month)),
            )
        })
        .collect::<Vec<_>>();
    let selected_total = chart_values
        .iter()
        .map(|(_, value, _)| *value)
        .sum::<f64>();
    let selected_complete = !incomplete_years.contains(&selected_year);

    // The headline is derived from the exact same twelve buckets as the chart,
    // so changing the period can never leave a stale forward run-rate above a
    // smaller set of bars.
    if selected_complete {
        set_dividend_income_text(
            &refs.dividend_income,
            &format_currency(selected_total, base),
        );
    } else if selected_total > 0.0 {
        set_dividend_income_text(
            &refs.dividend_income,
            &format!("{}+", format_currency(selected_total, base)),
        );
    } else {
        set_dividend_income_text(&refs.dividend_income, "—");
    }

    let portfolio_market = sum_optional_converted(
        positions
            .iter()
            .map(|position| (position.market_value(), position.currency.as_str())),
        base,
        usd_cad,
    );
    let yield_text = match (portfolio_market, selected_complete) {
        (Some(market), true) if market > f64::EPSILON => {
            format!("{:.2}% yield", selected_total / market * 100.0)
        }
        _ => "— yield".to_string(),
    };
    let monthly_text = if selected_complete {
        format!("{} avg/mo", format_currency(selected_total / 12.0, base))
    } else {
        "—/mo".to_string()
    };
    refs.dividend_yield
        .set_label(&format!("{yield_text} · {monthly_text}"));

    if chart_values.iter().any(|(_, value, _)| *value > 0.0) {
        refs.dividend_chart.set_values(chart_values, base);
    } else if fetched_symbols == holdings.len() {
        refs.dividend_chart
            .set_message("No distributions while held in this period");
    } else {
        refs.dividend_chart.set_message("Checking dividend history");
    }

    // Recent Distributions follows the same period selector as the chart. The
    // ListBox placeholder supplies the HIG-style empty state when nothing was
    // actually received in that calendar year.
    recent.retain(|distribution| {
        timestamp_year_month(distribution.timestamp)
            .map(|(year, _)| year == selected_year)
            .unwrap_or(false)
    });
    recent.sort_by(|left, right| right.timestamp.cmp(&left.timestamp));
    recent.truncate(16);
    for distribution in recent {
        let row = ActionRow::builder()
            .title(&format!(
                "{} · {}",
                distribution.code,
                format_distribution_date(distribution.timestamp)
            ))
            .subtitle(&format!(
                "{} · {} per share",
                distribution.name,
                format_currency(
                    distribution.amount_per_share,
                    &distribution.native_currency
                )
            ))
            .build();
        row.set_activatable(false);
        let estimate = distribution
            .estimated_base_value
            .map(|value| format!("≈ {}", format_currency(value, base)))
            .unwrap_or_else(|| "Native currency".into());
        row.add_suffix(
            &Label::builder()
                .label(&estimate)
                .halign(Align::End)
                .css_classes(["dim-label"])
                .build(),
        );
        refs.dividend_list.append(&row);
    }

    append_dividend_history_summaries(refs, positions, base, usd_cad, selected_year);

    // Use the normal libadwaita boxed-list treatment only when there are real
    // rows. The empty placeholder remains directly on the page surface.
    let has_recent = refs.dividend_list.row_at_index(0).is_some();
    if has_recent {
        refs.dividend_list.add_css_class("boxed-list");
    } else {
        refs.dividend_list.remove_css_class("boxed-list");
    }
    refs.dividend_list.set_show_separators(false);
    refs.dividend_recent_heading.set_visible(has_recent);

    // The selector already names historical years, so do not repeat the same
    // year immediately below it. Annual names the current year because its
    // selector label intentionally describes the mode rather than the year.
    let period_note = match selected_period {
        DividendPeriod::Annual => format!("{current_year} · received + estimated"),
        DividendPeriod::Year(_) => "Received while held".to_string(),
    };
    let mut note = if fetched_symbols < holdings.len() {
        format!("{period_note} · updating cached dividend data")
    } else if found_any {
        period_note
    } else {
        "No distributions found".to_string()
    };
    if !selected_complete {
        note.push_str(" · unsupported currencies excluded from totals");
    }
    refs.dividend_status.set_label(&note);
}

fn append_dividend_history_summaries(
    refs: &UiRefs,
    positions: &[Position],
    base: &str,
    usd_cad: Option<f64>,
    selected_year: i32,
) {
    let cash_entries = refs.state.database.load_cash_entries().unwrap_or_default();
    let paid = cash_entries
        .into_iter()
        .filter(|entry| {
            entry.kind == "DIVIDEND"
                && timestamp_year_month(entry.occurred_at)
                    .map(|(year, _)| year == selected_year)
                    .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    if paid.is_empty() {
        return;
    }

    let account_currency = refs
        .state
        .database
        .load_accounts()
        .unwrap_or_default()
        .into_iter()
        .map(|account| (account.id, account.currency))
        .collect::<HashMap<_, _>>();

    let paid_total = sum_converted(
        paid.iter().filter_map(|entry| {
            let currency = account_currency
                .get(&entry.account_id)
                .map(|value| value.as_str())
                .unwrap_or(entry.currency.as_str());
            if entry.amount > 0.0 {
                Some((entry.amount, currency))
            } else {
                None
            }
        }),
        base,
        usd_cad,
    );

    let mut by_year: HashMap<i32, f64> = HashMap::new();
    let mut by_month: HashMap<(i32, u32), f64> = HashMap::new();
    let mut by_symbol: HashMap<String, f64> = HashMap::new();

    for entry in &paid {
        let currency = account_currency
            .get(&entry.account_id)
            .map(|value| value.as_str())
            .unwrap_or(entry.currency.as_str());
        let Some(value) = convert_currency(entry.amount, currency, base, usd_cad) else {
            continue;
        };
        if let Some((year, month)) = timestamp_year_month(entry.occurred_at) {
            *by_year.entry(year).or_insert(0.0) += value;
            *by_month.entry((year, month)).or_insert(0.0) += value;
        }
        let symbol = entry
            .description
            .split_whitespace()
            .next()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("Dividend")
            .to_ascii_uppercase();
        *by_symbol.entry(symbol).or_insert(0.0) += value;
    }

    let total_row = ActionRow::builder()
        .title("Paid dividends")
        .subtitle("Recorded as cash activity")
        .build();
    total_row.set_activatable(false);
    total_row.add_suffix(
        &Label::builder()
            .label(&paid_total.map(|value| format_currency(value, base)).unwrap_or_else(|| "—".into()))
            .halign(Align::End)
            .css_classes(["heading"])
            .build(),
    );
    refs.dividend_list.append(&total_row);

    let mut years = by_year.into_iter().collect::<Vec<_>>();
    years.sort_by(|left, right| right.0.cmp(&left.0));
    for (year, value) in years.into_iter().take(3) {
        let row = ActionRow::builder()
            .title(&format!("{year} dividends"))
            .subtitle("Yearly total")
            .build();
        row.set_activatable(false);
        row.add_suffix(
            &Label::builder()
                .label(&format_currency(value, base))
                .halign(Align::End)
                .css_classes(["dim-label"])
                .build(),
        );
        refs.dividend_list.append(&row);
    }

    let mut months = by_month.into_iter().collect::<Vec<_>>();
    months.sort_by(|left, right| right.0.cmp(&left.0));
    for ((year, month), value) in months.into_iter().take(4) {
        let row = ActionRow::builder()
            .title(&format!("{} {year}", month_name(month)))
            .subtitle("Monthly dividend income")
            .build();
        row.set_activatable(false);
        row.add_suffix(
            &Label::builder()
                .label(&format_currency(value, base))
                .halign(Align::End)
                .css_classes(["dim-label"])
                .build(),
        );
        refs.dividend_list.append(&row);
    }

    let mut symbols = by_symbol.into_iter().collect::<Vec<_>>();
    symbols.sort_by(|left, right| right.1.partial_cmp(&left.1).unwrap_or(std::cmp::Ordering::Equal));
    for (symbol, value) in symbols.into_iter().take(5) {
        let name = positions
            .iter()
            .find(|position| position.code.eq_ignore_ascii_case(&symbol))
            .map(|position| position.name.as_str())
            .unwrap_or("Dividend source");
        let row = ActionRow::builder()
            .title(&symbol)
            .subtitle(&format!("{} total", name))
            .build();
        row.set_activatable(false);
        row.add_suffix(
            &Label::builder()
                .label(&format_currency(value, base))
                .halign(Align::End)
                .css_classes(["dim-label"])
                .build(),
        );
        refs.dividend_list.append(&row);
    }
}

fn page_content_box() -> GtkBox {
    let content = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(14)
        .margin_top(18)
        .margin_bottom(24)
        .margin_start(14)
        .margin_end(14)
        .build();
    content
}

fn page_scroller(content: &GtkBox, maximum_size: i32) -> gtk::ScrolledWindow {
    let clamp = adw::Clamp::builder()
        .maximum_size(maximum_size)
        .tightening_threshold(700)
        .child(content)
        .build();
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .child(&clamp)
        .build()
}

fn build_detail_pull_refresh(scroller: &gtk::ScrolledWindow) -> DetailPullRefresh {
    let spinner = Spinner::new();
    spinner.set_visible(false);
    spinner.set_size_request(18, 18);
    let icon = Image::from_icon_name("view-refresh-symbolic");
    icon.set_pixel_size(18);

    let indicator_box = Overlay::new();
    indicator_box.set_halign(Align::Center);
    indicator_box.set_valign(Align::Center);
    indicator_box.set_size_request(38, 38);
    indicator_box.set_can_target(false);
    icon.set_halign(Align::Center);
    icon.set_valign(Align::Center);
    spinner.set_halign(Align::Center);
    spinner.set_valign(Align::Center);
    indicator_box.set_child(Some(&icon));
    indicator_box.add_overlay(&spinner);
    indicator_box.set_margin_top(6);
    indicator_box.set_margin_bottom(6);

    let spacer = GtkBox::builder()
        .height_request(50)
        .hexpand(true)
        .build();
    let revealer = Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(140)
        .reveal_child(false)
        .hexpand(true)
        .child(&spacer)
        .build();
    let visual_revealer = Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(140)
        .reveal_child(false)
        .halign(Align::Fill)
        .valign(Align::Start)
        .hexpand(true)
        .child(&indicator_box)
        .build();
    visual_revealer.set_can_target(false);
    scroller.set_vexpand(true);

    DetailPullRefresh {
        revealer,
        visual_revealer,
        spinner,
        icon,
        adjustment: scroller.vadjustment(),
        pending: Rc::new(Cell::new(0)),
    }
}

fn install_detail_pull_to_refresh(
    scroller: &gtk::ScrolledWindow,
    header: &HeaderBar,
    pull: DetailPullRefresh,
    pending_tasks: u8,
    refresh: Rc<dyn Fn()>,
) {
    let gesture = GestureDrag::new();
    // Capture first, then claim only a confirmed downward pull. This prevents
    // the detail ScrolledWindow from receiving the same gesture as momentum.
    gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
    let can_pull = Rc::new(Cell::new(false));
    let pulling = Rc::new(Cell::new(false));
    let armed = Rc::new(Cell::new(false));

    {
        let can_pull = can_pull.clone();
        let pulling = pulling.clone();
        let armed = armed.clone();
        let pull = pull.clone();
        let header: HeaderBar = (*header).clone();
        gesture.connect_drag_begin(move |_, _, _| {
            position_pull_refresh_visual(&header, &pull.visual_revealer);
            if pull.pending.get() > 0 {
                can_pull.set(false);
                pulling.set(false);
                armed.set(false);
                return;
            }
            let at_top = pull.adjustment.value() <= pull.adjustment.lower() + 0.5;
            can_pull.set(at_top);
            pulling.set(false);
            armed.set(false);
            pull.spinner.stop();
            pull.spinner.set_visible(false);
            pull.icon.set_visible(true);
            pull.icon.set_opacity(0.28);
            pull.revealer.set_reveal_child(false);
            pull.visual_revealer.set_reveal_child(false);
        });
    }
    {
        let can_pull = can_pull.clone();
        let pulling = pulling.clone();
        let armed = armed.clone();
        let pull = pull.clone();
        let header: HeaderBar = (*header).clone();
        gesture.connect_drag_update(move |gesture, offset_x, offset_y| {
            position_pull_refresh_visual(&header, &pull.visual_revealer);
            if !can_pull.get() {
                return;
            }
            if !pulling.get() {
                if offset_y <= 8.0 || offset_y <= offset_x.abs() {
                    armed.set(false);
                    pull.icon.set_opacity(0.28);
                    pull.visual_revealer.set_reveal_child(false);
                    return;
                }
                let _ = gesture.set_state(gtk::EventSequenceState::Claimed);
                pulling.set(true);
                // Match the main pages: reveal the spacer once during the live
                // pull, then keep it stable until the sequence ends.
                pull.revealer.set_reveal_child(true);
                pull.visual_revealer.set_reveal_child(true);
            }
            reset_adjustment_to_top(&pull.adjustment);
            let progress = (offset_y / 84.0).clamp(0.0, 1.0);
            pull.icon.set_opacity(0.28 + progress * 0.72);
            armed.set(offset_y >= 84.0);
        });
    }
    {
        let can_pull = can_pull.clone();
        let pulling = pulling.clone();
        let armed = armed.clone();
        let pull = pull.clone();
        gesture.connect_drag_end(move |_, _, _| {
            pulling.set(false);
            if !can_pull.replace(false) {
                armed.set(false);
                return;
            }
            if armed.replace(false) {
                pull.begin(pending_tasks);
                refresh();
            } else {
                pull.cancel();
            }
        });
    }
    scroller.add_controller(gesture);
}

fn metric_card(title: &str, value: &Label) -> GtkBox {
    let card = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(4)
        .hexpand(true)
        .build();
    card.add_css_class("card");
    card.add_css_class("metric-card");
    card.append(
        &Label::builder()
            .label(title)
            .halign(Align::Start)
            .css_classes(["dim-label", "caption"])
            .build(),
    );
    card.append(value);
    card
}

fn metric_value_label() -> Label {
    Label::builder()
        .label("—")
        .halign(Align::Start)
        .css_classes(["heading"])
        .build()
}

fn section_heading(text: &str) -> Label {
    Label::builder()
        .label(text)
        .halign(Align::Start)
        .css_classes(["title-3"])
        .build()
}

fn positions_list() -> ListBox {
    ListBox::builder()
        .selection_mode(SelectionMode::None)
        .css_classes(["boxed-list"])
        .build()
}

fn rebuild_overview_list(
    list: &ListBox,
    positions: &[Position],
    accounts: &[Account],
    base: &str,
    usd_cad: Option<f64>,
) {
    #[derive(Clone)]
    enum HoldingRow {
        Position(Position, f64),
        Cash(Account, f64),
    }

    clear_list(list);
    let mut rows = Vec::<HoldingRow>::new();
    for position in positions {
        let value = converted_market_value(position, base, usd_cad)
            .or_else(|| position.market_value())
            .unwrap_or(0.0);
        rows.push(HoldingRow::Position(position.clone(), value));
    }
    for account in accounts {
        if account.cash.abs() <= 0.005 {
            continue;
        }
        let value = convert_currency(account.cash, &account.currency, base, usd_cad)
            .unwrap_or(account.cash);
        rows.push(HoldingRow::Cash(account.clone(), value));
    }
    rows.sort_by(|a, b| {
        let a_value = match a {
            HoldingRow::Position(_, value) | HoldingRow::Cash(_, value) => *value,
        };
        let b_value = match b {
            HoldingRow::Position(_, value) | HoldingRow::Cash(_, value) => *value,
        };
        b_value
            .partial_cmp(&a_value)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Overview is the portfolio's holdings view, so show every asset here rather
    // than silently truncating to a "largest positions" subset.
    for holding in rows {
        match holding {
            HoldingRow::Position(position, _) => {
                list.append(&position_row(&position, base, usd_cad, false));
            }
            HoldingRow::Cash(account, _) => {
                list.append(&cash_holding_row(&account, base, usd_cad));
            }
        }
    }
}

fn rebuild_allocation(
    refs: &UiRefs,
    positions: &[Position],
    accounts: &[Account],
    base: &str,
    usd_cad: Option<f64>,
) {
    let mut by_security = HashMap::<String, (String, String, f64)>::new();
    for position in positions {
        // Quotes normally provide current market value. Until a quote exists,
        // keep the holding represented with its cost basis rather than making
        // an asset disappear from the allocation ring altogether.
        let native_value = position.market_value().unwrap_or_else(|| position.cost_basis());
        let value = convert_currency(native_value, &position.currency, base, usd_cad)
            .unwrap_or(native_value)
            .max(0.0);
        if value <= f64::EPSILON {
            continue;
        }
        let entry = by_security
            .entry(position.provider_symbol.clone())
            .or_insert_with(|| (position.code.clone(), position.name.clone(), 0.0));
        // Prefer a later non-empty company name if an older imported position
        // for the same Yahoo symbol did not have one.
        if entry.1.trim().is_empty() && !position.name.trim().is_empty() {
            entry.1 = position.name.clone();
        }
        entry.2 += value;
    }

    let mut raw = by_security
        .into_iter()
        .map(|(key, (label, _, value))| AllocationSlice {
            key,
            label,
            value,
            color_index: 0,
            color: None,
            is_cash: false,
        })
        .collect::<Vec<_>>();

    let cash = accounts
        .iter()
        .filter_map(|account| {
            if account.cash <= 0.005 {
                return None;
            }
            Some(
                convert_currency(account.cash, &account.currency, base, usd_cad)
                    .unwrap_or(account.cash),
            )
        })
        .sum::<f64>();
    if cash > 0.005 {
        raw.push(AllocationSlice {
            key: "cash".into(),
            label: "Cash".into(),
            value: cash,
            color_index: 0,
            color: None,
            is_cash: true,
        });
    }

    // Keep every security visible. Small positions are intentionally not
    // collapsed into an "Other" slice so the allocation view stays complete.
    let mut slices = raw;

    // Largest positions receive picture-color priority. Each security starts
    // with the most populated non-white color in its user-selected circular
    // picture. If that color is already used by a larger position, move through
    // that picture's next dominant colors before falling back to Aureus' palette.
    slices.sort_by(|a, b| {
        b.value
            .partial_cmp(&a.value)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.label.cmp(&b.label))
    });

    let dark = adw::StyleManager::for_display(&refs.allocation_ring.widget().display()).is_dark();
    let mut used_colors = Vec::<(f64, f64, f64)>::new();
    let mut fallback_index = 0usize;
    for slice in &mut slices {
        if slice.is_cash {
            continue;
        }

        let candidates = stock_image_colors(&slice.key);
        if let Some(color) = candidates.into_iter().find(|candidate| {
            used_colors
                .iter()
                .all(|used| !allocation_colors_collide(*candidate, *used))
        }) {
            slice.color = Some(color);
            used_colors.push(color);
            continue;
        }

        // If this stock has no chosen picture or no distinct usable picture
        // color, choose a fallback palette slot that stays distinct from every
        // picture-derived/fallback security color already assigned.
        loop {
            let candidate = allocation_color(fallback_index, None, false, dark);
            let chosen_index = fallback_index;
            fallback_index += 1;
            if used_colors
                .iter()
                .all(|used| !allocation_colors_collide(candidate, *used))
            {
                slice.color_index = chosen_index;
                used_colors.push(candidate);
                break;
            }
        }
    }

    refs.allocation_ring.set_slices(slices.clone(), base);
    clear_box(&refs.allocation_legend);
    let total = slices.iter().map(|slice| slice.value).sum::<f64>();
    if total <= f64::EPSILON {
        refs.allocation_legend.append(
            &Label::builder()
                .label("No holdings yet")
                .halign(Align::Center)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
        return;
    }

    for (index, slice) in slices.into_iter().enumerate() {
        refs.allocation_legend.append(&allocation_legend_row(
            &slice,
            index,
            total,
            base,
            &refs.allocation_ring,
        ));
    }
}

fn allocation_legend_row(
    slice: &AllocationSlice,
    index: usize,
    total: f64,
    base: &str,
    ring: &AllocationRing,
) -> GtkBox {
    let row = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(9)
        .css_classes(["allocation-legend-row"])
        .build();

    let swatch = gtk::DrawingArea::builder()
        .width_request(10)
        .height_request(10)
        .halign(Align::Center)
        .valign(Align::Start)
        .margin_top(5)
        .build();
    let color_index = slice.color_index;
    let color = slice.color;
    let is_cash = slice.is_cash;
    swatch.set_draw_func(move |area, context, width, height| {
        let dark = adw::StyleManager::for_display(&area.display()).is_dark();
        let (red, green, blue) = allocation_color(color_index, color, is_cash, dark);
        let radius = f64::from(width.min(height)) / 2.0;
        context.set_source_rgb(red, green, blue);
        context.arc(
            f64::from(width) / 2.0,
            f64::from(height) / 2.0,
            radius,
            0.0,
            std::f64::consts::PI * 2.0,
        );
        let _ = context.fill();
    });
    row.append(&swatch);

    let labels = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(1)
        .hexpand(true)
        .build();

    let top_line = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .hexpand(true)
        .build();
    top_line.append(
        &Label::builder()
            .label(&slice.label)
            .halign(Align::Start)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build(),
    );
    top_line.append(
        &Label::builder()
            .label(&format!("{:.1}%", slice.value / total * 100.0))
            .halign(Align::End)
            .valign(Align::Start)
            .build(),
    );
    labels.append(&top_line);
    labels.append(
        &Label::builder()
            .label(&format_allocation_currency(slice.value, base))
            .halign(Align::Start)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label", "caption"])
            .build(),
    );
    row.append(&labels);

    let click = gtk::GestureClick::new();
    {
        let ring = ring.clone();
        click.connect_released(move |_, _, _, _| {
            ring.toggle_index(index);
        });
    }
    row.add_controller(click);
    row
}

fn format_allocation_currency(value: f64, currency: &str) -> String {
    let prefix = match currency {
        "CAD" => "C$",
        "USD" => "US$",
        "EUR" => "€",
        "GBP" => "£",
        _ => "",
    };
    let sign = if value.is_sign_negative() { "−" } else { "" };
    let raw = format!("{:.2}", value.abs());
    let (whole, fraction) = raw.split_once('.').unwrap_or((raw.as_str(), "00"));
    let mut grouped = String::with_capacity(whole.len() + whole.len() / 3);
    let first = whole.len() % 3;
    if first > 0 {
        grouped.push_str(&whole[..first]);
        if first < whole.len() {
            grouped.push(',');
        }
    }
    for (chunk_index, chunk) in whole[first..].as_bytes().chunks(3).enumerate() {
        if chunk_index > 0 {
            grouped.push(',');
        }
        grouped.push_str(std::str::from_utf8(chunk).unwrap_or_default());
    }
    let number = format!("{sign}{grouped}.{fraction}");
    if prefix.is_empty() {
        format!("{number} {currency}")
    } else {
        format!("{prefix}{number}")
    }
}

fn cash_holding_row(account: &Account, base: &str, usd_cad: Option<f64>) -> ListBoxRow {
    let row = ListBoxRow::new();
    row.set_selectable(false);
    row.set_activatable(true);
    row.set_widget_name(&format!("cash-{}", account.id));
    row.set_tooltip_text(Some("Manage cash"));

    let content = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(4)
        .margin_top(10)
        .margin_bottom(10)
        .margin_start(12)
        .margin_end(8)
        .build();
    let top = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .build();
    top.append(
        &Label::builder()
            .label("Cash")
            .halign(Align::Start)
            .hexpand(true)
            .css_classes(["heading"])
            .build(),
    );
    let display_value = convert_currency(account.cash, &account.currency, base, usd_cad)
        .map(|value| format_currency(value, base))
        .unwrap_or_else(|| format_currency(account.cash, &account.currency));
    top.append(
        &Label::builder()
            .label(&display_value)
            .halign(Align::End)
            .css_classes(["heading"])
            .build(),
    );
    top.append(
        &Image::builder()
            .icon_name("go-next-symbolic")
            .css_classes(["dim-label"])
            .build(),
    );
    content.append(&top);

    let mut subtitle = format!("{} · {}", account.name.clone(), account.currency);
    if account.currency != base {
        subtitle.push_str(&format!(" · {} native", format_currency(account.cash, &account.currency)));
    }
    content.append(
        &Label::builder()
            .label(&subtitle)
            .halign(Align::Start)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label", "caption"])
            .build(),
    );
    row.set_child(Some(&content));
    row
}

fn rebuild_accounts_list(
    list: &ListBox,
    accounts: &[Account],
    positions: &[Position],
    base: &str,
    usd_cad: Option<f64>,
) {
    clear_list(list);
    for account in accounts {
        let account_positions = positions
            .iter()
            .filter(|position| position.account_id == account.id)
            .collect::<Vec<_>>();
        let count = account_positions.len();
        let holdings_total = if count == 0 {
            Some(0.0)
        } else {
            sum_optional_converted(
                account_positions
                    .iter()
                    .map(|position| (position.market_value(), position.currency.as_str())),
                base,
                usd_cad,
            )
        };
        let cash_in_base = convert_currency(account.cash, &account.currency, base, usd_cad);
        let total = match (holdings_total, cash_in_base) {
            (Some(holdings), Some(cash)) => Some(holdings + cash),
            _ => None,
        };

        let subtitle = format!(
            "{} · {} cash · {}",
            account.currency,
            format_currency(account.cash, &account.currency),
            holding_count_text(count)
        );
        let row = ListBoxRow::new();
        row.set_selectable(false);
        row.set_activatable(true);
        row.set_widget_name(&format!("account-{}", account.id));
        row.set_tooltip_text(Some("Open account details"));
        let content = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(4)
            .margin_top(10)
            .margin_bottom(10)
            .margin_start(12)
            .margin_end(8)
            .build();
        let top = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(8)
            .build();
        top.append(
            &Label::builder()
                .label(&account.name)
                .halign(Align::Start)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["heading"])
                .build(),
        );
        top.append(
            &Label::builder()
                .label(
                    &total
                        .map(|value| format_currency(value, base))
                        .unwrap_or_else(|| "—".into()),
                )
                .halign(Align::End)
                .css_classes(["heading"])
                .build(),
        );
        top.append(
            &Image::builder()
                .icon_name("go-next-symbolic")
                .css_classes(["dim-label"])
                .build(),
        );
        top.append(&account_menu_button(account.id));
        content.append(&top);
        content.append(
            &Label::builder()
                .label(&subtitle)
                .halign(Align::Start)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
        row.set_child(Some(&content));
        list.append(&row);
    }
}

fn rebuild_watchlist_list(refs: &UiRefs, items: &[WatchlistItem]) {
    clear_list(&refs.watchlist_list);
    let now = current_unix_timestamp();
    let range = HistoryRange::OneMonth;
    for item in items {
        let row = ListBoxRow::new();
        row.set_selectable(false);
        row.set_activatable(true);
        row.set_tooltip_text(Some("Open stock details"));

        let content = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(5)
            .margin_top(10)
            .margin_bottom(10)
            .margin_start(12)
            .margin_end(8)
            .build();
        let top = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(8)
            .build();
        top.append(&stock_avatar(&item.provider_symbol, &item.code, 32));
        top.append(
            &Label::builder()
                .label(&item.code)
                .halign(Align::Start)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["heading"])
                .build(),
        );
        top.append(
            &Label::builder()
                .label(
                    &item
                        .last_price
                        .map(|price| format_currency(price, &item.currency))
                        .unwrap_or_else(|| "—".into()),
                )
                .halign(Align::End)
                .css_classes(["heading"])
                .build(),
        );
        top.append(
            &Image::builder()
                .icon_name("go-next-symbolic")
                .css_classes(["dim-label"])
                .build(),
        );
        top.append(&watchlist_menu_button(item.id));
        content.append(&top);

        content.append(
            &Label::builder()
                .label(&item.name)
                .halign(Align::Start)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["dim-label"])
                .build(),
        );

        let details = WrapBox::new();
        details.set_child_spacing(12);
        details.set_line_spacing(2);
        details.set_natural_line_length(460);
        details.append(
            &Label::builder()
                .label(&format!(
                    "{} · {}",
                    friendly_exchange(&item.exchange),
                    item.currency
                ))
                .halign(Align::Start)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
        if let Some(change) = item.day_change_percent {
            let label = Label::builder()
                .label(&format!("{change:+.2}% today"))
                .halign(Align::Start)
                .css_classes(["caption"])
                .build();
            set_gain_class(&label, change);
            details.append(&label);
        }
        details.append(&quote_health_label(item.last_price, item.quote_updated_at));
        content.append(&details);

        let sparkline = Sparkline::new();
        let points = market_data::display_history_points(
            refs.state
                .database
                .history_points(
                    &item.provider_symbol,
                    range.interval(),
                    range.minimum_timestamp(now),
                )
                .unwrap_or_default(),
            range,
        );
        sparkline.set_points(points);
        content.append(sparkline.widget());

        row.set_child(Some(&content));
        refs.watchlist_list.append(&row);
    }
}

fn clear_list(list: &ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}


fn quote_health_text(last_price: Option<f64>, quote_updated_at: Option<i64>) -> String {
    let Some(_) = last_price else {
        return "Quote unavailable".into();
    };
    let Some(updated_at) = quote_updated_at else {
        return "Cached quote · time unknown".into();
    };
    let now = current_unix_timestamp();
    let state = market_data::quote_state_label(None, updated_at, now);
    format!("{} · {}", state, relative_time(updated_at))
}

fn quote_health_css_class(text: &str) -> &'static str {
    if text.starts_with("Live") || text.starts_with("Current") {
        "success"
    } else if text.starts_with("Stale") || text.starts_with("Network") || text.starts_with("Quote unavailable") {
        "warning"
    } else {
        "dim-label"
    }
}

fn quote_health_label(last_price: Option<f64>, quote_updated_at: Option<i64>) -> Label {
    let text = quote_health_text(last_price, quote_updated_at);
    let label = Label::builder()
        .label(&text)
        .halign(Align::Start)
        .css_classes(["caption", quote_health_css_class(&text)])
        .build();
    label
}

fn set_quote_status(label: &Label, text: &str) {
    label.remove_css_class("dim-label");
    label.remove_css_class("success");
    label.remove_css_class("warning");
    label.set_label(text);
    label.add_css_class(quote_health_css_class(text));
}

fn position_row(
    position: &Position,
    base: &str,
    usd_cad: Option<f64>,
    show_menu: bool,
) -> ListBoxRow {
    let row = ListBoxRow::new();
    row.set_selectable(false);
    row.set_activatable(true);
    row.set_widget_name(&format!("position-{}", position.id));
    row.set_tooltip_text(Some("Open stock details"));

    let content = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(4)
        .margin_top(10)
        .margin_bottom(10)
        .margin_start(12)
        .margin_end(8)
        .build();

    let top = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .build();
    top.append(&stock_avatar(&position.provider_symbol, &position.code, 32));
    let symbol = Label::builder()
        .label(&position.code)
        .halign(Align::Start)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["heading"])
        .build();
    top.append(&symbol);

    let value = converted_market_value(position, base, usd_cad)
        .map(|value| format_currency(value, base))
        .or_else(|| position.market_value().map(|value| format_currency(value, &position.currency)))
        .unwrap_or_else(|| "—".into());
    top.append(
        &Label::builder()
            .label(&value)
            .halign(Align::End)
            .css_classes(["heading"])
            .build(),
    );
    top.append(
        &Image::builder()
            .icon_name("go-next-symbolic")
            .css_classes(["dim-label"])
            .build(),
    );
    if show_menu {
        top.append(&position_menu_button(position.id));
    }
    content.append(&top);

    let subtitle = format!(
        "{} · {} · {} · {}",
        position.name,
        friendly_exchange(&position.exchange),
        shares_text(position.shares),
        position.account_name
    );
    content.append(
        &Label::builder()
            .label(&subtitle)
            .halign(Align::Start)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label", "caption"])
            .build(),
    );

    let details = WrapBox::new();
    details.set_child_spacing(12);
    details.set_line_spacing(2);
    details.set_natural_line_length(460);

    let gain_label = match converted_total_gain(position, base, usd_cad) {
        Some(gain) => {
            let label = Label::builder()
                .label(&format!(
                    "{} ({:+.2}%) total",
                    format_signed_currency(gain, base),
                    position.total_return_percent().unwrap_or(0.0)
                ))
                .halign(Align::Start)
                .css_classes(["caption"])
                .build();
            set_gain_class(&label, gain);
            label
        }
        None => Label::builder()
            .label("Total return unavailable")
            .halign(Align::Start)
            .css_classes(["dim-label", "caption"])
            .build(),
    };
    details.append(&gain_label);

    if let Some(day_change) = position.day_change_percent {
        let day = Label::builder()
            .label(&format!("{day_change:+.2}% today"))
            .halign(Align::Start)
            .css_classes(["caption"])
            .build();
        set_gain_class(&day, day_change);
        details.append(&day);
    }

    details.append(&quote_health_label(position.last_price, position.quote_updated_at));

    if position.currency != base {
        if let Some(native_value) = position.market_value() {
            details.append(
                &Label::builder()
                    .label(&format!(
                        "{} native",
                        format_currency(native_value, &position.currency)
                    ))
                    .halign(Align::Start)
                    .css_classes(["dim-label", "caption"])
                    .build(),
            );
        }
    }

    content.append(&details);
    row.set_child(Some(&content));
    row
}

fn position_menu_button(position_id: i64) -> MenuButton {
    let menu = gio::Menu::new();
    menu.append_item(&targeted_menu_item("Activity", "win.position-activity", position_id));
    menu.append_item(&targeted_menu_item("Remove Holding", "win.remove-position", position_id));
    MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text("Position Menu")
        .css_classes(["flat", "circular"])
        .menu_model(&menu)
        .valign(Align::Center)
        .build()
}

fn account_menu_button(account_id: i64) -> MenuButton {
    let menu = gio::Menu::new();
    menu.append_item(&targeted_menu_item("Add Cash", "win.add-cash", account_id));
    menu.append_item(&targeted_menu_item("Add Activity", "win.add-activity-account", account_id));
    menu.append_item(&targeted_menu_item("Transfer", "win.transfer-account", account_id));
    menu.append_item(&targeted_menu_item("Edit Account", "win.edit-account", account_id));
    menu.append_item(&targeted_menu_item("Remove Account", "win.remove-account", account_id));
    MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text("Account Menu")
        .css_classes(["flat", "circular"])
        .menu_model(&menu)
        .valign(Align::Center)
        .build()
}

fn watchlist_item_id_for_symbol(refs: &UiRefs, provider_symbol: &str) -> Option<i64> {
    refs.state
        .database
        .load_watchlist()
        .ok()?
        .into_iter()
        .find(|item| item.provider_symbol.eq_ignore_ascii_case(provider_symbol))
        .map(|item| item.id)
}

fn set_watchlist_star_state(button: &Button, active: bool) {
    button.set_icon_name(if active {
        "starred-symbolic"
    } else {
        "non-starred-symbolic"
    });
    button.set_tooltip_text(Some(if active {
        "Remove from Watchlist"
    } else {
        "Add to Watchlist"
    }));
}

fn watchlist_star_button(refs: &UiRefs, asset: &SearchResult) -> Button {
    let current_id = Rc::new(Cell::new(watchlist_item_id_for_symbol(
        refs,
        &asset.provider_symbol,
    )));
    let button = Button::builder()
        .css_classes(["flat", "circular"])
        .valign(Align::Center)
        .build();
    set_watchlist_star_state(&button, current_id.get().is_some());

    let refs = refs.clone();
    let asset = asset.clone();
    let current_id_for_click = current_id.clone();
    let button_for_click = button.clone();
    button.connect_clicked(move |_| {
        if let Some(item_id) = current_id_for_click.get() {
            match refs.state.database.delete_watchlist_item(item_id) {
                Ok(true) => {
                    current_id_for_click.set(None);
                    set_watchlist_star_state(&button_for_click, false);
                    refs.refresh();
                    refs.toast_overlay.add_toast(Toast::new(&format!(
                        "Removed {} from watchlist",
                        asset.code
                    )));
                }
                Ok(false) => {
                    current_id_for_click.set(None);
                    set_watchlist_star_state(&button_for_click, false);
                    refs.refresh();
                }
                Err(error) => refs
                    .toast_overlay
                    .add_toast(Toast::new(&format!("Could not update watchlist: {error}"))),
            }
            return;
        }

        let item = NewWatchlistItem {
            code: asset.code.clone(),
            exchange: asset.exchange.clone(),
            provider_symbol: asset.provider_symbol.clone(),
            name: asset.name.clone(),
            asset_type: asset.asset_type.clone(),
            currency: asset.currency.clone(),
            last_price: asset.market_price,
        };
        match refs.state.database.add_watchlist_item(&item) {
            Ok(id) => {
                current_id_for_click.set(Some(id));
                set_watchlist_star_state(&button_for_click, true);
                refs.refresh();
                refs.toast_overlay.add_toast(Toast::new(&format!(
                    "Added {} to watchlist",
                    asset.code
                )));
                if let Ok(Some(added)) = refs.state.database.watchlist_item(id) {
                    refresh_watchlist_async(refs.clone(), vec![added], false);
                }
            }
            Err(error) if error.to_string().contains("UNIQUE") => {
                let existing = watchlist_item_id_for_symbol(&refs, &asset.provider_symbol);
                current_id_for_click.set(existing);
                set_watchlist_star_state(&button_for_click, existing.is_some());
            }
            Err(error) => refs
                .toast_overlay
                .add_toast(Toast::new(&format!("Could not add to watchlist: {error}"))),
        }
    });

    button
}

fn watchlist_menu_button(item_id: i64) -> MenuButton {
    let menu = gio::Menu::new();
    menu.append_item(&targeted_menu_item(
        "Remove from Watchlist",
        "win.remove-watchlist",
        item_id,
    ));
    MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text("Watchlist Menu")
        .css_classes(["flat", "circular"])
        .menu_model(&menu)
        .valign(Align::Center)
        .build()
}

fn targeted_menu_item(label: &str, action: &str, id: i64) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), None);
    item.set_action_and_target_value(Some(action), Some(&glib::Variant::from(id.to_string())));
    item
}

fn main_menu_button() -> MenuButton {
    let menu = gio::Menu::new();
    menu.append(Some("Transactions"), Some("win.activity"));
    menu.append(Some("Reports"), Some("win.reports"));
    menu.append(Some("Preferences"), Some("win.preferences"));
    menu.append(Some("Keyboard Shortcuts"), Some("win.shortcuts"));
    menu.append(Some("About Aureus"), Some("win.about"));

    MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Main Menu")
        .menu_model(&menu)
        .build()
}

fn install_window_actions(
    window: &ApplicationWindow,
    state: &AppState,
    refs: &UiRefs,
    pages: &ViewStack,
) {
    let close = gio::SimpleAction::new("close", None);
    {
        let window_weak = window.downgrade();
        close.connect_activate(move |_, _| {
            if let Some(window) = window_weak.upgrade() {
                window.close();
            }
        });
    }
    window.add_action(&close);

    let search = gio::SimpleAction::new("search", None);
    {
        let pages = pages.clone();
        let navigation = refs.navigation.clone();
        let search_entry = refs.search_entry.clone();
        search.connect_activate(move |_, _| {
            let _ = navigation.pop_to_tag("portfolio-root");
            pages.set_visible_child_name("search");
            let search_entry = search_entry.clone();
            glib::idle_add_local_once(move || {
                search_entry.grab_focus();
            });
        });
    }
    window.add_action(&search);

    let about = gio::SimpleAction::new("about", None);
    {
        let window_weak = window.downgrade();
        about.connect_activate(move |_, _| {
            let Some(window) = window_weak.upgrade() else {
                return;
            };
            let dialog = AboutDialog::builder()
                .application_name("Aureus")
                .application_icon(crate::APP_ID)
                .developer_name("Mars7x")
                .version(crate::APP_VERSION)
                .comments("A clean portfolio tracker that just works")
                .build();
            dialog.add_credit_section(
                Some("Data"),
                &[
                    "Market data: Yahoo Finance",
                    "Exchange rates: Bank of Canada",
                ],
            );
            dialog.present(Some(&window));
        });
    }
    window.add_action(&about);

    let preferences = gio::SimpleAction::new("preferences", None);
    {
        let window_weak = window.downgrade();
        let refs = refs.clone();
        preferences.connect_activate(move |_, _| {
            if let Some(parent) = window_weak.upgrade() {
                present_preferences_dialog(&parent, refs.clone());
            }
        });
    }
    window.add_action(&preferences);

    let shortcuts = gio::SimpleAction::new("shortcuts", None);
    {
        let window_weak = window.downgrade();
        shortcuts.connect_activate(move |_, _| {
            let Some(window) = window_weak.upgrade() else {
                return;
            };

            let section = adw::ShortcutsSection::new(Some("General"));
            section.add(adw::ShortcutsItem::from_action(
                "Refresh",
                "win.refresh-current",
            ));
            section.add(adw::ShortcutsItem::from_action("Search", "win.search"));
            section.add(adw::ShortcutsItem::from_action("Close Window", "win.close"));
            section.add(adw::ShortcutsItem::from_action("Quit", "app.quit"));

            let dialog = adw::ShortcutsDialog::new();
            dialog.add(section);
            dialog.present(Some(&window));
        });
    }
    window.add_action(&shortcuts);

    let activity = gio::SimpleAction::new("activity", None);
    {
        let window_weak = window.downgrade();
        let refs = refs.clone();
        activity.connect_activate(move |_, _| {
            if let Some(parent) = window_weak.upgrade() {
                present_transactions_dialog(&parent, refs.clone());
            }
        });
    }
    window.add_action(&activity);

    let reports = gio::SimpleAction::new("reports", None);
    {
        let window_weak = window.downgrade();
        let refs = refs.clone();
        reports.connect_activate(move |_, _| {
            if let Some(parent) = window_weak.upgrade() {
                present_reports_dialog(&parent, refs.clone());
            }
        });
    }
    window.add_action(&reports);

    let position_activity = gio::SimpleAction::new("position-activity", Some(glib::VariantTy::STRING));
    {
        let window_weak = window.downgrade();
        let refs = refs.clone();
        position_activity.connect_activate(move |_, parameter| {
            let Some(parent) = window_weak.upgrade() else {
                return;
            };
            let Some(position_id) = variant_id(parameter) else {
                return;
            };
            let Ok(Some(position)) = refs.state.database.position(position_id) else {
                return;
            };
            let filter = format!("{} {}", position.code, position.account_name);
            present_transactions_dialog_with_filter(&parent, refs.clone(), Some(&filter));
        });
    }
    window.add_action(&position_activity);

    let remove_position = gio::SimpleAction::new("remove-position", Some(glib::VariantTy::STRING));
    {
        let window_weak = window.downgrade();
        let database = state.database.clone();
        let refs = refs.clone();
        remove_position.connect_activate(move |_, parameter| {
            let Some(parent) = window_weak.upgrade() else {
                return;
            };
            let Some(position_id) = variant_id(parameter) else {
                return;
            };
            let Ok(Some(position)) = database.position(position_id) else {
                return;
            };

            let dialog = AlertDialog::builder()
                .heading(format!("Remove {}?", position.code))
                .body(format!(
                    "Removes this holding and its activity history from {}",
                    position.account_name
                ))
                .build();
            dialog.add_response("cancel", "Cancel");
            dialog.add_response("remove", "Remove");
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");
            dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);

            let database = database.clone();
            let refs = refs.clone();
            let removed_code = position.code.clone();
            let account_id = position.account_id;
            let provider_symbol = position.provider_symbol.clone();
            dialog.connect_response(Some("remove"), move |_, _| {
                match database.delete_activity_for_holding(account_id, &provider_symbol) {
                    Ok(changed) if changed > 0 => {
                        let _ = refs.state.database.sync_paid_dividends_to_cash();
                        refs.refresh();
                        refresh_portfolio_history_async(refs.clone(), false);
                        refs.toast_overlay
                            .add_toast(Toast::new(&format!("Removed {removed_code}")));
                    }
                    Ok(_) => {}
                    Err(error) => refs
                        .toast_overlay
                        .add_toast(Toast::new(&format!("Could not remove holding: {error}"))),
                }
            });
            dialog.present(Some(&parent));
        });
    }
    window.add_action(&remove_position);

    let edit_account = gio::SimpleAction::new("edit-account", Some(glib::VariantTy::STRING));
    {
        let window_weak = window.downgrade();
        let refs = refs.clone();
        edit_account.connect_activate(move |_, parameter| {
            let Some(parent) = window_weak.upgrade() else {
                return;
            };
            let Some(account_id) = variant_id(parameter) else {
                return;
            };
            let account = refs
                .state
                .database
                .load_accounts()
                .ok()
                .and_then(|accounts| accounts.into_iter().find(|account| account.id == account_id));
            if let Some(account) = account {
                present_edit_account_dialog(&parent, refs.clone(), account);
            }
        });
    }
    window.add_action(&edit_account);

    let add_cash = gio::SimpleAction::new("add-cash", Some(glib::VariantTy::STRING));
    {
        let window_weak = window.downgrade();
        let refs = refs.clone();
        add_cash.connect_activate(move |_, parameter| {
            let Some(parent) = window_weak.upgrade() else {
                return;
            };
            let Some(account_id) = variant_id(parameter) else {
                return;
            };
            let account = refs
                .state
                .database
                .load_accounts()
                .ok()
                .and_then(|accounts| accounts.into_iter().find(|account| account.id == account_id));
            if let Some(account) = account {
                present_add_cash_dialog(&parent, refs.clone(), account);
            }
        });
    }
    window.add_action(&add_cash);

    let transfer_account = gio::SimpleAction::new("transfer-account", Some(glib::VariantTy::STRING));
    {
        let window_weak = window.downgrade();
        let refs = refs.clone();
        transfer_account.connect_activate(move |_, parameter| {
            let Some(parent) = window_weak.upgrade() else {
                return;
            };
            let Some(account_id) = variant_id(parameter) else {
                return;
            };
            let account = refs
                .state
                .database
                .load_accounts()
                .ok()
                .and_then(|accounts| accounts.into_iter().find(|account| account.id == account_id));
            if let Some(account) = account {
                present_transfer_dialog(&parent, refs.clone(), account);
            }
        });
    }
    window.add_action(&transfer_account);

    let add_activity_account =
        gio::SimpleAction::new("add-activity-account", Some(glib::VariantTy::STRING));
    {
        let window_weak = window.downgrade();
        let refs = refs.clone();
        add_activity_account.connect_activate(move |_, parameter| {
            let Some(parent) = window_weak.upgrade() else {
                return;
            };
            let Some(account_id) = variant_id(parameter) else {
                return;
            };
            present_add_activity_for_account(&parent, refs.clone(), account_id);
        });
    }
    window.add_action(&add_activity_account);

    let remove_account = gio::SimpleAction::new("remove-account", Some(glib::VariantTy::STRING));
    {
        let window_weak = window.downgrade();
        let refs = refs.clone();
        remove_account.connect_activate(move |_, parameter| {
            let Some(parent) = window_weak.upgrade() else {
                return;
            };
            let Some(account_id) = variant_id(parameter) else {
                return;
            };
            let accounts = refs.state.database.load_accounts().unwrap_or_default();
            let Some(account) = accounts.iter().find(|account| account.id == account_id).cloned() else {
                return;
            };
            let position_count = refs
                .state
                .database
                .account_position_count(account_id)
                .unwrap_or(0);
            if accounts.len() <= 1 {
                let dialog = AlertDialog::builder()
                    .heading("Keep at least one account")
                    .body("Create another account before removing this one")
                    .build();
                dialog.add_response("close", "Close");
                dialog.present(Some(&parent));
                return;
            }

            let warning = if position_count > 0 {
                format!(
                    "This permanently removes the account, its {} holding{}, and all activity and cash history. This cannot be undone.",
                    position_count,
                    if position_count == 1 { "" } else { "s" }
                )
            } else {
                "This permanently removes the account and all activity and cash history. This cannot be undone."
                    .to_string()
            };
            let dialog = AlertDialog::builder()
                .heading(format!("Remove {}?", account.name))
                .body(warning)
                .build();
            dialog.add_response("cancel", "Cancel");
            dialog.add_response("remove", "Remove Account");
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");
            dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
            let refs = refs.clone();
            dialog.connect_response(Some("remove"), move |_, _| {
                match refs.state.database.delete_account(account_id) {
                    Ok(true) => {
                        refs.refresh();
                        refresh_portfolio_history_async(refs.clone(), false);
                        refs.toast_overlay.add_toast(Toast::new("Account removed"));
                    }
                    Ok(false) => {}
                    Err(error) => refs
                        .toast_overlay
                        .add_toast(Toast::new(&format!("Could not remove account: {error}"))),
                }
            });
            dialog.present(Some(&parent));
        });
    }
    window.add_action(&remove_account);

    let remove_watchlist = gio::SimpleAction::new("remove-watchlist", Some(glib::VariantTy::STRING));
    {
        let database = state.database.clone();
        let refs = refs.clone();
        remove_watchlist.connect_activate(move |_, parameter| {
            let Some(item_id) = variant_id(parameter) else {
                return;
            };
            let Ok(Some(item)) = database.watchlist_item(item_id) else {
                return;
            };
            match database.delete_watchlist_item(item_id) {
                Ok(true) => {
                    // If removal was triggered from a watchlist detail page, return to the
                    // root Watchlist view. On the root page pop() is simply a no-op.
                    let _ = refs.navigation.pop();
                    refs.refresh();
                    refs.toast_overlay
                        .add_toast(Toast::new(&format!("Removed {} from watchlist", item.code)));
                }
                Ok(false) => {}
                Err(error) => refs
                    .toast_overlay
                    .add_toast(Toast::new(&format!("Could not update watchlist: {error}"))),
            }
        });
    }
    window.add_action(&remove_watchlist);

    let add_account = gio::SimpleAction::new("add-account", None);
    {
        let window_weak = window.downgrade();
        let refs = refs.clone();
        add_account.connect_activate(move |_, _| {
            let Some(window) = window_weak.upgrade() else {
                return;
            };
            present_add_account_dialog(&window, refs.clone());
        });
    }
    window.add_action(&add_account);

    let refresh_current = gio::SimpleAction::new("refresh-current", None);
    {
        let refs = refs.clone();
        let pages = pages.clone();
        refresh_current.connect_activate(move |_, _| {
            // A pushed stock page owns its own header progress line and refresh
            // callback. Keep Ctrl+R contextual instead of refreshing the hidden
            // root tab behind it.
            if refs.navigation.navigation_stack().n_items() > 1 {
                if let Some(refresh) = refs.detail_refresh.borrow().clone() {
                    refresh();
                }
                return;
            }

            if !refs.pull_refresh_active.get() {
                if refs.shortcut_refresh_active.get() {
                    return;
                }
                begin_shortcut_refresh(&refs);
            }
            match pages.visible_child_name().as_deref() {
                Some("accounts") => {
                    let positions = refs.state.database.load_positions().unwrap_or_default();
                    let fetch_fx = portfolio_needs_fx_with_cash(
                        &refs.state,
                        &positions,
                        &base_currency(&refs.state),
                    );
                    refresh_market_async(refs.clone(), positions, fetch_fx, true);
                }
                Some("dividends") => {
                    let positions = refs.state.database.load_positions().unwrap_or_default();
                    refresh_dividends_async(refs.clone(), positions, true);
                }
                Some("search") => {
                    finish_refresh_feedback(&refs);
                }
                Some("watchlist") => {
                    let items = refs.state.database.load_watchlist().unwrap_or_default();
                    refresh_watchlist_async(refs.clone(), items, true);
                }
                _ => {
                    let positions = refs.state.database.load_positions().unwrap_or_default();
                    let fetch_fx = portfolio_needs_fx_with_cash(&refs.state, &positions, &base_currency(&refs.state));
                    refresh_market_async(refs.clone(), positions.clone(), fetch_fx, true);
                    if pages.visible_child_name().as_deref() == Some("overview") {
                        refresh_dividends_async(refs.clone(), positions, false);
                        refresh_portfolio_history_async(refs.clone(), false);
                    }
                }
            }
        });
    }
    window.add_action(&refresh_current);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReportKind {
    Portfolio,
    Dividends,
}

impl ReportKind {
    fn title(self) -> &'static str {
        match self {
            Self::Portfolio => "Portfolio Performance",
            Self::Dividends => "Dividend Income",
        }
    }

    fn file_stem(self) -> &'static str {
        match self {
            Self::Portfolio => "portfolio-performance",
            Self::Dividends => "dividend-income",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReportPeriod {
    Ytd,
    Year(i32),
}

impl ReportPeriod {
    fn label(self) -> String {
        match self {
            Self::Ytd => "Year to Date (YTD)".into(),
            Self::Year(year) => year.to_string(),
        }
    }

    fn filename_token(self) -> String {
        match self {
            Self::Ytd => {
                let (year, _, _) = civil_from_days(current_unix_timestamp().div_euclid(86_400));
                format!("{year}-ytd")
            }
            Self::Year(year) => year.to_string(),
        }
    }

    fn bounds(self) -> (i64, i64) {
        match self {
            Self::Ytd => {
                let now = current_unix_timestamp();
                let (year, _, _) = civil_from_days(now.div_euclid(86_400));
                (days_from_civil(year, 1, 1) * 86_400, now)
            }
            Self::Year(year) => {
                let start = days_from_civil(year, 1, 1) * 86_400;
                let end = days_from_civil(year + 1, 1, 1) * 86_400 - 1;
                (start, end)
            }
        }
    }

    fn date_range(self) -> String {
        let (start, end) = self.bounds();
        format!(
            "{} - {}",
            format_distribution_date(start),
            format_distribution_date(end.min(current_unix_timestamp()))
        )
    }
}

fn report_periods_for_account(refs: &UiRefs, account_id: i64) -> Vec<ReportPeriod> {
    let now = current_unix_timestamp();
    let (current_year, _, _) = civil_from_days(now.div_euclid(86_400));
    let earliest = refs
        .state
        .database
        .load_transactions()
        .unwrap_or_default()
        .into_iter()
        .filter(|transaction| transaction.account_id == account_id)
        .map(|transaction| transaction.timestamp)
        .chain(
            refs.state
                .database
                .load_cash_entries()
                .unwrap_or_default()
                .into_iter()
                .filter(|entry| entry.account_id == account_id)
                .map(|entry| entry.occurred_at),
        )
        .min();

    let earliest_year = earliest
        .map(|timestamp| civil_from_days(timestamp.div_euclid(86_400)).0)
        .unwrap_or(current_year);
    let mut periods = vec![ReportPeriod::Ytd];
    if earliest_year < current_year {
        for year in (earliest_year..current_year).rev() {
            periods.push(ReportPeriod::Year(year));
        }
    }
    periods
}

fn set_report_period_model(row: &ComboRow, periods: &[ReportPeriod]) {
    let labels = periods.iter().map(|period| period.label()).collect::<Vec<_>>();
    let model = StringList::new(&[]);
    for label in labels {
        model.append(&label);
    }
    row.set_model(Some(&model));
    row.set_selected(0);
}

fn present_reports_dialog(parent: &ApplicationWindow, refs: UiRefs) {
    let accounts = refs.state.database.load_accounts().unwrap_or_default();
    if accounts.is_empty() {
        refs.toast_overlay
            .add_toast(Toast::new("Create an account before exporting a report"));
        return;
    }

    let report_type = ComboRow::new();
    report_type.set_title("Report");
    report_type.set_model(Some(&string_model(&[
        "Portfolio Performance",
        "Dividend Income",
    ])));
    report_type.set_selected(0);

    let account = ComboRow::new();
    account.set_title("Account");
    let account_model = StringList::new(&[]);
    for item in &accounts {
        account_model.append(&format!("{} · {}", item.name, item.currency));
    }
    account.set_model(Some(&account_model));
    let preferred_account = refs
        .state
        .database
        .setting(LAST_ACCOUNT_ID_KEY)
        .ok()
        .flatten()
        .and_then(|value| value.parse::<i64>().ok())
        .and_then(|id| accounts.iter().position(|item| item.id == id))
        .unwrap_or(0);
    account.set_selected(preferred_account as u32);

    let period = ComboRow::new();
    period.set_title("Reporting Period");
    period.set_subtitle("YTD or a completed calendar year");
    let initial_periods = report_periods_for_account(&refs, accounts[preferred_account].id);
    set_report_period_model(&period, &initial_periods);
    let periods = Rc::new(RefCell::new(initial_periods));

    {
        let refs = refs.clone();
        let accounts = accounts.clone();
        let period = period.clone();
        let periods = periods.clone();
        account.connect_selected_notify(move |row| {
            let Some(selected) = accounts.get(row.selected() as usize) else {
                return;
            };
            let next = report_periods_for_account(&refs, selected.id);
            set_report_period_model(&period, &next);
            *periods.borrow_mut() = next;
        });
    }

    let group = PreferencesGroup::builder().title("Export Report").build();
    group.add(&report_type);
    group.add(&account);
    group.add(&period);

    let export = Button::builder()
        .label("Export PDF")
        .css_classes(["suggested-action", "pill"])
        .halign(Align::Fill)
        .hexpand(true)
        .build();

    let body = dialog_body();
    body.append(&group);
    let scroller = dialog_scroller(&body, 560);
    scroller.set_vexpand(true);
    let actions = dialog_bottom_action(&export);
    let page = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(8)
        .build();
    page.append(&scroller);
    page.append(&actions);

    let header = HeaderBar::new();
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    let dialog = Dialog::builder()
        .title("Reports")
        .content_width(560)
        .content_height(410)
        .child(&toolbar)
        .build();
    install_escape_to_close(&dialog);

    {
        let parent = parent.clone();
        let refs = refs.clone();
        let accounts = accounts.clone();
        let report_type = report_type.clone();
        let account = account.clone();
        let period = period.clone();
        let periods = periods.clone();
        let dialog = dialog.clone();
        export.connect_clicked(move |button| {
            let Some(selected_account) = accounts.get(account.selected() as usize).cloned() else {
                return;
            };
            let Some(selected_period) = periods
                .borrow()
                .get(period.selected() as usize)
                .copied()
            else {
                return;
            };
            let kind = if report_type.selected() == 0 {
                ReportKind::Portfolio
            } else {
                ReportKind::Dividends
            };
            button.set_sensitive(false);
            button.set_label("Preparing…");
            prepare_report_export(
                &parent,
                refs.clone(),
                dialog.clone(),
                button.clone(),
                kind,
                selected_account,
                selected_period,
            );
        });
    }

    dialog.present(Some(parent));
}

struct ReportPreparationResult {
    histories: HashMap<String, History>,
    dividend_histories: HashMap<String, DividendHistory>,
    fx_history: Option<History>,
    failures: usize,
}

fn prepare_report_export(
    parent: &ApplicationWindow,
    refs: UiRefs,
    dialog: Dialog,
    export_button: Button,
    kind: ReportKind,
    account: Account,
    period: ReportPeriod,
) {
    // The polling callback must be 'static, so keep our own strong window
    // reference instead of capturing the borrowed dialog parent.
    let parent = parent.clone();
    let transactions = refs
        .state
        .database
        .load_transactions()
        .unwrap_or_default()
        .into_iter()
        .filter(|transaction| transaction.account_id == account.id)
        .collect::<Vec<_>>();
    let mut symbols = transactions
        .iter()
        .map(|transaction| transaction.provider_symbol.trim().to_ascii_uppercase())
        .filter(|symbol| !symbol.is_empty())
        .collect::<Vec<_>>();
    symbols.sort();
    symbols.dedup();
    let needs_fx = transactions
        .iter()
        .any(|transaction| !transaction.currency.eq_ignore_ascii_case(&account.currency));
    let (period_start, period_end) = period.bounds();

    let (sender, receiver) = mpsc::channel::<ReportPreparationResult>();
    std::thread::spawn(move || {
        let mut histories = HashMap::new();
        let mut dividend_histories = HashMap::new();
        let mut failures = 0usize;
        for symbol in symbols {
            if kind == ReportKind::Portfolio {
                match market_data::daily_history_between(&symbol, period_start, period_end) {
                    Ok(history) => {
                        histories.insert(symbol.clone(), history);
                    }
                    Err(_) => failures += 1,
                }
            }
            match market_data::dividends(&symbol) {
                Ok(history) => {
                    dividend_histories.insert(symbol, history);
                }
                Err(_) => failures += 1,
            }
        }
        let fx_history = if needs_fx {
            match market_data::daily_history_between("CAD=X", period_start, period_end) {
                Ok(history) => Some(history),
                Err(_) => {
                    failures += 1;
                    None
                }
            }
        } else {
            None
        };
        let _ = sender.send(ReportPreparationResult {
            histories,
            dividend_histories,
            fx_history,
            failures,
        });
    });

    let receiver = Rc::new(RefCell::new(receiver));
    glib::timeout_add_local(Duration::from_millis(75), move || {
        let Ok(result) = receiver.borrow().try_recv() else {
            return glib::ControlFlow::Continue;
        };

        export_button.set_sensitive(true);
        export_button.set_label("Export PDF");
        for (symbol, history) in &result.dividend_histories {
            let currency = history
                .currency
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("N/A");
            let _ = refs
                .state
                .database
                .replace_dividend_events(symbol, currency, &history.events);
            let _ = refs
                .state
                .database
                .replace_split_events(symbol, &history.splits);
            if let Some(calendar) = &history.calendar {
                let _ = refs.state.database.set_dividend_calendar(
                    symbol,
                    calendar.ex_dividend_date,
                    calendar.payment_date,
                );
            }
            let _ = refs.state.database.set_dividends_fetched(symbol);
        }

        let report_result = match kind {
            ReportKind::Portfolio => build_portfolio_report(
                &refs,
                &account,
                period,
                &result.histories,
                result.fx_history.as_ref().map(|history| history.points.as_slice()),
            )
            .map(PreparedReport::Portfolio),
            ReportKind::Dividends => build_dividend_report(
                &refs,
                &account,
                period,
                result.fx_history.as_ref().map(|history| history.points.as_slice()),
            )
            .map(PreparedReport::Dividends),
        };

        let report = match report_result {
            Ok(report) => report,
            Err(error) => {
                refs.toast_overlay
                    .add_toast(Toast::new(&format!("Could not prepare report: {error}")));
                return glib::ControlFlow::Break;
            }
        };
        dialog.close();
        if result.failures > 0 {
            refs.toast_overlay.add_toast(Toast::new(
                "Some historical market data was unavailable; the report uses available records",
            ));
        }
        present_report_file_dialog(&parent, refs.clone(), kind, &account, period, report);
        glib::ControlFlow::Break
    });
}

enum PreparedReport {
    Portfolio(crate::report::PortfolioReport),
    Dividends(crate::report::DividendReport),
}

fn present_report_file_dialog(
    parent: &ApplicationWindow,
    refs: UiRefs,
    kind: ReportKind,
    account: &Account,
    period: ReportPeriod,
    report: PreparedReport,
) {
    let dialog = FileDialog::new();
    dialog.set_title(&format!("Export {} Report", kind.title()));
    dialog.set_accept_label(Some("Export"));
    dialog.set_initial_name(Some(&format!(
        "aureus-{}-{}-{}.pdf",
        kind.file_stem(),
        report_filename_slug(&account.name),
        period.filename_token(),
    )));
    dialog.save(
        Some(parent),
        None::<&gio::Cancellable>,
        move |result| {
            let Ok(file) = result else {
                return;
            };
            let Some(path) = file.path() else {
                refs.toast_overlay.add_toast(Toast::new(
                    "The selected destination is not writable by Aureus",
                ));
                return;
            };
            let written = match &report {
                PreparedReport::Portfolio(report) => crate::report::write_portfolio_pdf(&path, report),
                PreparedReport::Dividends(report) => crate::report::write_dividend_pdf(&path, report),
            };
            match written {
                Ok(()) => refs.toast_overlay.add_toast(Toast::new("Report exported")),
                Err(error) => refs
                    .toast_overlay
                    .add_toast(Toast::new(&format!("Could not export report: {error}"))),
            }
        },
    );
}

#[derive(Clone, Debug, Default)]
struct ReportHoldingState {
    code: String,
    name: String,
    provider_symbol: String,
    currency: String,
    shares: f64,
    cost_basis: f64,
}

fn report_holding_states_as_of(
    transactions: &[Transaction],
    split_events: &[SplitEvent],
    end_timestamp: i64,
) -> HashMap<String, ReportHoldingState> {
    #[derive(Clone)]
    enum Event {
        Transaction(Transaction),
        Split(SplitEvent),
    }
    let mut events = transactions
        .iter()
        .filter(|transaction| transaction.timestamp <= end_timestamp)
        .cloned()
        .map(Event::Transaction)
        .chain(
            split_events
                .iter()
                .filter(|split| split.timestamp <= end_timestamp)
                .cloned()
                .map(Event::Split),
        )
        .collect::<Vec<_>>();
    events.sort_by_key(|event| match event {
        Event::Split(split) => (split.timestamp, activity_sort_priority("SPLIT"), i64::MIN),
        Event::Transaction(transaction) => (
            transaction.timestamp,
            activity_sort_priority(&transaction.transaction_type),
            transaction.id,
        ),
    });

    let mut states = HashMap::<String, ReportHoldingState>::new();
    for event in events {
        match event {
            Event::Split(split) => {
                let symbol = split.provider_symbol.to_ascii_uppercase();
                if let Some(state) = states.get_mut(&symbol) {
                    if state.shares.abs() > 0.0000001 {
                        state.shares *= split.ratio;
                    }
                }
            }
            Event::Transaction(transaction) => {
                let symbol = transaction.provider_symbol.to_ascii_uppercase();
                let state = states.entry(symbol.clone()).or_insert_with(|| ReportHoldingState {
                    code: transaction.code.clone(),
                    name: transaction.name.clone(),
                    provider_symbol: symbol,
                    currency: transaction.currency.clone(),
                    ..Default::default()
                });
                state.code = transaction.code.clone();
                state.name = transaction.name.clone();
                state.currency = transaction.currency.clone();
                match transaction.transaction_type.as_str() {
                    "BUY" | "OPEN" => {
                        state.shares += transaction.shares;
                        state.cost_basis += transaction.shares * transaction.price + transaction.fees;
                    }
                    "TRANSFER_IN" => {
                        state.shares += transaction.shares;
                        state.cost_basis += transaction.shares * transaction.price;
                    }
                    "SELL" | "TRANSFER_OUT" => {
                        let average = if state.shares.abs() < f64::EPSILON {
                            0.0
                        } else {
                            state.cost_basis / state.shares
                        };
                        state.shares -= transaction.shares;
                        state.cost_basis = (state.cost_basis - average * transaction.shares).max(0.0);
                        if state.shares.abs() < 0.0000001 {
                            state.shares = 0.0;
                            state.cost_basis = 0.0;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    states.retain(|_, state| state.shares > 0.0000001);
    states
}

fn report_realized_gain_for_period(
    transactions: &[Transaction],
    split_events: &[SplitEvent],
    start_timestamp: i64,
    end_timestamp: i64,
    target_currency: &str,
    fx_points: Option<&[PricePoint]>,
    current_usd_cad: Option<f64>,
) -> Option<f64> {
    #[derive(Clone, Default)]
    struct LedgerState {
        shares: f64,
        cost_basis: f64,
    }
    #[derive(Clone)]
    enum Event {
        Transaction(Transaction),
        Split(SplitEvent),
    }
    let mut events = transactions
        .iter()
        .filter(|transaction| transaction.timestamp <= end_timestamp)
        .cloned()
        .map(Event::Transaction)
        .chain(
            split_events
                .iter()
                .filter(|split| split.timestamp <= end_timestamp)
                .cloned()
                .map(Event::Split),
        )
        .collect::<Vec<_>>();
    events.sort_by_key(|event| match event {
        Event::Split(split) => (split.timestamp, activity_sort_priority("SPLIT"), i64::MIN),
        Event::Transaction(transaction) => (
            transaction.timestamp,
            activity_sort_priority(&transaction.transaction_type),
            transaction.id,
        ),
    });

    let mut ledgers = HashMap::<String, LedgerState>::new();
    let mut realized = 0.0;
    for event in events {
        match event {
            Event::Split(split) => {
                if let Some(state) = ledgers.get_mut(&split.provider_symbol.to_ascii_uppercase()) {
                    state.shares *= split.ratio;
                }
            }
            Event::Transaction(transaction) => {
                let symbol = transaction.provider_symbol.to_ascii_uppercase();
                let state = ledgers.entry(symbol).or_default();
                match transaction.transaction_type.as_str() {
                    "BUY" | "OPEN" => {
                        state.shares += transaction.shares;
                        state.cost_basis += transaction.shares * transaction.price + transaction.fees;
                    }
                    "TRANSFER_IN" => {
                        state.shares += transaction.shares;
                        state.cost_basis += transaction.shares * transaction.price;
                    }
                    "SELL" | "TRANSFER_OUT" => {
                        if state.shares <= 0.0 {
                            continue;
                        }
                        let average = state.cost_basis / state.shares;
                        if transaction.transaction_type == "SELL"
                            && transaction.timestamp >= start_timestamp
                        {
                            let native_gain = transaction.shares * transaction.price
                                - transaction.fees
                                - average * transaction.shares;
                            realized += report_convert_at(
                                native_gain,
                                &transaction.currency,
                                target_currency,
                                fx_points,
                                transaction.timestamp,
                                current_usd_cad,
                            )?;
                        }
                        state.shares -= transaction.shares;
                        state.cost_basis = (state.cost_basis - average * transaction.shares).max(0.0);
                        if state.shares.abs() < 0.0000001 {
                            state.shares = 0.0;
                            state.cost_basis = 0.0;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Some(realized)
}

fn report_shares_held_at(
    transactions: &[Transaction],
    split_events: &[SplitEvent],
    provider_symbol: &str,
    timestamp: i64,
) -> f64 {
    let symbol = provider_symbol.to_ascii_uppercase();
    let mut events = transactions
        .iter()
        .filter(|transaction| {
            transaction.timestamp <= timestamp
                && transaction.provider_symbol.eq_ignore_ascii_case(&symbol)
        })
        .map(|transaction| {
            (
                transaction.timestamp,
                activity_sort_priority(&transaction.transaction_type),
                transaction.id,
                Some(transaction.transaction_type.clone()),
                transaction.shares,
                1.0,
            )
        })
        .chain(
            split_events
                .iter()
                .filter(|split| {
                    split.timestamp <= timestamp
                        && split.provider_symbol.eq_ignore_ascii_case(&symbol)
                })
                .map(|split| (split.timestamp, activity_sort_priority("SPLIT"), i64::MIN, None, 0.0, split.ratio)),
        )
        .collect::<Vec<_>>();
    events.sort_by_key(|event| (event.0, event.1, event.2));
    let mut shares = 0.0;
    for (_, _, _, kind, quantity, ratio) in events {
        match kind.as_deref() {
            None => shares *= ratio,
            Some("SELL") | Some("TRANSFER_OUT") => shares -= quantity,
            Some("BUY") | Some("OPEN") | Some("TRANSFER_IN") => shares += quantity,
            _ => {}
        }
    }
    shares.max(0.0)
}

fn report_convert_at(
    value: f64,
    from_currency: &str,
    to_currency: &str,
    fx_points: Option<&[PricePoint]>,
    timestamp: i64,
    current_usd_cad: Option<f64>,
) -> Option<f64> {
    if from_currency.eq_ignore_ascii_case(to_currency) {
        return Some(value);
    }
    let rate = historical_fx_at(fx_points, timestamp).or(current_usd_cad)?;
    if from_currency.eq_ignore_ascii_case("USD") && to_currency.eq_ignore_ascii_case("CAD") {
        Some(value * rate)
    } else if from_currency.eq_ignore_ascii_case("CAD")
        && to_currency.eq_ignore_ascii_case("USD")
        && rate > 0.0
    {
        Some(value / rate)
    } else {
        None
    }
}

fn build_reconstructed_dividends(
    refs: &UiRefs,
    account: &Account,
    period: ReportPeriod,
    transactions: &[Transaction],
    split_events: &[SplitEvent],
    fx_points: Option<&[PricePoint]>,
    current_usd_cad: Option<f64>,
) -> Vec<(i64, String, f64, f64, String, Option<f64>)> {
    let (start, end) = period.bounds();
    let mut symbols = transactions
        .iter()
        .map(|transaction| transaction.provider_symbol.to_ascii_uppercase())
        .collect::<Vec<_>>();
    symbols.sort();
    symbols.dedup();
    let mut rows = Vec::new();
    for symbol in symbols {
        for event in refs
            .state
            .database
            .dividend_events(&symbol)
            .unwrap_or_default()
            .into_iter()
            .filter(|event| event.timestamp >= start && event.timestamp <= end)
        {
            let shares = report_shares_held_at(transactions, split_events, &symbol, event.timestamp);
            if shares <= 0.0000001 {
                continue;
            }
            let native_gross = shares * event.amount;
            let converted = report_convert_at(
                native_gross,
                &event.currency,
                &account.currency,
                fx_points,
                event.timestamp,
                current_usd_cad,
            );
            rows.push((
                event.timestamp,
                symbol.clone(),
                shares,
                event.amount,
                event.currency.clone(),
                converted,
            ));
        }
    }
    rows.sort_by_key(|row| row.0);
    rows
}

fn build_portfolio_report(
    refs: &UiRefs,
    account: &Account,
    period: ReportPeriod,
    histories: &HashMap<String, History>,
    fx_points: Option<&[PricePoint]>,
) -> Result<crate::report::PortfolioReport, String> {
    let (period_start, period_end) = period.bounds();
    let transactions = refs
        .state
        .database
        .load_transactions()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|transaction| account.id == transaction.account_id)
        .collect::<Vec<_>>();
    let cash_entries = refs
        .state
        .database
        .load_cash_entries()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|entry| account.id == entry.account_id)
        .collect::<Vec<_>>();
    let split_events = refs.state.database.all_split_events().unwrap_or_default();
    let current_usd_cad = refs
        .state
        .database
        .fx_rate(USD_CAD_PAIR)
        .ok()
        .flatten()
        .map(|rate| rate.rate);
    let holdings = report_holding_states_as_of(&transactions, &split_events, period_end);
    let current_positions = refs.state.database.load_positions().unwrap_or_default();

    let mut market_total = 0.0;
    let mut basis_total = 0.0;
    let mut holding_rows = Vec::new();
    let mut complete_market = true;
    let mut complete_basis = true;
    for state in holdings.values() {
        let end_price = histories
            .get(&state.provider_symbol)
            .and_then(|history| historical_close_at(Some(&history.points), period_end))
            .or_else(|| {
                current_positions
                    .iter()
                    .find(|position| {
                        position.account_id == account.id
                            && position.provider_symbol.eq_ignore_ascii_case(&state.provider_symbol)
                    })
                    .and_then(|position| position.last_price)
                    .filter(|_| matches!(period, ReportPeriod::Ytd))
            });
        let converted_basis = report_convert_at(
            state.cost_basis,
            &state.currency,
            &account.currency,
            fx_points,
            period_end,
            current_usd_cad,
        );
        let converted_market = end_price.and_then(|price| {
            report_convert_at(
                state.shares * price,
                &state.currency,
                &account.currency,
                fx_points,
                period_end,
                current_usd_cad,
            )
        });
        match converted_market {
            Some(market) => market_total += market,
            None => complete_market = false,
        }
        match converted_basis {
            Some(basis) => basis_total += basis,
            None => complete_basis = false,
        }
        holding_rows.push((state.clone(), end_price, converted_market, converted_basis));
    }

    let cash_at_end = if matches!(period, ReportPeriod::Ytd) {
        account.cash
    } else {
        cash_entries
            .iter()
            .filter(|entry| entry.occurred_at <= period_end)
            .map(|entry| entry.amount)
            .sum::<f64>()
    };
    let ending_value = complete_market.then_some(market_total + cash_at_end);
    let unrealized = (complete_market && complete_basis).then_some(market_total - basis_total);
    let realized = report_realized_gain_for_period(
        &transactions,
        &split_events,
        period_start,
        period_end,
        &account.currency,
        fx_points,
        current_usd_cad,
    );
    let reconstructed_dividends = build_reconstructed_dividends(
        refs,
        account,
        period,
        &transactions,
        &split_events,
        fx_points,
        current_usd_cad,
    );
    let dividend_income = if reconstructed_dividends.is_empty() {
        Some(0.0)
    } else {
        reconstructed_dividends
            .iter()
            .map(|row| row.5)
            .try_fold(0.0, |total, value| value.map(|value| total + value))
    };

    let metrics = vec![
        crate::report::ReportMetric {
            label: "Ending account value".into(),
            value: report_currency(ending_value, &account.currency),
        },
        crate::report::ReportMetric {
            label: "Securities market value".into(),
            value: report_currency(complete_market.then_some(market_total), &account.currency),
        },
        crate::report::ReportMetric {
            label: "Cash balance".into(),
            value: format_currency(cash_at_end, &account.currency),
        },
        crate::report::ReportMetric {
            label: "Cost basis".into(),
            value: report_currency(complete_basis.then_some(basis_total), &account.currency),
        },
        crate::report::ReportMetric {
            label: "Unrealized gain / loss".into(),
            value: report_signed_currency(unrealized, &account.currency),
        },
        crate::report::ReportMetric {
            label: "Realized gain / loss - period".into(),
            value: report_signed_currency(realized, &account.currency),
        },
        crate::report::ReportMetric {
            label: "Gross dividend income - period".into(),
            value: report_currency(dividend_income, &account.currency),
        },
        crate::report::ReportMetric {
            label: "Securities held".into(),
            value: holdings.len().to_string(),
        },
    ];

    let mut activity = Vec::new();
    let mut push_activity = |label: &str, count: usize, amount: Option<f64>| {
        if count > 0 {
            activity.push(crate::report::PortfolioActivityRow {
                activity: label.into(),
                count,
                amount: report_currency(amount, &account.currency),
            });
        }
    };
    let period_transactions = transactions
        .iter()
        .filter(|transaction| transaction.timestamp >= period_start && transaction.timestamp <= period_end)
        .collect::<Vec<_>>();
    for (kind, label) in [("BUY", "Purchases"), ("SELL", "Sales")] {
        let matching = period_transactions
            .iter()
            .filter(|transaction| transaction.transaction_type == kind)
            .collect::<Vec<_>>();
        let amount = matching.iter().try_fold(0.0, |total, transaction| {
            let native = if kind == "SELL" {
                transaction.shares * transaction.price - transaction.fees
            } else {
                transaction.shares * transaction.price + transaction.fees
            };
            report_convert_at(
                native,
                &transaction.currency,
                &account.currency,
                fx_points,
                transaction.timestamp,
                current_usd_cad,
            )
            .map(|value| total + value)
        });
        push_activity(label, matching.len(), amount);
    }
    let deposits = cash_entries
        .iter()
        .filter(|entry| {
            entry.kind == "DEPOSIT"
                && entry.amount > 0.0
                && entry.occurred_at >= period_start
                && entry.occurred_at <= period_end
        })
        .collect::<Vec<_>>();
    push_activity(
        "Deposits",
        deposits.len(),
        Some(deposits.iter().map(|entry| entry.amount).sum()),
    );
    let withdrawals = cash_entries
        .iter()
        .filter(|entry| {
            entry.kind == "DEPOSIT"
                && entry.amount < 0.0
                && entry.occurred_at >= period_start
                && entry.occurred_at <= period_end
        })
        .collect::<Vec<_>>();
    push_activity(
        "Withdrawals",
        withdrawals.len(),
        Some(withdrawals.iter().map(|entry| entry.amount.abs()).sum()),
    );
    let cash_transfers = cash_entries
        .iter()
        .filter(|entry| {
            entry.kind == "TRANSFER"
                && entry.occurred_at >= period_start
                && entry.occurred_at <= period_end
        })
        .collect::<Vec<_>>();
    push_activity(
        "Cash transfers",
        cash_transfers.len(),
        Some(cash_transfers.iter().map(|entry| entry.amount.abs()).sum()),
    );
    let security_transfers = period_transactions
        .iter()
        .filter(|transaction| matches!(transaction.transaction_type.as_str(), "TRANSFER_IN" | "TRANSFER_OUT"))
        .collect::<Vec<_>>();
    let security_transfer_amount = security_transfers.iter().try_fold(0.0, |total, transaction| {
        report_convert_at(
            transaction.shares * transaction.price,
            &transaction.currency,
            &account.currency,
            fx_points,
            transaction.timestamp,
            current_usd_cad,
        )
        .map(|value| total + value.abs())
    });
    push_activity("Security transfers", security_transfers.len(), security_transfer_amount);
    push_activity(
        "Dividend distributions",
        reconstructed_dividends.len(),
        dividend_income,
    );

    holding_rows.sort_by(|left, right| {
        right
            .2
            .partial_cmp(&left.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.code.cmp(&right.0.code))
    });
    let holdings = holding_rows
        .into_iter()
        .map(|(state, price, market, basis)| {
            let gain = match (market, basis) {
                (Some(market), Some(basis)) => Some(market - basis),
                _ => None,
            };
            crate::report::PortfolioHoldingRow {
                code: state.code,
                name: state.name,
                shares: trim_number(state.shares),
                price: price
                    .map(|price| format_currency(price, &state.currency))
                    .unwrap_or_else(|| "—".into()),
                market_value: report_currency(market, &account.currency),
                cost_basis: report_currency(basis, &account.currency),
                gain: report_signed_currency(gain, &account.currency),
            }
        })
        .collect::<Vec<_>>();

    Ok(crate::report::PortfolioReport {
        generated_on: format_distribution_date(current_unix_timestamp()),
        account_name: account.name.clone(),
        account_currency: account.currency.clone(),
        period_label: period.label(),
        period_dates: period.date_range(),
        metrics,
        activity,
        holdings,
    })
}

fn build_dividend_report(
    refs: &UiRefs,
    account: &Account,
    period: ReportPeriod,
    fx_points: Option<&[PricePoint]>,
) -> Result<crate::report::DividendReport, String> {
    let transactions = refs
        .state
        .database
        .load_transactions()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|transaction| transaction.account_id == account.id)
        .collect::<Vec<_>>();
    let split_events = refs.state.database.all_split_events().unwrap_or_default();
    let current_usd_cad = refs
        .state
        .database
        .fx_rate(USD_CAD_PAIR)
        .ok()
        .flatten()
        .map(|rate| rate.rate);
    let reconstructed = build_reconstructed_dividends(
        refs,
        account,
        period,
        &transactions,
        &split_events,
        fx_points,
        current_usd_cad,
    );

    let payer_count = reconstructed
        .iter()
        .map(|row| row.1.clone())
        .collect::<HashSet<_>>()
        .len();
    let mut by_month = HashMap::<u32, Option<f64>>::new();
    let mut by_stock = HashMap::<String, Option<f64>>::new();
    let mut total = 0.0;
    let mut complete_total = true;
    let mut distributions = Vec::new();
    for (timestamp, symbol, shares, rate, native_currency, converted) in &reconstructed {
        if let Some(value) = converted {
            total += value;
        } else {
            complete_total = false;
        }
        if let Some((_, month)) = timestamp_year_month(*timestamp) {
            let entry = by_month.entry(month).or_insert(Some(0.0));
            *entry = match (*entry, *converted) {
                (Some(current), Some(value)) => Some(current + value),
                _ => None,
            };
        }
        let entry = by_stock.entry(symbol.clone()).or_insert(Some(0.0));
        *entry = match (*entry, *converted) {
            (Some(current), Some(value)) => Some(current + value),
            _ => None,
        };
        distributions.push(crate::report::DividendPaymentRow {
            ex_date: format_distribution_date(*timestamp),
            source: symbol.clone(),
            shares: trim_number(*shares),
            rate: format_currency(*rate, native_currency),
            gross: converted
                .map(|value| format_currency(value, &account.currency))
                .unwrap_or_else(|| format_currency(*shares * *rate, native_currency)),
        });
    }

    let month_count = by_month.len();
    let mut months = by_month.into_iter().collect::<Vec<_>>();
    months.sort_by_key(|item| item.0);
    let months = months
        .into_iter()
        .map(|(month, value)| crate::report::DividendSummaryRow {
            label: month_name(month).into(),
            value: report_currency(value, &account.currency),
        })
        .collect::<Vec<_>>();

    let mut stocks = by_stock.into_iter().collect::<Vec<_>>();
    stocks.sort_by(|left, right| match (left.1, right.1) {
        (Some(left), Some(right)) => right
            .partial_cmp(&left)
            .unwrap_or(std::cmp::Ordering::Equal),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => left.0.cmp(&right.0),
    });
    let stocks = stocks
        .into_iter()
        .map(|(label, value)| crate::report::DividendSummaryRow {
            label,
            value: report_currency(value, &account.currency),
        })
        .collect::<Vec<_>>();

    Ok(crate::report::DividendReport {
        generated_on: format_distribution_date(current_unix_timestamp()),
        account_name: account.name.clone(),
        account_currency: account.currency.clone(),
        period_label: period.label(),
        period_dates: period.date_range(),
        total_gross: if complete_total {
            format_currency(total, &account.currency)
        } else if reconstructed.is_empty() {
            format_currency(0.0, &account.currency)
        } else {
            "—".into()
        },
        distribution_count: reconstructed.len(),
        payer_count,
        month_count,
        months,
        stocks,
        distributions,
    })
}

fn report_currency(value: Option<f64>, currency: &str) -> String {
    value
        .map(|value| format_currency(value, currency))
        .unwrap_or_else(|| "—".into())
}

fn report_signed_currency(value: Option<f64>, currency: &str) -> String {
    value
        .map(|value| format_signed_currency(value, currency))
        .unwrap_or_else(|| "—".into())
}

fn report_filename_slug(value: &str) -> String {
    let mut slug = value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() { "account".into() } else { slug }
}


fn present_preferences_dialog(parent: &ApplicationWindow, refs: UiRefs) {
    let base_currency_row = ComboRow::new();
    base_currency_row.set_title("Base Currency");
    base_currency_row.set_subtitle("Currency used for portfolio totals");
    let currency_model = string_model(&["CAD", "USD"]);
    base_currency_row.set_model(Some(&currency_model));
    base_currency_row.set_selected(if base_currency(&refs.state) == "USD" { 1 } else { 0 });

    let portfolio_group = PreferencesGroup::builder().title("Portfolio").build();
    portfolio_group.add(&base_currency_row);

    let theme_row = SwitchRow::new();
    theme_row.set_title("Use Aureus Theme");
    theme_row.set_subtitle("Turn off to follow the system appearance");
    theme_row.set_active(aureus_theme_enabled(&refs.state));
    let appearance_group = PreferencesGroup::builder().title("Appearance").build();
    appearance_group.add(&theme_row);

    let export_row = ActionRow::builder()
        .title("Export Backup")
        .subtitle("Save portfolio data")
        .build();
    let export = Button::builder().label("Export").valign(Align::Center).build();
    export_row.add_suffix(&export);

    let import_row = ActionRow::builder()
        .title("Import Backup")
        .subtitle("Restore portfolio data")
        .build();
    let import = Button::builder().label("Import").valign(Align::Center).build();
    import_row.add_suffix(&import);

    let backup_group = PreferencesGroup::builder().title("Backups").build();
    backup_group.add(&export_row);
    backup_group.add(&import_row);

    let body = dialog_body();
    body.append(&portfolio_group);
    body.append(&appearance_group);
    body.append(&backup_group);
    let scroller = dialog_scroller(&body, 560);

    let header = HeaderBar::new();
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scroller));
    let dialog = Dialog::builder()
        .title("Preferences")
        .content_width(560)
        .content_height(480)
        .child(&toolbar)
        .build();

    {
        let refs = refs.clone();
        base_currency_row.connect_selected_notify(move |row| {
            if row.selected() > 1 {
                return;
            }
            let currency = currency_at(row.selected());
            if base_currency(&refs.state) == currency {
                return;
            }
            if let Err(error) = refs.state.database.set_setting(BASE_CURRENCY_KEY, currency) {
                refs.toast_overlay.add_toast(Toast::new(&format!(
                    "Could not change base currency: {error}"
                )));
                return;
            }
            refs.refresh();
            let positions = refs.state.database.load_positions().unwrap_or_default();
            let fetch_fx = portfolio_needs_fx_with_cash(&refs.state, &positions, currency);
            refresh_market_async(refs.clone(), positions, fetch_fx, false);
            refresh_portfolio_history_async(refs.clone(), false);
        });
    }

    {
        let refs = refs.clone();
        theme_row.connect_active_notify(move |row| {
            let value = if row.is_active() { "1" } else { "0" };
            if refs.state.database.set_setting(AUREUS_THEME_KEY, value).is_ok() {
                apply_appearance(&refs.state);
            }
        });
    }

    {
        let parent = parent.clone();
        let refs = refs.clone();
        export.connect_clicked(move |_| present_export_backup(&parent, refs.clone()));
    }
    {
        let parent = parent.clone();
        let refs = refs.clone();
        import.connect_clicked(move |_| present_import_backup(&parent, refs.clone()));
    }

    dialog.present(Some(parent));
}

fn variant_id(parameter: Option<&glib::Variant>) -> Option<i64> {
    parameter
        .and_then(|value| value.str())
        .and_then(|value| value.parse::<i64>().ok())
}

fn write_file_durably(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

fn build_backup_bundle(refs: &UiRefs) -> Result<Vec<u8>, String> {
    let portfolio_json = refs.state.database.export_backup_json()?;
    let portfolio: serde_json::Value = serde_json::from_str(&portfolio_json)
        .map_err(|error| format!("Could not prepare portfolio backup: {error}"))?;
    let bundle = serde_json::json!({
        "format": "Aureus Backup",
        "bundle_version": 2,
        "created_on": current_date_string(),
        "portfolio": portfolio,
    });
    serde_json::to_vec_pretty(&bundle).map_err(|error| error.to_string())
}

fn parse_backup_bundle(contents: &[u8]) -> Result<String, String> {
    let bundle: serde_json::Value = serde_json::from_slice(contents)
        .map_err(|error| format!("This is not a valid Aureus backup: {error}"))?;
    if bundle.get("format").and_then(|value| value.as_str()) != Some("Aureus Backup") {
        return Err("This is not a current Aureus backup".into());
    }
    let bundle_version = bundle
        .get("bundle_version")
        .and_then(|value| value.as_u64())
        .ok_or_else(|| "This is not a current Aureus backup".to_string())?;
    if !matches!(bundle_version, 1 | 2) {
        return Err("This is not a supported Aureus backup".into());
    }

    // Version 1 backups could contain stock pictures. Pictures are deliberately
    // ignored: backups restore portfolio data only and never replace local images.
    let portfolio = bundle
        .get("portfolio")
        .ok_or_else(|| "The backup is missing portfolio data".to_string())?;
    serde_json::to_string(portfolio).map_err(|error| error.to_string())
}

fn present_export_backup(parent: &ApplicationWindow, refs: UiRefs) {
    let backup = match build_backup_bundle(&refs) {
        Ok(backup) => backup,
        Err(error) => {
            refs.toast_overlay
                .add_toast(Toast::new(&format!("Could not create backup: {error}")));
            return;
        }
    };

    let dialog = FileDialog::new();
    dialog.set_title("Export Aureus Backup");
    dialog.set_accept_label(Some("Export"));
    let filename = format!("backup-{}.aureus", current_date_string());
    dialog.set_initial_name(Some(&filename));
    let refs_for_callback = refs.clone();
    dialog.save(
        Some(parent),
        None::<&gio::Cancellable>,
        move |result| {
            let Ok(file) = result else {
                return;
            };
            let Some(path) = file.path() else {
                refs_for_callback.toast_overlay.add_toast(Toast::new(
                    "The selected destination is not writable by Aureus",
                ));
                return;
            };
            match write_file_durably(&path, &backup) {
                Ok(()) => refs_for_callback
                    .toast_overlay
                    .add_toast(Toast::new("Backup exported")),
                Err(error) => refs_for_callback.toast_overlay.add_toast(Toast::new(&format!(
                    "Could not export backup: {error}"
                ))),
            }
        },
    );
}

fn present_import_backup(parent: &ApplicationWindow, refs: UiRefs) {
    present_import_backup_with_success(parent, refs, None);
}

fn present_import_backup_with_success(
    parent: &ApplicationWindow,
    refs: UiRefs,
    after_import: Option<Rc<dyn Fn()>>,
) {
    let dialog = FileDialog::new();
    dialog.set_title("Import Aureus Backup");
    dialog.set_accept_label(Some("Open"));
    let parent_weak = parent.downgrade();
    let refs_for_callback = refs.clone();
    dialog.open(
        Some(parent),
        None::<&gio::Cancellable>,
        move |result| {
            let Ok(file) = result else {
                return;
            };
            let Some(path) = file.path() else {
                refs_for_callback.toast_overlay.add_toast(Toast::new(
                    "The selected backup could not be opened",
                ));
                return;
            };
            let contents = match std::fs::read(&path) {
                Ok(contents) => contents,
                Err(error) => {
                    refs_for_callback.toast_overlay.add_toast(Toast::new(&format!(
                        "Could not read backup: {error}"
                    )));
                    return;
                }
            };
            let portfolio_json = match parse_backup_bundle(&contents) {
                Ok(portfolio_json) => portfolio_json,
                Err(error) => {
                    refs_for_callback.toast_overlay.add_toast(Toast::new(&error));
                    return;
                }
            };
            let Some(parent) = parent_weak.upgrade() else {
                return;
            };

            let has_existing_data = !refs_for_callback
                .state
                .database
                .load_accounts()
                .unwrap_or_default()
                .is_empty()
                || !refs_for_callback
                    .state
                    .database
                    .load_positions()
                    .unwrap_or_default()
                    .is_empty()
                || !refs_for_callback
                    .state
                    .database
                    .load_watchlist()
                    .unwrap_or_default()
                    .is_empty();

            if !has_existing_data {
                apply_import_backup(
                    refs_for_callback.clone(),
                    portfolio_json,
                    after_import.clone(),
                );
                return;
            }

            let confirm = AlertDialog::builder()
                .heading("Replace current Aureus data?")
                .body("Replaces accounts, transactions, cash, and watchlist. Stock pictures stay unchanged.")
                .build();
            confirm.add_response("cancel", "Cancel");
            confirm.add_response("import", "Import Backup");
            confirm.set_default_response(Some("cancel"));
            confirm.set_close_response("cancel");
            confirm.set_response_appearance("import", adw::ResponseAppearance::Destructive);
            let refs = refs_for_callback.clone();
            let after_import = after_import.clone();
            confirm.connect_response(Some("import"), move |_, _| {
                apply_import_backup(
                    refs.clone(),
                    portfolio_json.clone(),
                    after_import.clone(),
                );
            });
            confirm.present(Some(&parent));
        },
    );
}

fn apply_import_backup(
    refs: UiRefs,
    portfolio_json: String,
    after_import: Option<Rc<dyn Fn()>>,
) {
    match refs.state.database.import_backup_json(&portfolio_json) {
        Ok(()) => {
            refs.toast_overlay.add_toast(Toast::new("Backup imported"));
            if let Some(after_import) = after_import {
                apply_appearance(&refs.state);
                refs.refresh();
                refs.prime_hidden_pages();
                after_import();
                let refs = refs.clone();
                glib::timeout_add_local_once(Duration::from_millis(380), move || {
                    refresh_after_import(refs);
                });
            } else {
                refresh_after_import(refs);
            }
        }
        Err(error) => refs
            .toast_overlay
            .add_toast(Toast::new(&format!("Could not import backup: {error}"))),
    }
}

fn refresh_after_import(refs: UiRefs) {
    apply_appearance(&refs.state);
    refs.refresh();
    refs.prime_hidden_pages();
    let positions = refs.state.database.load_positions().unwrap_or_default();
    let fetch_fx =
        portfolio_needs_fx_with_cash(&refs.state, &positions, &base_currency(&refs.state));
    refresh_market_async(refs.clone(), positions.clone(), fetch_fx, false);
    refresh_dividends_async(refs.clone(), positions, false);
    let watchlist = refs.state.database.load_watchlist().unwrap_or_default();
    refresh_watchlist_async(refs.clone(), watchlist, false);
    refresh_portfolio_history_async(refs, false);
}

fn present_watchlist_detail(item_id: i64, refs: UiRefs) {
    let Ok(Some(item)) = refs.state.database.watchlist_item(item_id) else {
        refs.toast_overlay
            .add_toast(Toast::new("This watchlist item is no longer available"));
        return;
    };

    present_security_detail(activity_asset_from_watchlist(&item), refs, true);
}

fn present_search_result_detail(asset: SearchResult, refs: UiRefs) {
    // Search results can carry a quote that is already a little old by the
    // time the user opens it. Always fetch a fresh lightweight quote on
    // entry while the cached/search value keeps the page instantaneous.
    present_security_detail(asset, refs, true);
}

fn present_security_detail(asset: SearchResult, refs: UiRefs, refresh_quote_on_open: bool) {
    let current_price = Label::builder()
        .label(
            &asset
                .market_price
                .map(|price| format_currency(price, &asset.currency))
                .unwrap_or_else(|| "—".into()),
        )
        .halign(Align::Start)
        .css_classes(["title-1"])
        .build();
    let day_change = Label::builder()
        .label(
            &asset
                .change_percent
                .map(|change| format!("{change:+.2}% today"))
                .unwrap_or_else(|| "Today's change unavailable".into()),
        )
        .halign(Align::Start)
        .css_classes(["caption"])
        .build();
    if let Some(change) = asset.change_percent {
        set_gain_class(&day_change, change);
    } else {
        day_change.add_css_class("dim-label");
    }
    let quote_status = Label::builder()
        .label(if asset.market_price.is_some() { "Search result price" } else { "Quote unavailable" })
        .halign(Align::Start)
        .css_classes(["dim-label", "caption"])
        .build();

    let quote_refresh_spinner = Spinner::new();
    quote_refresh_spinner.set_size_request(12, 12);
    let quote_refresh_status = Label::builder()
        .label("Refreshing price…")
        .halign(Align::Start)
        .css_classes(["dim-label", "caption"])
        .build();
    let quote_refresh_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(6)
        .visible(false)
        .build();
    quote_refresh_box.append(&quote_refresh_spinner);
    quote_refresh_box.append(&quote_refresh_status);

    let hero = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(12)
        .css_classes(["card", "detail-hero"])
        .build();
    let stock_picture = stock_avatar(&asset.provider_symbol, &asset.code, 56);
    let stock_picture_control = stock_picture_control(&stock_picture, 56);
    hero.append(&stock_picture_control);
    let hero_text = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(5)
        .hexpand(true)
        .build();
    hero_text.append(
        &Label::builder()
            .label(&format!(
                "{} · {} · {}",
                asset.name,
                friendly_exchange(&asset.exchange),
                asset.currency
            ))
            .halign(Align::Start)
            .wrap(true)
            .css_classes(["dim-label"])
            .build(),
    );
    hero_text.append(&current_price);
    hero_text.append(&day_change);
    hero_text.append(&quote_status);
    hero_text.append(&quote_refresh_box);
    hero.append(&hero_text);

    let chart = PriceChart::new();
    let range_return = Label::builder()
        .label("—")
        .halign(Align::Start)
        .css_classes(["heading"])
        .build();
    let range_high_low = Label::builder()
        .label("Waiting for history")
        .halign(Align::Start)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .build();
    let history_status = Label::builder()
        .label("Loading history")
        .halign(Align::Start)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .build();
    let chart_card = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(8)
        .css_classes(["card", "chart-card"])
        .build();
    chart_card.append(chart.widget());
    let chart_summary = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(2)
        .margin_start(4)
        .margin_end(4)
        .build();
    chart_summary.append(&range_return);
    chart_summary.append(&range_high_low);
    chart_summary.append(&history_status);
    chart_card.append(&chart_summary);

    let range_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(6)
        .homogeneous(true)
        .build();
    let active_range = Rc::new(Cell::new(HistoryRange::OneMonth));
    let mut range_buttons = Vec::new();
    let mut first_button: Option<ToggleButton> = None;
    for range in [
        HistoryRange::OneDay,
        HistoryRange::OneWeek,
        HistoryRange::OneMonth,
        HistoryRange::ThreeMonths,
        HistoryRange::OneYear,
        HistoryRange::FiveYears,
        HistoryRange::All,
    ] {
        let button = ToggleButton::builder()
            .label(range.label())
            .css_classes(["pill", "range-toggle"])
            .build();
        if let Some(first) = first_button.as_ref() {
            button.set_group(Some(first));
        } else {
            first_button = Some(button.clone());
        }
        if range == HistoryRange::OneMonth {
            button.set_active(true);
        }
        range_box.append(&button);
        range_buttons.push((button, range));
    }

    let details = positions_list();
    details.append(&detail_value_row("Exchange", friendly_exchange(&asset.exchange)));
    details.append(&detail_value_row("Currency", &asset.currency));
    details.append(&detail_value_row("Yahoo symbol", &asset.provider_symbol));
    if !asset.asset_type.trim().is_empty() {
        details.append(&detail_value_row(
            "Type",
            friendly_asset_type(&asset.asset_type),
        ));
    }

    let add_activity = Button::builder()
        .label("Add Activity")
        .css_classes(["suggested-action", "pill"])
        .halign(Align::Fill)
        .hexpand(true)
        .build();
    let activity_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .halign(Align::Fill)
        .build();
    activity_box.append(&add_activity);

    let content = page_content_box();
    content.append(&hero);
    content.append(&activity_box);
    content.append(&section_heading("Price History"));
    content.append(&chart_card);
    content.append(&range_box);
    content.append(&section_heading("Security"));
    content.append(&details);
    let scroller = page_scroller(&content, 900);
    let pull_refresh = build_detail_pull_refresh(&scroller);

    let star = watchlist_star_button(&refs, &asset);
    let stock_header_actions = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(6)
        .build();
    stock_header_actions.append(&star);
    let header = HeaderBar::new();
    header.set_centering_policy(adw::CenteringPolicy::Strict);
    header.pack_end(&stock_header_actions);
    let header_title = adw::WindowTitle::new(&asset.code, &asset.name);
    header.set_title_widget(Some(&header_title));
    let shortcut_refresh = DetailShortcutRefresh::new();
    let header_overlay = Overlay::new();
    header_overlay.set_child(Some(&header));
    header_overlay.add_overlay(&shortcut_refresh.bar);
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&header_overlay);
    // The top-bar revealer is only a spacer for the natural pull-down
    // motion; the visible glyph is centered by the full-width Overlay below.
    toolbar.add_top_bar(&pull_refresh.revealer);
    toolbar.set_content(Some(&scroller));
    let toolbar_overlay = Overlay::new();
    toolbar_overlay.set_child(Some(&toolbar));
    toolbar_overlay.add_overlay(&pull_refresh.visual_revealer);
    let page = NavigationPage::builder()
        .title(&asset.code)
        .tag("stock-detail")
        .child(&toolbar_overlay)
        .build();

    let detail = WatchDetailRefs {
        app: refs.clone(),
        provider_symbol: asset.provider_symbol.clone(),
        currency: asset.currency.clone(),
        chart,
        current_price,
        day_change,
        quote_status,
        quote_refresh_box,
        quote_refresh_spinner,
        quote_refresh_status,
        range_return,
        range_high_low,
        history_status,
        active_range: active_range.clone(),
        pull_refresh: pull_refresh.clone(),
        shortcut_refresh: shortcut_refresh.clone(),
        generation: Rc::new(Cell::new(0)),
    };

    connect_stock_picture_control(
        &stock_picture_control,
        refs.clone(),
        asset.provider_symbol.clone(),
    );

    {
        let refs = refs.clone();
        let asset = asset.clone();
        add_activity.connect_clicked(move |button| {
            let Some(root) = button.root() else {
                return;
            };
            let Ok(window) = root.downcast::<ApplicationWindow>() else {
                return;
            };
            present_add_activity_dialog_with_context(
                &window,
                refs.clone(),
                None,
                Some(asset.clone()),
                None,
            );
        });
    }

    for (button, range) in range_buttons {
        let detail = detail.clone();
        let active_range = active_range.clone();
        button.connect_toggled(move |button| {
            if button.is_active() {
                active_range.set(range);
                load_watch_history_range(detail.clone(), range, false, false);
            }
        });
    }
    {
        let detail = detail.clone();
        let active_range = active_range.clone();
        install_detail_pull_to_refresh(
            &scroller,
            &header,
            pull_refresh.clone(),
            1,
            Rc::new(move || {
                load_watch_history_range(detail.clone(), active_range.get(), true, true);
            }),
        );
    }

    {
        let detail = detail.clone();
        let active_range = active_range.clone();
        let shortcut_refresh = shortcut_refresh.clone();
        let pull_refresh = pull_refresh.clone();
        *refs.detail_refresh.borrow_mut() = Some(Rc::new(move || {
            if pull_refresh.pending.get() > 0 || !shortcut_refresh.begin(1) {
                return;
            }
            load_watch_history_range(detail.clone(), active_range.get(), true, true);
        }));
    }

    refs.navigation.push(&page);
    if refresh_quote_on_open {
        refresh_watch_detail_quote(detail.clone());
    }
    load_watch_history_range(detail, HistoryRange::OneMonth, false, true);
}

fn refresh_watch_detail_quote(detail: WatchDetailRefs) {
    detail.quote_refresh_status.set_label("Refreshing price…");
    detail.quote_refresh_spinner.set_visible(true);
    detail.quote_refresh_spinner.start();
    detail.quote_refresh_box.set_visible(true);

    let symbol = detail.provider_symbol.clone();
    let (sender, receiver) = mpsc::channel::<WatchQuoteLoadResult>();
    std::thread::spawn(move || {
        let result = market_data::quote(&symbol).map_err(|error| error.to_string());
        let _ = sender.send(WatchQuoteLoadResult { result });
    });

    let receiver = Rc::new(RefCell::new(receiver));
    glib::timeout_add_local(Duration::from_millis(60), move || {
        let Ok(load) = receiver.borrow().try_recv() else {
            return glib::ControlFlow::Continue;
        };

        match load.result {
            Ok(quote) => {
                if let Some(item_id) =
                    watchlist_item_id_for_symbol(&detail.app, &detail.provider_symbol)
                {
                    let _ = detail.app.state.database.update_watchlist_quote(
                        item_id,
                        quote.close,
                        quote.change_percent,
                        quote.timestamp,
                    );
                }
                for position in detail
                    .app
                    .state
                    .database
                    .load_positions()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|position| {
                        position
                            .provider_symbol
                            .eq_ignore_ascii_case(&detail.provider_symbol)
                    })
                {
                    let _ = detail.app.state.database.update_quote(
                        position.id,
                        quote.close,
                        quote.change_percent,
                        quote.timestamp,
                    );
                }
                let price_text = format_currency(quote.close, &detail.currency);
                let quote_state = market_data::quote_state_label(
                    quote.market_state.as_deref(),
                    quote.timestamp,
                    current_unix_timestamp(),
                );
                let status_text = format!("{} · {}", quote_state, relative_time(quote.timestamp));
                let change = quote.change_percent;
                let update_day = detail.active_range.get() == HistoryRange::OneDay;
                let day_text = if update_day {
                    Some(match change {
                        Some(change) => format!("{change:+.2}% today"),
                        None => "Today's change unavailable".into(),
                    })
                } else {
                    None
                };
                let mut targets = vec![
                    (detail.current_price.clone(), price_text.clone()),
                    (detail.quote_status.clone(), status_text.clone()),
                ];
                if let Some(text) = &day_text {
                    targets.push((detail.day_change.clone(), text.clone()));
                }
                let detail_for_text = detail.clone();
                crossfade_loaded_labels(targets, move || {
                    detail_for_text.current_price.set_label(&price_text);
                    set_quote_status(&detail_for_text.quote_status, &status_text);
                    if update_day {
                        detail_for_text.day_change.remove_css_class("dim-label");
                        match change {
                            Some(change) => {
                                detail_for_text.day_change.set_label(&format!("{change:+.2}% today"));
                                set_gain_class(&detail_for_text.day_change, change);
                            }
                            None => {
                                detail_for_text.day_change.set_label("Today's change unavailable");
                                detail_for_text.day_change.add_css_class("dim-label");
                                set_gain_class(&detail_for_text.day_change, 0.0);
                            }
                        }
                    }
                });
                detail.quote_refresh_spinner.stop();
                detail.quote_refresh_box.set_visible(false);
                refresh_with_loaded_crossfade(detail.app.clone());
            }
            Err(error) => {
                detail.quote_refresh_spinner.stop();
                detail.quote_refresh_spinner.set_visible(false);
                let health = market_data::quote_health_from_error(&error);
                set_quote_status(&detail.quote_status, &format!("{} · using cached value", health));
                detail
                    .quote_refresh_status
                    .set_label(&format!("{} · using cached value", health));
                let status_box = detail.quote_refresh_box.clone();
                glib::timeout_add_local_once(Duration::from_millis(2200), move || {
                    status_box.set_visible(false);
                });
            }
        }

        glib::ControlFlow::Break
    });
}

fn load_watch_history_range(
    detail: WatchDetailRefs,
    range: HistoryRange,
    announce: bool,
    force_refresh: bool,
) {
    let now = current_unix_timestamp();
    let minimum = range.minimum_timestamp(now);
    let cached = market_data::display_history_points(
        detail
            .app
            .state
            .database
            .history_points(&detail.provider_symbol, range.interval(), minimum)
            .unwrap_or_default(),
        range,
    );
    let had_cache = cached.len() >= 2;

    // A user-initiated refresh must not repaint the visible chart from the
    // database cache before the network result arrives. The page may already
    // contain fresher data than the persisted cache, which otherwise causes a
    // brief jump to an older percentage/chart during refresh. Cache is still
    // used for initial/range loads and as the failure fallback.
    if !announce {
        if had_cache {
            detail
                .chart
                .set_points(cached.clone(), &detail.currency, range);
            update_watch_history_summary(&detail, &cached, range, false);
            detail.history_status.set_label("Cached history");
        } else {
            detail.chart.set_message("Loading price history");
            detail.range_return.set_label("—");
            detail.range_high_low.set_label("Waiting for history");
            detail.day_change.set_label(&format!("Loading {} change…", range.label()));
            detail.day_change.add_css_class("dim-label");
            set_gain_class(&detail.day_change, 0.0);
            detail.history_status.set_label("Loading history");
        }
    }

    let generation = detail.generation.get().saturating_add(1);
    detail.generation.set(generation);
    let needs_refresh = force_refresh
        || detail
            .app
            .state
            .database
            .history_needs_refresh(
                &detail.provider_symbol,
                range.key(),
                range.interval(),
                range.cache_seconds(),
            )
            .unwrap_or(true);
    if !needs_refresh {
        if announce {
            complete_detail_refresh(&detail.pull_refresh, &detail.shortcut_refresh);
        }
        return;
    }
    if had_cache {
        detail.history_status.set_label(if announce {
            "Refreshing history"
        } else {
            "Cached history · updating"
        });
    }
    let symbol = detail.provider_symbol.clone();
    let (sender, receiver) = mpsc::channel::<WatchHistoryLoadResult>();
    std::thread::spawn(move || {
        let result = market_data::history(&symbol, range).map_err(|error| error.to_string());
        let _ = sender.send(WatchHistoryLoadResult {
            generation,
            range,
            result,
            announce,
        });
    });
    let receiver = Rc::new(RefCell::new(receiver));
    glib::timeout_add_local(Duration::from_millis(75), move || {
        let Ok(load) = receiver.borrow().try_recv() else {
            return glib::ControlFlow::Continue;
        };
        if load.announce {
            complete_detail_refresh(&detail.pull_refresh, &detail.shortcut_refresh);
        }
        match load.result {
            Ok(history) => {
                let _ = detail.app.state.database.save_history(
                    &detail.provider_symbol,
                    load.range.interval(),
                    &history.points,
                );
                let _ = detail.app.state.database.set_history_fetched(
                    &detail.provider_symbol,
                    load.range.key(),
                    load.range.interval(),
                );
                if let Some(price) = history.current_price {
                    if let Some(item_id) =
                        watchlist_item_id_for_symbol(&detail.app, &detail.provider_symbol)
                    {
                        let _ = detail.app.state.database.update_watchlist_quote(
                            item_id,
                            price,
                            history.day_change_percent,
                            history.quote_timestamp,
                        );
                    }
                }
                refresh_with_loaded_crossfade(detail.app.clone());
                if detail.generation.get() == load.generation {
                    let currency = history
                        .currency
                        .as_deref()
                        .filter(|value| !value.trim().is_empty())
                        .unwrap_or(&detail.currency);
                    detail
                        .chart
                        .set_points(history.points.clone(), currency, load.range);
                    update_watch_history_summary(&detail, &history.points, load.range, true);
                    if let Some(price) = history.current_price {
                        crossfade_loaded_label(
                            &detail.current_price,
                            format_currency(price, &detail.currency),
                        );
                    }
                    crossfade_loaded_label(&detail.history_status, "Updated just now");
                }
            }
            Err(error) => {
                if detail.generation.get() == load.generation {
                    if had_cache {
                        detail.history_status.set_label(
                            "Update failed · showing cached history",
                        );
                    } else {
                        detail.chart.set_message("Price history is unavailable right now");
                        detail.day_change.set_label(history_range_unavailable_label(load.range));
                        detail.day_change.add_css_class("dim-label");
                        set_gain_class(&detail.day_change, 0.0);
                        detail.history_status.set_label(&error);
                    }
                    if load.announce {
                        detail
                            .app
                            .toast_overlay
                            .add_toast(Toast::new("Could not refresh price history"));
                    }
                }
            }
        }
        glib::ControlFlow::Break
    });
}

fn history_range_change_suffix(range: HistoryRange) -> &'static str {
    match range {
        HistoryRange::OneDay => "today",
        HistoryRange::OneWeek => "this week",
        HistoryRange::OneMonth => "this month",
        HistoryRange::ThreeMonths => "over 3 months",
        HistoryRange::OneYear => "over 1 year",
        HistoryRange::FiveYears => "over 5 years",
        HistoryRange::All => "all time",
    }
}

fn history_range_unavailable_label(range: HistoryRange) -> &'static str {
    match range {
        HistoryRange::OneDay => "Today's change unavailable",
        HistoryRange::OneWeek => "This week's change unavailable",
        HistoryRange::OneMonth => "This month's change unavailable",
        HistoryRange::ThreeMonths => "3-month change unavailable",
        HistoryRange::OneYear => "1-year change unavailable",
        HistoryRange::FiveYears => "5-year change unavailable",
        HistoryRange::All => "All-time change unavailable",
    }
}

fn update_watch_history_summary(
    detail: &WatchDetailRefs,
    points: &[PricePoint],
    range: HistoryRange,
    animate: bool,
) {
    let Some(first) = points.first() else {
        if animate {
            crossfade_loaded_label(&detail.range_return, "—");
            crossfade_loaded_label(&detail.range_high_low, "No history available");
            crossfade_loaded_label(&detail.day_change, history_range_unavailable_label(range));
        } else {
            detail.range_return.set_label("—");
            detail.range_high_low.set_label("No history available");
            detail.day_change.set_label(history_range_unavailable_label(range));
        }
        detail.day_change.add_css_class("dim-label");
        set_gain_class(&detail.day_change, 0.0);
        return;
    };
    let Some(last) = points.last() else {
        return;
    };
    let change = if first.close.abs() < f64::EPSILON {
        0.0
    } else {
        (last.close - first.close) / first.close * 100.0
    };
    let range_return = format!("{change:+.2}% over {}", range.label());
    let day_change = format!("{change:+.2}% {}", history_range_change_suffix(range));
    let low = points
        .iter()
        .map(|point| point.close)
        .fold(f64::INFINITY, f64::min);
    let high = points
        .iter()
        .map(|point| point.close)
        .fold(f64::NEG_INFINITY, f64::max);
    let high_low = if low.is_finite() && high.is_finite() {
        format!(
            "Low {} · High {} · {} points",
            format_currency(low, &detail.currency),
            format_currency(high, &detail.currency),
            points.len()
        )
    } else {
        format!("{} points", points.len())
    };

    if animate {
        let detail_for_update = detail.clone();
        crossfade_loaded_labels(
            vec![
                (detail.range_return.clone(), range_return.clone()),
                (detail.range_high_low.clone(), high_low.clone()),
                (detail.day_change.clone(), day_change.clone()),
            ],
            move || {
                detail_for_update.range_return.set_label(&range_return);
                detail_for_update.range_high_low.set_label(&high_low);
                detail_for_update.day_change.remove_css_class("dim-label");
                detail_for_update.day_change.set_label(&day_change);
                set_gain_class(&detail_for_update.range_return, change);
                set_gain_class(&detail_for_update.day_change, change);
            },
        );
    } else {
        detail.range_return.set_label(&range_return);
        detail.range_high_low.set_label(&high_low);
        detail.day_change.remove_css_class("dim-label");
        detail.day_change.set_label(&day_change);
        set_gain_class(&detail.range_return, change);
        set_gain_class(&detail.day_change, change);
    }
}

fn present_account_detail(account_id: i64, refs: UiRefs) {
    let Ok(accounts) = refs.state.database.load_accounts() else {
        refs.toast_overlay.add_toast(Toast::new("Unable to load account"));
        return;
    };
    let Some(account) = accounts.into_iter().find(|account| account.id == account_id) else {
        refs.toast_overlay.add_toast(Toast::new("This account is no longer available"));
        return;
    };

    let base = base_currency(&refs.state);
    let usd_cad = refs
        .state
        .database
        .fx_rate(USD_CAD_PAIR)
        .ok()
        .flatten()
        .map(|rate| rate.rate);
    let positions = refs
        .state
        .database
        .load_positions()
        .unwrap_or_default()
        .into_iter()
        .filter(|position| position.account_id == account.id)
        .collect::<Vec<_>>();
    let transactions = refs
        .state
        .database
        .load_transactions()
        .unwrap_or_default()
        .into_iter()
        .filter(|transaction| transaction.account_id == account.id)
        .collect::<Vec<_>>();
    let cash_entries = refs
        .state
        .database
        .load_cash_entries()
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry.account_id == account.id)
        .collect::<Vec<_>>();

    let holdings_value = sum_optional_converted(
        positions.iter().map(|position| (position.market_value(), position.currency.as_str())),
        &base,
        usd_cad,
    );
    let cash_value = convert_currency(account.cash, &account.currency, &base, usd_cad);
    let total_value = match (holdings_value, cash_value) {
        (Some(holdings), Some(cash)) => Some(holdings + cash),
        _ => None,
    };
    let unrealized_gain = sum_optional_converted(
        positions.iter().map(|position| (position.total_gain(), position.currency.as_str())),
        &base,
        usd_cad,
    );

    let split_symbols = transactions
        .iter()
        .map(|transaction| transaction.provider_symbol.to_ascii_uppercase())
        .collect::<HashSet<_>>();
    let mut split_events = Vec::new();
    for symbol in split_symbols {
        split_events.extend(refs.state.database.split_events(&symbol).unwrap_or_default());
    }
    let realized_gain = if transactions.is_empty() {
        Some(0.0)
    } else {
        realized_gain_from_transactions(&transactions, &split_events, &base, usd_cad)
    };
    let paid_dividends = sum_converted(
        cash_entries
            .iter()
            .filter(|entry| entry.kind == "DIVIDEND" && entry.amount > 0.0)
            .map(|entry| (entry.amount, entry.currency.as_str())),
        &base,
        usd_cad,
    );

    let hero = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(4)
        .css_classes(["card", "detail-hero"])
        .build();
    hero.append(
        &Label::builder()
            .label(&account.currency)
            .halign(Align::Start)
            .css_classes(["dim-label", "caption"])
            .build(),
    );
    hero.append(
        &Label::builder()
            .label(
                &total_value
                    .map(|value| format_currency(value, &base))
                    .unwrap_or_else(|| "—".into()),
            )
            .halign(Align::Start)
            .css_classes(["title-1"])
            .build(),
    );
    hero.append(
        &Label::builder()
            .label(&format!(
                "{} cash · {}",
                format_currency(account.cash, &account.currency),
                holding_count_text(positions.len())
            ))
            .halign(Align::Start)
            .wrap(true)
            .css_classes(["dim-label"])
            .build(),
    );

    let value_label = metric_value_label();
    value_label.set_label(&total_value.map(|value| format_currency(value, &base)).unwrap_or_else(|| "—".into()));
    let cash_label = metric_value_label();
    cash_label.set_label(&format_currency(account.cash, &account.currency));
    let gain_label = metric_value_label();
    match unrealized_gain {
        Some(value) => {
            gain_label.set_label(&format_signed_currency(value, &base));
            set_gain_class(&gain_label, value);
        }
        None => gain_label.set_label("—"),
    }
    let realized_label = metric_value_label();
    match realized_gain {
        Some(value) => {
            realized_label.set_label(&format_signed_currency(value, &base));
            set_gain_class(&realized_label, value);
        }
        None => realized_label.set_label("—"),
    }
    let dividends_label = metric_value_label();
    dividends_label.set_label(&paid_dividends.map(|value| format_currency(value, &base)).unwrap_or_else(|| "—".into()));

    let metrics = WrapBox::new();
    metrics.set_child_spacing(12);
    metrics.set_line_spacing(12);
    metrics.set_natural_line_length(720);
    metrics.set_line_homogeneous(true);
    metrics.append(&metric_card("Account value", &value_label));
    metrics.append(&metric_card("Cash", &cash_label));
    metrics.append(&metric_card("Unrealized gain", &gain_label));
    metrics.append(&metric_card("Realized gain", &realized_label));
    metrics.append(&metric_card("Paid dividends", &dividends_label));

    let holdings_list = positions_list();
    for position in &positions {
        holdings_list.append(&position_row(position, &base, usd_cad, true));
    }
    holdings_list.connect_row_activated({
        let refs = refs.clone();
        move |_, row| {
            let key = row.widget_name();
            if let Some(id) = key.strip_prefix("position-").and_then(|value| value.parse::<i64>().ok()) {
                present_position_detail(id, refs.clone());
            }
        }
    });

    let activity_list = positions_list();
    let mut events = Vec::new();
    for transaction in &transactions {
        let amount = transaction.shares * transaction.price + transaction.fees;
        let title = match transaction.transaction_type.as_str() {
            "BUY" => format!("Buy {}", transaction.code),
            "SELL" => format!("Sell {}", transaction.code),
            "OPEN" => format!("Opening Position · {}", transaction.code),
            "TRANSFER_OUT" => format!("Transfer Out {}", transaction.code),
            "TRANSFER_IN" => format!("Transfer In {}", transaction.code),
            other => format!("{} · {}", other, transaction.code),
        };
        let subtitle = format!(
            "{} · {} shares at {}",
            format_distribution_date(transaction.timestamp),
            trim_number(transaction.shares),
            format_currency(transaction.price, &transaction.currency)
        );
        events.push((transaction.timestamp, title, subtitle, format_currency(amount, &transaction.currency)));
    }
    for entry in &cash_entries {
        let title = match entry.kind.as_str() {
            "DEPOSIT" if entry.amount >= 0.0 => "Deposit".to_string(),
            "DEPOSIT" => "Withdrawal".to_string(),
            "DIVIDEND" => if entry.description.trim().is_empty() { "Dividend".to_string() } else { entry.description.clone() },
            "TRADE" => entry.description.clone(),
            _ => entry.description.clone(),
        };
        let subtitle = format_distribution_date(entry.occurred_at);
        events.push((entry.occurred_at, title, subtitle, format_signed_currency(entry.amount, &entry.currency)));
    }
    events.sort_by(|left, right| right.0.cmp(&left.0));
    for (_, title, subtitle, amount) in events.into_iter().take(20) {
        let row = ActionRow::builder()
            .title(&title)
            .subtitle(&subtitle)
            .build();
        row.set_activatable(false);
        row.add_suffix(
            &Label::builder()
                .label(&amount)
                .halign(Align::End)
                .css_classes(["dim-label"])
                .build(),
        );
        activity_list.append(&row);
    }

    let add_activity = Button::builder()
        .label("Add Activity")
        .css_classes(["suggested-action", "pill"])
        .hexpand(true)
        .halign(Align::Fill)
        .build();
    let add_cash = Button::builder()
        .label("Add Cash")
        .css_classes(["pill"])
        .hexpand(true)
        .halign(Align::Fill)
        .build();

    // Account-level actions are one balanced action row at every width. Fixed
    // button widths previously made the pair drift left and stack unnecessarily.
    let actions = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .homogeneous(true)
        .hexpand(true)
        .halign(Align::Fill)
        .build();
    actions.append(&add_activity);
    actions.append(&add_cash);
    let actions_clamp = adw::Clamp::builder()
        .maximum_size(540)
        .tightening_threshold(360)
        .hexpand(true)
        .child(&actions)
        .build();

    let content = page_content_box();
    content.append(&hero);
    content.append(&metrics);
    content.append(&actions_clamp);
    content.append(&section_heading("Holdings"));
    if positions.is_empty() {
        content.append(&Label::builder().label("No holdings in this account yet").halign(Align::Start).css_classes(["dim-label"]).build());
    } else {
        content.append(&holdings_list);
    }
    content.append(&section_heading("Account History"));
    if activity_list.first_child().is_some() {
        content.append(&activity_list);
    } else {
        content.append(&Label::builder().label("No account activity yet").halign(Align::Start).css_classes(["dim-label"]).build());
    }

    let scroller = page_scroller(&content, 900);
    let header = HeaderBar::new();
    header.set_centering_policy(adw::CenteringPolicy::Strict);
    header.set_title_widget(Some(&adw::WindowTitle::new(&account.name, "Account")));
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scroller));
    let page = NavigationPage::builder()
        .title(&account.name)
        .tag("account-detail")
        .child(&toolbar)
        .build();

    {
        let refs = refs.clone();
        let account = account.clone();
        add_cash.connect_clicked(move |button| {
            let Some(root) = button.root() else { return; };
            let Ok(window) = root.downcast::<ApplicationWindow>() else { return; };
            present_manage_cash_dialog(&window, refs.clone(), account.clone());
        });
    }
    {
        let refs = refs.clone();
        let account = account.clone();
        add_activity.connect_clicked(move |button| {
            let Some(root) = button.root() else { return; };
            let Ok(window) = root.downcast::<ApplicationWindow>() else { return; };
            present_add_activity_dialog_with_context(&window, refs.clone(), Some(account.id), None, None);
        });
    }

    refs.navigation.push(&page);
}

fn present_position_detail(position_id: i64, refs: UiRefs) {
    let Ok(Some(position)) = refs.state.database.position(position_id) else {
        refs.toast_overlay
            .add_toast(Toast::new("This position is no longer available"));
        return;
    };

    let base = base_currency(&refs.state);
    let usd_cad = refs
        .state
        .database
        .fx_rate(USD_CAD_PAIR)
        .ok()
        .flatten()
        .map(|rate| rate.rate);

    let current_price = Label::builder()
        .label(
            &position
                .last_price
                .map(|price| format_currency(price, &position.currency))
                .unwrap_or_else(|| "—".into()),
        )
        .halign(Align::Start)
        .css_classes(["title-1"])
        .build();
    let day_change = Label::builder()
        .label(
            &position
                .day_change_percent
                .map(|change| format!("{change:+.2}% today"))
                .unwrap_or_else(|| "Today's change unavailable".into()),
        )
        .halign(Align::Start)
        .css_classes(["caption"])
        .build();
    if let Some(change) = position.day_change_percent {
        set_gain_class(&day_change, change);
    } else {
        day_change.add_css_class("dim-label");
    }
    let quote_status = quote_health_label(position.last_price, position.quote_updated_at);

    let hero = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(12)
        .css_classes(["card", "detail-hero"])
        .build();
    let stock_picture = stock_avatar(&position.provider_symbol, &position.code, 56);
    let stock_picture_control = stock_picture_control(&stock_picture, 56);
    hero.append(&stock_picture_control);
    let hero_text = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(5)
        .hexpand(true)
        .build();
    hero_text.append(
        &Label::builder()
            .label(&format!(
                "{} · {} · {}",
                position.name,
                friendly_exchange(&position.exchange),
                position.currency
            ))
            .halign(Align::Start)
            .wrap(true)
            .css_classes(["dim-label"])
            .build(),
    );
    hero_text.append(&current_price);
    hero_text.append(&day_change);
    hero_text.append(&quote_status);
    hero.append(&hero_text);

    let market_value = metric_value_label();
    market_value.set_label(
        &converted_market_value(&position, &base, usd_cad)
            .map(|value| format_currency(value, &base))
            .unwrap_or_else(|| {
                position
                    .market_value()
                    .map(|value| format_currency(value, &position.currency))
                    .unwrap_or_else(|| "—".into())
            }),
    );
    let total_gain = metric_value_label();
    match converted_total_gain(&position, &base, usd_cad) {
        Some(gain) => {
            total_gain.set_label(&format_signed_currency(gain, &base));
            set_gain_class(&total_gain, gain);
        }
        None => total_gain.set_label("—"),
    }
    let average_cost = metric_value_label();
    average_cost.set_label(&format_currency(position.average_cost, &position.currency));

    let position_metrics = WrapBox::new();
    position_metrics.set_child_spacing(12);
    position_metrics.set_line_spacing(12);
    position_metrics.set_natural_line_length(680);
    position_metrics.set_line_homogeneous(true);
    position_metrics.append(&metric_card("Market value", &market_value));
    position_metrics.append(&metric_card("Unrealized gain", &total_gain));
    position_metrics.append(&metric_card("Average cost", &average_cost));

    let chart = PriceChart::new();
    let range_return = Label::builder()
        .label("—")
        .halign(Align::Start)
        .css_classes(["heading"])
        .build();
    let range_high_low = Label::builder()
        .label("Waiting for history")
        .halign(Align::Start)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .build();
    let history_status = Label::builder()
        .label("Loading history")
        .halign(Align::Start)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .build();

    let chart_card = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(8)
        .css_classes(["card", "chart-card"])
        .build();
    chart_card.append(chart.widget());
    let chart_summary = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(2)
        .margin_start(4)
        .margin_end(4)
        .build();
    chart_summary.append(&range_return);
    chart_summary.append(&range_high_low);
    chart_summary.append(&history_status);
    chart_card.append(&chart_summary);

    let range_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(6)
        .homogeneous(true)
        .build();
    let active_range = Rc::new(Cell::new(HistoryRange::OneMonth));
    let mut range_buttons = Vec::new();
    let mut first_button: Option<ToggleButton> = None;
    for range in [
        HistoryRange::OneDay,
        HistoryRange::OneWeek,
        HistoryRange::OneMonth,
        HistoryRange::ThreeMonths,
        HistoryRange::OneYear,
        HistoryRange::FiveYears,
        HistoryRange::All,
    ] {
        let button = ToggleButton::builder()
            .label(range.label())
            .css_classes(["pill", "range-toggle"])
            .build();
        if let Some(first) = first_button.as_ref() {
            button.set_group(Some(first));
        } else {
            first_button = Some(button.clone());
        }
        if range == HistoryRange::OneMonth {
            button.set_active(true);
        }
        range_box.append(&button);
        range_buttons.push((button, range));
    }

    let details_list = ListBox::builder()
        .selection_mode(SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    details_list.append(&detail_value_row("Account", &position.account_name));
    details_list.append(&detail_value_row("Shares", &trim_number(position.shares)));
    details_list.append(&detail_value_row(
        "Cost basis",
        &format_currency(position.cost_basis(), &position.currency),
    ));
    details_list.append(&detail_value_row(
        "Unrealized return",
        &position
            .total_return_percent()
            .map(|value| format!("{value:+.2}%"))
            .unwrap_or_else(|| "—".into()),
    ));
    let holding_transactions = refs
        .state
        .database
        .load_transactions()
        .unwrap_or_default()
        .into_iter()
        .filter(|transaction| {
            transaction.account_id == position.account_id
                && transaction
                    .provider_symbol
                    .eq_ignore_ascii_case(&position.provider_symbol)
        })
        .collect::<Vec<_>>();
    if !holding_transactions.is_empty() {
        let holding_splits = refs
            .state
            .database
            .split_events(&position.provider_symbol)
            .unwrap_or_default();
        if let Some(realized) = realized_gain_from_transactions(&holding_transactions, &holding_splits, &base, usd_cad) {
            details_list.append(&detail_value_row(
                "Realized gain",
                &format_signed_currency(realized, &base),
            ));
            if let Some(unrealized) = converted_total_gain(&position, &base, usd_cad) {
                details_list.append(&detail_value_row(
                    "Investment return",
                    &format_signed_currency(unrealized + realized, &base),
                ));
            }
        }
    }
    if position.currency != base {
        if let Some(native_value) = position.market_value() {
            details_list.append(&detail_value_row(
                "Native market value",
                &format_currency(native_value, &position.currency),
            ));
        }
    }

    let dividend_annual = metric_value_label();
    let dividend_per_share = metric_value_label();
    let dividend_yield = metric_value_label();
    let dividend_metrics = WrapBox::new();
    dividend_metrics.set_child_spacing(12);
    dividend_metrics.set_line_spacing(12);
    dividend_metrics.set_natural_line_length(680);
    dividend_metrics.set_line_homogeneous(true);
    dividend_metrics.append(&metric_card("Estimated annual income", &dividend_annual));
    dividend_metrics.append(&metric_card("Trailing 12M per share", &dividend_per_share));
    dividend_metrics.append(&metric_card("Distribution yield", &dividend_yield));
    let dividend_status = Label::builder()
        .label("Checking dividend history")
        .halign(Align::Start)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .build();
    let dividend_list = positions_list();

    let add_activity = Button::builder()
        .label("Add Activity")
        .css_classes(["suggested-action", "pill"])
        .halign(Align::Fill)
        .hexpand(true)
        .build();
    let trade_actions = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .build();
    trade_actions.append(&add_activity);

    let content = page_content_box();
    content.append(&hero);
    content.append(&position_metrics);
    content.append(&trade_actions);
    content.append(&section_heading("Price History"));
    content.append(&chart_card);
    content.append(&range_box);
    content.append(&section_heading("Your Position"));
    content.append(&details_list);
    content.append(&section_heading("Dividends"));
    content.append(&dividend_metrics);
    content.append(&dividend_status);
    content.append(&dividend_list);
    let scroller = page_scroller(&content, 900);
    let pull_refresh = build_detail_pull_refresh(&scroller);

    let transactions_button = Button::builder()
        .icon_name("view-list-symbolic")
        .tooltip_text("Transactions")
        .build();
    let star = watchlist_star_button(&refs, &activity_asset_from_position(&position));
    let stock_header_actions = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(6)
        .build();
    stock_header_actions.append(&transactions_button);
    stock_header_actions.append(&star);

    let header = HeaderBar::new();
    header.set_centering_policy(adw::CenteringPolicy::Strict);
    header.pack_end(&stock_header_actions);
    let header_title = adw::WindowTitle::new(&position.code, &position.name);
    header.set_title_widget(Some(&header_title));
    let shortcut_refresh = DetailShortcutRefresh::new();
    let header_overlay = Overlay::new();
    header_overlay.set_child(Some(&header));
    header_overlay.add_overlay(&shortcut_refresh.bar);

    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&header_overlay);
    toolbar.add_top_bar(&pull_refresh.revealer);
    toolbar.set_content(Some(&scroller));
    let toolbar_overlay = Overlay::new();
    toolbar_overlay.set_child(Some(&toolbar));
    toolbar_overlay.add_overlay(&pull_refresh.visual_revealer);
    let page = NavigationPage::builder()
        .title(&position.code)
        .tag("stock-detail")
        .child(&toolbar_overlay)
        .build();

    let detail = DetailRefs {
        app: refs.clone(),
        position_id: position.id,
        provider_symbol: position.provider_symbol.clone(),
        currency: position.currency.clone(),
        chart,
        current_price,
        day_change,
        quote_status,
        market_value,
        total_gain,
        base_currency: base,
        usd_cad,
        range_return,
        range_high_low,
        history_status,
        active_range: active_range.clone(),
        pull_refresh: pull_refresh.clone(),
        shortcut_refresh: shortcut_refresh.clone(),
        generation: Rc::new(Cell::new(0)),
    };
    let dividend_detail = DividendDetailRefs {
        app: refs.clone(),
        position_id: position.id,
        provider_symbol: position.provider_symbol.clone(),
        currency: position.currency.clone(),
        annual_income: dividend_annual,
        per_share: dividend_per_share,
        yield_label: dividend_yield,
        status: dividend_status,
        list: dividend_list,
        pull_refresh: pull_refresh.clone(),
        shortcut_refresh: shortcut_refresh.clone(),
    };

    connect_stock_picture_control(
        &stock_picture_control,
        refs.clone(),
        position.provider_symbol.clone(),
    );

    {
        let refs = refs.clone();
        let position = position.clone();
        add_activity.connect_clicked(move |button| {
            let Some(root) = button.root() else {
                return;
            };
            let Ok(window) = root.downcast::<ApplicationWindow>() else {
                return;
            };
            present_add_activity_dialog_with_context(
                &window,
                refs.clone(),
                Some(position.account_id),
                Some(activity_asset_from_position(&position)),
                None,
            );
        });
    }

    {
        let refs = refs.clone();
        let transaction_filter = format!("{} {}", position.code, position.account_name);
        transactions_button.connect_clicked(move |button| {
            let Some(root) = button.root() else {
                return;
            };
            let Ok(window) = root.downcast::<ApplicationWindow>() else {
                return;
            };
            present_transactions_dialog_with_filter(
                &window,
                refs.clone(),
                Some(&transaction_filter),
            );
        });
    }

    for (button, range) in range_buttons {
        let detail = detail.clone();
        let active_range = active_range.clone();
        button.connect_toggled(move |button| {
            if button.is_active() {
                active_range.set(range);
                load_history_range(detail.clone(), range, false, false);
            }
        });
    }

    {
        let detail = detail.clone();
        let dividend_detail = dividend_detail.clone();
        let active_range = active_range.clone();
        install_detail_pull_to_refresh(
            &scroller,
            &header,
            pull_refresh.clone(),
            2,
            Rc::new(move || {
                load_history_range(detail.clone(), active_range.get(), true, true);
                load_position_dividends(dividend_detail.clone(), true);
            }),
        );
    }

    {
        let detail = detail.clone();
        let dividend_detail = dividend_detail.clone();
        let active_range = active_range.clone();
        let shortcut_refresh = shortcut_refresh.clone();
        let pull_refresh = pull_refresh.clone();
        *refs.detail_refresh.borrow_mut() = Some(Rc::new(move || {
            if pull_refresh.pending.get() > 0 || !shortcut_refresh.begin(2) {
                return;
            }
            load_history_range(detail.clone(), active_range.get(), true, true);
            load_position_dividends(dividend_detail.clone(), true);
        }));
    }

    refs.navigation.push(&page);
    load_history_range(detail, HistoryRange::OneMonth, false, true);
    load_position_dividends(dividend_detail, false);
}

fn load_position_dividends(detail: DividendDetailRefs, announce: bool) {
    let cached = detail
        .app
        .state
        .database
        .dividend_events(&detail.provider_symbol)
        .unwrap_or_default();
    let had_cache = !cached.is_empty();
    update_position_dividend_widgets(&detail, &cached, false);

    let needs_refresh = announce
        || detail
            .app
            .state
            .database
            .dividends_fetched_at(&detail.provider_symbol)
            .ok()
            .flatten()
            .map(|timestamp| {
                current_unix_timestamp().saturating_sub(timestamp) >= DIVIDEND_CACHE_SECONDS
            })
            .unwrap_or(true);
    if !needs_refresh {
        detail.status.set_label(if had_cache {
            "Trailing 12 months · estimated at current shares"
        } else {
            "No distributions found"
        });
        if announce {
            complete_detail_refresh(&detail.pull_refresh, &detail.shortcut_refresh);
        }
        return;
    }

    detail.status.set_label(if had_cache {
        "Cached dividend history · updating"
    } else {
        "Loading dividend history"
    });

    let symbol = detail.provider_symbol.clone();
    let (sender, receiver) = mpsc::channel::<DividendDetailLoadResult>();
    std::thread::spawn(move || {
        let result = market_data::dividends(&symbol).map_err(|error| error.to_string());
        let _ = sender.send(DividendDetailLoadResult { result, announce });
    });

    let receiver = Rc::new(RefCell::new(receiver));
    glib::timeout_add_local(Duration::from_millis(75), move || {
        let Ok(load) = receiver.borrow().try_recv() else {
            return glib::ControlFlow::Continue;
        };
        if load.announce {
            complete_detail_refresh(&detail.pull_refresh, &detail.shortcut_refresh);
        }
        match load.result {
            Ok(history) => {
                let currency = history
                    .currency
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or(&detail.currency);
                let _ = detail.app.state.database.replace_dividend_events(
                    &detail.provider_symbol,
                    currency,
                    &history.events,
                );
                let _ = detail
                    .app
                    .state
                    .database
                    .replace_split_events(&detail.provider_symbol, &history.splits);
                let _ = detail
                    .app
                    .state
                    .database
                    .set_dividends_fetched(&detail.provider_symbol);
                let _ = detail.app.state.database.sync_positions_from_activity();
                let _ = detail.app.state.database.sync_paid_dividends_to_cash();
                refresh_with_loaded_crossfade(detail.app.clone());
                update_position_dividend_widgets(&detail, &history.events, true);
                crossfade_loaded_label(
                    &detail.status,
                    if history.events.is_empty() {
                        "No distributions found"
                    } else {
                        "Updated just now · estimated at current shares"
                    },
                );
            }
            Err(error) => {
                if had_cache {
                    detail
                        .status
                        .set_label("Update failed · showing cached dividend history");
                } else {
                    detail.status.set_label(&error);
                }
                if load.announce {
                    detail
                        .app
                        .toast_overlay
                        .add_toast(Toast::new("Could not refresh dividend data"));
                }
            }
        }
        glib::ControlFlow::Break
    });
}

fn update_position_dividend_widgets(detail: &DividendDetailRefs, events: &[DividendEvent], animate: bool) {
    clear_list(&detail.list);
    let Ok(Some(position)) = detail.app.state.database.position(detail.position_id) else {
        return;
    };
    let now = current_unix_timestamp();
    let cutoff = now.saturating_sub(366 * 24 * 60 * 60);
    let trailing_per_share = events
        .iter()
        .filter(|event| event.timestamp >= cutoff && event.timestamp <= now)
        .map(|event| event.amount)
        .sum::<f64>();
    let annual_income = trailing_per_share * position.shares;
    let annual_text = format_currency(annual_income, &position.currency);
    let per_share_text = format_currency(trailing_per_share, &position.currency);
    let yield_text = match position.last_price {
        Some(price) if price > f64::EPSILON => {
            format!("{:.2}%", trailing_per_share / price * 100.0)
        }
        _ => "—".into(),
    };
    if animate {
        let detail_for_update = detail.clone();
        crossfade_loaded_labels(
            vec![
                (detail.annual_income.clone(), annual_text.clone()),
                (detail.per_share.clone(), per_share_text.clone()),
                (detail.yield_label.clone(), yield_text.clone()),
            ],
            move || {
                detail_for_update.annual_income.set_label(&annual_text);
                detail_for_update.per_share.set_label(&per_share_text);
                detail_for_update.yield_label.set_label(&yield_text);
            },
        );
    } else {
        detail.annual_income.set_label(&annual_text);
        detail.per_share.set_label(&per_share_text);
        detail.yield_label.set_label(&yield_text);
    }

    let mut recent = events.to_vec();
    recent.sort_by(|left, right| right.timestamp.cmp(&left.timestamp));
    recent.truncate(8);
    for event in recent {
        let currency = if event.currency.trim().is_empty() {
            position.currency.as_str()
        } else {
            event.currency.as_str()
        };
        let row = ActionRow::builder()
            .title(&format_distribution_date(event.timestamp))
            .subtitle(&format!("{} per share", format_currency(event.amount, currency)))
            .build();
        row.set_activatable(false);
        row.add_suffix(
            &Label::builder()
                .label(&format!(
                    "≈ {}",
                    format_currency(event.amount * position.shares, currency)
                ))
                .halign(Align::End)
                .css_classes(["dim-label"])
                .build(),
        );
        detail.list.append(&row);
    }
}

fn detail_value_row(title: &str, value: &str) -> ActionRow {
    let row = ActionRow::builder().title(title).build();
    row.set_activatable(false);
    row.add_suffix(
        &Label::builder()
            .label(value)
            .halign(Align::End)
            .selectable(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(24)
            .css_classes(["dim-label"])
            .build(),
    );
    row
}

fn load_history_range(
    detail: DetailRefs,
    range: HistoryRange,
    announce: bool,
    force_refresh: bool,
) {
    let now = current_unix_timestamp();
    let minimum = range.minimum_timestamp(now);
    let cached = market_data::display_history_points(
        detail
            .app
            .state
            .database
            .history_points(&detail.provider_symbol, range.interval(), minimum)
            .unwrap_or_default(),
        range,
    );
    let had_cache = cached.len() >= 2;

    // Keep the last coherent visible snapshot in place during a manual
    // refresh. Reapplying persisted cache here can make the chart and range
    // percentage visibly jump backward until the fresh request completes.
    if !announce {
        if had_cache {
            detail
                .chart
                .set_points(cached.clone(), &detail.currency, range);
            update_history_summary(&detail, &cached, range, false);
            detail
                .history_status
                .set_label("Cached history");
        } else {
            detail.chart.set_message("Loading price history");
            detail.range_return.set_label("—");
            detail.range_high_low.set_label("Waiting for history");
            detail.day_change.set_label(&format!("Loading {} change…", range.label()));
            detail.day_change.add_css_class("dim-label");
            set_gain_class(&detail.day_change, 0.0);
            detail
                .history_status
                .set_label("Loading history");
        }
    }

    // Every range selection advances the generation, even when its cached data is
    // still fresh. That invalidates an older in-flight request so it cannot
    // overwrite the newly selected chart when it eventually finishes.
    let generation = detail.generation.get().saturating_add(1);
    detail.generation.set(generation);

    let needs_refresh = force_refresh
        || detail
            .app
            .state
            .database
            .history_needs_refresh(
                &detail.provider_symbol,
                range.key(),
                range.interval(),
                range.cache_seconds(),
            )
            .unwrap_or(true);
    if !needs_refresh {
        if announce {
            complete_detail_refresh(&detail.pull_refresh, &detail.shortcut_refresh);
        }
        return;
    }
    if had_cache {
        detail.history_status.set_label(if announce {
            "Refreshing history"
        } else {
            "Cached history · updating"
        });
    }

    let symbol = detail.provider_symbol.clone();
    let (sender, receiver) = mpsc::channel::<HistoryLoadResult>();
    std::thread::spawn(move || {
        let result = market_data::history(&symbol, range).map_err(|error| error.to_string());
        let _ = sender.send(HistoryLoadResult {
            generation,
            range,
            result,
            announce,
        });
    });

    let receiver = Rc::new(RefCell::new(receiver));
    glib::timeout_add_local(Duration::from_millis(75), move || {
        let Ok(load) = receiver.borrow().try_recv() else {
            return glib::ControlFlow::Continue;
        };
        if load.announce {
            complete_detail_refresh(&detail.pull_refresh, &detail.shortcut_refresh);
        }

        match load.result {
            Ok(history) => {
                let _ = detail.app.state.database.save_history(
                    &detail.provider_symbol,
                    load.range.interval(),
                    &history.points,
                );
                let _ = detail.app.state.database.set_history_fetched(
                    &detail.provider_symbol,
                    load.range.key(),
                    load.range.interval(),
                );
                if let Some(price) = history.current_price {
                    let _ = detail.app.state.database.update_quote(
                        detail.position_id,
                        price,
                        history.day_change_percent,
                        history.quote_timestamp,
                    );
                }
                refresh_with_loaded_crossfade(detail.app.clone());

                if detail.generation.get() == load.generation {
                    update_detail_position_metrics(&detail, true);
                    let currency = history
                        .currency
                        .as_deref()
                        .filter(|value| !value.trim().is_empty())
                        .unwrap_or(&detail.currency);
                    detail
                        .chart
                        .set_points(history.points.clone(), currency, load.range);
                    update_history_summary(&detail, &history.points, load.range, true);
                    crossfade_loaded_label(&detail.history_status, "Updated just now");
                    update_detail_quote(&detail, &history, true);
                }
            }
            Err(error) => {
                if detail.generation.get() == load.generation {
                    if had_cache {
                        detail.history_status.set_label(
                            "Update failed · showing cached history",
                        );
                    } else {
                        detail.chart.set_message("Price history is unavailable right now");
                        detail.day_change.set_label(history_range_unavailable_label(load.range));
                        detail.day_change.add_css_class("dim-label");
                        set_gain_class(&detail.day_change, 0.0);
                        detail.history_status.set_label(&error);
                    }
                    if load.announce {
                        detail
                            .app
                            .toast_overlay
                            .add_toast(Toast::new("Could not refresh price history"));
                    }
                }
            }
        }
        glib::ControlFlow::Break
    });
}

fn update_detail_position_metrics(detail: &DetailRefs, animate: bool) {
    let Ok(Some(position)) = detail.app.state.database.position(detail.position_id) else {
        return;
    };

    let market = converted_market_value(
        &position,
        &detail.base_currency,
        detail.usd_cad,
    )
    .map(|value| format_currency(value, &detail.base_currency))
    .or_else(|| {
        position
            .market_value()
            .map(|value| format_currency(value, &position.currency))
    })
    .unwrap_or_else(|| "—".into());
    let (gain_text, gain_value) = match converted_total_gain(
        &position,
        &detail.base_currency,
        detail.usd_cad,
    ) {
        Some(gain) => (format_signed_currency(gain, &detail.base_currency), gain),
        None => ("—".into(), 0.0),
    };
    if animate {
        let detail_for_update = detail.clone();
        crossfade_loaded_labels(
            vec![
                (detail.market_value.clone(), market.clone()),
                (detail.total_gain.clone(), gain_text.clone()),
            ],
            move || {
                detail_for_update.market_value.set_label(&market);
                detail_for_update.total_gain.set_label(&gain_text);
                set_gain_class(&detail_for_update.total_gain, gain_value);
            },
        );
    } else {
        detail.market_value.set_label(&market);
        detail.total_gain.set_label(&gain_text);
        set_gain_class(&detail.total_gain, gain_value);
    }
}

fn update_detail_quote(detail: &DetailRefs, history: &History, animate: bool) {
    let Some(price) = history.current_price else {
        return;
    };
    let price_text = format_currency(price, &detail.currency);
    let state = market_data::quote_state_label(None, history.quote_timestamp, current_unix_timestamp());
    let status_text = format!("{} · {}", state, relative_time(history.quote_timestamp));
    let update_day = detail.active_range.get() == HistoryRange::OneDay;
    let change = history.day_change_percent;
    let mut targets = vec![
        (detail.current_price.clone(), price_text.clone()),
        (detail.quote_status.clone(), status_text.clone()),
    ];
    if update_day {
        let day_text = match change {
            Some(change) => format!("{change:+.2}% today"),
            None => "Today's change unavailable".into(),
        };
        targets.push((detail.day_change.clone(), day_text));
    }
    let apply = {
        let detail = detail.clone();
        move || {
            detail.current_price.set_label(&price_text);
            set_quote_status(&detail.quote_status, &status_text);
            if update_day {
                detail.day_change.remove_css_class("dim-label");
                match change {
                    Some(change) => {
                        detail.day_change.set_label(&format!("{change:+.2}% today"));
                        set_gain_class(&detail.day_change, change);
                    }
                    None => {
                        detail.day_change.set_label("Today's change unavailable");
                        detail.day_change.add_css_class("dim-label");
                        set_gain_class(&detail.day_change, 0.0);
                    }
                }
            }
        }
    };
    if animate {
        crossfade_loaded_labels(targets, apply);
    } else {
        apply();
    }
}

fn update_history_summary(
    detail: &DetailRefs,
    points: &[PricePoint],
    range: HistoryRange,
    animate: bool,
) {
    let Some(first) = points.first() else {
        if animate {
            crossfade_loaded_label(&detail.range_return, "—");
            crossfade_loaded_label(&detail.range_high_low, "No history available");
            crossfade_loaded_label(&detail.day_change, history_range_unavailable_label(range));
        } else {
            detail.range_return.set_label("—");
            detail.range_high_low.set_label("No history available");
            detail.day_change.set_label(history_range_unavailable_label(range));
        }
        detail.day_change.add_css_class("dim-label");
        set_gain_class(&detail.day_change, 0.0);
        return;
    };
    let Some(last) = points.last() else {
        return;
    };
    let change = if first.close.abs() < f64::EPSILON {
        0.0
    } else {
        (last.close - first.close) / first.close * 100.0
    };
    let range_return = format!("{change:+.2}% over {}", range.label());
    let day_change = format!("{change:+.2}% {}", history_range_change_suffix(range));
    let low = points.iter().map(|point| point.close).fold(f64::INFINITY, f64::min);
    let high = points.iter().map(|point| point.close).fold(f64::NEG_INFINITY, f64::max);
    let high_low = format!(
        "Low {} · High {} · {} points",
        format_currency(low, &detail.currency),
        format_currency(high, &detail.currency),
        points.len()
    );

    let targets = vec![
        (detail.range_return.clone(), range_return.clone()),
        (detail.range_high_low.clone(), high_low.clone()),
        (detail.day_change.clone(), day_change.clone()),
    ];
    let apply = {
        let detail = detail.clone();
        move || {
            detail.range_return.set_label(&range_return);
            detail.range_high_low.set_label(&high_low);
            detail.day_change.remove_css_class("dim-label");
            detail.day_change.set_label(&day_change);
            set_gain_class(&detail.range_return, change);
            set_gain_class(&detail.day_change, change);
        }
    };
    if animate {
        crossfade_loaded_labels(targets, apply);
    } else {
        apply();
    }
}


fn present_add_account_dialog(parent: &ApplicationWindow, refs: UiRefs) {
    let name = EntryRow::new();
    name.set_title("Account name");

    let currency = ComboRow::new();
    currency.set_title("Currency");
    let currency_model = string_model(&["CAD", "USD"]);
    currency.set_model(Some(&currency_model));
    currency.set_selected(if base_currency(&refs.state) == "USD" { 1 } else { 0 });

    let group = PreferencesGroup::new();
    group.add(&name);
    group.add(&currency);

    let body = dialog_body();
    body.append(&group);
    let scroller = dialog_scroller(&body, 480);
    scroller.set_vexpand(true);

    let add = Button::builder()
        .label("Add Account")
        .css_classes(["suggested-action", "pill"])
        .halign(Align::Fill)
        .hexpand(true)
        .sensitive(false)
        .build();
    let actions = dialog_bottom_action(&add);
    let page = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(8)
        .build();
    page.append(&scroller);
    page.append(&actions);

    let header = HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Add Account", "")));
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));

    let dialog = Dialog::builder()
        .title("Add Account")
        .content_width(480)
        .content_height(320)
        .child(&toolbar)
        .build();

    {
        let add = add.clone();
        name.connect_changed(move |entry| {
            add.set_sensitive(!entry.text().trim().is_empty());
        });
    }

    {
        let dialog = dialog.clone();
        let refs = refs.clone();
        let name = name.clone();
        let currency = currency.clone();
        add.connect_clicked(move |_| {
            let account_name = name.text().trim().to_string();
            if account_name.is_empty() {
                refs.toast_overlay.add_toast(Toast::new("Enter an account name"));
                return;
            }
            if currency.selected() > 1 {
                refs.toast_overlay.add_toast(Toast::new("Choose CAD or USD"));
                return;
            }
            let account = NewAccount {
                name: account_name.clone(),
                currency: currency_at(currency.selected()).into(),
            };
            match refs.state.database.add_account(&account) {
                Ok(account_id) => {
                    let _ = refs
                        .state
                        .database
                        .set_setting(LAST_ACCOUNT_ID_KEY, &account_id.to_string());
                    if refs
                        .state
                        .database
                        .setting(BASE_CURRENCY_KEY)
                        .ok()
                        .flatten()
                        .is_none()
                    {
                        let _ = refs
                            .state
                            .database
                            .set_setting(BASE_CURRENCY_KEY, &account.currency);
                    }
                    refs.refresh();
                    refs.toast_overlay
                        .add_toast(Toast::new(&format!("Added {account_name}")));
                    dialog.close();
                }
                Err(error) => refs
                    .toast_overlay
                    .add_toast(Toast::new(&format!("Could not add account: {error}"))),
            }
        });
    }

    dialog.present(Some(parent));
    name.grab_focus();
}

fn present_manage_cash_dialog(parent: &ApplicationWindow, refs: UiRefs, account: Account) {
    let base = base_currency(&refs.state);
    let usd_cad = refs
        .state
        .database
        .fx_rate(USD_CAD_PAIR)
        .ok()
        .flatten()
        .map(|rate| rate.rate);
    let converted = convert_currency(account.cash, &account.currency, &base, usd_cad);
    let balance = converted
        .map(|value| format_currency(value, &base))
        .unwrap_or_else(|| format_currency(account.cash, &account.currency));

    let value = Label::builder()
        .label(&balance)
        .halign(Align::Center)
        .css_classes(["title-1"])
        .build();
    let account_label = Label::builder()
        .label(&format!("{} · {}", account.name, account.currency))
        .halign(Align::Center)
        .css_classes(["dim-label"])
        .build();
    let hero = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(4)
        .margin_top(8)
        .margin_bottom(4)
        .build();
    hero.append(&value);
    hero.append(&account_label);

    let add = Button::builder()
        .label("Add Cash")
        .css_classes(["suggested-action", "pill"])
        .hexpand(true)
        .build();
    let withdraw = Button::builder()
        .label("Withdraw")
        .css_classes(["pill"])
        .hexpand(true)
        .sensitive(account.cash > 0.005)
        .build();
    let actions = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .halign(Align::Fill)
        .build();
    actions.append(&add);
    actions.append(&withdraw);

    let history_group = PreferencesGroup::builder().title("Cash Activity").build();
    let mut entries = refs
        .state
        .database
        .load_cash_entries()
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry.account_id == account.id)
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| (std::cmp::Reverse(entry.occurred_at), std::cmp::Reverse(entry.id)));
    if entries.is_empty() {
        history_group.add(
            &ActionRow::builder()
                .title("No cash activity yet")
                .build(),
        );
    } else {
        for entry in entries {
            let title = if entry.kind == "DEPOSIT" && entry.amount < 0.0 {
                "Cash withdrawn".to_string()
            } else {
                entry.description.clone()
            };
            let row = ActionRow::builder()
                .title(&title)
                .subtitle(format_distribution_date(entry.occurred_at))
                .build();
            let amount = Label::builder()
                .label(&format_signed_currency(entry.amount, &entry.currency))
                .valign(Align::Center)
                .css_classes(["heading"])
                .build();
            if entry.amount > 0.0000001 {
                amount.add_css_class("success");
            } else if entry.amount < -0.0000001 {
                amount.add_css_class("error");
            }
            row.add_suffix(&amount);
            history_group.add(&row);
        }
    }

    let body = dialog_body();
    body.append(&hero);
    body.append(&actions);
    body.append(&history_group);
    let scroller = dialog_scroller(&body, 520);
    let header = HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Cash", "")));
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scroller));
    let dialog = Dialog::builder()
        .title("Cash")
        .content_width(520)
        .content_height(560)
        .child(&toolbar)
        .build();

    {
        let dialog = dialog.clone();
        let refs = refs.clone();
        let account = account.clone();
        let parent = parent.clone();
        add.connect_clicked(move |_| {
            dialog.close();
            present_add_cash_dialog(&parent, refs.clone(), account.clone());
        });
    }
    {
        let dialog = dialog.clone();
        let refs = refs.clone();
        let account = account.clone();
        let parent = parent.clone();
        withdraw.connect_clicked(move |_| {
            dialog.close();
            present_withdraw_cash_dialog(&parent, refs.clone(), account.clone());
        });
    }

    dialog.present(Some(parent));
}

fn present_withdraw_cash_dialog(parent: &ApplicationWindow, refs: UiRefs, account: Account) {
    let amount = money_entry_row(&format!("Amount ({})", account.currency), 0.0);
    let date = DateChooser::today();
    let balance = ActionRow::builder()
        .title(&account.name)
        .subtitle(&format!("{} cash available", format_currency(account.cash, &account.currency)))
        .build();
    let group = PreferencesGroup::new();
    group.add(&balance);
    group.add(&amount);
    group.add(&date.row);

    let body = dialog_body();
    body.append(&group);
    let scroller = dialog_scroller(&body, 480);
    // GtkScrolledWindow does not propagate its child's natural height by default.
    // In this fixed-height dialog that left the form at its minimum allocation,
    // clipping the balance subtitle and hiding Amount/Date while the rest of the
    // dialog remained empty. Let the form own the available vertical space.
    scroller.set_vexpand(true);
    let withdraw = Button::builder()
        .label("Withdraw Cash")
        .css_classes(["destructive-action", "pill"])
        .halign(Align::Fill)
        .hexpand(true)
        .build();
    let actions = dialog_bottom_action(&withdraw);
    let page = GtkBox::builder().orientation(Orientation::Vertical).spacing(8).build();
    page.append(&scroller);
    page.append(&actions);
    let header = HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Withdraw Cash", "")));
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    let dialog = Dialog::builder()
        .title("Withdraw Cash")
        .content_width(480)
        .content_height(350)
        .child(&toolbar)
        .build();

    {
        let dialog = dialog.clone();
        let refs = refs.clone();
        let account_id = account.id;
        let amount_for_save = amount.clone();
        withdraw.connect_clicked(move |_| {
            let Some(amount_value) = money_value(&amount_for_save).filter(|value| *value > 0.0) else {
                refs.toast_overlay.add_toast(Toast::new("Cash amount must be greater than zero"));
                return;
            };
            let date_value = date.value();
            let Ok(timestamp) = activity_timestamp(&date_value) else {
                refs.toast_overlay.add_toast(Toast::new("Choose a valid date"));
                return;
            };
            if date_is_future(&date_value) {
                refs.toast_overlay.add_toast(Toast::new("Cash date cannot be in the future"));
                return;
            }
            match refs.state.database.withdraw_cash(account_id, amount_value, timestamp) {
                Ok(_) => {
                    refs.refresh();
                    refresh_portfolio_history_async(refs.clone(), false);
                    dialog.close();
                }
                Err(error) => refs.toast_overlay.add_toast(Toast::new(&format!("Could not withdraw cash: {error}"))),
            }
        });
    }

    dialog.present(Some(parent));
    amount.grab_focus();
}

fn present_add_cash_dialog(parent: &ApplicationWindow, refs: UiRefs, account: Account) {
    let amount = money_entry_row(&format!("Amount ({})", account.currency), 0.0);

    let date = DateChooser::today();

    let balance = ActionRow::builder()
        .title(&account.name)
        .subtitle(&format!(
            "{} cash available",
            format_currency(account.cash, &account.currency)
        ))
        .build();

    let group = PreferencesGroup::new();
    group.add(&balance);
    group.add(&amount);
    group.add(&date.row);

    let body = dialog_body();
    body.append(&group);
    let scroller = dialog_scroller(&body, 480);
    // Keep the complete form visible above the bottom action. Without vexpand,
    // GtkScrolledWindow collapses to its minimum height and the dialog shows the
    // exact broken layout where only the account row is visible.
    scroller.set_vexpand(true);

    let save = Button::builder()
        .label("Add Cash")
        .css_classes(["suggested-action", "pill"])
        .halign(Align::Fill)
        .hexpand(true)
        .build();
    let actions = dialog_bottom_action(&save);
    let page = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(8)
        .build();
    page.append(&scroller);
    page.append(&actions);
    let header = HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Add Cash", "")));
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    let dialog = Dialog::builder()
        .title("Add Cash")
        .content_width(480)
        .content_height(350)
        .child(&toolbar)
        .build();

    {
        let dialog = dialog.clone();
        let refs = refs.clone();
        let account_id = account.id;
        let amount_for_save = amount.clone();
        save.connect_clicked(move |_| {
            let Some(amount_value) = money_value(&amount_for_save).filter(|value| *value > 0.0) else {
                refs.toast_overlay
                    .add_toast(Toast::new("Cash amount must be greater than zero"));
                return;
            };
            let date_value = date.value();
            let Ok(timestamp) = activity_timestamp(&date_value) else {
                refs.toast_overlay
                    .add_toast(Toast::new("Choose a valid date"));
                return;
            };
            if date_is_future(&date_value) {
                refs.toast_overlay
                    .add_toast(Toast::new("Cash date cannot be in the future"));
                return;
            }
            match refs.state.database.add_cash(account_id, amount_value, timestamp) {
                Ok(_) => {
                    refs.refresh();
                    refresh_portfolio_history_async(refs.clone(), false);
                    refs.toast_overlay.add_toast(Toast::new("Cash added"));
                    dialog.close();
                }
                Err(error) => refs.toast_overlay.add_toast(Toast::new(&format!(
                    "Could not add cash: {error}"
                ))),
            }
        });
    }

    dialog.present(Some(parent));
    amount.grab_focus();
}

fn present_transfer_dialog(parent: &ApplicationWindow, refs: UiRefs, from_account: Account) {
    let accounts = refs.state.database.load_accounts().unwrap_or_default();
    let destinations = accounts
        .into_iter()
        .filter(|account| account.id != from_account.id)
        .collect::<Vec<_>>();
    if destinations.is_empty() {
        refs.toast_overlay
            .add_toast(Toast::new("Create another account before transferring"));
        return;
    }

    let positions = refs
        .state
        .database
        .load_positions()
        .unwrap_or_default()
        .into_iter()
        .filter(|position| position.account_id == from_account.id)
        .collect::<Vec<_>>();

    let from_row = ActionRow::builder()
        .title(&from_account.name)
        .subtitle(&format!(
            "{} cash available",
            format_currency(from_account.cash, &from_account.currency)
        ))
        .build();

    let transfer_type = ComboRow::new();
    transfer_type.set_title("Transfer");
    let type_model = string_model(&["Cash", "Holding"]);
    transfer_type.set_model(Some(&type_model));
    transfer_type.set_selected(0);

    let to_account = ComboRow::new();
    to_account.set_title("To Account");
    let destination_model = StringList::new(&[]);
    for account in &destinations {
        destination_model.append(&account_choice_label(account));
    }
    to_account.set_model(Some(&destination_model));
    to_account.set_selected(0);

    let holding = ComboRow::new();
    holding.set_title("Holding");
    let holding_model = StringList::new(&[]);
    for position in &positions {
        holding_model.append(&format!(
            "{} · {} shares",
            position.code,
            trim_number(position.shares)
        ));
    }
    holding.set_model(Some(&holding_model));
    holding.set_selected(0);
    holding.set_visible(false);

    let amount = money_entry_row(&format!("Amount ({})", from_account.currency), 0.0);

    let date = DateChooser::today();

    let group = PreferencesGroup::new();
    group.add(&from_row);
    group.add(&transfer_type);
    group.add(&to_account);
    group.add(&holding);
    group.add(&amount);
    group.add(&date.row);

    let body = dialog_body();
    body.append(&group);
    let scroller = dialog_scroller(&body, 520);
    scroller.set_vexpand(true);

    let save = Button::builder()
        .label("Transfer")
        .css_classes(["suggested-action", "pill"])
        .halign(Align::Fill)
        .hexpand(true)
        .build();
    let actions = dialog_bottom_action(&save);
    let page = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(8)
        .build();
    page.append(&scroller);
    page.append(&actions);

    let header = HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Transfer", "")));
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    let dialog = Dialog::builder()
        .title("Transfer")
        .content_width(520)
        .content_height(430)
        .child(&toolbar)
        .build();
    install_escape_to_close(&dialog);

    {
        let amount = amount.clone();
        let holding = holding.clone();
        let from_currency = from_account.currency.clone();
        transfer_type.connect_selected_notify(move |row| {
            let is_holding = row.selected() == 1;
            holding.set_visible(is_holding);
            if is_holding {
                amount.set_title("Shares");
            } else {
                amount.set_title(&format!("Amount ({from_currency})"));
            }
        });
    }

    {
        let dialog = dialog.clone();
        let refs = refs.clone();
        let from_account = from_account.clone();
        let destinations = destinations.clone();
        let positions = positions.clone();
        let amount_for_save = amount.clone();
        save.connect_clicked(move |_| {
            let Some(destination) = destinations.get(to_account.selected() as usize).cloned() else {
                refs.toast_overlay.add_toast(Toast::new("Choose a destination account"));
                return;
            };
            let date_value = date.value();
            let Ok(timestamp) = activity_timestamp(&date_value) else {
                refs.toast_overlay
                    .add_toast(Toast::new("Choose a valid date"));
                return;
            };
            if date_is_future(&date_value) {
                refs.toast_overlay
                    .add_toast(Toast::new("Transfer date cannot be in the future"));
                return;
            }

            if transfer_type.selected() == 0 {
                let Some(amount_value) = money_value(&amount_for_save).filter(|value| *value > 0.0) else {
                    refs.toast_overlay
                        .add_toast(Toast::new("Transfer amount must be greater than zero"));
                    return;
                };
                match refs.state.database.transfer_cash(
                    from_account.id,
                    destination.id,
                    amount_value,
                    timestamp,
                ) {
                    Ok(()) => {
                        refs.refresh();
                        refresh_portfolio_history_async(refs.clone(), false);
                        refs.toast_overlay.add_toast(Toast::new("Cash transferred"));
                        dialog.close();
                    }
                    Err(error) => refs.toast_overlay.add_toast(Toast::new(&format!(
                        "Could not transfer cash: {error}"
                    ))),
                }
            } else {
                if positions.is_empty() {
                    refs.toast_overlay
                        .add_toast(Toast::new("This account has no holdings to transfer"));
                    return;
                }
                let Some(position) = positions.get(holding.selected() as usize).cloned() else {
                    refs.toast_overlay.add_toast(Toast::new("Choose a holding"));
                    return;
                };
                let Some(share_count) = shares_value(&amount_for_save) else {
                    refs.toast_overlay
                        .add_toast(Toast::new("Enter a valid number of shares"));
                    return;
                };
                match refs.state.database.transfer_holding(
                    from_account.id,
                    destination.id,
                    &position.provider_symbol,
                    share_count,
                    &date_value,
                    timestamp,
                ) {
                    Ok(()) => {
                        refs.refresh();
                        refresh_portfolio_history_async(refs.clone(), false);
                        refs.toast_overlay
                            .add_toast(Toast::new(&format!("Transferred {}", position.code)));
                        dialog.close();
                    }
                    Err(error) => refs.toast_overlay.add_toast(Toast::new(&format!(
                        "Could not transfer holding: {error}"
                    ))),
                }
            }
        });
    }

    dialog.present(Some(parent));
    amount.grab_focus();
}

fn present_edit_account_dialog(parent: &ApplicationWindow, refs: UiRefs, account: Account) {
    let name = EntryRow::new();
    name.set_title("Account name");
    name.set_text(&account.name);

    let currency = ComboRow::new();
    currency.set_title("Currency");
    let currency_model = string_model(&["CAD", "USD"]);
    currency.set_model(Some(&currency_model));
    currency.set_selected(if account.currency == "USD" { 1 } else { 0 });
    let currency_locked = refs
        .state
        .database
        .account_cash_entry_count(account.id)
        .unwrap_or(0)
        > 0
        || refs
            .state
            .database
            .account_transaction_count(account.id)
            .unwrap_or(0)
            > 0;
    if currency_locked {
        currency.set_sensitive(false);
        currency.set_subtitle("Currency is fixed after cash or activity is recorded");
    }

    let group = PreferencesGroup::new();
    group.add(&name);
    group.add(&currency);

    let body = dialog_body();
    body.append(&group);
    let scroller = dialog_scroller(&body, 480);
    scroller.set_vexpand(true);

    let save = Button::builder()
        .label("Save")
        .css_classes(["suggested-action", "pill"])
        .halign(Align::Fill)
        .hexpand(true)
        .build();
    let actions = dialog_bottom_action(&save);
    let page = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(8)
        .build();
    page.append(&scroller);
    page.append(&actions);

    let header = HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Edit Account", "")));
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    let dialog = Dialog::builder()
        .title("Edit Account")
        .content_width(480)
        .content_height(340)
        .child(&toolbar)
        .build();

    {
        let save = save.clone();
        name.connect_changed(move |entry| {
            save.set_sensitive(!entry.text().trim().is_empty());
        });
    }

    {
        let dialog = dialog.clone();
        let refs = refs.clone();
        let account_id = account.id;
        save.connect_clicked(move |_| {
            let account_name = name.text().trim().to_string();
            if account_name.is_empty() {
                refs.toast_overlay.add_toast(Toast::new("Enter an account name"));
                return;
            }
            match refs.state.database.update_account(
                account_id,
                &account_name,
                currency_at(currency.selected()),
            ) {
                Ok(()) => {
                    refs.refresh();
                    refs.toast_overlay.add_toast(Toast::new("Account updated"));
                    dialog.close();
                }
                Err(error) => refs
                    .toast_overlay
                    .add_toast(Toast::new(&format!("Could not update account: {error}"))),
            }
        });
    }

    dialog.present(Some(parent));
}

fn validate_transaction_removal(
    refs: &UiRefs,
    removed: &Transaction,
) -> Result<(), &'static str> {
    let mut transactions = refs
        .state
        .database
        .load_transactions()
        .unwrap_or_default()
        .into_iter()
        .filter(|transaction| {
            transaction.id != removed.id
                && transaction.account_id == removed.account_id
                && transaction
                    .provider_symbol
                    .eq_ignore_ascii_case(&removed.provider_symbol)
        })
        .collect::<Vec<_>>();
    transactions.sort_by_key(|transaction| {
        (
            transaction.timestamp,
            activity_sort_priority(&transaction.transaction_type),
            transaction.id,
        )
    });

    if let Some(opening) = transactions
        .iter()
        .find(|transaction| transaction.transaction_type == "OPEN")
    {
        if transactions.iter().any(|transaction| {
            transaction.transaction_type != "OPEN" && transaction.timestamp < opening.timestamp
        }) {
            return Err("Delete or move the earlier transaction first");
        }
    }

    let mut events = transactions
        .into_iter()
        .map(|transaction| {
            (
                transaction.timestamp,
                activity_sort_priority(&transaction.transaction_type),
                transaction.id,
                transaction.transaction_type,
                transaction.shares,
            )
        })
        .collect::<Vec<_>>();
    for split in refs
        .state
        .database
        .split_events(&removed.provider_symbol)
        .unwrap_or_default()
    {
        events.push((split.timestamp, activity_sort_priority("SPLIT"), i64::MIN, "SPLIT".into(), split.ratio));
    }
    events.sort_by_key(|event| (event.0, event.1, event.2));

    let mut held = 0.0;
    for (_, _, _, kind, amount) in events {
        match kind.as_str() {
            "SELL" => {
                held -= amount;
                if held < -0.0005 {
                    return Err("Delete later sell transactions first");
                }
            }
            "BUY" | "OPEN" => held += amount,
            "SPLIT" => held *= amount,
            _ => {}
        }
    }
    Ok(())
}

fn install_escape_to_close(dialog: &Dialog) {
    let controller = EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let dialog_weak = dialog.downgrade();
    controller.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.close();
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    dialog.add_controller(controller);
}

#[derive(Clone, Debug, Default)]
struct TransactionsFilterState {
    query: String,
    kind: u32,
    account_id: Option<i64>,
}

fn present_transactions_dialog(parent: &ApplicationWindow, refs: UiRefs) {
    present_transactions_dialog_with_filter(parent, refs, None);
}

fn present_transactions_dialog_with_filter(
    parent: &ApplicationWindow,
    refs: UiRefs,
    initial_filter: Option<&str>,
) {
    // Transactions uses date sections containing their own boxed lists. The
    // outer list stays visually transparent so date headings sit above each
    // card instead of looking like another transaction row.
    let list = ListBox::builder()
        .selection_mode(SelectionMode::None)
        .css_classes(["transactions-list"])
        .build();
    list.set_valign(Align::Start);
    list.set_vexpand(false);
    let filter = SearchEntry::builder()
        .placeholder_text("Filter transactions")
        .build();
    filter.set_search_delay(150);
    if let Some(initial_filter) = initial_filter {
        filter.set_text(initial_filter);
    }

    let kind = ComboRow::new();
    kind.set_title("Type");
    let kind_model = string_model(&[
        "All Transactions",
        "Buys",
        "Sells",
        "Deposits",
        "Withdrawals",
        "Transfers",
        "Dividends",
    ]);
    kind.set_model(Some(&kind_model));
    kind.set_selected(0);

    let accounts = refs.state.database.load_accounts().unwrap_or_default();
    let account = ComboRow::new();
    account.set_title("Account");
    let account_model = StringList::new(&["All Accounts"]);
    for item in &accounts {
        account_model.append(&item.name);
    }
    account.set_model(Some(&account_model));
    account.set_selected(0);

    let filters = PreferencesGroup::new();
    filters.add(&kind);
    filters.add(&account);

    let empty = StatusPage::builder()
        .icon_name("view-list-symbolic")
        .title("No Matching Transactions")
        .description("Change the filters or search to see other transactions")
        .build();

    // Filters belong outside the results stack. An empty filter result should
    // replace only the list, never the controls needed to escape that state.
    let stack = Stack::builder()
        .transition_type(gtk::StackTransitionType::None)
        .build();
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&list, Some("list"));

    let content = dialog_body();
    content.append(&filter);
    content.append(&filters);
    content.append(&stack);
    let scroller = dialog_scroller(&content, 720);

    let header = HeaderBar::new();
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scroller));

    let dialog = Dialog::builder()
        .title("Transactions")
        .content_width(720)
        .content_height(680)
        .child(&toolbar)
        .build();
    install_escape_to_close(&dialog);

    let state = Rc::new(RefCell::new(TransactionsFilterState {
        query: filter.text().to_string(),
        kind: 0,
        account_id: None,
    }));
    rebuild_transactions_list(&list, &stack, &dialog, refs.clone(), state.clone());

    {
        let list = list.clone();
        let stack = stack.clone();
        let dialog = dialog.clone();
        let refs = refs.clone();
        let state = state.clone();
        filter.connect_search_changed(move |entry| {
            state.borrow_mut().query = entry.text().to_string();
            rebuild_transactions_list(&list, &stack, &dialog, refs.clone(), state.clone());
        });
    }
    {
        let list = list.clone();
        let stack = stack.clone();
        let dialog = dialog.clone();
        let refs = refs.clone();
        let state = state.clone();
        kind.connect_selected_notify(move |row| {
            state.borrow_mut().kind = row.selected();
            rebuild_transactions_list(&list, &stack, &dialog, refs.clone(), state.clone());
        });
    }
    {
        let list = list.clone();
        let stack = stack.clone();
        let dialog = dialog.clone();
        let refs = refs.clone();
        let state = state.clone();
        let accounts = accounts.clone();
        account.connect_selected_notify(move |row| {
            state.borrow_mut().account_id = if row.selected() == 0 {
                None
            } else {
                accounts.get(row.selected().saturating_sub(1) as usize).map(|item| item.id)
            };
            rebuild_transactions_list(&list, &stack, &dialog, refs.clone(), state.clone());
        });
    }

    dialog.present(Some(parent));
}

fn rebuild_transactions_list(
    list: &ListBox,
    stack: &Stack,
    manager: &Dialog,
    refs: UiRefs,
    filter_state: Rc<RefCell<TransactionsFilterState>>,
) {
    #[derive(Clone)]
    enum ActivityItem {
        Investment(Transaction),
        Cash(CashEntry, String),
    }

    impl ActivityItem {
        fn timestamp(&self) -> i64 {
            match self {
                Self::Investment(transaction) => transaction.timestamp,
                Self::Cash(entry, _) => entry.occurred_at,
            }
        }

        fn id(&self) -> i64 {
            match self {
                Self::Investment(transaction) => transaction.id,
                Self::Cash(entry, _) => entry.id,
            }
        }

        fn account_id(&self) -> i64 {
            match self {
                Self::Investment(transaction) => transaction.account_id,
                Self::Cash(entry, _) => entry.account_id,
            }
        }

        fn matches_kind(&self, kind: u32) -> bool {
            match kind {
                0 => true,
                1 => matches!(self, Self::Investment(transaction) if transaction.transaction_type == "BUY"),
                2 => matches!(self, Self::Investment(transaction) if transaction.transaction_type == "SELL"),
                3 => matches!(self, Self::Cash(entry, _) if entry.kind == "DEPOSIT" && entry.amount >= 0.0),
                4 => matches!(self, Self::Cash(entry, _) if entry.kind == "DEPOSIT" && entry.amount < 0.0),
                5 => matches!(self, Self::Cash(entry, _) if entry.kind == "TRANSFER")
                    || matches!(self, Self::Investment(transaction) if matches!(transaction.transaction_type.as_str(), "TRANSFER_IN" | "TRANSFER_OUT")),
                6 => matches!(self, Self::Cash(entry, _) if entry.kind == "DIVIDEND"),
                _ => true,
            }
        }

        fn search_text(&self) -> String {
            match self {
                Self::Investment(transaction) => format!(
                    "{} {} {} {} {} {}",
                    transaction.code,
                    transaction.name,
                    transaction.account_name,
                    transaction.trade_date,
                    transaction.transaction_type,
                    transaction.currency
                ),
                Self::Cash(entry, account_name) => {
                    let kind = if entry.kind == "DIVIDEND" {
                        "dividend distribution income"
                    } else if entry.kind == "TRANSFER" {
                        "transfer move cash account"
                    } else if entry.amount < 0.0 {
                        "withdrawal withdraw cash"
                    } else {
                        "deposit add cash"
                    };
                    format!(
                        "{} {} {} {} {}",
                        kind,
                        entry.description,
                        account_name,
                        format_distribution_date(entry.occurred_at),
                        entry.currency
                    )
                }
            }
            .to_ascii_lowercase()
        }
    }

    clear_list(list);
    let manager = <Dialog as Clone>::clone(manager);

    let accounts = refs.state.database.load_accounts().unwrap_or_default();
    let account_names = accounts
        .iter()
        .map(|account| (account.id, account.name.clone()))
        .collect::<HashMap<_, _>>();

    let mut activity = refs
        .state
        .database
        .load_transactions()
        .unwrap_or_default()
        .into_iter()
        .map(ActivityItem::Investment)
        .collect::<Vec<_>>();

    // Trade settlement rows are bookkeeping mirrors of BUY/SELL transactions.
    // Explicit deposits/withdrawals and paid dividends are first-class rows.
    activity.extend(
        refs.state
            .database
            .load_cash_entries()
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| matches!(entry.kind.as_str(), "DEPOSIT" | "DIVIDEND" | "TRANSFER"))
            .map(|entry| {
                let account_name = account_names
                    .get(&entry.account_id)
                    .cloned()
                    .unwrap_or_else(|| "Account".to_string());
                ActivityItem::Cash(entry, account_name)
            }),
    );

    activity.sort_by(|left, right| {
        right
            .timestamp()
            .cmp(&left.timestamp())
            .then_with(|| right.id().cmp(&left.id()))
    });

    let state = filter_state.borrow().clone();
    let tokens = state
        .query
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let filtered = activity
        .into_iter()
        .filter(|item| state.account_id.map(|id| item.account_id() == id).unwrap_or(true))
        .filter(|item| item.matches_kind(state.kind))
        .filter(|item| {
            if tokens.is_empty() {
                return true;
            }
            let haystack = item.search_text();
            tokens.iter().all(|token| haystack.contains(token.as_str()))
        })
        .collect::<Vec<_>>();

    stack.set_visible_child_name(if filtered.is_empty() { "empty" } else { "list" });
    if filtered.is_empty() {
        return;
    }

    let mut last_date = String::new();
    let mut date_group: Option<ListBox> = None;
    for item in filtered {
        let date_heading = format_distribution_date(item.timestamp());
        let row: ListBoxRow = match item {
            ActivityItem::Cash(entry, account_name) => {
                let is_dividend = entry.kind == "DIVIDEND";
                let is_transfer = entry.kind == "TRANSFER";
                let is_withdrawal = entry.kind == "DEPOSIT" && entry.amount < 0.0;
                let title = if is_dividend {
                    if entry.description.trim().is_empty() { "Dividend" } else { entry.description.as_str() }
                } else if is_transfer {
                    if entry.description.trim().is_empty() { "Transfer" } else { entry.description.as_str() }
                } else if is_withdrawal {
                    "Withdrawal"
                } else {
                    "Deposit"
                };
                let subtitle = if is_dividend || is_transfer {
                    format!("{} · {}", account_name, entry.currency)
                } else {
                    account_name.clone()
                };
                let row = ActionRow::builder().title(title).subtitle(&subtitle).build();
                let amount = Label::builder()
                    .label(&format_signed_currency(entry.amount, &entry.currency))
                    .valign(Align::Center)
                    .css_classes(["heading"])
                    .build();
                if entry.amount < -0.0000001 {
                    amount.add_css_class("error");
                } else {
                    amount.add_css_class("success");
                }
                row.add_suffix(&amount);

                if entry.kind == "DEPOSIT" {
                    let edit = Button::builder()
                        .icon_name("document-edit-symbolic")
                        .tooltip_text("Edit Transaction")
                        .css_classes(["flat", "circular"])
                        .valign(Align::Center)
                        .build();
                    let remove = Button::builder()
                        .icon_name("edit-delete-symbolic")
                        .tooltip_text("Delete Transaction")
                        .css_classes(["flat", "circular"])
                        .valign(Align::Center)
                        .build();
                    row.add_suffix(&edit);
                    row.add_suffix(&remove);

                    {
                        let manager = manager.clone();
                        let list = list.clone();
                        let stack = stack.clone();
                        let refs = refs.clone();
                        let filter_state = filter_state.clone();
                        let entry = entry.clone();
                        edit.connect_clicked(move |_| {
                            present_edit_cash_entry_dialog(
                                &manager,
                                refs.clone(),
                                entry.clone(),
                                list.clone(),
                                stack.clone(),
                                filter_state.clone(),
                            );
                        });
                    }
                    {
                        let manager = manager.clone();
                        let list = list.clone();
                        let stack = stack.clone();
                        let refs = refs.clone();
                        let filter_state = filter_state.clone();
                        let entry_id = entry.id;
                        let title = if is_withdrawal { "Delete withdrawal?" } else { "Delete deposit?" };
                        remove.connect_clicked(move |_| {
                            let confirm = AlertDialog::builder()
                                .heading(title)
                                .body("This recalculates the account cash balance")
                                .build();
                            confirm.add_response("cancel", "Cancel");
                            confirm.add_response("delete", "Delete");
                            confirm.set_default_response(Some("cancel"));
                            confirm.set_close_response("cancel");
                            confirm.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                            let refs = refs.clone();
                            let list = list.clone();
                            let stack = stack.clone();
                            let manager_for_callback = manager.clone();
                            let filter_state = filter_state.clone();
                            confirm.connect_response(Some("delete"), move |_, _| {
                                match refs.state.database.delete_cash_entry(entry_id) {
                                    Ok(true) => {
                                        rebuild_transactions_list(&list, &stack, &manager_for_callback, refs.clone(), filter_state.clone());
                                        refs.refresh();
                                        refresh_portfolio_history_async(refs.clone(), false);
                                    }
                                    Ok(false) => {}
                                    Err(error) => refs.toast_overlay.add_toast(Toast::new(&format!(
                                        "Could not delete transaction: {error}"
                                    ))),
                                }
                            });
                            confirm.present(Some(&manager));
                        });
                    }
                }
                row.set_vexpand(false);
                row.set_valign(Align::Start);
                row.upcast::<ListBoxRow>()
            }
            ActivityItem::Investment(transaction) => {
                let title = match transaction.transaction_type.as_str() {
                    "OPEN" => format!(
                        "Opening Position · {} {}",
                        trim_number(transaction.shares),
                        transaction.code
                    ),
                    "SELL" => format!("Sell {} {}", trim_number(transaction.shares), transaction.code),
                    "TRANSFER_OUT" => format!("Transfer Out {} {}", trim_number(transaction.shares), transaction.code),
                    "TRANSFER_IN" => format!("Transfer In {} {}", trim_number(transaction.shares), transaction.code),
                    _ => format!("Buy {} {}", trim_number(transaction.shares), transaction.code),
                };
                let mut subtitle = format!(
                    "{} · {}/share",
                    transaction.account_name,
                    format_currency(transaction.price, &transaction.currency)
                );
                if transaction.fees > 0.005 {
                    subtitle.push_str(&format!(
                        " · {} fee",
                        format_currency(transaction.fees, &transaction.currency)
                    ));
                }
                if matches!(transaction.transaction_type.as_str(), "TRANSFER_IN" | "TRANSFER_OUT") {
                    subtitle.push_str(" · account transfer");
                } else if transaction.settle_cash {
                    subtitle.push_str(" · cash");
                }
                let row = ActionRow::builder().title(&title).subtitle(&subtitle).build();

                let gross = transaction.shares * transaction.price;
                let total_value = match transaction.transaction_type.as_str() {
                    "SELL" => gross - transaction.fees,
                    "TRANSFER_OUT" | "TRANSFER_IN" => gross,
                    _ => gross + transaction.fees,
                };
                let total = Label::builder()
                    .label(&format_currency(total_value, &transaction.currency))
                    .css_classes(["dim-label"])
                    .valign(Align::Center)
                    .build();
                row.add_suffix(&total);

                let edit = Button::builder()
                    .icon_name("document-edit-symbolic")
                    .tooltip_text("Edit Transaction")
                    .css_classes(["flat", "circular"])
                    .valign(Align::Center)
                    .build();
                if !matches!(transaction.transaction_type.as_str(), "TRANSFER_IN" | "TRANSFER_OUT") {
                    row.add_suffix(&edit);
                }

                let remove = Button::builder()
                    .icon_name("edit-delete-symbolic")
                    .tooltip_text("Delete Transaction")
                    .css_classes(["flat", "circular"])
                    .valign(Align::Center)
                    .build();
                if !matches!(transaction.transaction_type.as_str(), "TRANSFER_IN" | "TRANSFER_OUT") {
                    row.add_suffix(&remove);
                }

                if !matches!(transaction.transaction_type.as_str(), "TRANSFER_IN" | "TRANSFER_OUT") {
                {
                    let manager = manager.clone();
                    let list = list.clone();
                    let stack = stack.clone();
                    let refs = refs.clone();
                    let transaction = transaction.clone();
                    let filter_state = filter_state.clone();
                    edit.connect_clicked(move |_| {
                        present_edit_transaction_dialog(
                            &manager,
                            refs.clone(),
                            transaction.clone(),
                            list.clone(),
                            stack.clone(),
                            filter_state.clone(),
                        );
                    });
                }

                {
                    let manager = manager.clone();
                    let list = list.clone();
                    let stack = stack.clone();
                    let refs = refs.clone();
                    let transaction = transaction.clone();
                    let transaction_id = transaction.id;
                    let transaction_code = transaction.code.clone();
                    let filter_state = filter_state.clone();
                    remove.connect_clicked(move |_| {
                        if let Err(message) = validate_transaction_removal(&refs, &transaction) {
                            refs.toast_overlay.add_toast(Toast::new(message));
                            return;
                        }
                        let confirm = AlertDialog::builder()
                            .heading(format!("Delete {} transaction?", transaction_code))
                            .body("This recalculates the holding and account cash")
                            .build();
                        confirm.add_response("cancel", "Cancel");
                        confirm.add_response("delete", "Delete");
                        confirm.set_default_response(Some("cancel"));
                        confirm.set_close_response("cancel");
                        confirm.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                        let refs = refs.clone();
                        let list = list.clone();
                        let stack = stack.clone();
                        let manager_for_callback = manager.clone();
                        let filter_state = filter_state.clone();
                        confirm.connect_response(Some("delete"), move |_, _| {
                            match refs.state.database.delete_transaction(transaction_id) {
                                Ok(true) => {
                                    let _ = refs.state.database.sync_paid_dividends_to_cash();
                                    rebuild_transactions_list(
                                        &list,
                                        &stack,
                                        &manager_for_callback,
                                        refs.clone(),
                                        filter_state.clone(),
                                    );
                                    refs.refresh();
                                    refresh_portfolio_history_async(refs.clone(), false);
                                }
                                Ok(false) => {}
                                Err(error) => refs.toast_overlay.add_toast(Toast::new(&format!(
                                    "Could not delete transaction: {error}"
                                ))),
                            }
                        });
                        confirm.present(Some(&manager));
                    });
                }
                }
                row.set_vexpand(false);
                row.set_valign(Align::Start);
                row.upcast::<ListBoxRow>()
            }
        };

        if date_heading != last_date {
            let header = Label::builder()
                .label(&date_heading)
                .halign(Align::Start)
                .margin_start(2)
                .margin_bottom(6)
                .css_classes(["heading", "dim-label"])
                .build();
            let group = positions_list();
            group.add_css_class("transaction-day-list");
            let section = GtkBox::builder()
                .orientation(Orientation::Vertical)
                .margin_top(if last_date.is_empty() { 0 } else { 8 })
                .margin_bottom(4)
                .build();
            section.append(&header);
            section.append(&group);

            let section_row = ListBoxRow::builder()
                .selectable(false)
                .activatable(false)
                .child(&section)
                .build();
            section_row.add_css_class("transaction-date-section");
            list.append(&section_row);
            date_group = Some(group);
            last_date = date_heading;
        }
        if let Some(group) = date_group.as_ref() {
            group.append(&row);
        }
    }
}

fn present_edit_cash_entry_dialog(
    parent: &Dialog,
    refs: UiRefs,
    entry: CashEntry,
    list: ListBox,
    stack: Stack,
    filter_state: Rc<RefCell<TransactionsFilterState>>,
) {
    let account = refs
        .state
        .database
        .load_accounts()
        .unwrap_or_default()
        .into_iter()
        .find(|account| account.id == entry.account_id);
    let Some(account) = account else {
        refs.toast_overlay.add_toast(Toast::new("The account no longer exists"));
        return;
    };

    let kind = ComboRow::new();
    kind.set_title("Type");
    let kind_model = string_model(&["Deposit", "Withdrawal"]);
    kind.set_model(Some(&kind_model));
    kind.set_selected(if entry.amount < 0.0 { 1 } else { 0 });

    let amount = money_entry_row(&format!("Amount ({})", entry.currency), entry.amount.abs());
    let (year, month, day) = civil_from_days(entry.occurred_at.div_euclid(86_400));
    let date = DateChooser::new(&format!("{year:04}-{month:02}-{day:02}"));

    let account_row = ActionRow::builder()
        .title(&account.name)
        .subtitle(&account.currency)
        .build();
    let group = PreferencesGroup::new();
    group.add(&account_row);
    group.add(&kind);
    group.add(&amount);
    group.add(&date.row);

    let body = dialog_body();
    body.append(&group);
    let scroller = dialog_scroller(&body, 500);
    let save = Button::builder().label("Save").css_classes(["suggested-action"]).build();
    let header = HeaderBar::new();
    header.pack_end(&save);
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scroller));
    let dialog = Dialog::builder()
        .title("Edit Transaction")
        .content_width(500)
        .content_height(430)
        .child(&toolbar)
        .build();
    install_escape_to_close(&dialog);

    {
        let dialog = dialog.clone();
        let manager = <Dialog as Clone>::clone(parent);
        let refs = refs.clone();
        let entry_id = entry.id;
        save.connect_clicked(move |_| {
            let Some(amount_value) = money_value(&amount).filter(|value| *value > 0.0) else {
                refs.toast_overlay.add_toast(Toast::new("Cash amount must be greater than zero"));
                return;
            };
            let date_value = date.value();
            let Ok(timestamp) = activity_timestamp(&date_value) else {
                refs.toast_overlay.add_toast(Toast::new("Choose a valid date"));
                return;
            };
            if date_is_future(&date_value) {
                refs.toast_overlay.add_toast(Toast::new("Cash date cannot be in the future"));
                return;
            }
            let signed = if kind.selected() == 1 { -amount_value } else { amount_value };
            match refs.state.database.update_cash_entry(entry_id, signed, timestamp) {
                Ok(true) => {
                    rebuild_transactions_list(&list, &stack, &manager, refs.clone(), filter_state.clone());
                    refs.refresh();
                    refresh_portfolio_history_async(refs.clone(), false);
                    dialog.close();
                }
                Ok(false) => refs.toast_overlay.add_toast(Toast::new("This transaction can no longer be edited")),
                Err(error) => refs.toast_overlay.add_toast(Toast::new(&format!(
                    "Could not update transaction: {error}"
                ))),
            }
        });
    }

    dialog.present(Some(parent));
}

fn update_cash_settlement_row(
    row: &SwitchRow,
    accounts: &[Account],
    account_index: u32,
    kind: &str,
    asset_currency: Option<&str>,
) {
    let Some(asset_currency) = asset_currency else {
        row.set_visible(false);
        row.set_active(false);
        return;
    };
    if kind == "OPEN" {
        row.set_visible(false);
        row.set_active(false);
        return;
    }

    row.set_visible(true);
    row.set_title(if kind == "SELL" {
        "Add Proceeds to Account Cash"
    } else {
        "Use Account Cash"
    });

    let Some(account) = accounts.get(account_index as usize) else {
        row.set_sensitive(false);
        row.set_active(false);
        row.set_subtitle("Choose an account");
        return;
    };
    if !account.currency.eq_ignore_ascii_case(asset_currency) {
        row.set_sensitive(false);
        row.set_active(false);
        row.set_subtitle(&format!("Account cash is {}", account.currency));
        return;
    }

    row.set_sensitive(true);
    row.set_subtitle(&format!(
        "Current cash: {}",
        format_currency(account.cash, &account.currency)
    ));
}

fn update_add_activity_cash_settlement_row(
    row: &SwitchRow,
    accounts: &[Account],
    account_index: u32,
    kind: &str,
    asset_currency: Option<&str>,
    shares: &EntryRow,
    price: &EntryRow,
    fees: &EntryRow,
) {
    update_cash_settlement_row(row, accounts, account_index, kind, asset_currency);

    if kind != "BUY" || !row.is_visible() || !row.is_sensitive() {
        return;
    }

    let Some(account) = accounts.get(account_index as usize) else {
        return;
    };
    if account.cash <= 0.005 {
        row.set_sensitive(false);
        row.set_active(false);
        row.set_subtitle("No account cash available");
        return;
    }

    let Some(share_count) = shares_value(shares) else {
        row.set_sensitive(false);
        row.set_active(false);
        row.set_subtitle("Enter a valid number of shares");
        return;
    };
    let Some(price_value) = money_value(price) else {
        row.set_sensitive(false);
        row.set_active(false);
        row.set_subtitle("Enter a valid price");
        return;
    };
    let Some(fee_value) = money_value(fees) else {
        row.set_sensitive(false);
        row.set_active(false);
        row.set_subtitle("Enter valid fees");
        return;
    };

    let purchase_total = share_count * price_value + fee_value;
    if !purchase_total.is_finite() || purchase_total <= 0.0 {
        row.set_sensitive(false);
        row.set_active(false);
        row.set_subtitle("Enter a valid purchase amount");
        return;
    }
    if purchase_total > account.cash + 0.005 {
        row.set_sensitive(false);
        row.set_active(false);
        row.set_subtitle(&format!(
            "Needs {} · available {}",
            format_currency(purchase_total, &account.currency),
            format_currency(account.cash, &account.currency)
        ));
    }
}

fn activity_asset_from_position(position: &Position) -> SearchResult {
    SearchResult {
        provider_symbol: position.provider_symbol.clone(),
        code: position.code.clone(),
        exchange: position.exchange.clone(),
        name: position.name.clone(),
        asset_type: String::new(),
        currency: position.currency.clone(),
        market_price: position.last_price,
        change_percent: position.day_change_percent,
    }
}

fn activity_asset_from_watchlist(item: &WatchlistItem) -> SearchResult {
    SearchResult {
        provider_symbol: item.provider_symbol.clone(),
        code: item.code.clone(),
        exchange: item.exchange.clone(),
        name: item.name.clone(),
        asset_type: item.asset_type.clone(),
        currency: item.currency.clone(),
        market_price: item.last_price,
        change_percent: item.day_change_percent,
    }
}

fn present_add_activity_dialog(parent: &ApplicationWindow, refs: UiRefs) {
    present_add_activity_dialog_with_context(parent, refs, None, None, None);
}

fn present_add_activity_for_account(parent: &ApplicationWindow, refs: UiRefs, account_id: i64) {
    present_add_activity_dialog_with_context(parent, refs, Some(account_id), None, None);
}

fn present_add_activity_dialog_with_context(
    parent: &ApplicationWindow,
    refs: UiRefs,
    preferred_account_id: Option<i64>,
    preset_asset: Option<SearchResult>,
    preset_kind: Option<&'static str>,
) {
    let accounts = match refs.state.database.load_accounts() {
        Ok(accounts) if !accounts.is_empty() => accounts,
        _ => {
            present_add_account_dialog(parent, refs.clone());
            return;
        }
    };

    let contextual_asset = preset_asset.is_some();
    let selected: Rc<RefCell<Option<SearchResult>>> =
        Rc::new(RefCell::new(preset_asset.clone()));
    let results: Rc<RefCell<Vec<SearchResult>>> = Rc::new(RefCell::new(Vec::new()));

    let search = SearchEntry::builder()
        .placeholder_text("Search ticker or company name")
        .hexpand(true)
        .build();
    search.set_search_delay(300);

    let spinner = Spinner::new();
    let search_status = Label::builder()
        .halign(Align::Start)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .build();
    let search_feedback = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .visible(false)
        .build();
    search_feedback.append(&spinner);
    search_feedback.append(&search_status);

    let result_list = ListBox::builder()
        .selection_mode(SelectionMode::Single)
        .activate_on_single_click(true)
        .css_classes(["boxed-list"])
        .visible(false)
        .build();

    let search_body = dialog_body();
    search_body.append(&search);
    search_body.append(&search_feedback);
    search_body.append(&result_list);
    let search_scroller = dialog_scroller(&search_body, 560);

    let account = ComboRow::new();
    account.set_title("Account");
    let account_model = account_model(&accounts);
    account.set_model(Some(&account_model));
    let last_account = refs
        .state
        .database
        .setting(LAST_ACCOUNT_ID_KEY)
        .ok()
        .flatten()
        .and_then(|value| value.parse::<i64>().ok());
    account.set_selected(
        preferred_account_id
            .or(last_account)
            .and_then(|id| accounts.iter().position(|item| item.id == id))
            .unwrap_or(0) as u32,
    );

    let activity_type = ComboRow::new();
    activity_type.set_title("Type");
    let type_model = string_model(&["Buy", "Sell", "Opening Position"]);
    activity_type.set_model(Some(&type_model));
    activity_type.set_selected(preset_kind.map(transaction_kind_index).unwrap_or(0));

    let date = DateChooser::today();

    let shares = shares_entry_row(1.0);

    let price = money_entry_row("Price per share", 0.0);
    let fees = money_entry_row("Fees", 0.0);

    let settle_cash = SwitchRow::new();
    settle_cash.set_title("Use Account Cash");
    settle_cash.set_visible(
        preset_asset.is_some()
            && preset_kind
                .map(|kind| !kind.eq_ignore_ascii_case("OPEN"))
                .unwrap_or(false),
    );

    let activity_group = PreferencesGroup::new();
    activity_group.add(&account);
    activity_group.add(&activity_type);
    activity_group.add(&date.row);
    activity_group.add(&shares);
    activity_group.add(&price);
    activity_group.add(&fees);
    activity_group.add(&settle_cash);

    let record = Button::builder()
        .label("Record")
        .css_classes(["suggested-action", "pill"])
        .halign(Align::Fill)
        .hexpand(true)
        .sensitive(preset_asset.is_some())
        .build();
    let form_body = dialog_body();
    form_body.append(&activity_group);
    let form_scroller = dialog_scroller(&form_body, 560);
    form_scroller.set_vexpand(true);
    let form_page = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(8)
        .build();
    form_page.append(&form_scroller);
    let form_actions = dialog_bottom_action(&record);
    form_page.append(&form_actions);

    let flow_stack = Stack::builder()
        .transition_type(gtk::StackTransitionType::None)
        .transition_duration(200)
        .vhomogeneous(false)
        .hhomogeneous(false)
        .build();
    flow_stack.add_named(&search_scroller, Some("search"));
    flow_stack.add_named(&form_page, Some("form"));
    flow_stack.set_visible_child_name(if preset_asset.is_some() { "form" } else { "search" });

    let back = Button::builder()
        .icon_name("go-previous-symbolic")
        .tooltip_text("Choose Another Security")
        .css_classes(["flat", "circular"])
        .visible(false)
        .build();
    let title = if let Some(asset) = preset_asset.as_ref() {
        adw::WindowTitle::new(&asset.code, &asset.name)
    } else {
        adw::WindowTitle::new("Add Activity", "")
    };
    let header = HeaderBar::new();
    header.set_title_widget(Some(&title));
    header.pack_start(&back);
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&flow_stack));
    let dialog = Dialog::builder()
        .title("Add Activity")
        .content_width(560)
        .content_height(570)
        .child(&toolbar)
        .build();
    install_escape_to_close(&dialog);

    if let Some(asset) = preset_asset.as_ref() {
        price.set_title(&format!("Price per share ({})", asset.currency));
        price.set_text(&trim_number(asset.market_price.unwrap_or(0.0).max(0.0)));
        fees.set_title(&format!("Fees ({})", asset.currency));
        fees.set_text("0");
        update_add_activity_cash_settlement_row(
            &settle_cash,
            &accounts,
            account.selected(),
            transaction_kind_from_index(activity_type.selected()),
            Some(&asset.currency),
            &shares,
            &price,
            &fees,
        );

        // Contextual Buy/Sell dialogs start directly on the form. Re-evaluate
        // once the widgets are realized so the cash row cannot inherit the
        // hidden search-state visibility from construction.
        let settle_cash_for_idle = settle_cash.clone();
        let accounts_for_idle = accounts.clone();
        let account_for_idle = account.clone();
        let activity_type_for_idle = activity_type.clone();
        let currency_for_idle = asset.currency.clone();
        let shares_for_idle = shares.clone();
        let price_for_idle = price.clone();
        let fees_for_idle = fees.clone();
        glib::idle_add_local_once(move || {
            update_add_activity_cash_settlement_row(
                &settle_cash_for_idle,
                &accounts_for_idle,
                account_for_idle.selected(),
                transaction_kind_from_index(activity_type_for_idle.selected()),
                Some(&currency_for_idle),
                &shares_for_idle,
                &price_for_idle,
                &fees_for_idle,
            );
        });
    }

    enum SearchMessage {
        Complete(String, Result<Vec<SearchResult>, String>),
    }
    let (sender, receiver) = mpsc::channel::<SearchMessage>();
    let receiver = Rc::new(RefCell::new(receiver));

    {
        let result_list = result_list.clone();
        let spinner = spinner.clone();
        let search_status = search_status.clone();
        let search_feedback = search_feedback.clone();
        let results_store = results.clone();
        let search_entry = search.clone();
        let dialog_weak = dialog.downgrade();
        glib::timeout_add_local(Duration::from_millis(50), move || {
            if dialog_weak.upgrade().is_none() {
                return glib::ControlFlow::Break;
            }

            for message in receiver.borrow().try_iter() {
                match message {
                    SearchMessage::Complete(query, result) => {
                        if search_entry.text().trim() != query {
                            continue;
                        }
                        spinner.set_spinning(false);
                        match result {
                            Ok(items) => {
                                *results_store.borrow_mut() = items.clone();
                                rebuild_search_results(&result_list, &items);
                                result_list.set_visible(!items.is_empty());
                                if items.is_empty() {
                                    search_status.set_label("No matching stocks or ETFs");
                                    search_feedback.set_visible(true);
                                } else {
                                    search_feedback.set_visible(false);
                                }
                            }
                            Err(error) => {
                                results_store.borrow_mut().clear();
                                clear_list(&result_list);
                                result_list.set_visible(false);
                                search_status.set_label(&error);
                                search_feedback.set_visible(true);
                            }
                        }
                    }
                }
            }
            glib::ControlFlow::Continue
        });
    }

    if !contextual_asset {
        let sender = sender.clone();
        let spinner = spinner.clone();
        let search_status = search_status.clone();
        let search_feedback = search_feedback.clone();
        let result_list = result_list.clone();
        let selected = selected.clone();
        let record = record.clone();
        let settle_cash = settle_cash.clone();
        search.connect_search_changed(move |entry| {
            *selected.borrow_mut() = None;
            record.set_sensitive(false);
            settle_cash.set_active(false);
            settle_cash.set_visible(false);

            let query = entry.text().trim().to_string();
            if query.is_empty() {
                spinner.set_spinning(false);
                clear_list(&result_list);
                result_list.set_visible(false);
                search_feedback.set_visible(false);
                return;
            }

            spinner.set_spinning(true);
            search_status.set_label("Searching");
            search_feedback.set_visible(true);
            let sender = sender.clone();
            std::thread::spawn(move || {
                let result = market_data::search(&query).map_err(|error| error.to_string());
                let _ = sender.send(SearchMessage::Complete(query, result));
            });
        });
    }

    {
        let selected = selected.clone();
        let results = results.clone();
        let shares = shares.clone();
        let price = price.clone();
        let fees = fees.clone();
        let settle_cash = settle_cash.clone();
        let account = account.clone();
        let activity_type = activity_type.clone();
        let accounts = accounts.clone();
        let record = record.clone();
        let flow_stack = flow_stack.clone();
        let back = back.clone();
        let title = title.clone();
        result_list.connect_row_activated(move |_, row| {
            let index = row.index();
            if index < 0 {
                return;
            }
            let Some(item) = results.borrow().get(index as usize).cloned() else {
                return;
            };

            price.set_title(&format!("Price per share ({})", item.currency));
            price.set_text(&trim_number(item.market_price.unwrap_or(0.0).max(0.0)));
            fees.set_title(&format!("Fees ({})", item.currency));
            fees.set_text("0");
            update_add_activity_cash_settlement_row(
                &settle_cash,
                &accounts,
                account.selected(),
                transaction_kind_from_index(activity_type.selected()),
                Some(&item.currency),
                &shares,
                &price,
                &fees,
            );
            title.set_title(&item.code);
            title.set_subtitle(&item.name);
            back.set_visible(true);
            record.set_sensitive(true);
            *selected.borrow_mut() = Some(item);
            flow_stack.set_transition_type(gtk::StackTransitionType::SlideLeft);
            flow_stack.set_visible_child_name("form");
        });
    }

    {
        let selected = selected.clone();
        let settle_cash = settle_cash.clone();
        let record = record.clone();
        let flow_stack = flow_stack.clone();
        let back_for_callback = back.clone();
        let title = title.clone();
        let search = search.clone();
        back.connect_clicked(move |_| {
            *selected.borrow_mut() = None;
            settle_cash.set_active(false);
            settle_cash.set_visible(false);
            record.set_sensitive(false);
            back_for_callback.set_visible(false);
            title.set_title("Add Activity");
            title.set_subtitle("");
            flow_stack.set_transition_type(gtk::StackTransitionType::SlideRight);
            flow_stack.set_visible_child_name("search");
            search.grab_focus();
        });
    }

    {
        let selected = selected.clone();
        let settle_cash = settle_cash.clone();
        let accounts = accounts.clone();
        let activity_type = activity_type.clone();
        let shares = shares.clone();
        let price = price.clone();
        let fees = fees.clone();
        account.connect_selected_notify(move |row| {
            let currency = selected.borrow().as_ref().map(|asset| asset.currency.clone());
            update_add_activity_cash_settlement_row(
                &settle_cash,
                &accounts,
                row.selected(),
                transaction_kind_from_index(activity_type.selected()),
                currency.as_deref(),
                &shares,
                &price,
                &fees,
            );
        });
    }
    {
        let selected = selected.clone();
        let settle_cash = settle_cash.clone();
        let accounts = accounts.clone();
        let account = account.clone();
        let shares = shares.clone();
        let price = price.clone();
        let fees = fees.clone();
        activity_type.connect_selected_notify(move |row| {
            let currency = selected.borrow().as_ref().map(|asset| asset.currency.clone());
            update_add_activity_cash_settlement_row(
                &settle_cash,
                &accounts,
                account.selected(),
                transaction_kind_from_index(row.selected()),
                currency.as_deref(),
                &shares,
                &price,
                &fees,
            );
        });
    }

    for entry in [&shares, &price, &fees] {
        let selected = selected.clone();
        let settle_cash = settle_cash.clone();
        let accounts = accounts.clone();
        let account = account.clone();
        let activity_type = activity_type.clone();
        let shares = shares.clone();
        let price = price.clone();
        let fees = fees.clone();
        entry.connect_changed(move |_| {
            let currency = selected.borrow().as_ref().map(|asset| asset.currency.clone());
            update_add_activity_cash_settlement_row(
                &settle_cash,
                &accounts,
                account.selected(),
                transaction_kind_from_index(activity_type.selected()),
                currency.as_deref(),
                &shares,
                &price,
                &fees,
            );
        });
    }

    {
        let selected = selected.clone();
        let refs = refs.clone();
        let dialog = dialog.clone();
        let accounts = accounts.clone();
        let shares_for_record = shares.clone();
        record.connect_clicked(move |_| {
            let Some(asset) = selected.borrow().clone() else {
                return;
            };
            let Some(selected_account) = accounts.get(account.selected() as usize) else {
                return;
            };
            let trade_date = date.value();
            let Ok(timestamp) = activity_timestamp(&trade_date) else {
                refs.toast_overlay
                    .add_toast(Toast::new("Choose a valid date"));
                return;
            };
            if date_is_future(&trade_date) {
                refs.toast_overlay
                    .add_toast(Toast::new("Transaction date cannot be in the future"));
                return;
            }
            let kind = transaction_kind_from_index(activity_type.selected());
            let Some(share_count) = shares_value(&shares_for_record) else {
                refs.toast_overlay
                    .add_toast(Toast::new("Enter a valid number of shares"));
                return;
            };
            if let Err(message) = validate_transaction_change(
                &refs,
                selected_account.id,
                &asset.provider_symbol,
                None,
                kind,
                timestamp,
                share_count,
            ) {
                refs.toast_overlay.add_toast(Toast::new(message));
                return;
            }
            let Some(price_value) = money_value(&price) else {
                refs.toast_overlay.add_toast(Toast::new("Enter a valid price"));
                return;
            };
            let Some(fee_value) = money_value(&fees) else {
                refs.toast_overlay.add_toast(Toast::new("Enter valid fees"));
                return;
            };

            let activity = NewTransaction {
                account_id: selected_account.id,
                code: asset.code.clone(),
                exchange: asset.exchange.clone(),
                provider_symbol: asset.provider_symbol.clone(),
                name: asset.name.clone(),
                transaction_type: kind.into(),
                trade_date,
                timestamp,
                shares: share_count,
                price: price_value,
                fees: fee_value,
                settle_cash: kind != "OPEN" && settle_cash.is_active(),
                currency: asset.currency.clone(),
            };

            match refs.state.database.add_transaction(&activity) {
                Ok(_) => {
                    let _ = refs.state.database.set_setting(
                        LAST_ACCOUNT_ID_KEY,
                        &selected_account.id.to_string(),
                    );
                    let _ = refs.state.database.sync_paid_dividends_to_cash();

                    let all_positions = refs.state.database.load_positions().unwrap_or_default();
                    if let Some(position) = all_positions
                        .iter()
                        .find(|position| {
                            position.account_id == selected_account.id
                                && position
                                    .provider_symbol
                                    .eq_ignore_ascii_case(&asset.provider_symbol)
                        })
                        .cloned()
                    {
                        if let Some(market_price) = asset.market_price {
                            let _ = refs.state.database.update_quote(
                                position.id,
                                market_price,
                                asset.change_percent,
                                current_unix_timestamp(),
                            );
                        }

                        // Always fetch the newly added holding immediately. Search results
                        // do not always include a quote, and the portfolio should never
                        // require a manual refresh before its value becomes available.
                        let fetch_fx = portfolio_needs_fx_with_cash(
                            &refs.state,
                            &all_positions,
                            &base_currency(&refs.state),
                        );
                        refresh_market_async(
                            refs.clone(),
                            vec![position.clone()],
                            fetch_fx,
                            false,
                        );
                        refresh_dividends_async(refs.clone(), vec![position], false);
                    }
                    refs.refresh();
                    refresh_portfolio_history_async(refs.clone(), false);
                    refs.toast_overlay.add_toast(Toast::new(&format!(
                        "Recorded {} activity",
                        asset.code
                    )));
                    dialog.close();
                }
                Err(error) => refs.toast_overlay.add_toast(Toast::new(&format!(
                    "Could not record activity: {error}"
                ))),
            }
        });
    }

    {
        let dialog = dialog.clone();
        search.connect_stop_search(move |_| {
            dialog.close();
        });
    }

    dialog.present(Some(parent));
    if preset_asset.is_some() {
        shares.grab_focus();
    } else {
        search.grab_focus();
    }
}

fn present_edit_transaction_dialog(
    parent: &Dialog,
    refs: UiRefs,
    transaction: Transaction,
    list: ListBox,
    stack: Stack,
    filter_state: Rc<RefCell<TransactionsFilterState>>,
) {
    let accounts = refs.state.database.load_accounts().unwrap_or_default();
    let account_index = accounts
        .iter()
        .position(|account| account.id == transaction.account_id)
        .unwrap_or(0) as u32;

    let security = ActionRow::builder()
        .title(&format!("{} · {}", transaction.code, transaction.account_name))
        .subtitle(&transaction.name)
        .build();
    let security_group = PreferencesGroup::new();
    security_group.add(&security);

    let transaction_type = ComboRow::new();
    transaction_type.set_title("Type");
    let type_model = string_model(&["Buy", "Sell", "Opening Position"]);
    transaction_type.set_model(Some(&type_model));
    transaction_type.set_selected(transaction_kind_index(&transaction.transaction_type));

    let date = DateChooser::new(&transaction.trade_date);

    let shares = shares_entry_row(transaction.shares);

    let price = money_entry_row(
        &format!("Price per share ({})", transaction.currency),
        transaction.price,
    );
    let fees = money_entry_row(&format!("Fees ({})", transaction.currency), transaction.fees);

    let settle_cash = SwitchRow::new();
    settle_cash.set_active(transaction.settle_cash);
    update_cash_settlement_row(
        &settle_cash,
        &accounts,
        account_index,
        &transaction.transaction_type,
        Some(&transaction.currency),
    );
    if transaction.settle_cash && transaction.transaction_type != "OPEN" {
        settle_cash.set_active(true);
    }

    let group = PreferencesGroup::new();
    group.add(&transaction_type);
    group.add(&date.row);
    group.add(&shares);
    group.add(&price);
    group.add(&fees);
    group.add(&settle_cash);

    let body = dialog_body();
    body.append(&security_group);
    body.append(&group);
    let scroller = dialog_scroller(&body, 520);

    let save = Button::builder()
        .label("Save")
        .css_classes(["suggested-action"])
        .build();
    let header = HeaderBar::new();
    header.pack_end(&save);
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scroller));
    let dialog = Dialog::builder()
        .title("Edit Transaction")
        .content_width(520)
        .content_height(560)
        .child(&toolbar)
        .build();

    {
        let settle_cash = settle_cash.clone();
        let accounts = accounts.clone();
        let currency = transaction.currency.clone();
        transaction_type.connect_selected_notify(move |row| {
            update_cash_settlement_row(
                &settle_cash,
                &accounts,
                account_index,
                transaction_kind_from_index(row.selected()),
                Some(&currency),
            );
        });
    }

    {
        let dialog = dialog.clone();
        let manager = <Dialog as Clone>::clone(parent);
        let refs = refs.clone();
        let transaction_id = transaction.id;
        save.connect_clicked(move |_| {
            let trade_date = date.value();
            let Ok(timestamp) = activity_timestamp(&trade_date) else {
                refs.toast_overlay
                    .add_toast(Toast::new("Choose a valid date"));
                return;
            };
            if date_is_future(&trade_date) {
                refs.toast_overlay
                    .add_toast(Toast::new("Transaction date cannot be in the future"));
                return;
            }
            let kind = transaction_kind_from_index(transaction_type.selected());
            let Some(share_count) = shares_value(&shares) else {
                refs.toast_overlay
                    .add_toast(Toast::new("Enter a valid number of shares"));
                return;
            };
            if let Err(message) = validate_transaction_change(
                &refs,
                transaction.account_id,
                &transaction.provider_symbol,
                Some(transaction_id),
                kind,
                timestamp,
                share_count,
            ) {
                refs.toast_overlay.add_toast(Toast::new(message));
                return;
            }
            let Some(price_value) = money_value(&price) else {
                refs.toast_overlay.add_toast(Toast::new("Enter a valid price"));
                return;
            };
            let Some(fee_value) = money_value(&fees) else {
                refs.toast_overlay.add_toast(Toast::new("Enter valid fees"));
                return;
            };
            match refs.state.database.update_transaction(
                transaction_id,
                kind,
                &trade_date,
                timestamp,
                share_count,
                price_value,
                fee_value,
                kind != "OPEN" && settle_cash.is_active(),
            ) {
                Ok(()) => {
                    let _ = refs.state.database.sync_paid_dividends_to_cash();
                    rebuild_transactions_list(&list, &stack, &manager, refs.clone(), filter_state.clone());
                    refs.refresh();
                    refresh_portfolio_history_async(refs.clone(), false);
                    dialog.close();
                }
                Err(error) => refs.toast_overlay.add_toast(Toast::new(&format!(
                    "Could not update transaction: {error}"
                ))),
            }
        });
    }

    dialog.present(Some(parent));
}

fn rebuild_search_results(list: &ListBox, results: &[SearchResult]) {
    clear_list(list);
    for result in results {
        let row = ListBoxRow::new();
        row.set_activatable(true);
        row.set_selectable(false);

        let content = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(3)
            .margin_top(10)
            .margin_bottom(10)
            .margin_start(12)
            .margin_end(12)
            .build();

        let top = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(8)
            .build();
        top.append(&stock_avatar(&result.provider_symbol, &result.code, 32));
        top.append(
            &Label::builder()
                .label(&result.code)
                .halign(Align::Start)
                .hexpand(true)
                .css_classes(["heading"])
                .build(),
        );
        if let Some(price) = result.market_price {
            top.append(
                &Label::builder()
                    .label(&format_currency(price, &result.currency))
                    .halign(Align::End)
                    .css_classes(["heading"])
                    .build(),
            );
        }
        content.append(&top);

        content.append(
            &Label::builder()
                .label(&result.name)
                .halign(Align::Start)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["dim-label"])
                .build(),
        );
        let mut details = format!(
            "{} · {} · {}",
            friendly_exchange(&result.exchange),
            result.currency,
            friendly_asset_type(&result.asset_type)
        );
        if let Some(change) = result.change_percent {
            details.push_str(&format!(" · {change:+.2}% today"));
        }
        content.append(
            &Label::builder()
                .label(&details)
                .halign(Align::Start)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["dim-label", "caption"])
                .build(),
        );

        row.set_child(Some(&content));
        list.append(&row);
    }
}

fn accounts_icon_name() -> &'static str {
    // Bundle the GNOME credit-card metaphor so Accounts never falls back to
    // the visually different smart-card device icon on systems where Icon
    // Library extras are not installed.
    "aureus-credit-card-symbolic"
}

fn dialog_bottom_action(button: &Button) -> GtkBox {
    // Bottom-of-dialog pill actions should read as controls, not edge-to-edge bars.
    // Keep one shared geometry so Reports, Add Account, Add Cash, Withdraw Cash,
    // Transfer, and Activity stay visually consistent across dialog widths.
    let clamp = adw::Clamp::builder()
        .maximum_size(260)
        .tightening_threshold(220)
        .hexpand(true)
        .child(button)
        .build();
    let actions = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .halign(Align::Fill)
        .hexpand(true)
        .margin_top(4)
        .margin_bottom(24)
        .margin_start(18)
        .margin_end(18)
        .build();
    actions.append(&clamp);
    actions
}

fn dialog_body() -> GtkBox {
    GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(16)
        .margin_top(18)
        .margin_bottom(24)
        .margin_start(18)
        .margin_end(18)
        .build()
}

fn dialog_scroller(body: &GtkBox, maximum_size: i32) -> gtk::ScrolledWindow {
    let clamp = adw::Clamp::builder()
        .maximum_size(maximum_size)
        .tightening_threshold(400)
        .child(body)
        .build();
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .child(&clamp)
        .build()
}

fn update_portfolio_range_return_label(
    refs: &UiRefs,
    investment_return: Option<f64>,
    base: &str,
    range: HistoryRange,
) {
    refs.investment_return.set_tooltip_text(Some(&format!(
        "Investment return {}",
        history_range_change_suffix(range)
    )));

    let text = investment_return
        .map(|value| format_signed_currency(value, base))
        .unwrap_or_else(|| "—".into());
    let value = investment_return.unwrap_or(0.0);

    if refs.investment_return.label().as_str() == text {
        set_gain_class(&refs.investment_return, value);
        return;
    }

    let label = refs.investment_return.clone();
    crossfade_loaded_labels(vec![(label.clone(), text.clone())], move || {
        label.set_label(&text);
        set_gain_class(&label, value);
    });
}

fn update_portfolio_history_from_cache(refs: &UiRefs) {
    let range = refs.portfolio_history_range.get();
    let base = base_currency(&refs.state);
    let transactions = match refs.state.database.load_transactions() {
        Ok(transactions) => transactions,
        Err(error) => {
            refs.portfolio_history_chart
                .set_message("Portfolio history is unavailable");
            update_portfolio_range_return_label(refs, None, &base, range);
            refs.toast_overlay
                .add_toast(Toast::new(&format!("Could not load transactions: {error}")));
            return;
        }
    };
    let cash_entries = match refs.state.database.load_cash_entries() {
        Ok(entries) => entries,
        Err(error) => {
            refs.portfolio_history_chart
                .set_message("Portfolio history is unavailable");
            update_portfolio_range_return_label(refs, None, &base, range);
            refs.toast_overlay
                .add_toast(Toast::new(&format!("Could not load cash activity: {error}")));
            return;
        }
    };
    if transactions.is_empty() && cash_entries.is_empty() {
        refs.portfolio_history_chart
            .set_message("Add activity or cash to see portfolio history");
        update_portfolio_range_return_label(refs, None, &base, range);
        return;
    }

    let now = current_unix_timestamp();
    let minimum = range.minimum_timestamp(now);
    let mut symbols = HashSet::<String>::new();
    for transaction in &transactions {
        symbols.insert(transaction.provider_symbol.to_ascii_uppercase());
    }

    let mut histories = HashMap::<String, Vec<PricePoint>>::new();
    let mut missing = 0usize;
    for symbol in &symbols {
        let points = market_data::display_history_points(
            refs.state
                .database
                .history_points(symbol, range.interval(), minimum)
                .unwrap_or_default(),
            range,
        );
        if points.is_empty() {
            missing += 1;
        } else {
            histories.insert(symbol.clone(), points);
        }
    }

    let needs_fx = transactions
        .iter()
        .any(|transaction| transaction.currency != base)
        || cash_entries.iter().any(|entry| entry.currency != base);
    let fx_points = needs_fx.then(|| {
        // Keep the whole backing FX window. A Sunday FX session may be newer
        // than the latest Friday equity session, but Friday bars are still
        // needed to value that equity session correctly.
        refs
            .state
            .database
            .history_points("CAD=X", range.interval(), minimum)
            .unwrap_or_default()
    });
    let fx_missing = needs_fx
        && fx_points
            .as_ref()
            .map(|points| points.is_empty())
            .unwrap_or(true);

    let split_events = refs.state.database.all_split_events().unwrap_or_default();
    // 1D is the most recent trading session, not the last 24 clock hours. The
    // cache lookup intentionally reaches back across weekends/holidays, but the
    // rendered portfolio series should begin at the newest common session.
    let visible_minimum = if range == HistoryRange::OneDay {
        // The securities define the portfolio trading session. FX can trade on
        // a newer calendar day (for example Sunday evening) and must not push
        // a Friday equity session out of the 1D window. Only fall back to the FX
        // session when the account has no security history at all.
        histories
            .values()
            .filter_map(|points| points.first().map(|point| point.timestamp))
            .max()
            .or_else(|| {
                fx_points
                    .as_ref()
                    .and_then(|points| points.first().map(|point| point.timestamp))
            })
            .unwrap_or_else(|| now.saturating_sub(24 * 60 * 60))
    } else {
        minimum
    };
    let visible_maximum = if range == HistoryRange::OneDay {
        // Do not append a synthetic "now" point after the market has closed.
        // The 1D portfolio chart ends at the newest common security-session
        // timestamp, matching the stock charts through weekends and holidays.
        histories
            .values()
            .filter_map(|points| points.last().map(|point| point.timestamp))
            .min()
            .or_else(|| {
                fx_points
                    .as_ref()
                    .and_then(|points| points.last().map(|point| point.timestamp))
            })
    } else {
        None
    };
    // Activity dates in Aureus are date-only. For a 1D chart, keep the opening
    // portfolio snapshot as the baseline, then apply activity recorded for that
    // trading date immediately after it. That lets today's buys and sells use
    // their entered transaction values in the range P&L instead of pretending
    // the resulting holdings existed before the session began.
    let (history_transactions, history_cash_entries) = if range == HistoryRange::OneDay {
        normalize_one_day_activity_to_session_start(
            &transactions,
            &cash_entries,
            visible_minimum,
            visible_maximum,
        )
    } else {
        (transactions.clone(), cash_entries.clone())
    };

    let points = build_portfolio_value_points(
        &history_transactions,
        &history_cash_entries,
        &split_events,
        &histories,
        fx_points.as_deref(),
        &base,
        visible_minimum,
        visible_maximum,
    );

    if points.len() >= 2 {
        let investment_return = if range == HistoryRange::OneDay {
            visible_maximum.and_then(|session_end| {
                portfolio_one_day_investment_return(
                    &transactions,
                    &cash_entries,
                    &split_events,
                    &histories,
                    fx_points.as_deref(),
                    &base,
                    visible_minimum,
                    session_end,
                )
            })
        } else {
            portfolio_range_investment_return(
                &points,
                &history_transactions,
                &history_cash_entries,
                fx_points.as_deref(),
                &base,
            )
        };
        refs.portfolio_history_chart
            .set_points_with_trend(points, &base, range, investment_return);
        update_portfolio_range_return_label(refs, investment_return, &base, range);
    } else if missing > 0 || fx_missing {
        refs.portfolio_history_chart
            .set_message("Loading portfolio history");
        update_portfolio_range_return_label(refs, None, &base, range);
    } else {
        refs.portfolio_history_chart
            .set_message("Not enough history yet");
        update_portfolio_range_return_label(refs, None, &base, range);
    }

}

fn normalize_one_day_activity_to_session_start(
    transactions: &[Transaction],
    cash_entries: &[CashEntry],
    session_start: i64,
    session_end: Option<i64>,
) -> (Vec<Transaction>, Vec<CashEntry>) {
    let mut transactions = transactions.to_vec();
    let mut cash_entries = cash_entries.to_vec();
    let Some(session_end) = session_end.filter(|timestamp| *timestamp > session_start) else {
        return (transactions, cash_entries);
    };

    let session_day = session_start.div_euclid(86_400);
    let session_end_day = session_end.div_euclid(86_400);
    if session_day != session_end_day {
        return (transactions, cash_entries);
    }
    let (year, month, day) = civil_from_days(session_day);
    let session_date = format!("{year:04}-{month:02}-{day:02}");

    // Keep the opening market point as the baseline, then apply all date-only
    // activity for that trading date immediately after it. This is important for
    // 1D P&L: a buy entered for today must contribute at its entered purchase
    // price instead of being treated as if those shares were already held at the
    // opening market price. One second is enough to preserve the before/after
    // snapshots without extending the chart beyond the real trading session.
    let activity_timestamp = session_start.saturating_add(1).min(session_end);

    for transaction in &mut transactions {
        if transaction.trade_date.as_str() == session_date.as_str() {
            transaction.timestamp = activity_timestamp;
        }
    }
    for entry in &mut cash_entries {
        if entry.occurred_at.div_euclid(86_400) == session_day {
            entry.occurred_at = activity_timestamp;
        }
    }

    (transactions, cash_entries)
}

fn portfolio_one_day_investment_return(
    transactions: &[Transaction],
    cash_entries: &[CashEntry],
    split_events: &[SplitEvent],
    histories: &HashMap<String, Vec<PricePoint>>,
    fx_points: Option<&[PricePoint]>,
    base: &str,
    session_start: i64,
    session_end: i64,
) -> Option<f64> {
    if session_end <= session_start {
        return None;
    }

    let (year, month, day) = civil_from_days(session_start.div_euclid(86_400));
    let session_date = format!("{year:04}-{month:02}-{day:02}");
    let session_day = session_start.div_euclid(86_400);

    let convert_at = |amount: f64, currency: &str, timestamp: i64| -> Option<f64> {
        if currency.eq_ignore_ascii_case(base) {
            return Some(amount);
        }
        let rate = historical_fx_at(fx_points, timestamp)?;
        if currency.eq_ignore_ascii_case("USD") && base.eq_ignore_ascii_case("CAD") {
            Some(amount * rate)
        } else if currency.eq_ignore_ascii_case("CAD")
            && base.eq_ignore_ascii_case("USD")
            && rate > 0.0
        {
            Some(amount / rate)
        } else {
            None
        }
    };

    #[derive(Clone)]
    enum OpeningEvent {
        Transaction(Transaction),
        Split(SplitEvent),
    }

    // Build the holdings that actually existed when the visible trading session
    // opened. trade_date is authoritative here because Aureus activity is
    // date-only; a transaction recorded for this session must not leak into the
    // opening snapshot just because its stored timestamp happens to be midnight.
    let mut opening_events = transactions
        .iter()
        .filter(|transaction| transaction.trade_date.as_str() < session_date.as_str())
        .cloned()
        .map(OpeningEvent::Transaction)
        .chain(
            split_events
                .iter()
                .filter(|split| split.timestamp < session_start)
                .cloned()
                .map(OpeningEvent::Split),
        )
        .collect::<Vec<_>>();
    opening_events.sort_by_key(|event| match event {
        OpeningEvent::Split(split) => (split.timestamp, activity_sort_priority("SPLIT"), i64::MIN),
        OpeningEvent::Transaction(transaction) => (
            transaction.timestamp,
            activity_sort_priority(&transaction.transaction_type),
            transaction.id,
        ),
    });

    let mut opening_shares = HashMap::<String, f64>::new();
    let mut currencies = HashMap::<String, String>::new();
    for event in opening_events {
        match event {
            OpeningEvent::Split(split) => {
                let symbol = split.provider_symbol.to_ascii_uppercase();
                if let Some(shares) = opening_shares.get_mut(&symbol) {
                    *shares *= split.ratio;
                }
            }
            OpeningEvent::Transaction(transaction) => {
                let symbol = transaction.provider_symbol.to_ascii_uppercase();
                let shares = opening_shares.entry(symbol.clone()).or_insert(0.0);
                match transaction.transaction_type.as_str() {
                    "SELL" | "TRANSFER_OUT" => *shares -= transaction.shares,
                    "BUY" | "OPEN" | "TRANSFER_IN" => *shares += transaction.shares,
                    _ => {}
                }
                currencies.insert(symbol, transaction.currency.clone());
            }
        }
    }

    let mut closing_shares = opening_shares.clone();

    // A split on the visible session is effective before date-only user activity
    // for that date. This keeps the opening pre-split holdings and closing
    // post-split holdings internally consistent without inventing a trade time.
    for split in split_events.iter().filter(|split| {
        split.timestamp.div_euclid(86_400) == session_day && split.timestamp <= session_end
    }) {
        let symbol = split.provider_symbol.to_ascii_uppercase();
        if let Some(shares) = closing_shares.get_mut(&symbol) {
            *shares *= split.ratio;
        }
    }

    let mut same_day_transactions = transactions
        .iter()
        .filter(|transaction| transaction.trade_date.as_str() == session_date.as_str())
        .collect::<Vec<_>>();
    same_day_transactions.sort_by_key(|transaction| transaction.id);
    for transaction in &same_day_transactions {
        let symbol = transaction.provider_symbol.to_ascii_uppercase();
        let shares = closing_shares.entry(symbol.clone()).or_insert(0.0);
        match transaction.transaction_type.as_str() {
            "SELL" | "TRANSFER_OUT" => *shares -= transaction.shares,
            "BUY" | "OPEN" | "TRANSFER_IN" => *shares += transaction.shares,
            _ => {}
        }
        currencies.insert(symbol, transaction.currency.clone());
    }

    let mut symbols = HashSet::<String>::new();
    symbols.extend(opening_shares.keys().cloned());
    symbols.extend(closing_shares.keys().cloned());

    let mut opening_market = 0.0;
    let mut closing_market = 0.0;
    for symbol in symbols {
        let opening_count = opening_shares.get(&symbol).copied().unwrap_or(0.0);
        let closing_count = closing_shares.get(&symbol).copied().unwrap_or(0.0);
        if opening_count.abs() < 0.0000001 && closing_count.abs() < 0.0000001 {
            continue;
        }

        let history = histories.get(&symbol)?;
        let opening_price = historical_close_at(Some(history.as_slice()), session_start)?;
        let closing_price = historical_close_at(Some(history.as_slice()), session_end)?;
        let currency = currencies.get(&symbol).map(String::as_str).unwrap_or(base);

        opening_market += convert_at(opening_count * opening_price, currency, session_start)?;
        closing_market += convert_at(closing_count * closing_price, currency, session_end)?;
    }

    // Cash is part of portfolio value. Reconstruct the opening and closing cash
    // balances from the ledger so settled trades, dividends, FX movement on cash,
    // deposits, and withdrawals are handled consistently with the security side.
    let mut opening_cash = HashMap::<(i64, String), f64>::new();
    let mut closing_cash = HashMap::<(i64, String), f64>::new();
    let mut external_flow = 0.0;
    for entry in cash_entries {
        let entry_day = entry.occurred_at.div_euclid(86_400);
        let key = (entry.account_id, entry.currency.to_ascii_uppercase());
        if entry_day < session_day {
            *opening_cash.entry(key.clone()).or_insert(0.0) += entry.amount;
            *closing_cash.entry(key).or_insert(0.0) += entry.amount;
        } else if entry_day == session_day {
            *closing_cash.entry(key).or_insert(0.0) += entry.amount;
            // Manual cash deposits/withdrawals are external portfolio flows and
            // must not be reported as investment performance.
            if entry.kind == "DEPOSIT" {
                external_flow += convert_at(entry.amount, &entry.currency, session_start)?;
            }
        }
    }

    let mut opening_cash_value = 0.0;
    for ((_, currency), amount) in &opening_cash {
        opening_cash_value += convert_at(*amount, currency, session_start)?;
    }
    let mut closing_cash_value = 0.0;
    for ((_, currency), amount) in &closing_cash {
        closing_cash_value += convert_at(*amount, currency, session_end)?;
    }

    // Trades that do not settle against tracked account cash bring money/assets
    // into or out of Aureus from outside the portfolio. Use the user's actual
    // transaction price, not the market quote, for that external flow. This is
    // what makes a same-day 100-share buy at $2 correctly recognize the gap from
    // $2 to the session's market price as investment return.
    for transaction in same_day_transactions {
        if transaction.settle_cash {
            continue;
        }
        let native_flow = match transaction.transaction_type.as_str() {
            "BUY" | "OPEN" => transaction.shares * transaction.price + transaction.fees,
            "SELL" => -(transaction.shares * transaction.price - transaction.fees),
            "TRANSFER_IN" | "TRANSFER_OUT" => continue,
            _ => continue,
        };
        external_flow += convert_at(native_flow, &transaction.currency, session_start)?;
    }

    let gain = (closing_market + closing_cash_value)
        - (opening_market + opening_cash_value)
        - external_flow;
    gain.is_finite().then_some(gain)
}

fn portfolio_range_investment_return(
    points: &[PricePoint],
    transactions: &[Transaction],
    cash_entries: &[CashEntry],
    fx_points: Option<&[PricePoint]>,
    base: &str,
) -> Option<f64> {
    let first = points.first()?;
    let last = points.last()?;
    if last.timestamp <= first.timestamp {
        return None;
    }

    let convert_at = |amount: f64, currency: &str, timestamp: i64| -> Option<f64> {
        if currency.eq_ignore_ascii_case(base) {
            return Some(amount);
        }
        let rate = historical_fx_at(fx_points, timestamp)?;
        if currency.eq_ignore_ascii_case("USD") && base.eq_ignore_ascii_case("CAD") {
            Some(amount * rate)
        } else if currency.eq_ignore_ascii_case("CAD")
            && base.eq_ignore_ascii_case("USD")
            && rate > 0.0
        {
            Some(amount / rate)
        } else {
            None
        }
    };

    // Portfolio value includes account cash, so deposits/withdrawals and trades
    // funded outside the account must be removed from the raw value change.
    // Internal cash settlement, dividends, and account-to-account transfers stay
    // in performance because they do not add or remove wealth from the portfolio.
    let mut external_flow = 0.0;
    for entry in cash_entries.iter().filter(|entry| {
        entry.kind == "DEPOSIT"
            && entry.occurred_at > first.timestamp
            && entry.occurred_at <= last.timestamp
    }) {
        external_flow += convert_at(entry.amount, &entry.currency, entry.occurred_at)?;
    }

    for transaction in transactions.iter().filter(|transaction| {
        !transaction.settle_cash
            && transaction.timestamp > first.timestamp
            && transaction.timestamp <= last.timestamp
    }) {
        let native_flow = match transaction.transaction_type.as_str() {
            // A non-cash-funded buy/opening position brings an asset into the
            // tracked portfolio from outside it, so treat its cost as a contribution.
            "BUY" | "OPEN" => transaction.shares * transaction.price + transaction.fees,
            // A non-cash-settled sale removes the net proceeds from the tracked
            // portfolio, so it is an external withdrawal.
            "SELL" => -(transaction.shares * transaction.price - transaction.fees),
            // Holding transfers are paired internal movements between accounts.
            "TRANSFER_IN" | "TRANSFER_OUT" => continue,
            _ => continue,
        };
        external_flow += convert_at(
            native_flow,
            &transaction.currency,
            transaction.timestamp,
        )?;
    }

    let gain = last.close - first.close - external_flow;
    gain.is_finite().then_some(gain)
}

fn realized_gain_from_transactions(
    transactions: &[Transaction],
    splits: &[SplitEvent],
    base: &str,
    usd_cad: Option<f64>,
) -> Option<f64> {
    #[derive(Default)]
    struct LedgerState {
        shares: f64,
        cost_basis: f64,
    }

    #[derive(Clone)]
    enum Event {
        Transaction(Transaction),
        Split(SplitEvent),
    }
    let mut events = transactions
        .iter()
        .cloned()
        .map(Event::Transaction)
        .chain(splits.iter().cloned().map(Event::Split))
        .collect::<Vec<_>>();
    events.sort_by_key(|event| match event {
        Event::Split(split) => (split.timestamp, activity_sort_priority("SPLIT"), i64::MIN),
        Event::Transaction(transaction) => (
            transaction.timestamp,
            activity_sort_priority(&transaction.transaction_type),
            transaction.id,
        ),
    });

    let mut ledgers = HashMap::<String, LedgerState>::new();
    let mut realized = 0.0;

    for event in events {
        match event {
            Event::Split(split) => {
                let symbol = split.provider_symbol.to_ascii_uppercase();
                let suffix = format!("|{symbol}");
                for (key, state) in ledgers.iter_mut() {
                    if key.ends_with(suffix.as_str()) && state.shares > 0.0000001 {
                        // Total basis is unchanged by a split; only the share count changes.
                        state.shares *= split.ratio;
                    }
                }
            }
            Event::Transaction(transaction) => {
                let key = format!(
                    "{}|{}",
                    transaction.account_id,
                    transaction.provider_symbol.to_ascii_uppercase()
                );
                let state = ledgers.entry(key).or_default();
                match transaction.transaction_type.as_str() {
                    "SELL" | "TRANSFER_OUT" => {
                        if state.shares + 0.0005 < transaction.shares || state.shares <= 0.0 {
                            return None;
                        }
                        let average_cost = if state.shares.abs() < f64::EPSILON {
                            0.0
                        } else {
                            state.cost_basis / state.shares
                        };
                        if transaction.transaction_type == "SELL" {
                            let native_gain = transaction.shares * transaction.price
                                - transaction.fees
                                - average_cost * transaction.shares;
                            realized += convert_currency(
                                native_gain,
                                &transaction.currency,
                                base,
                                usd_cad,
                            )?;
                        }
                        let removed_basis = average_cost * transaction.shares;
                        state.shares -= transaction.shares;
                        state.cost_basis = (state.cost_basis - removed_basis).max(0.0);
                        if state.shares.abs() < 0.0000001 {
                            state.shares = 0.0;
                            state.cost_basis = 0.0;
                        }
                    }
                    "BUY" | "OPEN" => {
                        state.shares += transaction.shares;
                        state.cost_basis += transaction.shares * transaction.price + transaction.fees;
                    }
                    "TRANSFER_IN" => {
                        state.shares += transaction.shares;
                        state.cost_basis += transaction.shares * transaction.price;
                    }
                    _ => return None,
                }
            }
        }
    }

    Some(realized)
}

fn build_portfolio_value_points(
    transactions: &[Transaction],
    cash_entries: &[CashEntry],
    split_events: &[SplitEvent],
    histories: &HashMap<String, Vec<PricePoint>>,
    fx_points: Option<&[PricePoint]>,
    base: &str,
    minimum: i64,
    maximum: Option<i64>,
) -> Vec<PricePoint> {
    if transactions.is_empty() && cash_entries.is_empty() {
        return Vec::new();
    }

    let mut sorted_transactions = transactions.to_vec();
    sorted_transactions.sort_by_key(|transaction| {
        (
            transaction.timestamp,
            activity_sort_priority(&transaction.transaction_type),
            transaction.id,
        )
    });
    let mut sorted_cash = cash_entries.to_vec();
    sorted_cash.sort_by_key(|entry| (entry.occurred_at, entry.id));
    let mut sorted_splits = split_events.to_vec();
    sorted_splits.sort_by_key(|split| split.timestamp);

    #[derive(Clone)]
    enum ShareEvent {
        Transaction(Transaction),
        Split(SplitEvent),
    }
    let mut share_events = sorted_transactions
        .iter()
        .cloned()
        .map(ShareEvent::Transaction)
        .chain(sorted_splits.iter().cloned().map(ShareEvent::Split))
        .collect::<Vec<_>>();
    share_events.sort_by_key(|event| match event {
        ShareEvent::Split(split) => (split.timestamp, activity_sort_priority("SPLIT"), i64::MIN),
        ShareEvent::Transaction(transaction) => (
            transaction.timestamp,
            activity_sort_priority(&transaction.transaction_type),
            transaction.id,
        ),
    });

    let first_event = share_events
        .first()
        .map(|event| match event {
            ShareEvent::Split(split) => split.timestamp,
            ShareEvent::Transaction(transaction) => transaction.timestamp,
        })
        .into_iter()
        .chain(sorted_cash.first().map(|entry| entry.occurred_at))
        .min()
        .unwrap_or_else(current_unix_timestamp);
    let first_visible = if minimum <= 0 {
        first_event
    } else {
        minimum.max(first_event)
    };

    let last_visible = maximum
        .unwrap_or_else(current_unix_timestamp)
        .max(first_visible);
    let mut timeline = BTreeSet::<i64>::new();
    timeline.insert(first_visible);
    timeline.insert(last_visible);
    for transaction in &sorted_transactions {
        if transaction.timestamp >= first_visible && transaction.timestamp <= last_visible {
            timeline.insert(transaction.timestamp);
        }
    }
    for entry in &sorted_cash {
        if entry.occurred_at >= first_visible && entry.occurred_at <= last_visible {
            timeline.insert(entry.occurred_at);
        }
    }
    for split in &sorted_splits {
        if split.timestamp >= first_visible && split.timestamp <= last_visible {
            timeline.insert(split.timestamp);
        }
    }
    for points in histories.values() {
        for point in points {
            if point.timestamp >= first_visible && point.timestamp <= last_visible {
                timeline.insert(point.timestamp);
            }
        }
    }

    let mut shares = HashMap::<String, f64>::new();
    let mut symbols = HashMap::<String, String>::new();
    let mut currencies = HashMap::<String, String>::new();
    let mut cash_balances = HashMap::<(i64, String), f64>::new();
    let mut share_event_index = 0usize;
    let mut cash_index = 0usize;
    let mut result = Vec::new();

    for timestamp in timeline {
        // Replay trades and corporate actions in one chronological stream. This
        // is important for short chart ranges: a split that happened before the
        // visible range must still be applied between the older buys around it.
        while share_event_index < share_events.len() {
            let event_timestamp = match &share_events[share_event_index] {
                ShareEvent::Split(split) => split.timestamp,
                ShareEvent::Transaction(transaction) => transaction.timestamp,
            };
            if event_timestamp > timestamp {
                break;
            }
            match &share_events[share_event_index] {
                ShareEvent::Split(split) => {
                    let split_symbol = split.provider_symbol.to_ascii_uppercase();
                    for (key, share_count) in shares.iter_mut() {
                        if symbols
                            .get(key)
                            .map(|symbol| symbol == &split_symbol)
                            .unwrap_or(false)
                        {
                            *share_count *= split.ratio;
                        }
                    }
                }
                ShareEvent::Transaction(transaction) => {
                    let symbol = transaction.provider_symbol.to_ascii_uppercase();
                    let key = format!("{}|{}", transaction.account_id, symbol);
                    let holding = shares.entry(key.clone()).or_insert(0.0);
                    match transaction.transaction_type.as_str() {
                        "SELL" | "TRANSFER_OUT" => *holding -= transaction.shares,
                        "BUY" | "OPEN" | "TRANSFER_IN" => *holding += transaction.shares,
                        _ => {}
                    }
                    symbols.insert(key.clone(), symbol);
                    currencies.insert(key, transaction.currency.clone());
                }
            }
            share_event_index += 1;
        }
        while cash_index < sorted_cash.len() && sorted_cash[cash_index].occurred_at <= timestamp {
            let entry = &sorted_cash[cash_index];
            *cash_balances
                .entry((entry.account_id, entry.currency.to_ascii_uppercase()))
                .or_insert(0.0) += entry.amount;
            cash_index += 1;
        }

        let mut total = 0.0;
        let mut complete = true;
        for (key, share_count) in &shares {
            if share_count.abs() < 0.0000001 {
                continue;
            }
            let Some(symbol) = symbols.get(key) else {
                complete = false;
                break;
            };
            let Some(symbol_history) = histories.get(symbol) else {
                complete = false;
                break;
            };
            let Some(price) = historical_close_at(Some(symbol_history.as_slice()), timestamp)
                .or_else(|| transaction_price_at(&sorted_transactions, symbol, timestamp))
            else {
                complete = false;
                break;
            };
            let native_value = share_count * price;
            let currency = currencies.get(key).map(String::as_str).unwrap_or(base);
            let converted = if currency == base {
                Some(native_value)
            } else {
                historical_fx_at(fx_points, timestamp).and_then(|rate| {
                    if currency == "USD" && base == "CAD" {
                        Some(native_value * rate)
                    } else if currency == "CAD" && base == "USD" && rate > 0.0 {
                        Some(native_value / rate)
                    } else {
                        None
                    }
                })
            };
            let Some(converted) = converted else {
                complete = false;
                break;
            };
            total += converted;
        }

        if complete {
            for ((_, currency), amount) in &cash_balances {
                let converted = if currency == base {
                    Some(*amount)
                } else {
                    historical_fx_at(fx_points, timestamp).and_then(|rate| {
                        if currency == "USD" && base == "CAD" {
                            Some(*amount * rate)
                        } else if currency == "CAD" && base == "USD" && rate > 0.0 {
                            Some(*amount / rate)
                        } else {
                            None
                        }
                    })
                };
                let Some(converted) = converted else {
                    complete = false;
                    break;
                };
                total += converted;
            }
        }

        if complete && total.is_finite() && total >= -0.005 {
            result.push(PricePoint {
                timestamp,
                close: total.max(0.0),
            });
        }
    }

    result.sort_by_key(|point| point.timestamp);
    result.dedup_by_key(|point| point.timestamp);
    result
}

fn historical_close_at(points: Option<&[PricePoint]>, timestamp: i64) -> Option<f64> {
    let points = points?;
    let index = points.partition_point(|point| point.timestamp <= timestamp);
    index.checked_sub(1).map(|index| points[index].close)
}

fn historical_fx_at(points: Option<&[PricePoint]>, timestamp: i64) -> Option<f64> {
    historical_close_at(points, timestamp)
}

fn transaction_price_at(
    transactions: &[Transaction],
    symbol: &str,
    timestamp: i64,
) -> Option<f64> {
    transactions
        .iter()
        .rev()
        .find(|transaction| {
            transaction.provider_symbol.eq_ignore_ascii_case(symbol)
                && transaction.timestamp <= timestamp
                && transaction.price > 0.0
        })
        .map(|transaction| transaction.price)
}

fn reset_adjustment_to_top(adjustment: &gtk::Adjustment) {
    adjustment.set_value(adjustment.lower());
}

fn restore_current_page_top(refs: &UiRefs) {
    let page = refs.current_page.borrow().clone();
    let adjustment = refs.page_scroll_adjustments.borrow().get(&page).cloned();
    let Some(adjustment) = adjustment else {
        return;
    };
    reset_adjustment_to_top(&adjustment);
    glib::timeout_add_local_once(Duration::from_millis(170), move || {
        reset_adjustment_to_top(&adjustment);
    });
}

fn begin_shortcut_refresh(refs: &UiRefs) {
    if refs.pull_refresh_active.get() {
        return;
    }
    let generation = refs.shortcut_refresh_generation.get().wrapping_add(1);
    refs.shortcut_refresh_generation.set(generation);
    refs.shortcut_refresh_active.set(true);

    // Ctrl+R uses a determinate browser-style line rather than ProgressBar's
    // pulse mode. Pulse mode is intentionally an independent moving segment;
    // keeping a real fraction makes the line stay attached to the left edge
    // and continuously extend across the header instead.
    refs.shortcut_refresh_bar.set_opacity(1.0);
    refs.shortcut_refresh_bar.set_fraction(0.015);
    refs.shortcut_refresh_bar.set_visible(true);

    let bar = refs.shortcut_refresh_bar.clone();
    let active = refs.shortcut_refresh_active.clone();
    let current_generation = refs.shortcut_refresh_generation.clone();
    glib::timeout_add_local(Duration::from_millis(16), move || {
        if !active.get() || current_generation.get() != generation {
            return glib::ControlFlow::Break;
        }

        // Ease toward 94% while work is outstanding. The remaining six percent
        // is reserved for the actual completion signal, so a slow request never
        // appears finished early. Updating at roughly the display refresh rate
        // keeps the leading edge fluid without turning back into a moving chunk.
        let current = bar.fraction();
        let remaining = (0.94 - current).max(0.0);
        let step = (remaining * 0.035).max(0.0007);
        bar.set_fraction((current + step).min(0.94));
        glib::ControlFlow::Continue
    });
}

fn finish_shortcut_refresh(refs: &UiRefs) {
    if !refs.shortcut_refresh_active.replace(false) {
        return;
    }
    let generation = refs.shortcut_refresh_generation.get();
    let bar = refs.shortcut_refresh_bar.clone();
    let active = refs.shortcut_refresh_active.clone();
    let current_generation = refs.shortcut_refresh_generation.clone();

    // Finish the same connected line all the way to the right edge. Keep it
    // fully visible for a moment, then crossfade the completed line away rather
    // than cutting it off. A newer Ctrl+R bumps the generation and cancels this
    // fade, while begin_shortcut_refresh() restores full opacity immediately.
    bar.set_fraction(1.0);
    bar.set_opacity(1.0);
    glib::timeout_add_local_once(Duration::from_millis(110), move || {
        if active.get() || current_generation.get() != generation {
            return;
        }

        let bar = bar.clone();
        let active = active.clone();
        let current_generation = current_generation.clone();
        let fade_started = std::time::Instant::now();
        glib::timeout_add_local(Duration::from_millis(16), move || {
            if active.get() || current_generation.get() != generation {
                return glib::ControlFlow::Break;
            }

            let progress = (fade_started.elapsed().as_secs_f64() / 0.16).clamp(0.0, 1.0);
            bar.set_opacity(1.0 - progress);
            if progress >= 1.0 {
                bar.set_visible(false);
                bar.set_fraction(0.0);
                bar.set_opacity(1.0);
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    });
}

fn finish_refresh_feedback(refs: &UiRefs) {
    if refs.pull_refresh_active.get() {
        finish_pull_refresh(refs);
    } else {
        finish_shortcut_refresh(refs);
    }
}

fn finish_pull_refresh(refs: &UiRefs) {
    if !refs.pull_refresh_active.get() {
        return;
    }
    let refs = refs.clone();
    // A local-only refresh can finish in the same frame that the pull is
    // released. Hold the spinner briefly so the armed -> refreshing state is
    // still perceptible, especially on Accounts with cached data.
    glib::timeout_add_local_once(Duration::from_millis(180), move || {
        if !refs.pull_refresh_active.replace(false) {
            return;
        }
        refs.pull_refresh_spinner.stop();
        refs.pull_refresh_spinner.set_visible(false);
        refs.pull_refresh_icon.set_visible(true);
        refs.pull_refresh_icon.set_opacity(1.0);
        restore_current_page_top(&refs);
        refs.pull_refresh_revealer.set_reveal_child(false);
        refs.pull_refresh_visual_revealer.set_reveal_child(false);
    });
}

struct PortfolioHistoryRefreshResult {
    generation: u64,
    range: HistoryRange,
    histories: Vec<(String, History)>,
    fx_history: Option<History>,
    failures: usize,
}

fn refresh_portfolio_history_async(refs: UiRefs, announce: bool) {
    let transactions = refs.state.database.load_transactions().unwrap_or_default();
    let cash_entries = refs.state.database.load_cash_entries().unwrap_or_default();
    if transactions.is_empty() && cash_entries.is_empty() {
        update_portfolio_history_from_cache(&refs);
        return;
    }

    let range = refs.portfolio_history_range.get();
    let now = current_unix_timestamp();
    let minimum = range.minimum_timestamp(now);
    let mut symbols = HashSet::<String>::new();
    for transaction in &transactions {
        symbols.insert(transaction.provider_symbol.to_ascii_uppercase());
    }

    let mut to_fetch = Vec::new();
    for symbol in symbols {
        let cached_empty = refs
            .state
            .database
            .history_points(&symbol, range.interval(), minimum)
            .map(|points| points.is_empty())
            .unwrap_or(true);
        let stale = refs
            .state
            .database
            .history_needs_refresh(&symbol, range.key(), range.interval(), range.cache_seconds())
            .unwrap_or(true);
        if cached_empty || stale {
            to_fetch.push(symbol);
        }
    }

    let base = base_currency(&refs.state);
    let needs_fx = transactions
        .iter()
        .any(|transaction| transaction.currency != base)
        || cash_entries.iter().any(|entry| entry.currency != base);
    let fetch_fx = if needs_fx {
        let cached_fx = refs
            .state
            .database
            .history_points("CAD=X", range.interval(), minimum)
            .unwrap_or_default();
        let cached_empty = cached_fx.is_empty();
        let one_day_window_too_short = range == HistoryRange::OneDay
            && cached_fx
                .first()
                .zip(cached_fx.last())
                .map(|(first, last)| last.timestamp.saturating_sub(first.timestamp) < 2 * 24 * 60 * 60)
                .unwrap_or(true);
        let stale = refs
            .state
            .database
            .history_needs_refresh("CAD=X", range.key(), range.interval(), range.cache_seconds())
            .unwrap_or(true);
        cached_empty || stale || one_day_window_too_short
    } else {
        false
    };

    if to_fetch.is_empty() && !fetch_fx {
        update_portfolio_history_from_cache(&refs);
        return;
    }

    let generation = refs.portfolio_history_generation.get().wrapping_add(1);
    refs.portfolio_history_generation.set(generation);

    let (sender, receiver) = mpsc::channel::<PortfolioHistoryRefreshResult>();
    std::thread::spawn(move || {
        let mut histories = Vec::new();
        let mut failures = 0usize;
        for symbol in to_fetch {
            match market_data::history(&symbol, range) {
                Ok(history) => histories.push((symbol, history)),
                Err(_) => failures += 1,
            }
        }
        let fx_history = if fetch_fx {
            // Keep the full OneDay backing window for conversion. Securities
            // themselves are trimmed to their latest market session.
            match market_data::history_window("CAD=X", range) {
                Ok(history) => Some(history),
                Err(_) => {
                    failures += 1;
                    None
                }
            }
        } else {
            None
        };
        let _ = sender.send(PortfolioHistoryRefreshResult {
            generation,
            range,
            histories,
            fx_history,
            failures,
        });
    });

    let receiver = Rc::new(RefCell::new(receiver));
    glib::timeout_add_local(Duration::from_millis(75), move || {
        let Ok(result) = receiver.borrow().try_recv() else {
            return glib::ControlFlow::Continue;
        };
        if refs.portfolio_history_generation.get() != result.generation {
            return glib::ControlFlow::Break;
        }

        for (symbol, history) in &result.histories {
            let _ = refs
                .state
                .database
                .save_history(symbol, result.range.interval(), &history.points);
            let _ = refs.state.database.set_history_fetched(
                symbol,
                result.range.key(),
                result.range.interval(),
            );
        }
        if let Some(history) = &result.fx_history {
            let _ = refs
                .state
                .database
                .save_history("CAD=X", result.range.interval(), &history.points);
            let _ = refs.state.database.set_history_fetched(
                "CAD=X",
                result.range.key(),
                result.range.interval(),
            );
        }

        update_portfolio_history_from_cache(&refs);
        if announce && result.failures > 0 {
            let message = if result.histories.is_empty() && result.fx_history.is_none() {
                "Could not refresh portfolio history"
            } else {
                "Some portfolio history could not be updated"
            };
            refs.toast_overlay.add_toast(Toast::new(message));
        }
        glib::ControlFlow::Break
    });
}

struct DividendRefreshResult {
    generation: u64,
    histories: Vec<(String, DividendHistory)>,
    failures: usize,
}

fn refresh_dividends_async(refs: UiRefs, positions: Vec<Position>, announce: bool) {
    if positions.is_empty() {
        if announce {
            refs.toast_overlay
                .add_toast(Toast::new("Add a holding before refreshing dividends"));
            finish_refresh_feedback(&refs);
        }
        return;
    }

    let generation = refs.dividend_refresh_generation.get().wrapping_add(1);
    refs.dividend_refresh_generation.set(generation);
    if announce {
        refs.dividend_status
            .set_label("Updating dividend history");
    }

    let mut symbols = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    for position in positions {
        let symbol = position.provider_symbol.trim().to_ascii_uppercase();
        if !symbol.is_empty() && seen.insert(symbol.clone()) {
            symbols.push(symbol);
        }
    }

    let (sender, receiver) = mpsc::channel::<DividendRefreshResult>();
    std::thread::spawn(move || {
        let mut histories = Vec::new();
        let mut failures = 0usize;
        for symbol in symbols {
            match market_data::dividends(&symbol) {
                Ok(history) => histories.push((symbol, history)),
                Err(_) => failures += 1,
            }
        }
        let _ = sender.send(DividendRefreshResult {
            generation,
            histories,
            failures,
        });
    });

    let receiver = Rc::new(RefCell::new(receiver));
    glib::timeout_add_local(Duration::from_millis(75), move || {
        let Ok(result) = receiver.borrow().try_recv() else {
            return glib::ControlFlow::Continue;
        };
        if refs.dividend_refresh_generation.get() != result.generation {
            if announce {
                finish_refresh_feedback(&refs);
            }
            return glib::ControlFlow::Break;
        }

        for (symbol, history) in &result.histories {
            let currency = history
                .currency
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("N/A");
            let _ = refs
                .state
                .database
                .replace_dividend_events(symbol, currency, &history.events);
            let _ = refs
                .state
                .database
                .replace_split_events(symbol, &history.splits);
            if let Some(calendar) = &history.calendar {
                let _ = refs.state.database.set_dividend_calendar(
                    symbol,
                    calendar.ex_dividend_date,
                    calendar.payment_date,
                );
            }
            let _ = refs.state.database.set_dividends_fetched(symbol);
        }

        let _ = refs.state.database.sync_positions_from_activity();
        let _ = refs.state.database.sync_paid_dividends_to_cash();
        refresh_with_loaded_crossfade(refs.clone());
        refresh_portfolio_history_async(refs.clone(), false);

        if announce {
            if result.failures > 0 {
                let message = if result.histories.is_empty() {
                    "Could not refresh dividend data"
                } else {
                    "Some dividend data could not be updated"
                };
                refs.toast_overlay.add_toast(Toast::new(message));
            }
            finish_refresh_feedback(&refs);
        }
        glib::ControlFlow::Break
    });
}

struct WatchRefreshResult {
    generation: u64,
    histories: Vec<(i64, String, History)>,
    failures: usize,
}

fn refresh_watchlist_async(refs: UiRefs, items: Vec<WatchlistItem>, announce: bool) {
    if items.is_empty() {
        if announce {
            refs.toast_overlay
                .add_toast(Toast::new("Add a stock to your watchlist first"));
            finish_refresh_feedback(&refs);
        }
        return;
    }
    let generation = refs.watchlist_refresh_generation.get().wrapping_add(1);
    refs.watchlist_refresh_generation.set(generation);

    let (sender, receiver) = mpsc::channel::<WatchRefreshResult>();
    std::thread::spawn(move || {
        let mut histories = Vec::new();
        let mut failures = 0usize;
        for item in items {
            match market_data::history(&item.provider_symbol, HistoryRange::OneMonth) {
                Ok(history) => histories.push((item.id, item.provider_symbol, history)),
                Err(_) => failures += 1,
            }
        }
        let _ = sender.send(WatchRefreshResult {
            generation,
            histories,
            failures,
        });
    });

    let receiver = Rc::new(RefCell::new(receiver));
    glib::timeout_add_local(Duration::from_millis(75), move || {
        let Ok(result) = receiver.borrow().try_recv() else {
            return glib::ControlFlow::Continue;
        };
        if refs.watchlist_refresh_generation.get() != result.generation {
            if announce {
                finish_refresh_feedback(&refs);
            }
            return glib::ControlFlow::Break;
        }
        for (item_id, symbol, history) in &result.histories {
            let _ = refs.state.database.save_history(
                symbol,
                HistoryRange::OneMonth.interval(),
                &history.points,
            );
            let _ = refs.state.database.set_history_fetched(
                symbol,
                HistoryRange::OneMonth.key(),
                HistoryRange::OneMonth.interval(),
            );
            if let Some(price) = history.current_price {
                let _ = refs.state.database.update_watchlist_quote(
                    *item_id,
                    price,
                    history.day_change_percent,
                    history.quote_timestamp,
                );
            }
        }
        refresh_with_loaded_crossfade(refs.clone());
        if announce {
            if result.failures > 0 {
                let message = if result.histories.is_empty() {
                    "Could not refresh watchlist data"
                } else {
                    "Some watchlist data could not be updated"
                };
                refs.toast_overlay.add_toast(Toast::new(message));
            }
            finish_refresh_feedback(&refs);
        }
        glib::ControlFlow::Break
    });
}

struct RefreshResult {
    generation: u64,
    quotes: Vec<(Vec<i64>, Quote)>,
    quote_failures: usize,
    quote_network_failures: usize,
    quote_attempted: bool,
    fx_attempted: bool,
    fx: Option<Result<FxQuote, String>>,
}

fn refresh_market_async(
    refs: UiRefs,
    quote_positions: Vec<Position>,
    fetch_fx: bool,
    announce: bool,
) {
    if quote_positions.is_empty() && !fetch_fx {
        if announce {
            finish_refresh_feedback(&refs);
        }
        return;
    }

    let quote_attempted = !quote_positions.is_empty();
    let generation = refs.market_refresh_generation.get().wrapping_add(1);
    refs.market_refresh_generation.set(generation);

    let (sender, receiver) = mpsc::channel::<RefreshResult>();
    std::thread::spawn(move || {
        let mut quote_targets = HashMap::<String, Vec<i64>>::new();
        for position in quote_positions {
            let symbol = position.api_symbol().trim().to_ascii_uppercase();
            if !symbol.is_empty() {
                quote_targets.entry(symbol).or_default().push(position.id);
            }
        }

        let mut quotes = Vec::new();
        let mut quote_failures = 0usize;
        let mut quote_network_failures = 0usize;
        for (symbol, position_ids) in quote_targets {
            match market_data::quote(&symbol) {
                Ok(quote) => quotes.push((position_ids, quote)),
                Err(error) => {
                    quote_failures += 1;
                    if market_data::quote_health_from_error(&error.to_string()) == "Network unavailable" {
                        quote_network_failures += 1;
                    }
                }
            }
        }
        let fx = fetch_fx.then(|| fx::usd_cad().map_err(|error| error.to_string()));
        let _ = sender.send(RefreshResult {
            generation,
            quotes,
            quote_failures,
            quote_network_failures,
            quote_attempted,
            fx_attempted: fetch_fx,
            fx,
        });
    });

    let receiver = Rc::new(RefCell::new(receiver));
    glib::timeout_add_local(Duration::from_millis(75), move || {
        let Ok(result) = receiver.borrow().try_recv() else {
            return glib::ControlFlow::Continue;
        };
        if refs.market_refresh_generation.get() != result.generation {
            if announce {
                finish_refresh_feedback(&refs);
            }
            return glib::ControlFlow::Break;
        }

        for (position_ids, quote) in &result.quotes {
            for position_id in position_ids {
                let _ = refs.state.database.update_quote(
                    *position_id,
                    quote.close,
                    quote.change_percent,
                    quote.timestamp,
                );
            }
        }
        let mut fx_failed = false;
        if let Some(fx_result) = result.fx {
            match fx_result {
                Ok(rate) => {
                    let _ = refs.state.database.set_fx_rate(
                        USD_CAD_PAIR,
                        rate.rate,
                        &rate.observation_date,
                    );
                }
                Err(_) => fx_failed = true,
            }
        }

        let _ = refs.state.database.sync_paid_dividends_to_cash();
        refresh_with_loaded_crossfade(refs.clone());

        if announce {
            let message = if !result.quote_attempted && result.fx_attempted && fx_failed {
                Some("Could not refresh exchange rate")
            } else if result.quotes.is_empty() && result.quote_failures > 0 && result.quote_network_failures == result.quote_failures {
                Some("Network unavailable · using cached prices")
            } else if result.quotes.is_empty() && result.quote_failures > 0 {
                Some("Quotes unavailable · using cached prices")
            } else if result.quote_failures > 0 && fx_failed {
                Some("Some prices and the exchange rate could not be updated")
            } else if result.quote_failures > 0 {
                Some("Some prices could not be updated · stale values may remain")
            } else if fx_failed {
                Some("Exchange rate could not be updated")
            } else {
                None
            };
            if let Some(message) = message {
                refs.toast_overlay.add_toast(Toast::new(message));
            }
            finish_refresh_feedback(&refs);
        }
        glib::ControlFlow::Break
    });
}

fn aureus_theme_enabled(state: &AppState) -> bool {
    !matches!(
        state
            .database
            .setting(AUREUS_THEME_KEY)
            .ok()
            .flatten()
            .as_deref(),
        Some("0")
    )
}

fn apply_appearance(state: &AppState) {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let enabled = aureus_theme_enabled(state);
    crate::style::set_aureus_theme(enabled);
    let manager = adw::StyleManager::for_display(&display);
    manager.set_color_scheme(if enabled {
        adw::ColorScheme::ForceDark
    } else {
        adw::ColorScheme::Default
    });
}

fn base_currency(state: &AppState) -> String {
    match state
        .database
        .setting(BASE_CURRENCY_KEY)
        .ok()
        .flatten()
        .as_deref()
    {
        Some("CAD") => "CAD".into(),
        Some("USD") => "USD".into(),
        _ => state
            .database
            .load_accounts()
            .ok()
            .and_then(|accounts| accounts.into_iter().next())
            .map(|account| account.currency)
            .filter(|currency| matches!(currency.as_str(), "CAD" | "USD"))
            .unwrap_or_else(|| "USD".into()),
    }
}

fn portfolio_needs_fx_with_cash(state: &AppState, positions: &[Position], base: &str) -> bool {
    portfolio_needs_fx(positions, base)
        || state
            .database
            .load_accounts()
            .unwrap_or_default()
            .iter()
            .any(|account| {
                account.cash.abs() > 0.005
                    && account.currency != base
                    && matches!(account.currency.as_str(), "CAD" | "USD")
                    && matches!(base, "CAD" | "USD")
            })
}

fn portfolio_needs_fx(positions: &[Position], base: &str) -> bool {
    positions.iter().any(|position| {
        position.currency != base
            && matches!(position.currency.as_str(), "CAD" | "USD")
            && matches!(base, "CAD" | "USD")
    })
}

fn converted_market_value(position: &Position, base: &str, usd_cad: Option<f64>) -> Option<f64> {
    convert_currency(position.market_value()?, &position.currency, base, usd_cad)
}

fn converted_total_gain(position: &Position, base: &str, usd_cad: Option<f64>) -> Option<f64> {
    convert_currency(position.total_gain()?, &position.currency, base, usd_cad)
}

fn sum_converted<'a>(
    values: impl Iterator<Item = (f64, &'a str)>,
    base: &str,
    usd_cad: Option<f64>,
) -> Option<f64> {
    let mut total = 0.0;
    for (value, currency) in values {
        total += convert_currency(value, currency, base, usd_cad)?;
    }
    Some(total)
}

fn sum_optional_converted<'a>(
    values: impl Iterator<Item = (Option<f64>, &'a str)>,
    base: &str,
    usd_cad: Option<f64>,
) -> Option<f64> {
    let mut total = 0.0;
    for (value, currency) in values {
        total += convert_currency(value?, currency, base, usd_cad)?;
    }
    Some(total)
}

fn market_status_text(positions: &[Position], base: &str, fx: Option<&FxRate>) -> String {
    let quote_times = positions
        .iter()
        .filter_map(|position| position.quote_updated_at)
        .collect::<Vec<_>>();
    let mut parts = Vec::new();
    if quote_times.len() == positions.len() {
        if let Some(oldest) = quote_times.iter().min() {
            let state = market_data::quote_state_label(None, *oldest, current_unix_timestamp());
            parts.push(format!("{} · oldest {}", state, relative_time(*oldest)));
        }
    } else if let Some(oldest) = quote_times.iter().min() {
        parts.push(format!("Some quotes unavailable · cached {}", relative_time(*oldest)));
    } else {
        parts.push("Quotes unavailable".into());
    }

    if portfolio_needs_fx(positions, base) {
        if let Some(rate) = fx {
            parts.push(format!("USD/CAD {:.4} · {}", rate.rate, rate.observation_date));
        } else {
            parts.push("waiting for USD/CAD rate".into());
        }
    }

    if positions
        .iter()
        .any(|position| !matches!(position.currency.as_str(), "CAD" | "USD"))
    {
        parts.push("some currencies are shown natively".into());
    }
    parts.join(" · ")
}

#[derive(Clone)]
struct DateChooser {
    row: ActionRow,
    calendar: gtk::Calendar,
}

impl DateChooser {
    fn today() -> Self {
        Self::new(&current_date_string())
    }

    fn new(initial: &str) -> Self {
        let (year, month, day) = parse_trade_date(initial)
            .ok()
            .map(|timestamp| civil_from_days(timestamp.div_euclid(86_400)))
            .unwrap_or_else(local_date_parts);

        // Use GTK's calendar widget directly. It owns the month/year controls,
        // day grid, keyboard handling, and theme integration; Aureus only wraps
        // it in a popover and mirrors the selected date in the preferences row.
        let calendar = gtk::Calendar::new();
        calendar.set_show_week_numbers(false);
        let local_timezone = glib::TimeZone::local();
        if let Ok(selected) = glib::DateTime::new(
            &local_timezone,
            year,
            month as i32,
            day as i32,
            12,
            0,
            0.0,
        ) {
            calendar.set_date(&selected);
        }

        let row = ActionRow::builder()
            .title("Date")
            .subtitle(format!("{} {day}, {year}", month_name(month)))
            .build();

        let chooser_button = MenuButton::builder()
            .icon_name("x-office-calendar-symbolic")
            .css_classes(["flat"])
            .valign(Align::Center)
            .tooltip_text("Choose Date")
            .build();

        let popover = gtk::Popover::new();
        popover.set_child(Some(&calendar));
        chooser_button.set_popover(Some(&popover));
        row.add_suffix(&chooser_button);
        row.set_activatable_widget(Some(&chooser_button));

        {
            let row = row.clone();
            let popover = popover.clone();
            calendar.connect_day_selected(move |calendar| {
                let selected = calendar.date();
                row.set_subtitle(&format!(
                    "{} {}, {}",
                    month_name(selected.month() as u32),
                    selected.day_of_month(),
                    selected.year()
                ));
                popover.popdown();
            });
        }

        Self { row, calendar }
    }

    fn value(&self) -> String {
        let selected = self.calendar.date();
        format!(
            "{:04}-{:02}-{:02}",
            selected.year(),
            selected.month(),
            selected.day_of_month()
        )
    }
}

fn local_date_parts() -> (i32, u32, u32) {
    if let Ok(now) = glib::DateTime::now_local() {
        return (now.year(), now.month() as u32, now.day_of_month() as u32);
    }
    civil_from_days(current_unix_timestamp().div_euclid(86_400))
}

fn current_date_string() -> String {
    let (year, month, day) = local_date_parts();
    format!("{year:04}-{month:02}-{day:02}")
}

fn parse_trade_date(value: &str) -> Result<i64, ()> {
    let mut parts = value.trim().split('-');
    let year = parts.next().ok_or(())?.parse::<i32>().map_err(|_| ())?;
    let month = parts.next().ok_or(())?.parse::<u32>().map_err(|_| ())?;
    let day = parts.next().ok_or(())?.parse::<u32>().map_err(|_| ())?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(());
    }
    let days = days_from_civil(year, month, day);
    if civil_from_days(days) != (year, month, day) {
        return Err(());
    }
    Ok(days.saturating_mul(86_400))
}

fn activity_timestamp(value: &str) -> Result<i64, ()> {
    // Activity uses a date picker without a time field, so persist the selected
    // date consistently at the start of that date. Using the current clock time
    // for today's activity made identical date-only entries behave differently
    // depending on when they were entered.
    parse_trade_date(value)
}

fn date_is_future(value: &str) -> bool {
    let Ok(date) = parse_trade_date(value) else {
        return false;
    };
    let Ok(today) = parse_trade_date(&current_date_string()) else {
        return false;
    };
    date > today
}

// Inverse of civil_from_days, based on Howard Hinnant's public-domain algorithm.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let mut y = i64::from(year);
    let m = i64::from(month);
    let d = i64::from(day);
    y -= if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = m + if m > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn timestamp_year_month(timestamp: i64) -> Option<(i32, u32)> {
    if timestamp <= 0 {
        return None;
    }
    let (year, month, _) = civil_from_days(timestamp.div_euclid(86_400));
    Some((year, month))
}

fn format_distribution_date(timestamp: i64) -> String {
    if timestamp <= 0 {
        return "Unknown date".into();
    }
    let (year, month, day) = civil_from_days(timestamp.div_euclid(86_400));
    format!("{} {day}, {year}", month_name(month))
}

fn month_name(month: u32) -> &'static str {
    match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "?",
    }
}

// Gregorian civil-date conversion adapted from Howard Hinnant's public-domain
// civil_from_days algorithm. Keeping this local avoids pulling in a date crate
// just for compact dividend labels.
fn civil_from_days(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year as i32, month as u32, day as u32)
}

fn current_unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn relative_time(timestamp: i64) -> String {
    let seconds = current_unix_timestamp().saturating_sub(timestamp);
    if seconds < 60 {
        "just now".into()
    } else if seconds < 60 * 60 {
        format!("{} min ago", seconds / 60)
    } else if seconds < 24 * 60 * 60 {
        format!("{} h ago", seconds / (60 * 60))
    } else {
        format!("{} d ago", seconds / (24 * 60 * 60))
    }
}

fn set_gain_class(label: &Label, value: f64) {
    label.remove_css_class("success");
    label.remove_css_class("error");
    if value > 0.0 {
        label.add_css_class("success");
    } else if value < 0.0 {
        label.add_css_class("error");
    }
}

fn format_money_number(value: f64) -> String {
    let sign = if value.is_sign_negative() { "-" } else { "" };
    let raw = format!("{:.2}", value.abs());
    let (whole, fraction) = raw.split_once('.').unwrap_or((raw.as_str(), "00"));
    if whole.len() < 5 {
        return format!("{sign}{raw}");
    }

    let mut grouped = String::with_capacity(whole.len() + whole.len() / 3);
    let first = whole.len() % 3;
    if first > 0 {
        grouped.push_str(&whole[..first]);
        if first < whole.len() {
            grouped.push(',');
        }
    }
    for (index, chunk) in whole[first..].as_bytes().chunks(3).enumerate() {
        if index > 0 {
            grouped.push(',');
        }
        grouped.push_str(std::str::from_utf8(chunk).unwrap_or_default());
    }
    format!("{sign}{grouped}.{fraction}")
}

// Keep the dividend headline on GTK/Pango's normal text path. Per-glyph font
// markup caused the dollar stem to render as a detached artifact on some
// systems/themes.
fn set_dividend_income_text(label: &Label, text: &str) {
    label.set_label(text);
}

fn format_currency(value: f64, currency: &str) -> String {
    let prefix = match currency {
        "CAD" => "C$",
        "USD" => "US$",
        "EUR" => "€",
        "GBP" => "£",
        _ => "",
    };
    let number = format_money_number(value);
    if prefix.is_empty() {
        format!("{number} {currency}")
    } else {
        format!("{prefix}{number}")
    }
}

fn format_signed_currency(value: f64, currency: &str) -> String {
    let absolute = format_currency(value.abs(), currency);
    if value < 0.0 {
        format!("−{absolute}")
    } else {
        format!("+{absolute}")
    }
}

fn shares_entry_row(initial: f64) -> EntryRow {
    let row = EntryRow::new();
    row.set_title("Shares");
    row.set_text(&trim_number(initial));
    row
}

fn shares_value(row: &EntryRow) -> Option<f64> {
    let value = row.text().trim().replace(',', "").parse::<f64>().ok()?;
    (value.is_finite() && value > 0.0).then_some(value)
}

fn money_entry_row(title: &str, initial: f64) -> EntryRow {
    let row = EntryRow::new();
    row.set_title(title);
    row.set_text(&trim_number(initial.max(0.0)));
    row
}

fn money_value(row: &EntryRow) -> Option<f64> {
    let value = row.text().trim().replace(',', "").parse::<f64>().ok()?;
    (value.is_finite() && value >= 0.0).then_some(value)
}

fn shares_text(value: f64) -> String {
    format!(
        "{} {}",
        trim_number(value),
        if (value - 1.0).abs() < f64::EPSILON {
            "share"
        } else {
            "shares"
        }
    )
}

fn holding_count_text(count: usize) -> String {
    if count == 1 {
        "1 holding".into()
    } else {
        format!("{count} holdings")
    }
}

fn trim_number(value: f64) -> String {
    let mut text = format!("{value:.3}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

fn string_model(values: &[&str]) -> StringList {
    let model = StringList::new(&[]);
    for value in values {
        model.append(value);
    }
    model
}

fn account_model(accounts: &[Account]) -> StringList {
    let model = StringList::new(&[]);
    for account in accounts {
        model.append(&account_choice_label(account));
    }
    model
}

fn account_choice_label(account: &Account) -> String {
    format!("{} · {}", account.name, account.currency)
}

fn currency_at(index: u32) -> &'static str {
    if index == 1 { "USD" } else { "CAD" }
}

fn friendly_asset_type(asset_type: &str) -> &str {
    if asset_type.eq_ignore_ascii_case("common stock") || asset_type.eq_ignore_ascii_case("stock") {
        "Stock"
    } else if asset_type.eq_ignore_ascii_case("etf") {
        "ETF"
    } else if asset_type.eq_ignore_ascii_case("fund")
        || asset_type.eq_ignore_ascii_case("mutual fund")
    {
        "Fund"
    } else if asset_type.eq_ignore_ascii_case("index") {
        "Index"
    } else {
        asset_type
    }
}

fn friendly_exchange(exchange: &str) -> &str {
    match exchange {
        "TOR" | "TO" => "Toronto",
        "VAN" | "V" => "TSX Venture",
        "NMS" | "NGM" | "NCM" | "NASDAQ" => "Nasdaq",
        "NYQ" | "NYSE" => "NYSE",
        "ASE" => "NYSE American",
        "PCX" => "NYSE Arca",
        "BTS" => "Cboe",
        "LSE" => "London",
        "US" => "US",
        other => other,
    }
}

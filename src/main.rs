mod allocation_ring;
mod chart;
mod database;
mod dividend_chart;
mod fx;
mod market_data;
mod market_providers;
mod report;
mod model;
mod sparkline;
mod stock_image;
mod style;
mod storage;
mod ui;

use adw::prelude::*;
use adw::Application;
use gtk::gio;

pub const APP_ID: &str = "io.github.Mars7x.Aureus";
pub const APP_VERSION: &str = "1.0.2";

fn main() -> adw::glib::ExitCode {
    let app = Application::builder().application_id(APP_ID).build();

    app.connect_startup(|app| {
        style::install();

        let quit = gio::SimpleAction::new("quit", None);
        let app_weak = app.downgrade();
        quit.connect_activate(move |_, _| {
            if let Some(app) = app_weak.upgrade() {
                app.quit();
            }
        });
        app.add_action(&quit);
        app.set_accels_for_action("app.quit", &["<Primary>q"]);
    });

    app.connect_activate(|app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }

        let window = match ui::build_window(app) {
            Ok(window) => window,
            Err(error) => ui::build_error_window(app, &error),
        };
        window.present();
    });

    app.run()
}

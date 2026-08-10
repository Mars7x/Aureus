use std::path::PathBuf;

use crate::APP_ID;

/// Root for persistent Aureus-owned data.
///
/// Flatpak already gives each application a private XDG data directory at
/// ~/.var/app/<APP_ID>/data, so adding APP_ID again would create a redundant
/// data/<APP_ID> nesting. Native installs still need their own namespace under
/// the shared user data directory.
pub fn data_root() -> PathBuf {
    let base = gtk::glib::user_data_dir();
    if std::env::var_os("FLATPAK_ID").is_some() {
        base
    } else {
        base.join(APP_ID)
    }
}

pub fn database_path() -> PathBuf {
    data_root().join("portfolio.db")
}

pub fn stock_pictures_dir() -> PathBuf {
    data_root().join("stock-pictures")
}

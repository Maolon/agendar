#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Started from a terminal the app inherits its I/O policy; an iCloud
    // Vault needs evicted files downloaded on read, not refused.
    agendar_materialize::allow_downloads();
    agendar_app::run();
}

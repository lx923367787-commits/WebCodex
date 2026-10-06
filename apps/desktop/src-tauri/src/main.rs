#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args().any(|argument| argument == "--check-embedded-assets") {
        std::process::exit(if webcodex_desktop_lib::has_embedded_frontend() {
            0
        } else {
            2
        });
    }
    webcodex_desktop_lib::run();
}

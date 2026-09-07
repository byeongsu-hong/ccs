//! The switcher as a desktop app: one window on every platform, a menu bar
//! item on macOS, and the gateway and the watcher in this very process.

mod backend;
mod format;

ui_lang::include_app!("src/ui/app.ice");

fn main() -> iced::Result {
    Ccs::run()
}

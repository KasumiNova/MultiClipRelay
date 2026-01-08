mod config;
mod i18n;
mod procs;
mod systemd;
mod ui;
mod util;

use gtk4::prelude::*;

fn main() {
    // GTK apps must run on the main thread.
    let app = gtk4::Application::builder()
        .application_id("io.github.kasumiknova.multicliprelay.ui-gtk")
        .build();

    // Single-instance safety net:
    // If another instance is already running, we become a remote and just activate it.
    // This prevents multiple UI processes from piling up when the launcher is clicked repeatedly.
    if app.register(None::<&gtk4::gio::Cancellable>).is_ok() && app.is_remote() {
        app.activate();
        return;
    }

    app.connect_activate(|app| {
        // `activate` can be triggered multiple times (e.g. clicking the launcher repeatedly).
        // Ensure we don't create multiple windows within a single process.
        if let Some(win) = app
            .active_window()
            .or_else(|| app.windows().into_iter().next())
        {
            win.present();
            return;
        }

        ui::build_ui(app);
    });
    app.run();
}

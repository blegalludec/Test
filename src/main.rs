mod actions;
mod inventory;
mod ostree;
mod runner;
mod ssh;
mod ui;

use adw::prelude::*;

pub const APP_ID: &str = "io.github.blegalludec.SilverblueFleet";

fn main() -> gtk::glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(|app| {
        // Une seule fenêtre : la réafficher si l'application est relancée.
        if let Some(win) = app.active_window() {
            win.present();
            return;
        }
        let path = std::env::args()
            .nth(1)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(inventory::Inventory::default_path);
        ui::Window::build(app, path);
    });
    // Le chemin d'inventaire éventuel est lu ci-dessus ; GTK ne doit pas
    // l'interpréter comme un fichier à ouvrir.
    app.run_with_args::<&str>(&[])
}

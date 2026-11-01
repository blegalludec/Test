//! Dialogue d'ajout / modification d'un poste.

use crate::inventory::{validate_host, Host};
use adw::prelude::*;

pub enum HostDialogResult {
    Save(Host),
    Delete,
}

/// Affiche le dialogue. `check` est appelé à chaque saisie pour valider le
/// formulaire (unicité du nom…) ; `on_done` reçoit le résultat final.
pub fn show(
    parent: &impl IsA<gtk::Widget>,
    existing: Option<&Host>,
    check: impl Fn(&Host) -> Result<(), String> + 'static,
    on_done: impl Fn(HostDialogResult) + 'static,
) {
    let editing = existing.is_some();
    let dialog = adw::AlertDialog::new(
        Some(if editing {
            "Modifier le poste"
        } else {
            "Ajouter un poste"
        }),
        None,
    );

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);

    let name = adw::EntryRow::builder().title("Nom").build();
    let address = adw::EntryRow::builder()
        .title("Adresse (DNS ou IP)")
        .build();
    let user = adw::EntryRow::builder().title("Utilisateur SSH").build();
    let port = adw::SpinRow::with_range(1.0, 65535.0, 1.0);
    port.set_title("Port SSH");
    let groups = adw::EntryRow::builder()
        .title("Groupes (séparés par des virgules)")
        .build();
    let notes = adw::EntryRow::builder().title("Notes").build();

    let host = existing.cloned().unwrap_or_else(|| Host::new("", ""));
    name.set_text(&host.name);
    address.set_text(&host.address);
    user.set_text(&host.user);
    port.set_value(host.port as f64);
    groups.set_text(&host.groups.join(", "));
    notes.set_text(&host.notes);

    let entries = [name.clone(), address.clone(), user.clone(), groups.clone()];
    for row in [
        name.upcast_ref::<gtk::Widget>(),
        address.upcast_ref(),
        user.upcast_ref(),
        port.upcast_ref(),
        groups.upcast_ref(),
        notes.upcast_ref(),
    ] {
        list.append(row);
    }

    let error = gtk::Label::builder().wrap(true).visible(false).build();
    error.add_css_class("error");

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.append(&list);
    content.append(&error);
    dialog.set_extra_child(Some(&content));

    dialog.add_response("cancel", "Annuler");
    if editing {
        dialog.add_response("delete", "Supprimer");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    }
    dialog.add_response("save", "Enregistrer");
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("save"));
    dialog.set_close_response("cancel");

    let collect = move || Host {
        name: name.text().trim().to_string(),
        address: address.text().trim().to_string(),
        user: user.text().trim().to_string(),
        port: port.value() as u16,
        groups: groups
            .text()
            .split(',')
            .map(|g| g.trim().to_string())
            .filter(|g| !g.is_empty())
            .collect(),
        notes: notes.text().trim().to_string(),
    };
    let collect = std::rc::Rc::new(collect);

    let revalidate = {
        // Référence faible : évite un cycle dialogue -> signal -> dialogue.
        let dialog = dialog.downgrade();
        let collect = collect.clone();
        move || {
            let Some(dialog) = dialog.upgrade() else {
                return;
            };
            let res = {
                let h = collect();
                validate_host(&h).and_then(|_| check(&h))
            };
            match res {
                Ok(()) => {
                    error.set_visible(false);
                    dialog.set_response_enabled("save", true);
                }
                Err(msg) => {
                    error.set_label(&msg);
                    error.set_visible(true);
                    dialog.set_response_enabled("save", false);
                }
            }
        }
    };
    let revalidate = std::rc::Rc::new(revalidate);
    revalidate();
    for entry in &entries {
        let r = revalidate.clone();
        entry.connect_changed(move |_| r());
    }

    dialog.connect_response(None, move |_, response| match response {
        "save" => on_done(HostDialogResult::Save(collect())),
        "delete" => on_done(HostDialogResult::Delete),
        _ => {}
    });

    dialog.present(Some(parent));
}

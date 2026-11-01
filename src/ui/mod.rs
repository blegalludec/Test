//! Fenêtre principale : liste des postes, détail, actions et journal.

mod host_dialog;

use crate::actions::{split_list, Action, AutoUpdatePolicy};
use crate::inventory::{Host, Inventory};
use crate::ostree::HostStatus;
use crate::runner::{self, Job, JobEvent};
use adw::prelude::*;
use gtk::{gio, glib};
use host_dialog::HostDialogResult;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

/// Nombre maximal de lignes conservées dans le journal.
const LOG_MAX_LINES: i32 = 5000;

type WinCallback = Box<dyn Fn(&Rc<Window>)>;

/// Ce que l'on sait d'un poste.
#[derive(Default, Clone)]
struct HostInfo {
    /// Nombre de tâches en cours.
    busy: usize,
    status: Option<HostStatus>,
    /// Dernière erreur rencontrée.
    error: Option<String>,
}

struct State {
    inventory: Inventory,
    path: PathBuf,
    /// L'inventaire n'a pas pu être lu : on refuse de l'écraser.
    read_only: bool,
    infos: HashMap<String, HostInfo>,
    checked: HashSet<String>,
    current: Option<String>,
    /// Noms des postes affichés, dans l'ordre de la liste.
    visible: Vec<String>,
    filter: String,
}

struct Details {
    stack: gtk::Stack,
    group: adw::PreferencesGroup,
    connection: adw::ActionRow,
    state: adw::ActionRow,
    booted: adw::ActionRow,
    source: adw::ActionRow,
    pending: adw::ActionRow,
    layered: adw::ActionRow,
    groups: adw::ActionRow,
}

pub struct Window {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    list: gtk::ListBox,
    target_label: gtk::Label,
    details: Details,
    log: gtk::TextBuffer,
    log_view: gtk::TextView,
    state: RefCell<State>,
    tx: async_channel::Sender<JobEvent>,
}

fn info_row(title: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .use_markup(false)
        .subtitle_selectable(true)
        .build();
    row.add_css_class("property");
    row
}

fn button(label: &str, css: Option<&str>) -> gtk::Button {
    let b = gtk::Button::builder()
        .label(label)
        .valign(gtk::Align::Center)
        .build();
    if let Some(c) = css {
        b.add_css_class(c);
    }
    b
}

fn format_timestamp(ts: Option<i64>) -> String {
    ts.and_then(|t| glib::DateTime::from_unix_local(t).ok())
        .and_then(|d| d.format("%d/%m/%Y %H:%M").ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "?".into())
}

impl Window {
    pub fn build(app: &adw::Application, path: PathBuf) -> Rc<Self> {
        let (inventory, load_error) = match Inventory::load(&path) {
            Ok(inv) => (inv, None),
            Err(e) => (Inventory::default(), Some(e.to_string())),
        };
        let (tx, rx) = async_channel::unbounded();

        // ---------- Barre d'en-tête ----------
        let header = adw::HeaderBar::new();
        let add_btn = gtk::Button::from_icon_name("list-add-symbolic");
        add_btn.set_tooltip_text(Some("Ajouter un poste (Ctrl+N)"));
        add_btn.set_action_name(Some("win.add-host"));
        let refresh_btn = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh_btn.set_tooltip_text(Some("Actualiser l'état des postes affichés (Ctrl+R)"));
        refresh_btn.set_action_name(Some("win.refresh"));
        header.pack_start(&add_btn);
        header.pack_start(&refresh_btn);

        let menu = gio::Menu::new();
        menu.append(Some("Recharger l'inventaire"), Some("win.reload"));
        menu.append(
            Some("Ouvrir le fichier d'inventaire"),
            Some("win.open-inventory"),
        );
        menu.append(Some("Effacer le journal"), Some("win.clear-log"));
        menu.append(Some("À propos"), Some("win.about"));
        let menu_btn = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .primary(true)
            .tooltip_text("Menu principal")
            .build();
        header.pack_end(&menu_btn);

        // ---------- Barre latérale : postes ----------
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Filtrer (nom, adresse, groupe)")
            .build();
        let select_all = gtk::Button::builder().label("Tout cocher").build();
        select_all.add_css_class("flat");
        let select_none = gtk::Button::builder().label("Aucun").build();
        select_none.add_css_class("flat");
        let target_label = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label", "caption"])
            .build();
        let sel_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        sel_box.append(&select_all);
        sel_box.append(&select_none);

        let list = gtk::ListBox::new();
        list.add_css_class("navigation-sidebar");
        list.set_selection_mode(gtk::SelectionMode::Single);
        let placeholder = adw::StatusPage::builder()
            .icon_name("computer-symbolic")
            .title("Aucun poste")
            .description("Ajoutez des postes avec le bouton +")
            .build();
        placeholder.add_css_class("compact");
        list.set_placeholder(Some(&placeholder));
        let list_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();

        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 6);
        sidebar.set_margin_top(6);
        sidebar.set_margin_start(6);
        sidebar.set_margin_end(6);
        sidebar.set_margin_bottom(6);
        sidebar.append(&search);
        sidebar.append(&sel_box);
        sidebar.append(&target_label);
        sidebar.append(&list_scroll);
        sidebar.set_size_request(280, -1);

        // ---------- Détail du poste ----------
        let group = adw::PreferencesGroup::builder().title("Poste").build();
        let details = Details {
            connection: info_row("Connexion"),
            state: info_row("État"),
            booted: info_row("Déploiement actif"),
            source: info_row("Source"),
            pending: info_row("Au prochain démarrage"),
            layered: info_row("Paquets superposés"),
            groups: info_row("Groupes"),
            group: group.clone(),
            stack: gtk::Stack::new(),
        };
        for r in [
            &details.connection,
            &details.state,
            &details.booted,
            &details.source,
            &details.pending,
            &details.layered,
            &details.groups,
        ] {
            group.add(r);
        }
        // Pas d'AdwStatusPage ici : elle défile elle-même et serait tronquée.
        let none_page = gtk::Box::new(gtk::Orientation::Vertical, 6);
        none_page.set_margin_top(24);
        none_page.set_margin_bottom(12);
        let none_icon = gtk::Image::from_icon_name("computer-symbolic");
        none_icon.set_pixel_size(64);
        none_icon.add_css_class("dim-label");
        let none_title = gtk::Label::new(Some("Aucun poste sélectionné"));
        none_title.add_css_class("title-2");
        let none_desc = gtk::Label::builder()
            .label("Sélectionnez un poste pour voir son état, ou cochez-en plusieurs pour agir sur un groupe.")
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["dim-label"])
            .build();
        none_page.append(&none_icon);
        none_page.append(&none_title);
        none_page.append(&none_desc);
        details.stack.add_named(&none_page, Some("none"));
        details.stack.add_named(&group, Some("host"));
        details.stack.set_vhomogeneous(false);

        // ---------- Actions ----------
        let sys = adw::PreferencesGroup::builder()
            .title("Système (rpm-ostree)")
            .description(
                "Les actions s'appliquent aux postes cochés, ou à défaut au poste sélectionné.",
            )
            .build();
        let row_update = adw::ActionRow::builder().title("Mises à jour").build();
        let b_check = button("Vérifier", None);
        let b_upgrade = button("Mettre à jour", Some("suggested-action"));
        row_update.add_suffix(&b_check);
        row_update.add_suffix(&b_upgrade);
        let row_deploy = adw::ActionRow::builder()
            .title("Déploiements")
            .subtitle("Revenir au précédent, annuler celui en attente, nettoyer")
            .build();
        let b_rollback = button("Retour arrière", None);
        let b_cancel = button("Annuler l'attente", None);
        let b_cleanup = button("Nettoyer", None);
        row_deploy.add_suffix(&b_rollback);
        row_deploy.add_suffix(&b_cancel);
        row_deploy.add_suffix(&b_cleanup);
        let row_power = adw::ActionRow::builder()
            .title("Redémarrage")
            .subtitle("Nécessaire pour appliquer un nouveau déploiement")
            .build();
        let b_reboot = button("Redémarrer", Some("destructive-action"));
        row_power.add_suffix(&b_reboot);
        let pkg_entry = adw::EntryRow::builder()
            .title("Paquets RPM à superposer")
            .build();
        let b_pkg_add = button("Installer", None);
        let b_pkg_del = button("Retirer", None);
        pkg_entry.add_suffix(&b_pkg_add);
        pkg_entry.add_suffix(&b_pkg_del);
        let rebase_entry = adw::EntryRow::builder()
            .title("Rebase (ex. fedora:fedora/42/x86_64/silverblue)")
            .build();
        let b_rebase = button("Rebase", None);
        rebase_entry.add_suffix(&b_rebase);
        let labels: Vec<&str> = AutoUpdatePolicy::ALL.iter().map(|p| p.label()).collect();
        let auto_row = adw::ComboRow::builder()
            .title("Mises à jour automatiques")
            .model(&gtk::StringList::new(&labels))
            .selected(2)
            .build();
        let b_auto = button("Appliquer", None);
        auto_row.add_suffix(&b_auto);
        for r in [
            row_update.upcast_ref::<gtk::Widget>(),
            row_deploy.upcast_ref(),
            row_power.upcast_ref(),
            pkg_entry.upcast_ref(),
            rebase_entry.upcast_ref(),
            auto_row.upcast_ref(),
        ] {
            sys.add(r);
        }

        let fp = adw::PreferencesGroup::builder()
            .title("Flatpak (installation système)")
            .build();
        let row_fp = adw::ActionRow::builder().title("Applications").build();
        let b_fp_list = button("Lister", None);
        let b_fp_update = button("Tout mettre à jour", None);
        row_fp.add_suffix(&b_fp_list);
        row_fp.add_suffix(&b_fp_update);
        let fp_entry = adw::EntryRow::builder()
            .title("Identifiants (ex. org.mozilla.firefox)")
            .build();
        let b_fp_add = button("Installer", None);
        let b_fp_del = button("Retirer", None);
        fp_entry.add_suffix(&b_fp_add);
        fp_entry.add_suffix(&b_fp_del);
        fp.add(&row_fp);
        fp.add(&fp_entry);

        let custom = adw::PreferencesGroup::builder()
            .title("Commande personnalisée")
            .description("Exécutée avec les droits de l'utilisateur SSH ; préfixez par « sudo -n » si besoin.")
            .build();
        let cmd_entry = adw::EntryRow::builder().title("Commande").build();
        let b_cmd = button("Exécuter", None);
        cmd_entry.add_suffix(&b_cmd);
        custom.add(&cmd_entry);

        let main_box = gtk::Box::new(gtk::Orientation::Vertical, 24);
        main_box.set_margin_top(18);
        main_box.set_margin_bottom(18);
        main_box.set_margin_start(12);
        main_box.set_margin_end(12);
        main_box.append(&details.stack);
        main_box.append(&sys);
        main_box.append(&fp);
        main_box.append(&custom);
        let clamp = adw::Clamp::builder()
            .maximum_size(900)
            .child(&main_box)
            .build();
        let main_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&clamp)
            .build();

        // ---------- Journal ----------
        let log = gtk::TextBuffer::new(None);
        let tags = log.tag_table();
        tags.add(
            &gtk::TextTag::builder()
                .name("dim")
                .foreground("#888a85")
                .build(),
        );
        tags.add(&gtk::TextTag::builder().name("host").weight(700).build());
        tags.add(
            &gtk::TextTag::builder()
                .name("err")
                .foreground("#e01b24")
                .build(),
        );
        tags.add(
            &gtk::TextTag::builder()
                .name("ok")
                .foreground("#26a269")
                .build(),
        );
        log.create_mark(Some("end"), &log.end_iter(), false);
        let log_view = gtk::TextView::builder()
            .buffer(&log)
            .editable(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(6)
            .bottom_margin(6)
            .left_margin(8)
            .right_margin(8)
            .build();
        let log_scroll = gtk::ScrolledWindow::builder()
            .child(&log_view)
            .vexpand(true)
            .min_content_height(120)
            .build();

        let right = gtk::Paned::builder()
            .orientation(gtk::Orientation::Vertical)
            .start_child(&main_scroll)
            .end_child(&log_scroll)
            .resize_start_child(true)
            .shrink_end_child(false)
            .position(520)
            .build();
        let split = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .start_child(&sidebar)
            .end_child(&right)
            .resize_start_child(false)
            .shrink_start_child(false)
            .position(300)
            .build();

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&split));
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&toasts));

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Parc Silverblue")
            .default_width(1200)
            .default_height(820)
            .content(&toolbar)
            .build();

        let this = Rc::new(Window {
            window,
            toasts,
            list,
            target_label,
            details,
            log,
            log_view,
            state: RefCell::new(State {
                inventory,
                path,
                read_only: load_error.is_some(),
                infos: HashMap::new(),
                checked: HashSet::new(),
                current: None,
                visible: Vec::new(),
                filter: String::new(),
            }),
            tx,
        });

        // ---------- Signaux ----------
        {
            let w = this.clone();
            this.list.connect_row_selected(move |_, row| {
                let Some(row) = row else { return };
                let name = w.state.borrow().visible.get(row.index() as usize).cloned();
                if name.is_some() {
                    w.state.borrow_mut().current = name;
                    w.refresh_details();
                    w.update_target_label();
                }
            });
        }
        {
            let w = this.clone();
            search.connect_search_changed(move |s| {
                w.state.borrow_mut().filter = s.text().to_string();
                w.rebuild_list();
            });
        }
        {
            let w = this.clone();
            select_all.connect_clicked(move |_| {
                {
                    let mut st = w.state.borrow_mut();
                    let vis = st.visible.clone();
                    st.checked.extend(vis);
                }
                w.rebuild_list();
            });
        }
        {
            let w = this.clone();
            select_none.connect_clicked(move |_| {
                w.state.borrow_mut().checked.clear();
                w.rebuild_list();
            });
        }

        // Boutons d'action : chaque bouton construit son action au clic.
        let bind = |b: &gtk::Button, make: Box<dyn Fn() -> Option<Action>>| {
            let w = this.clone();
            b.connect_clicked(move |_| {
                if let Some(a) = make() {
                    w.request(a);
                }
            });
        };
        bind(&b_check, Box::new(|| Some(Action::CheckUpdate)));
        bind(&b_upgrade, Box::new(|| Some(Action::Upgrade)));
        bind(&b_rollback, Box::new(|| Some(Action::Rollback)));
        bind(&b_cancel, Box::new(|| Some(Action::CancelPending)));
        bind(&b_cleanup, Box::new(|| Some(Action::Cleanup)));
        bind(&b_reboot, Box::new(|| Some(Action::Reboot)));
        {
            let e = pkg_entry.clone();
            bind(
                &b_pkg_add,
                Box::new(move || Some(Action::Install(split_list(&e.text())))),
            );
            let e = pkg_entry.clone();
            bind(
                &b_pkg_del,
                Box::new(move || Some(Action::Uninstall(split_list(&e.text())))),
            );
        }
        {
            let e = rebase_entry.clone();
            bind(
                &b_rebase,
                Box::new(move || Some(Action::Rebase(e.text().trim().to_string()))),
            );
        }
        {
            let r = auto_row.clone();
            bind(
                &b_auto,
                Box::new(move || {
                    AutoUpdatePolicy::ALL
                        .get(r.selected() as usize)
                        .map(|p| Action::SetAutoUpdate(*p))
                }),
            );
        }
        bind(&b_fp_list, Box::new(|| Some(Action::FlatpakList)));
        bind(&b_fp_update, Box::new(|| Some(Action::FlatpakUpdate)));
        {
            let e = fp_entry.clone();
            let w = this.clone();
            bind(
                &b_fp_add,
                Box::new(move || {
                    Some(Action::FlatpakInstall {
                        remote: w.state.borrow().inventory.settings.flatpak_remote.clone(),
                        apps: split_list(&e.text()),
                    })
                }),
            );
            let e = fp_entry.clone();
            bind(
                &b_fp_del,
                Box::new(move || Some(Action::FlatpakUninstall(split_list(&e.text())))),
            );
        }
        {
            let e = cmd_entry.clone();
            bind(
                &b_cmd,
                Box::new(move || Some(Action::Custom(e.text().trim().to_string()))),
            );
        }

        this.install_actions(app);

        // Réception des résultats des tâches SSH.
        {
            let w = this.clone();
            glib::spawn_future_local(async move {
                while let Ok(ev) = rx.recv().await {
                    w.handle_event(ev);
                }
            });
        }

        {
            let mut st = this.state.borrow_mut();
            st.current = st.inventory.hosts.first().map(|h| h.name.clone());
        }
        this.rebuild_list();
        this.refresh_details();
        this.window.present();

        let path_display = this.state.borrow().path.display().to_string();
        match load_error {
            Some(e) => {
                this.log_line(None, &format!("{path_display} : {e}"), Some("err"));
                this.toast("Inventaire illisible : modifications désactivées");
            }
            None => {
                let n = this.state.borrow().inventory.hosts.len();
                this.log_line(
                    None,
                    &format!("Inventaire {path_display} : {n} poste(s)"),
                    Some("dim"),
                );
                this.refresh_all();
            }
        }
        this
    }

    fn install_actions(self: &Rc<Self>, app: &adw::Application) {
        let add = |name: &str, f: WinCallback| {
            let action = gio::SimpleAction::new(name, None);
            let w = self.clone();
            action.connect_activate(move |_, _| f(&w));
            self.window.add_action(&action);
        };
        add("add-host", Box::new(|w| w.edit_host(None)));
        add("refresh", Box::new(|w| w.refresh_all()));
        add("clear-log", Box::new(|w| w.log.set_text("")));
        add(
            "reload",
            Box::new(|w| {
                let path = w.state.borrow().path.clone();
                match Inventory::load(&path) {
                    Ok(inv) => {
                        {
                            let mut st = w.state.borrow_mut();
                            st.inventory = inv;
                            st.read_only = false;
                        }
                        w.rebuild_list();
                        w.refresh_details();
                        w.toast("Inventaire rechargé");
                    }
                    Err(e) => w.log_line(None, &e.to_string(), Some("err")),
                }
            }),
        );
        add(
            "open-inventory",
            Box::new(|w| {
                let path = w.state.borrow().path.clone();
                if !path.exists() {
                    w.toast("Le fichier d'inventaire n'existe pas encore : ajoutez un poste");
                    return;
                }
                gtk::FileLauncher::new(Some(&gio::File::for_path(&path))).launch(
                    Some(&w.window),
                    gio::Cancellable::NONE,
                    |_| {},
                );
            }),
        );
        add(
            "about",
            Box::new(|w| {
                let about = adw::AboutDialog::builder()
                    .application_name("Parc Silverblue")
                    .application_icon("computer-symbolic")
                    .version(env!("CARGO_PKG_VERSION"))
                    .comments("Administration d'un parc de postes Fedora Silverblue via SSH et rpm-ostree.")
                    .license_type(gtk::License::Gpl30)
                    .build();
                about.present(Some(&w.window));
            }),
        );
        app.set_accels_for_action("win.add-host", &["<Control>n"]);
        app.set_accels_for_action("win.refresh", &["<Control>r"]);
    }

    // ---------------------------------------------------------------------
    // Liste et détail
    // ---------------------------------------------------------------------

    fn rebuild_list(self: &Rc<Self>) {
        let (rows, current) = {
            let st = self.state.borrow();
            let rows: Vec<(Host, HostInfo, bool)> = st
                .inventory
                .hosts
                .iter()
                .filter(|h| h.matches(&st.filter))
                .map(|h| {
                    (
                        h.clone(),
                        st.infos.get(&h.name).cloned().unwrap_or_default(),
                        st.checked.contains(&h.name),
                    )
                })
                .collect();
            (rows, st.current.clone())
        };
        self.state.borrow_mut().visible = rows.iter().map(|(h, _, _)| h.name.clone()).collect();

        self.list.remove_all();
        let mut to_select = None;
        for (host, info, checked) in rows {
            let row = self.make_row(&host, &info, checked);
            self.list.append(&row);
            if current.as_deref() == Some(host.name.as_str()) {
                to_select = Some(row);
            }
        }
        if let Some(row) = to_select {
            self.list.select_row(Some(&row));
        }
        self.update_target_label();
    }

    fn make_row(self: &Rc<Self>, host: &Host, info: &HostInfo, checked: bool) -> adw::ActionRow {
        let version = info
            .status
            .as_ref()
            .and_then(|s| s.booted())
            .and_then(|d| d.version.clone());
        let mut subtitle = host.address.clone();
        if let Some(v) = version {
            subtitle.push_str(" · ");
            subtitle.push_str(&v);
        }
        let row = adw::ActionRow::builder()
            .title(&host.name)
            .subtitle(&subtitle)
            .use_markup(false)
            .activatable(true)
            .build();

        let check = gtk::CheckButton::builder()
            .active(checked)
            .valign(gtk::Align::Center)
            .tooltip_text("Inclure dans la cible des actions")
            .build();
        {
            let w = self.clone();
            let name = host.name.clone();
            check.connect_toggled(move |c| {
                {
                    let mut st = w.state.borrow_mut();
                    if c.is_active() {
                        st.checked.insert(name.clone());
                    } else {
                        st.checked.remove(&name);
                    }
                }
                w.update_target_label();
            });
        }
        row.add_prefix(&check);

        let indicator: gtk::Widget = if info.busy > 0 {
            let s = gtk::Spinner::new();
            s.start();
            s.set_tooltip_text(Some("Opération en cours"));
            s.upcast()
        } else {
            let (icon, tip, css) = if let Some(e) = &info.error {
                ("dialog-error-symbolic", e.clone(), Some("error"))
            } else if let Some(st) = &info.status {
                if st.reboot_required() {
                    (
                        "system-reboot-symbolic",
                        "Nouveau déploiement en attente : redémarrage nécessaire".to_string(),
                        Some("warning"),
                    )
                } else {
                    (
                        "emblem-ok-symbolic",
                        "À jour du point de vue du déploiement".into(),
                        Some("success"),
                    )
                }
            } else {
                ("network-offline-symbolic", "État inconnu".into(), None)
            };
            let img = gtk::Image::from_icon_name(icon);
            img.set_tooltip_text(Some(&tip));
            if let Some(c) = css {
                img.add_css_class(c);
            }
            img.upcast()
        };
        row.add_suffix(&indicator);

        let edit = gtk::Button::from_icon_name("document-edit-symbolic");
        edit.add_css_class("flat");
        edit.set_valign(gtk::Align::Center);
        edit.set_tooltip_text(Some("Modifier le poste"));
        {
            let w = self.clone();
            let name = host.name.clone();
            edit.connect_clicked(move |_| w.edit_host(Some(name.clone())));
        }
        row.add_suffix(&edit);
        row
    }

    fn refresh_details(&self) {
        let st = self.state.borrow();
        let Some(host) = st.current.as_ref().and_then(|n| st.inventory.get(n)) else {
            self.details.stack.set_visible_child_name("none");
            return;
        };
        let info = st.infos.get(&host.name).cloned().unwrap_or_default();
        let d = &self.details;
        d.stack.set_visible_child_name("host");
        d.group.set_title(&host.name);
        d.group.set_description(if host.notes.is_empty() {
            None
        } else {
            Some(&host.notes)
        });
        d.connection
            .set_subtitle(&format!("ssh -p {} {}", host.port, host.destination()));
        d.groups.set_subtitle(&if host.groups.is_empty() {
            "aucun".to_string()
        } else {
            host.groups.join(", ")
        });
        d.state.set_subtitle(&if info.busy > 0 {
            format!("{} opération(s) en cours…", info.busy)
        } else if let Some(e) = &info.error {
            format!("Erreur : {e}")
        } else if info.status.is_some() {
            "Joignable".to_string()
        } else {
            "Inconnu (actualisez)".to_string()
        });

        let booted = info.status.as_ref().and_then(|s| s.booted());
        let pending = info.status.as_ref().and_then(|s| s.pending());
        match booted {
            Some(b) => {
                d.booted.set_subtitle(&format!(
                    "{} — {}",
                    b.display_version(),
                    format_timestamp(b.timestamp)
                ));
                d.source.set_subtitle(b.source());
                d.layered.set_subtitle(&b.layering_summary());
            }
            None => {
                for r in [&d.booted, &d.source, &d.layered] {
                    r.set_subtitle("—");
                }
            }
        }
        d.pending.set_subtitle(&match pending {
            Some(p) => format!(
                "{}{} — {}",
                if p.staged { "[préparé] " } else { "" },
                p.display_version(),
                p.source()
            ),
            None if booted.is_some() => "inchangé".to_string(),
            None => "—".to_string(),
        });
    }

    fn update_target_label(&self) {
        let st = self.state.borrow();
        let text = if !st.checked.is_empty() {
            format!("Cible : {} poste(s) coché(s)", st.checked.len())
        } else if let Some(c) = &st.current {
            format!("Cible : « {c} »")
        } else {
            "Aucune cible : cochez ou sélectionnez des postes".to_string()
        };
        self.target_label.set_label(&text);
    }

    /// Postes cochés, ou à défaut le poste sélectionné.
    fn targets(&self) -> Vec<Host> {
        let st = self.state.borrow();
        if !st.checked.is_empty() {
            st.inventory
                .hosts
                .iter()
                .filter(|h| st.checked.contains(&h.name))
                .cloned()
                .collect()
        } else {
            st.current
                .as_ref()
                .and_then(|n| st.inventory.get(n))
                .cloned()
                .into_iter()
                .collect()
        }
    }

    // ---------------------------------------------------------------------
    // Exécution
    // ---------------------------------------------------------------------

    fn request(self: &Rc<Self>, action: Action) {
        let targets = self.targets();
        if targets.is_empty() {
            self.toast("Aucun poste ciblé");
            return;
        }
        // Validation des paramètres avant toute confirmation.
        if let Err(e) = action.remote_command("root") {
            self.toast(&e);
            return;
        }
        if !action.needs_confirmation() {
            self.run(targets, action);
            return;
        }

        let mut names: Vec<&str> = targets.iter().map(|h| h.name.as_str()).take(8).collect();
        if targets.len() > 8 {
            names.push("…");
        }
        let dialog = adw::AlertDialog::new(
            Some(&format!("{} ?", action.label())),
            Some(&format!(
                "Cette opération sera lancée sur {} poste(s) : {}",
                targets.len(),
                names.join(", ")
            )),
        );
        dialog.add_response("cancel", "Annuler");
        dialog.add_response("run", "Lancer");
        let appearance = if matches!(action, Action::Upgrade) {
            adw::ResponseAppearance::Suggested
        } else {
            adw::ResponseAppearance::Destructive
        };
        dialog.set_response_appearance("run", appearance);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let w = self.clone();
        dialog.connect_response(Some("run"), move |_, _| {
            w.run(targets.clone(), action.clone())
        });
        dialog.present(Some(&self.window));
    }

    fn run(self: &Rc<Self>, targets: Vec<Host>, action: Action) {
        let settings = {
            let mut st = self.state.borrow_mut();
            for h in &targets {
                st.infos.entry(h.name.clone()).or_default().busy += 1;
            }
            st.inventory.settings.clone()
        };
        if !matches!(action, Action::Status) {
            self.log_line(
                None,
                &format!("▶ {} sur {} poste(s)", action.label(), targets.len()),
                Some("host"),
            );
        }
        let jobs = targets
            .into_iter()
            .map(|host| Job {
                host,
                action: action.clone(),
            })
            .collect();
        runner::spawn(jobs, settings, self.tx.clone());
        self.rebuild_list();
        self.refresh_details();
    }

    fn refresh_all(self: &Rc<Self>) {
        let hosts: Vec<Host> = {
            let st = self.state.borrow();
            st.inventory
                .hosts
                .iter()
                .filter(|h| st.visible.is_empty() || st.visible.contains(&h.name))
                .cloned()
                .collect()
        };
        if !hosts.is_empty() {
            self.run(hosts, Action::Status);
        }
    }

    fn handle_event(self: &Rc<Self>, ev: JobEvent) {
        match ev {
            JobEvent::Started {
                host,
                action,
                command,
            } => {
                if !matches!(action, Action::Status) {
                    self.log_line(Some(&host), &format!("$ {command}"), Some("dim"));
                }
            }
            JobEvent::Finished {
                host,
                action,
                result,
            } => {
                let mut follow_up = false;
                {
                    let mut st = self.state.borrow_mut();
                    let info = st.infos.entry(host.clone()).or_default();
                    info.busy = info.busy.saturating_sub(1);
                }
                match result {
                    Err(e) => {
                        self.log_line(Some(&host), &e, Some("err"));
                        self.set_error(&host, Some(e));
                    }
                    Ok(out) => {
                        let ok = action.is_success(out.status);
                        if let Action::Status = action {
                            let parsed = if ok {
                                HostStatus::parse(&out.stdout)
                            } else {
                                Err(describe_failure(&out.stderr, out.status))
                            };
                            match parsed {
                                Ok(status) => {
                                    let mut st = self.state.borrow_mut();
                                    let info = st.infos.entry(host.clone()).or_default();
                                    info.status = Some(status);
                                    info.error = None;
                                }
                                Err(e) => {
                                    self.log_line(Some(&host), &format!("état : {e}"), Some("err"));
                                    self.set_error(&host, Some(e));
                                }
                            }
                        } else {
                            let stdout = out.stdout.trim_end();
                            if !stdout.is_empty() {
                                self.log_line(Some(&host), stdout, None);
                            }
                            let stderr = out.stderr.trim_end();
                            if !stderr.is_empty() && !ok {
                                self.log_line(Some(&host), stderr, Some("err"));
                            }
                            if ok {
                                let msg = match (&action, out.status) {
                                    (Action::CheckUpdate, Some(77)) => {
                                        "aucune mise à jour disponible".into()
                                    }
                                    _ => format!("✔ {} : terminé", action.label()),
                                };
                                self.log_line(Some(&host), &msg, Some("ok"));
                                self.set_error(&host, None);
                                if matches!(action, Action::Reboot) {
                                    let mut st = self.state.borrow_mut();
                                    st.infos.entry(host.clone()).or_default().status = None;
                                }
                                follow_up = action.changes_deployments();
                            } else {
                                let e = describe_failure(stderr, out.status);
                                self.log_line(
                                    Some(&host),
                                    &format!("✘ {} : {e}", action.label()),
                                    Some("err"),
                                );
                                self.set_error(&host, Some(e));
                            }
                        }
                    }
                }
                if follow_up {
                    let h = self.state.borrow().inventory.get(&host).cloned();
                    if let Some(h) = h {
                        self.run(vec![h], Action::Status);
                        return;
                    }
                }
                self.rebuild_list();
                self.refresh_details();
            }
        }
    }

    fn set_error(&self, host: &str, error: Option<String>) {
        let mut st = self.state.borrow_mut();
        st.infos.entry(host.to_string()).or_default().error = error;
    }

    // ---------------------------------------------------------------------
    // Inventaire
    // ---------------------------------------------------------------------

    fn edit_host(self: &Rc<Self>, name: Option<String>) {
        if self.state.borrow().read_only {
            self.toast("Inventaire illisible : corrigez le fichier puis rechargez-le");
            return;
        }
        let (existing, others) = {
            let st = self.state.borrow();
            let existing = name.as_ref().and_then(|n| st.inventory.get(n)).cloned();
            let others: HashSet<String> = st
                .inventory
                .hosts
                .iter()
                .map(|h| h.name.clone())
                .filter(|n| Some(n) != name.as_ref())
                .collect();
            (existing, others)
        };
        let check = move |h: &Host| {
            if others.contains(&h.name) {
                Err(format!("un poste nommé « {} » existe déjà", h.name))
            } else {
                Ok(())
            }
        };
        let w = self.clone();
        host_dialog::show(&self.window, existing.as_ref(), check, move |res| {
            w.apply_host_change(name.as_deref(), res)
        });
    }

    fn apply_host_change(self: &Rc<Self>, previous: Option<&str>, res: HostDialogResult) {
        let outcome = {
            let mut st = self.state.borrow_mut();
            let backup = st.inventory.clone();
            let r = match res {
                HostDialogResult::Save(host) => {
                    let new_name = host.name.clone();
                    st.inventory.upsert(previous, host).map(|_| {
                        if let Some(old) = previous.filter(|o| *o != new_name) {
                            if let Some(info) = st.infos.remove(old) {
                                st.infos.insert(new_name.clone(), info);
                            }
                            if st.checked.remove(old) {
                                st.checked.insert(new_name.clone());
                            }
                        }
                        st.current = Some(new_name);
                    })
                }
                HostDialogResult::Delete => {
                    if let Some(old) = previous {
                        st.inventory.remove(old);
                        st.infos.remove(old);
                        st.checked.remove(old);
                        if st.current.as_deref() == Some(old) {
                            st.current = None;
                        }
                    }
                    Ok(())
                }
            };
            let r = r.and_then(|_| st.inventory.save(&st.path));
            if r.is_err() {
                st.inventory = backup;
            }
            r
        };
        match outcome {
            Ok(()) => {
                self.rebuild_list();
                self.refresh_details();
            }
            Err(e) => {
                self.log_line(None, &e.to_string(), Some("err"));
                self.toast("Impossible d'enregistrer l'inventaire");
            }
        }
    }

    // ---------------------------------------------------------------------
    // Journal et notifications
    // ---------------------------------------------------------------------

    fn toast(&self, msg: &str) {
        self.toasts.add_toast(adw::Toast::new(msg));
    }

    fn log_line(&self, host: Option<&str>, text: &str, tag: Option<&str>) {
        let buf = &self.log;
        let ts = glib::DateTime::now_local()
            .ok()
            .and_then(|d| d.format("%H:%M:%S").ok())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let mut end = buf.end_iter();
        buf.insert_with_tags_by_name(&mut end, &format!("[{ts}] "), &["dim"]);
        if let Some(h) = host {
            buf.insert_with_tags_by_name(&mut end, &format!("{h} │ "), &["host"]);
        }
        // Les sorties multilignes sont indentées sous l'en-tête.
        let body = if text.contains('\n') {
            format!("\n    {}", text.replace('\n', "\n    "))
        } else {
            text.to_string()
        };
        match tag {
            Some(t) => buf.insert_with_tags_by_name(&mut end, &body, &[t]),
            None => buf.insert(&mut end, &body),
        }
        buf.insert(&mut end, "\n");

        let excess = buf.line_count() - LOG_MAX_LINES;
        if excess > 0 {
            let mut start = buf.start_iter();
            if let Some(mut cut) = buf.iter_at_line(excess) {
                buf.delete(&mut start, &mut cut);
            }
        }
        if let Some(mark) = buf.mark("end") {
            self.log_view.scroll_to_mark(&mark, 0.0, false, 0.0, 1.0);
        }
    }
}

/// Message lisible à partir de la sortie d'erreur et du code de retour.
fn describe_failure(stderr: &str, code: Option<i32>) -> String {
    let last = stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if last.contains("a password is required") || last.contains("a terminal is required") {
        return "sudo exige un mot de passe : configurez sudoers (voir README)".into();
    }
    match code {
        Some(255) if last.is_empty() => "connexion SSH impossible".into(),
        Some(255) => format!("SSH : {last}"),
        Some(c) if last.is_empty() => format!("échec (code {c})"),
        Some(c) => format!("{last} (code {c})"),
        None => "processus interrompu".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::describe_failure;

    #[test]
    fn failures() {
        assert_eq!(describe_failure("", Some(255)), "connexion SSH impossible");
        assert!(describe_failure("sudo: a password is required\n", Some(1)).contains("sudoers"));
        assert_eq!(
            describe_failure("x\nerror: boom\n\n", Some(1)),
            "error: boom (code 1)"
        );
    }
}

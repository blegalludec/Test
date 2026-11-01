//! Opérations d'administration et traduction en commandes distantes.

use crate::ssh::shell_quote;

/// Politique de mises à jour automatiques de rpm-ostreed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoUpdatePolicy {
    /// Désactivées.
    None,
    /// Vérification seulement.
    Check,
    /// Téléchargement et préparation du déploiement (appliqué au redémarrage).
    Stage,
}

impl AutoUpdatePolicy {
    pub const ALL: [AutoUpdatePolicy; 3] = [
        AutoUpdatePolicy::None,
        AutoUpdatePolicy::Check,
        AutoUpdatePolicy::Stage,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AutoUpdatePolicy::None => "none",
            AutoUpdatePolicy::Check => "check",
            AutoUpdatePolicy::Stage => "stage",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AutoUpdatePolicy::None => "Désactivées",
            AutoUpdatePolicy::Check => "Vérifier uniquement",
            AutoUpdatePolicy::Stage => "Télécharger et préparer",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// État des déploiements rpm-ostree.
    Status,
    /// Vérifie la disponibilité d'une mise à jour.
    CheckUpdate,
    /// Télécharge et prépare la mise à jour (active au redémarrage).
    Upgrade,
    /// Revient au déploiement précédent au prochain démarrage.
    Rollback,
    /// Supprime le déploiement en attente.
    CancelPending,
    /// Nettoie les métadonnées et caches rpm-ostree.
    Cleanup,
    Reboot,
    /// Superpose des paquets RPM.
    Install(Vec<String>),
    /// Retire des paquets superposés.
    Uninstall(Vec<String>),
    /// Change de branche / d'image (ex. passage à une nouvelle version de Fedora).
    Rebase(String),
    FlatpakList,
    FlatpakUpdate,
    FlatpakInstall {
        remote: String,
        apps: Vec<String>,
    },
    FlatpakUninstall(Vec<String>),
    SetAutoUpdate(AutoUpdatePolicy),
    /// Commande libre (exécutée avec les droits de l'utilisateur SSH).
    Custom(String),
}

fn valid_token(s: &str, extra: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

/// Découpe une saisie « a b, c » en liste de noms.
pub fn split_list(input: &str) -> Vec<String> {
    input
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

impl Action {
    pub fn label(&self) -> String {
        match self {
            Action::Status => "État".into(),
            Action::CheckUpdate => "Vérification des mises à jour".into(),
            Action::Upgrade => "Mise à jour".into(),
            Action::Rollback => "Retour arrière".into(),
            Action::CancelPending => "Annulation du déploiement en attente".into(),
            Action::Cleanup => "Nettoyage".into(),
            Action::Reboot => "Redémarrage".into(),
            Action::Install(p) => format!("Installation de {}", p.join(", ")),
            Action::Uninstall(p) => format!("Désinstallation de {}", p.join(", ")),
            Action::Rebase(r) => format!("Rebase vers {r}"),
            Action::FlatpakList => "Liste des Flatpak".into(),
            Action::FlatpakUpdate => "Mise à jour des Flatpak".into(),
            Action::FlatpakInstall { apps, .. } => {
                format!("Installation Flatpak de {}", apps.join(", "))
            }
            Action::FlatpakUninstall(a) => format!("Désinstallation Flatpak de {}", a.join(", ")),
            Action::SetAutoUpdate(p) => format!("Mises à jour automatiques : {}", p.label()),
            Action::Custom(c) => format!("Commande « {c} »"),
        }
    }

    /// Demande une confirmation avant exécution.
    pub fn needs_confirmation(&self) -> bool {
        matches!(
            self,
            Action::Upgrade
                | Action::Rollback
                | Action::CancelPending
                | Action::Reboot
                | Action::Uninstall(_)
                | Action::Rebase(_)
                | Action::FlatpakUninstall(_)
                | Action::Custom(_)
        )
    }

    /// L'état rpm-ostree doit être rafraîchi après l'action.
    pub fn changes_deployments(&self) -> bool {
        matches!(
            self,
            Action::Upgrade
                | Action::Rollback
                | Action::CancelPending
                | Action::Install(_)
                | Action::Uninstall(_)
                | Action::Rebase(_)
        )
    }

    /// Codes de sortie à considérer comme un succès.
    pub fn is_success(&self, code: Option<i32>) -> bool {
        match (self, code) {
            (_, Some(0)) => true,
            // `rpm-ostree upgrade --check` renvoie 77 s'il n'y a rien de nouveau.
            (Action::CheckUpdate, Some(77)) => true,
            // La connexion est coupée par l'arrêt de la machine.
            (Action::Reboot, Some(255)) | (Action::Reboot, None) => true,
            _ => false,
        }
    }

    /// Construit la commande shell exécutée sur le poste.
    ///
    /// Les commandes privilégiées sont préfixées par `sudo -n` sauf si l'on se
    /// connecte en root ; chaque binaire est appelé séparément afin de
    /// pouvoir restreindre précisément le fichier sudoers.
    pub fn remote_command(&self, user: &str) -> Result<String, String> {
        let sudo = if user == "root" { "" } else { "sudo -n " };
        let pkgs = |list: &[String]| -> Result<String, String> {
            if list.is_empty() {
                return Err("aucun paquet indiqué".into());
            }
            for p in list {
                if !valid_token(p, "._+-:~") {
                    return Err(format!("nom de paquet invalide : « {p} »"));
                }
            }
            Ok(list.join(" "))
        };
        let apps = |list: &[String]| -> Result<String, String> {
            if list.is_empty() {
                return Err("aucune application indiquée".into());
            }
            for a in list {
                if !valid_token(a, "._-/") {
                    return Err(format!("identifiant Flatpak invalide : « {a} »"));
                }
            }
            Ok(list.join(" "))
        };

        Ok(match self {
            Action::Status => "rpm-ostree status --json".into(),
            Action::CheckUpdate => format!("{sudo}rpm-ostree upgrade --check"),
            Action::Upgrade => format!("{sudo}rpm-ostree upgrade"),
            Action::Rollback => format!("{sudo}rpm-ostree rollback"),
            Action::CancelPending => format!("{sudo}rpm-ostree cleanup --pending"),
            Action::Cleanup => format!("{sudo}rpm-ostree cleanup --base --repomd"),
            Action::Reboot => format!("{sudo}systemctl reboot"),
            Action::Install(p) => format!("{sudo}rpm-ostree install --idempotent {}", pkgs(p)?),
            Action::Uninstall(p) => format!("{sudo}rpm-ostree uninstall {}", pkgs(p)?),
            Action::Rebase(r) => {
                if !valid_token(r, "._-/:+@") {
                    return Err(format!("référence de rebase invalide : « {r} »"));
                }
                format!("{sudo}rpm-ostree rebase {r}")
            }
            Action::FlatpakList => {
                "flatpak list --app --columns=application,version,branch,origin,installation".into()
            }
            Action::FlatpakUpdate => {
                format!("{sudo}flatpak update --system --noninteractive -y")
            }
            Action::FlatpakInstall { remote, apps: list } => {
                if !valid_token(remote, "._-") {
                    return Err(format!("dépôt Flatpak invalide : « {remote} »"));
                }
                format!(
                    "{sudo}flatpak install --system --noninteractive -y {remote} {}",
                    apps(list)?
                )
            }
            Action::FlatpakUninstall(list) => format!(
                "{sudo}flatpak uninstall --system --noninteractive -y {}",
                apps(list)?
            ),
            Action::SetAutoUpdate(policy) => {
                let p = policy.as_str();
                let conf = "/etc/rpm-ostreed.conf";
                let line = format!("AutomaticUpdatePolicy={p}");
                let timer = if *policy == AutoUpdatePolicy::None {
                    format!("{sudo}systemctl disable --now rpm-ostreed-automatic.timer")
                } else {
                    format!("{sudo}systemctl enable --now rpm-ostreed-automatic.timer")
                };
                // Le fichier est lu et transformé sans privilège ; seul `tee`
                // l'écrit en root. rpm-ostreed.conf ne contient que la section
                // [Daemon] : la ligne ajoutée à la fin lui appartient.
                format!(
                    "conf=$(sed -E {del} {conf} && echo {line}) && \
                     printf '%s\\n' \"$conf\" | {sudo}tee {conf} >/dev/null && \
                     {sudo}rpm-ostree reload && {timer}",
                    del = shell_quote("/^#?[[:space:]]*AutomaticUpdatePolicy[[:space:]]*=/d"),
                )
            }
            Action::Custom(c) => {
                if c.trim().is_empty() {
                    return Err("commande vide".into());
                }
                c.clone()
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sudo_prefix() {
        assert_eq!(
            Action::Upgrade.remote_command("root").unwrap(),
            "rpm-ostree upgrade"
        );
        assert_eq!(
            Action::Upgrade.remote_command("admin").unwrap(),
            "sudo -n rpm-ostree upgrade"
        );
        assert_eq!(
            Action::Status.remote_command("admin").unwrap(),
            "rpm-ostree status --json"
        );
    }

    #[test]
    fn packages_validated() {
        let ok = Action::Install(split_list("htop, vim-enhanced  gcc-c++"));
        assert_eq!(
            ok.remote_command("root").unwrap(),
            "rpm-ostree install --idempotent htop vim-enhanced gcc-c++"
        );
        assert!(Action::Install(vec![]).remote_command("root").is_err());
        assert!(Action::Install(vec!["a;reboot".into()])
            .remote_command("root")
            .is_err());
        assert!(Action::Install(vec!["--foo".into()])
            .remote_command("root")
            .is_err());
        assert!(Action::Rebase("x $(id)".into())
            .remote_command("root")
            .is_err());
        assert!(Action::Rebase(
            "ostree-image-signed:docker://quay.io/fedora/fedora-silverblue:41".into()
        )
        .remote_command("root")
        .is_ok());
    }

    #[test]
    fn flatpak() {
        let a = Action::FlatpakInstall {
            remote: "flathub".into(),
            apps: vec!["org.gnome.Calculator".into()],
        };
        assert_eq!(
            a.remote_command("u").unwrap(),
            "sudo -n flatpak install --system --noninteractive -y flathub org.gnome.Calculator"
        );
        assert!(Action::FlatpakUninstall(vec!["x y".into()])
            .remote_command("u")
            .is_err());
    }

    #[test]
    fn auto_update() {
        let cmd = Action::SetAutoUpdate(AutoUpdatePolicy::Stage)
            .remote_command("root")
            .unwrap();
        assert!(cmd.starts_with("conf=$(sed -E '/^#?[[:space:]]*AutomaticUpdatePolicy"));
        assert!(cmd.contains("echo AutomaticUpdatePolicy=stage)"));
        assert!(cmd.contains("| tee /etc/rpm-ostreed.conf >/dev/null"));
        assert!(!cmd.contains("sed -i"));
        assert!(cmd.contains("enable --now rpm-ostreed-automatic.timer"));
        let cmd = Action::SetAutoUpdate(AutoUpdatePolicy::None)
            .remote_command("a")
            .unwrap();
        assert!(cmd.contains("sudo -n systemctl disable --now"));
    }

    /// Exécute réellement le script généré sur une copie de rpm-ostreed.conf.
    #[test]
    fn auto_update_script_runs() {
        let dir = std::env::temp_dir().join(format!("sbf-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let conf = dir.join("rpm-ostreed.conf");
        std::fs::write(
            &conf,
            "# This file configures the rpm-ostree daemon.\n\n[Daemon]\n#AutomaticUpdatePolicy=none\n#IdleExitTimeout=60\n",
        )
        .unwrap();
        let conf_s = conf.to_str().unwrap();
        for policy in [AutoUpdatePolicy::Stage, AutoUpdatePolicy::Check] {
            let cmd = Action::SetAutoUpdate(policy)
                .remote_command("root")
                .unwrap()
                .replace("/etc/rpm-ostreed.conf", conf_s)
                .replace("rpm-ostree reload", "true")
                .replace("systemctl", "true");
            let st = std::process::Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .status()
                .unwrap();
            assert!(st.success());
        }
        let out = std::fs::read_to_string(&conf).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(out.matches("AutomaticUpdatePolicy").count(), 1);
        assert!(out.contains("[Daemon]\n#IdleExitTimeout=60\nAutomaticUpdatePolicy=check\n"));
    }

    #[test]
    fn exit_codes() {
        assert!(Action::CheckUpdate.is_success(Some(77)));
        assert!(!Action::Upgrade.is_success(Some(77)));
        assert!(Action::Reboot.is_success(Some(255)));
    }
}

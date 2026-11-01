//! Inventaire du parc : liste des postes et réglages, stockés en TOML.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

fn default_user() -> String {
    "root".into()
}

fn default_port() -> u16 {
    22
}

/// Un poste de travail Silverblue joignable en SSH.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    /// Nom unique affiché dans l'interface.
    pub name: String,
    /// Nom DNS ou adresse IP.
    pub address: String,
    #[serde(default = "default_user")]
    pub user: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Groupes (salle, service, site…) pour filtrer et cibler.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

impl Host {
    pub fn new(name: impl Into<String>, address: impl Into<String>) -> Self {
        Host {
            name: name.into(),
            address: address.into(),
            user: default_user(),
            port: default_port(),
            groups: Vec::new(),
            notes: String::new(),
        }
    }

    /// `utilisateur@adresse` tel que passé à ssh.
    pub fn destination(&self) -> String {
        format!("{}@{}", self.user, self.address)
    }

    /// Vrai si la recherche correspond au nom, à l'adresse ou à un groupe.
    pub fn matches(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return true;
        }
        self.name.to_lowercase().contains(&q)
            || self.address.to_lowercase().contains(&q)
            || self.groups.iter().any(|g| g.to_lowercase().contains(&q))
    }
}

fn default_parallel() -> usize {
    8
}

fn default_timeout() -> u32 {
    10
}

/// Réglages globaux de connexion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Nombre maximal de postes traités simultanément.
    #[serde(default = "default_parallel")]
    pub max_parallel: usize,
    /// Délai de connexion SSH en secondes.
    #[serde(default = "default_timeout")]
    pub connect_timeout: u32,
    /// Clé privée SSH à utiliser (sinon : agent / configuration ssh).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_file: Option<PathBuf>,
    /// Dépôt Flatpak utilisé pour les installations.
    #[serde(default = "default_remote")]
    pub flatpak_remote: String,
}

fn default_remote() -> String {
    "flathub".into()
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            max_parallel: default_parallel(),
            connect_timeout: default_timeout(),
            identity_file: None,
            flatpak_remote: default_remote(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inventory {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default, rename = "host")]
    pub hosts: Vec<Host>,
}

#[derive(Debug)]
pub enum InventoryError {
    Io(io::Error),
    Parse(toml::de::Error),
    Serialize(toml::ser::Error),
    Invalid(String),
}

impl std::fmt::Display for InventoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InventoryError::Io(e) => write!(f, "erreur d'accès au fichier : {e}"),
            InventoryError::Parse(e) => write!(f, "inventaire invalide : {e}"),
            InventoryError::Serialize(e) => write!(f, "sérialisation impossible : {e}"),
            InventoryError::Invalid(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for InventoryError {}

impl From<io::Error> for InventoryError {
    fn from(e: io::Error) -> Self {
        InventoryError::Io(e)
    }
}

impl Inventory {
    /// `~/.config/silverblue-fleet/inventory.toml`
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("silverblue-fleet")
            .join("inventory.toml")
    }

    /// Charge l'inventaire ; un fichier absent donne un inventaire vide.
    pub fn load(path: &Path) -> Result<Self, InventoryError> {
        match fs::read_to_string(path) {
            Ok(text) => Self::from_toml(&text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Inventory::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn from_toml(text: &str) -> Result<Self, InventoryError> {
        let inv: Inventory = toml::from_str(text).map_err(InventoryError::Parse)?;
        inv.validate()?;
        Ok(inv)
    }

    pub fn to_toml(&self) -> Result<String, InventoryError> {
        toml::to_string_pretty(self).map_err(InventoryError::Serialize)
    }

    /// Écriture atomique (fichier temporaire puis renommage).
    pub fn save(&self, path: &Path) -> Result<(), InventoryError> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, self.to_toml()?)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn validate(&self) -> Result<(), InventoryError> {
        let mut seen = std::collections::HashSet::new();
        for h in &self.hosts {
            validate_host(h).map_err(InventoryError::Invalid)?;
            if !seen.insert(h.name.as_str()) {
                return Err(InventoryError::Invalid(format!(
                    "le nom de poste « {} » est en double",
                    h.name
                )));
            }
        }
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&Host> {
        self.hosts.iter().find(|h| h.name == name)
    }

    /// Ajoute ou remplace un poste. `previous` est l'ancien nom en cas de
    /// modification (renommage possible).
    pub fn upsert(&mut self, previous: Option<&str>, host: Host) -> Result<(), InventoryError> {
        validate_host(&host).map_err(InventoryError::Invalid)?;
        let clash = self
            .hosts
            .iter()
            .any(|h| h.name == host.name && Some(h.name.as_str()) != previous);
        if clash {
            return Err(InventoryError::Invalid(format!(
                "un poste nommé « {} » existe déjà",
                host.name
            )));
        }
        match previous.and_then(|p| self.hosts.iter().position(|h| h.name == p)) {
            Some(i) => self.hosts[i] = host,
            None => self.hosts.push(host),
        }
        self.hosts.sort_by_key(|h| h.name.to_lowercase());
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Option<Host> {
        let i = self.hosts.iter().position(|h| h.name == name)?;
        Some(self.hosts.remove(i))
    }
}

/// Refuse les valeurs pouvant être interprétées comme des options par ssh.
pub fn validate_host(h: &Host) -> Result<(), String> {
    if h.name.trim().is_empty() {
        return Err("le nom du poste est obligatoire".into());
    }
    let addr_ok = !h.address.is_empty()
        && !h.address.starts_with('-')
        && h.address
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '_' | '[' | ']'));
    if !addr_ok {
        return Err(format!(
            "adresse invalide pour « {} » : « {} »",
            h.name, h.address
        ));
    }
    let user_ok = !h.user.is_empty()
        && !h.user.starts_with('-')
        && h.user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if !user_ok {
        return Err(format!(
            "utilisateur invalide pour « {} » : « {} »",
            h.name, h.user
        ));
    }
    if h.port == 0 {
        return Err(format!("port invalide pour « {} »", h.name));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut inv = Inventory::default();
        let mut h = Host::new("poste-01", "10.0.0.1");
        h.groups = vec!["salle-a".into()];
        inv.upsert(None, h).unwrap();
        inv.upsert(None, Host::new("poste-00", "poste00.lan"))
            .unwrap();
        let text = inv.to_toml().unwrap();
        let back = Inventory::from_toml(&text).unwrap();
        assert_eq!(inv, back);
        assert_eq!(back.hosts[0].name, "poste-00");
        assert_eq!(back.hosts[1].groups, vec!["salle-a".to_string()]);
    }

    #[test]
    fn defaults_applied() {
        let inv = Inventory::from_toml("[[host]]\nname='a'\naddress='a.lan'\n").unwrap();
        assert_eq!(inv.hosts[0].user, "root");
        assert_eq!(inv.hosts[0].port, 22);
        assert_eq!(inv.settings.max_parallel, 8);
        assert_eq!(inv.settings.flatpak_remote, "flathub");
    }

    #[test]
    fn rejects_duplicates_and_options() {
        let mut inv = Inventory::default();
        inv.upsert(None, Host::new("a", "a.lan")).unwrap();
        assert!(inv.upsert(None, Host::new("a", "b.lan")).is_err());
        assert!(inv
            .upsert(None, Host::new("b", "-oProxyCommand=x"))
            .is_err());
        let mut h = Host::new("c", "c.lan");
        h.user = "-x".into();
        assert!(inv.upsert(None, h).is_err());
    }

    #[test]
    fn rename_keeps_single_entry() {
        let mut inv = Inventory::default();
        inv.upsert(None, Host::new("a", "a.lan")).unwrap();
        inv.upsert(Some("a"), Host::new("b", "a.lan")).unwrap();
        assert_eq!(inv.hosts.len(), 1);
        assert!(inv.get("b").is_some());
    }

    #[test]
    fn search() {
        let mut h = Host::new("Poste-12", "10.1.2.12");
        h.groups = vec!["Compta".into()];
        assert!(h.matches("compta"));
        assert!(h.matches("poste"));
        assert!(h.matches("10.1"));
        assert!(!h.matches("rh"));
    }
}

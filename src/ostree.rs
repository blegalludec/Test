//! Analyse de `rpm-ostree status --json`.

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Deployment {
    #[serde(default)]
    pub booted: bool,
    #[serde(default)]
    pub staged: bool,
    #[serde(default)]
    pub pinned: bool,
    pub checksum: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub osname: Option<String>,
    /// Référence ostree (ex. `fedora:fedora/41/x86_64/silverblue`).
    #[serde(default)]
    pub origin: Option<String>,
    /// Référence d'image OCI (déploiements « ostree native container »).
    #[serde(default, rename = "container-image-reference")]
    pub container_image_reference: Option<String>,
    #[serde(default)]
    pub timestamp: Option<i64>,
    #[serde(default, rename = "requested-packages")]
    pub requested_packages: Vec<String>,
    #[serde(default, rename = "requested-local-packages")]
    pub requested_local_packages: Vec<String>,
    #[serde(default, rename = "requested-base-removals")]
    pub requested_base_removals: Vec<String>,
}

impl Deployment {
    /// Source du déploiement (ref ostree ou image conteneur).
    pub fn source(&self) -> &str {
        self.container_image_reference
            .as_deref()
            .or(self.origin.as_deref())
            .unwrap_or("inconnue")
    }

    pub fn short_checksum(&self) -> &str {
        &self.checksum[..self.checksum.len().min(10)]
    }

    pub fn display_version(&self) -> String {
        match &self.version {
            Some(v) => format!("{v} ({})", self.short_checksum()),
            None => self.short_checksum().to_string(),
        }
    }

    /// Paquets superposés, ajouts locaux et retraits de la base.
    pub fn layering_summary(&self) -> String {
        let mut parts: Vec<String> = self.requested_packages.clone();
        parts.extend(self.requested_local_packages.iter().cloned());
        parts.extend(self.requested_base_removals.iter().map(|p| format!("-{p}")));
        if parts.is_empty() {
            "aucun".into()
        } else {
            parts.join(", ")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HostStatus {
    pub deployments: Vec<Deployment>,
}

impl HostStatus {
    pub fn parse(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| format!("sortie rpm-ostree illisible : {e}"))
    }

    pub fn booted(&self) -> Option<&Deployment> {
        self.deployments.iter().find(|d| d.booted)
    }

    /// Déploiement qui sera utilisé au prochain démarrage s'il diffère
    /// de l'actuel (mise à jour, rebase ou rollback en attente).
    pub fn pending(&self) -> Option<&Deployment> {
        self.deployments.first().filter(|d| !d.booted)
    }

    pub fn reboot_required(&self) -> bool {
        self.pending().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "deployments": [
        {"id":"fedora-b","osname":"fedora","booted":false,"staged":true,"pinned":false,
         "checksum":"bbbbbbbbbbbbbbbbbbbb","version":"41.20241102.0",
         "origin":"fedora:fedora/41/x86_64/silverblue","timestamp":1730500000,
         "requested-packages":["htop","vim-enhanced"]},
        {"id":"fedora-a","osname":"fedora","booted":true,"staged":false,"pinned":false,
         "checksum":"aaaaaaaaaaaaaaaaaaaa","version":"41.20241020.0",
         "origin":"fedora:fedora/41/x86_64/silverblue","timestamp":1729400000,
         "requested-packages":["htop"],"requested-base-removals":["firefox"]}
      ],
      "transaction": null, "cached-update": null, "update-driver": null
    }"#;

    #[test]
    fn parse_sample() {
        let st = HostStatus::parse(SAMPLE).unwrap();
        let b = st.booted().unwrap();
        assert_eq!(b.display_version(), "41.20241020.0 (aaaaaaaaaa)");
        assert_eq!(b.layering_summary(), "htop, -firefox");
        assert_eq!(b.source(), "fedora:fedora/41/x86_64/silverblue");
        let p = st.pending().unwrap();
        assert!(p.staged);
        assert!(st.reboot_required());
    }

    #[test]
    fn container_source_and_no_pending() {
        let st = HostStatus::parse(
            r#"{"deployments":[{"booted":true,"checksum":"c",
            "container-image-reference":"ostree-image-signed:docker://quay.io/fedora/fedora-silverblue:41"}]}"#,
        )
        .unwrap();
        assert!(!st.reboot_required());
        assert!(st
            .booted()
            .unwrap()
            .source()
            .starts_with("ostree-image-signed"));
        assert_eq!(st.booted().unwrap().layering_summary(), "aucun");
    }

    #[test]
    fn garbage() {
        assert!(HostStatus::parse("Error: not ostree").is_err());
    }
}

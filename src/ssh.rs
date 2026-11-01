//! Exécution de commandes distantes via le client OpenSSH du système.
//!
//! On s'appuie volontairement sur `ssh` : il réutilise `~/.ssh/config`,
//! l'agent, les known_hosts et le multiplexage, comme un administrateur
//! s'y attend.

use crate::inventory::{Host, Settings};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// Code de sortie (`None` si le processus a été tué par un signal).
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Entoure une chaîne de guillemets simples pour le shell distant.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./_-".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Arguments passés à `ssh` (hors binaire).
pub fn build_args(host: &Host, settings: &Settings, remote_cmd: &str) -> Vec<String> {
    let mut args = vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        format!("ConnectTimeout={}", settings.connect_timeout),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-p".into(),
        host.port.to_string(),
    ];
    if let Some(key) = &settings.identity_file {
        args.push("-i".into());
        args.push(key.to_string_lossy().into_owned());
    }
    args.push("--".into());
    args.push(host.destination());
    // Forcer une locale neutre pour pouvoir analyser les sorties.
    args.push(format!("LC_ALL=C.UTF-8 {remote_cmd}"));
    args
}

/// Lance la commande et attend sa fin (à appeler hors du thread GTK).
pub fn run(host: &Host, settings: &Settings, remote_cmd: &str) -> std::io::Result<CommandOutput> {
    let out = Command::new("ssh")
        .args(build_args(host, settings, remote_cmd))
        .stdin(Stdio::null())
        .output()?;
    Ok(CommandOutput {
        status: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("firefox"), "firefox");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("$(rm -rf /)"), "'$(rm -rf /)'");
    }

    #[test]
    fn args() {
        let mut h = Host::new("p", "p.lan");
        h.user = "admin".into();
        h.port = 2222;
        let s = Settings {
            identity_file: Some("/k/id_ed25519".into()),
            ..Settings::default()
        };
        let a = build_args(&h, &s, "rpm-ostree status --json");
        assert!(a.contains(&"BatchMode=yes".to_string()));
        assert!(a.windows(2).any(|w| w == ["-p", "2222"]));
        assert!(a.windows(2).any(|w| w == ["-i", "/k/id_ed25519"]));
        let n = a.len();
        assert_eq!(a[n - 3], "--");
        assert_eq!(a[n - 2], "admin@p.lan");
        assert_eq!(a[n - 1], "LC_ALL=C.UTF-8 rpm-ostree status --json");
    }
}

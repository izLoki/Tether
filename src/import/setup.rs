//! Portable connection metadata. This schema cannot serialize credentials or key paths.
use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use super::{Candidate, Source};
use crate::profile::Profile;

const FORMAT: &str = "tether-hosts";
const VERSION: u32 = 1;
const MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Setup {
    format: String,
    version: u32,
    hosts: Vec<Host>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Host {
    name: String,
    host: String,
    port: u16,
    user: String,
    hue: f32,
}

pub fn encode(profiles: &[Profile]) -> Result<String> {
    let hosts = profiles.iter().map(|p| Host { name: p.name.clone(), host: p.host.clone(), port: p.port, user: p.user.clone(), hue: p.hue }).collect();
    serde_json::to_string_pretty(&Setup { format: FORMAT.into(), version: VERSION, hosts }).context("encoding setup")
}

pub fn decode(text: &str) -> Result<Vec<Profile>> {
    ensure!(text.len() as u64 <= MAX_BYTES, "Setup file exceeds 4 MiB");
    let setup: Setup = serde_json::from_str(text).context("Not a Tether setup file")?;
    ensure!(setup.format == FORMAT && setup.version == VERSION, "Unsupported Tether setup format or version");
    ensure!(setup.hosts.len() <= 10_000, "Setup has too many hosts");
    setup
        .hosts
        .into_iter()
        .map(|h| {
            ensure!(h.name.len() <= 256 && h.host.len() <= 253 && h.user.len() <= 256, "Host details are too long");
            ensure!(
                !h.host.chars().any(char::is_control) && !h.user.chars().any(char::is_control) && !h.name.chars().any(char::is_control),
                "Host details contain control characters"
            );
            ensure!(h.hue.is_finite() && (0.0..=360.0).contains(&h.hue), "Invalid host color");
            let mut profile = Profile::new();
            profile.name = h.name;
            profile.host = h.host;
            profile.port = h.port;
            profile.user = h.user;
            profile.hue = h.hue;
            profile.validate().map_err(anyhow::Error::msg)?;
            Ok(profile)
        })
        .collect()
}

pub fn load(path: &Path) -> Result<Vec<Candidate>> {
    ensure!(std::fs::metadata(path)?.len() <= MAX_BYTES, "Setup file exceeds 4 MiB");
    Ok(decode(&std::fs::read_to_string(path)?)?.into_iter().map(|profile| Candidate { source: Source::Tether, origin: path.into(), profile }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::AuthMethod;

    #[test]
    fn roundtrip_excludes_every_credential_and_generates_new_ids() {
        let mut p = Profile::parse_quick("alice@10.0.0.1:2222").unwrap();
        p.name = "Production".into();
        p.auth = AuthMethod::Key { path: "secret/private-key".into() };
        let text = encode(&[p.clone()]).unwrap();
        for secret in ["auth", "secret", "path", "password", "key", "folder", "id"] {
            assert!(!text.contains(secret));
        }
        let imported = decode(&text).unwrap().remove(0);
        assert_ne!(p.id, imported.id);
        assert_eq!(imported.auth, AuthMethod::Password);
        assert_eq!(imported.address(), p.address());
        assert_eq!(imported.name, "Production");
    }

    #[test]
    fn invalid_host_or_version_rejects_whole_file() {
        let p = Profile::parse_quick("alice@host").unwrap();
        let text = encode(&[p]).unwrap();
        assert!(decode(&text.replace("\"version\": 1", "\"version\": 2")).is_err());
        assert!(decode(&text.replace("\"port\": 22", "\"port\": 0")).is_err());
        assert!(decode(&text.replace("\"port\": 22", "\"password\": \"no\", \"port\": 22")).is_err());
    }
}

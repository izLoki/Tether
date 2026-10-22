//! Import hosts from other SSH clients. Only connection details (name, host, port,
//! user, key file path) are imported; passwords stay where they are.

pub mod bitvise;
pub mod discovery;
pub mod openssh;
pub mod setup;

use std::path::PathBuf;

use crate::profile::{Profile, ProfileStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Bitvise,
    OpenSsh,
    Tether,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Bitvise => "Bitvise",
            Source::OpenSsh => "SSH config",
            Source::Tether => "Tether setup",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub source: Source,
    pub origin: PathBuf,
    pub profile: Profile,
}

impl Candidate {
    /// Same user@host:port already saved.
    pub fn exists_in(&self, store: &ProfileStore) -> bool {
        store.profiles.iter().any(|p| p.host.eq_ignore_ascii_case(&self.profile.host) && p.port == self.profile.port && p.user == self.profile.user)
    }
}

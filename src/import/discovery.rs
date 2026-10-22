//! Bounded discovery under OS-provided user folders; never scans disks or follows links.
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{Candidate, bitvise};

#[derive(Default)]
pub struct Found {
    pub keys: Vec<PathBuf>,
    pub profiles: Vec<Candidate>,
    pub searched: Vec<PathBuf>,
    pub limited: bool,
}

pub fn roots() -> Vec<PathBuf> {
    let mut dirs = bitvise::search_dirs();
    if let Some(u) = directories::UserDirs::new() {
        dirs.extend(u.download_dir().map(Path::to_path_buf));
        for name in [".ssh", "Bitvise", "Bitvise SSH Client"] {
            dirs.push(u.home_dir().join(name));
        }
    }
    if let Some(b) = directories::BaseDirs::new() {
        for base in [b.config_dir(), b.data_local_dir(), b.data_dir()] {
            for name in ["Bitvise", "Bitvise SSH Client", "BvSshClient"] {
                dirs.push(base.join(name));
            }
        }
    }
    let mut seen = HashSet::new();
    dirs.retain(|p| p.is_dir() && seen.insert(p.clone()));
    dirs
}

pub fn scan(roots: Vec<PathBuf>) -> Found {
    let mut found = Found { searched: roots.clone(), ..Default::default() };
    let mut queue: VecDeque<_> = roots.into_iter().map(|p| (p, 0)).collect();
    let mut visited = HashSet::new();
    let started = Instant::now();
    let mut entries_seen = 0;
    while let Some((dir, depth)) = queue.pop_front() {
        if started.elapsed() > Duration::from_secs(10) || entries_seen >= 20_000 {
            found.limited = true;
            break;
        }
        let Ok(canonical) = dir.canonicalize() else { continue };
        if !visited.insert(canonical) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            entries_seen += 1;
            if entries_seen >= 20_000 || started.elapsed() > Duration::from_secs(10) {
                found.limited = true;
                break;
            }
            let Ok(kind) = entry.file_type() else { continue };
            let path = entry.path();
            if kind.is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if kind.is_dir() && depth < 4 && !["node_modules", ".git", "target", "cache"].contains(&name.as_str()) {
                queue.push_back((path, depth + 1));
            } else if kind.is_file() && entry.metadata().is_ok_and(|m| m.len() < 1024 * 1024) {
                if name.ends_with(".tlp") {
                    if let Some(c) = bitvise::load(&path) {
                        found.profiles.push(c);
                    }
                } else if is_key_name(&name) && is_private_key(&path) {
                    found.keys.push(path);
                }
            }
        }
    }
    found.keys.sort();
    found.keys.dedup();
    found.profiles.sort_by(|a, b| a.origin.cmp(&b.origin));
    found.profiles.dedup_by(|a, b| a.origin == b.origin);
    found
}

fn is_key_name(name: &str) -> bool {
    !name.ends_with(".pub") && (name.starts_with("id_") || [".pem", ".key", ".ppk", ".prv"].iter().any(|ext| name.ends_with(ext)) || name.contains("private"))
}

fn is_private_key(path: &Path) -> bool {
    use std::io::Read;
    let Ok(file) = std::fs::File::open(path) else { return false };
    let mut header = String::new();
    if file.take(160).read_to_string(&mut header).is_err() {
        return false;
    }
    [
        "-----BEGIN OPENSSH PRIVATE KEY-----",
        "-----BEGIN RSA PRIVATE KEY-----",
        "-----BEGIN EC PRIVATE KEY-----",
        "-----BEGIN PRIVATE KEY-----",
        "-----BEGIN ENCRYPTED PRIVATE KEY-----",
    ]
    .iter()
    .any(|h| header.starts_with(h))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_generic_key_names_in_nested_folders_without_importing_them() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("Bitvise/Keys");
        std::fs::create_dir_all(&nested).unwrap();
        let key = nested.join("id_ed25519");
        std::fs::write(&key, "-----BEGIN OPENSSH PRIVATE KEY-----\nsample").unwrap();
        std::fs::write(nested.join("id_ed25519.pub"), "ssh-ed25519 public").unwrap();
        std::fs::write(nested.join("random.key"), "unrelated").unwrap();
        let found = scan(vec![dir.path().into(), nested]);
        assert_eq!(found.keys, vec![key]);
        assert!(!found.limited);
    }
}

//! Local project shortcuts and bounded directory history, shared by both launchers.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub path: String,
    pub name: String,
    pub saved: bool,
    pub visits: u32,
    pub last_visit: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Projects {
    pub entries: Vec<Project>,
    pub last_directory: String,
}

impl Projects {
    pub fn record(&mut self, path: &str, now: u64) {
        let path = path.trim_end_matches(['/', '\\']);
        // Keep filesystem roots intact.
        let path = if path.is_empty() || path.ends_with(':') { format!("{path}{}", std::path::MAIN_SEPARATOR) } else { path.to_owned() };
        self.last_directory.clone_from(&path);
        if let Some(p) = self.entries.iter_mut().find(|p| same_path(&p.path, &path)) {
            p.visits = p.visits.saturating_add(1);
            p.last_visit = now;
        } else {
            let name = PathBuf::from(&path).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
            self.entries.push(Project { path, name, saved: false, visits: 1, last_visit: now });
        }
        self.entries.sort_by_key(|p| (std::cmp::Reverse(p.saved), std::cmp::Reverse(p.last_visit)));
        let mut recent = 0;
        self.entries.retain(|p| {
            if p.saved {
                true
            } else {
                recent += 1;
                recent <= 32
            }
        });
    }
}

pub fn same_path(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        let s = s.replace('\\', "/").trim_end_matches('/').to_owned();
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    norm(a) == norm(b)
}

pub fn home() -> PathBuf {
    directories::UserDirs::new().map(|d| d.home_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from("."))
}

pub fn resolve(text: &str) -> PathBuf {
    let text = text.trim().trim_matches('"');
    if text == "~" {
        home()
    } else if let Some(rest) = text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\")) {
        home().join(rest)
    } else {
        let path = PathBuf::from(text);
        if path.is_absolute() { path } else { home().join(path) }
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recent_history_is_bounded_but_saved_projects_survive() {
        let mut projects = Projects::default();
        projects.record("/saved project", 1);
        projects.entries[0].saved = true;
        for n in 0..50 {
            projects.record(&format!("/project{n}"), n + 2);
        }
        assert_eq!(projects.entries.len(), 33);
        assert!(projects.entries[0].saved);
        projects.record("/project49/", 100);
        assert_eq!(projects.entries.len(), 33);
        assert_eq!(projects.entries.iter().find(|p| p.path.ends_with("project49")).unwrap().visits, 2);
        assert_eq!(resolve("~/project with spaces"), home().join("project with spaces"));
    }
}

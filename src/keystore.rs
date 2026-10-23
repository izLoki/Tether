//! Managed SSH keys (Bitvise-style client key manager): OpenSSH private key files under
//! `<config>/keys/<uuid>` plus a metadata index in `<config>/keys/keys.json`. Passphrases
//! never touch disk; remembered ones live in the OS keyring under the key's id.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use russh::keys::ssh_key::rand_core::UnwrapErr;
use russh::keys::ssh_key::{self, LineEnding, getrandom::SysRng, private::KeypairData, private::RsaKeypair};
use russh::keys::{Algorithm, EcdsaCurve, HashAlg, PrivateKey, PublicKey};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::profile::{secret, unix_now, write_json_atomic};

const INDEX_FILE: &str = "keys.json";
/// Longest key name / comment accepted from the UI.
pub const MAX_LABEL: usize = 120;

/// Key types offered when generating a key, most recommended first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    Ed25519,
    EcdsaP256,
    EcdsaP384,
    EcdsaP521,
    Rsa2048,
    Rsa3072,
    Rsa4096,
}

impl KeyKind {
    pub const ALL: [KeyKind; 7] =
        [KeyKind::Ed25519, KeyKind::EcdsaP256, KeyKind::EcdsaP384, KeyKind::EcdsaP521, KeyKind::Rsa2048, KeyKind::Rsa3072, KeyKind::Rsa4096];

    pub fn label(self) -> &'static str {
        match self {
            KeyKind::Ed25519 => "Ed25519",
            KeyKind::EcdsaP256 => "ECDSA P-256",
            KeyKind::EcdsaP384 => "ECDSA P-384",
            KeyKind::EcdsaP521 => "ECDSA P-521",
            KeyKind::Rsa2048 => "RSA 2048",
            KeyKind::Rsa3072 => "RSA 3072",
            KeyKind::Rsa4096 => "RSA 4096",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            KeyKind::Ed25519 => "Recommended: small, fast and supported by every modern server",
            KeyKind::EcdsaP256 | KeyKind::EcdsaP384 | KeyKind::EcdsaP521 => "NIST curves, for servers that require ECDSA",
            KeyKind::Rsa2048 | KeyKind::Rsa3072 | KeyKind::Rsa4096 => "For old servers; larger sizes take a few seconds to generate",
        }
    }

    fn algorithm(self) -> Algorithm {
        match self {
            KeyKind::Ed25519 => Algorithm::Ed25519,
            KeyKind::EcdsaP256 => Algorithm::Ecdsa { curve: EcdsaCurve::NistP256 },
            KeyKind::EcdsaP384 => Algorithm::Ecdsa { curve: EcdsaCurve::NistP384 },
            KeyKind::EcdsaP521 => Algorithm::Ecdsa { curve: EcdsaCurve::NistP521 },
            KeyKind::Rsa2048 | KeyKind::Rsa3072 | KeyKind::Rsa4096 => Algorithm::Rsa { hash: None },
        }
    }

    fn rsa_bits(self) -> Option<usize> {
        match self {
            KeyKind::Rsa2048 => Some(2048),
            KeyKind::Rsa3072 => Some(3072),
            KeyKind::Rsa4096 => Some(4096),
            _ => None,
        }
    }
}

/// What the index remembers about a key (everything but the secret material).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyMeta {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub comment: String,
    /// Display name of the algorithm, e.g. `Ed25519` or `RSA 4096`.
    pub algorithm: String,
    /// `SHA256:...`
    pub fingerprint: String,
    /// OpenSSH one-line public key (`ssh-ed25519 AAAA... comment`).
    pub public: String,
    pub encrypted: bool,
    #[serde(default)]
    pub created: u64,
}

impl KeyMeta {
    /// The public key line without its comment: safe to embed in shell commands
    /// (algorithm name and base64 only).
    pub fn public_without_comment(&self) -> String {
        self.public.split_whitespace().take(2).collect::<Vec<_>>().join(" ")
    }

    /// Shell command appending the public key to `~/.ssh/authorized_keys` unless it is
    /// already there. Runs under `sh` whatever the login shell is; every byte embedded
    /// is checked to be inert inside double quotes.
    pub fn install_command(&self) -> Result<String> {
        let mut line = self.public_without_comment();
        let safe_comment = !self.comment.is_empty() && self.comment.chars().all(|c| c.is_ascii_alphanumeric() || "@._-".contains(c));
        if safe_comment {
            line = format!("{line} {}", self.comment);
        }
        if line.is_empty() || !line.chars().all(|c| c.is_ascii_alphanumeric() || "+/= @._-".contains(c)) {
            bail!("unexpected characters in the public key");
        }
        let key = self.public_without_comment();
        // The `tail -c1` guard adds a missing final newline so the key isn't glued onto
        // the last line of the file.
        Ok(format!(
            "sh -c 'umask 077; f=~/.ssh/authorized_keys; mkdir -p ~/.ssh && touch \"$f\" && \
             {{ grep -qF \"{key}\" \"$f\" || {{ [ -z \"$(tail -c1 \"$f\")\" ] || echo >> \"$f\"; printf \"%s\\n\" \"{line}\" >> \"$f\"; }}; }}'"
        ))
    }

    /// "ssh-keygen -lv"-style art box for the fingerprint.
    pub fn randomart(&self) -> Option<String> {
        let key = PublicKey::from_openssh(&self.public).ok()?;
        Some(key.fingerprint(HashAlg::Sha256).to_randomart(&format!("[{}]", self.algorithm)))
    }
}

/// The passphrase did not unlock the key.
#[derive(Debug)]
pub struct BadPassphrase(pub Uuid);

impl std::fmt::Display for BadPassphrase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Wrong passphrase for the private key")
    }
}

impl std::error::Error for BadPassphrase {}

#[derive(Debug, Default)]
pub struct KeyStore {
    dir: PathBuf,
    pub keys: Vec<KeyMeta>,
}

/// `<config>/keys`.
pub fn default_dir() -> PathBuf {
    crate::app::config_dir().join("keys")
}

impl KeyStore {
    pub fn load(dir: PathBuf) -> Result<Self> {
        let index = dir.join(INDEX_FILE);
        let keys = match std::fs::read_to_string(&index) {
            Ok(text) => serde_json::from_str(&text).with_context(|| format!("key index is corrupt: {}", index.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).context("reading key index"),
        };
        let store = Self { dir, keys };
        store.sweep_temp_files();
        Ok(store)
    }

    pub fn save(&self) -> Result<()> {
        write_json_atomic(&self.dir.join(INDEX_FILE), &self.keys)
    }

    pub fn get(&self, id: Uuid) -> Option<&KeyMeta> {
        self.keys.iter().find(|k| k.id == id)
    }

    fn key_path(&self, id: Uuid) -> PathBuf {
        self.dir.join(id.simple().to_string())
    }

    /// Stores a freshly generated key, encrypted when `passphrase` is non-empty.
    /// Encrypting runs the bcrypt KDF (~75 ms): the UI calls [`encrypt`] off-thread and
    /// then [`Self::add_key`] instead.
    pub fn add_generated(&mut self, name: &str, key: PrivateKey, passphrase: Option<&str>) -> Result<KeyMeta> {
        let key = encrypt(key, passphrase)?;
        self.add_key(name, key)
    }

    /// Stores `key` exactly as given (already encrypted, or not).
    pub fn add_key(&mut self, name: &str, key: PrivateKey) -> Result<KeyMeta> {
        let pem = key.to_openssh(LineEnding::LF).map_err(|e| anyhow!("encoding key: {e}"))?;
        self.insert(name, &key, pem.as_bytes())
    }

    /// Imports an OpenSSH private key file as-is (encrypted keys stay encrypted; the
    /// public half is readable without the passphrase).
    pub fn import_file(&mut self, path: &Path) -> Result<KeyMeta> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let key = PrivateKey::from_openssh(&bytes).map_err(|e| anyhow!("not an OpenSSH private key ({e})"))?;
        if let Some(dup) = self.keys.iter().find(|k| k.fingerprint == fingerprint(key.public_key())) {
            bail!("This key is already in the manager as “{}”", dup.name);
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Imported key".into());
        self.insert(&name, &key, &bytes)
    }

    fn insert(&mut self, name: &str, key: &PrivateKey, pem: &[u8]) -> Result<KeyMeta> {
        let id = Uuid::new_v4();
        std::fs::create_dir_all(&self.dir).context("creating keys folder")?;
        write_private(&self.key_path(id), pem)?;
        let meta = KeyMeta {
            id,
            name: clean_label(name, "Unnamed key"),
            comment: key.comment().as_str_lossy().to_string(),
            algorithm: algorithm_label(key.public_key()),
            fingerprint: fingerprint(key.public_key()),
            public: key.public_key().to_openssh().map_err(|e| anyhow!("encoding public key: {e}"))?,
            encrypted: key.is_encrypted(),
            created: unix_now(),
        };
        self.keys.push(meta.clone());
        self.save()?;
        Ok(meta)
    }

    /// The decrypted private key, ready for authentication.
    pub fn load_private(&self, id: Uuid, passphrase: Option<&str>) -> Result<PrivateKey> {
        load_file(&self.key_path(id), id, passphrase)
    }

    /// Where the private key of `id` lives (for [`prepare_rewrite`] on another thread).
    pub fn key_file(&self, id: Uuid) -> PathBuf {
        self.key_path(id)
    }

    /// Re-encrypts with `new` (or removes the passphrase when it is empty).
    pub fn change_passphrase(&mut self, id: Uuid, old: Option<&str>, new: Option<&str>) -> Result<()> {
        let rewrite = prepare_rewrite(&self.key_path(id), id, old, new, None)?;
        self.apply(rewrite)
    }

    /// Updates the label and (if it changed) the key comment, which lives inside the
    /// private file and so needs the passphrase of an encrypted key.
    pub fn edit(&mut self, id: Uuid, name: &str, comment: &str, passphrase: Option<&str>) -> Result<()> {
        if let Some(work) = self.prepare_edit(id, comment, passphrase.map(str::to_string))? {
            self.apply(work()?)?;
        }
        self.rename(id, name);
        self.save()
    }

    /// The work a comment change needs (`None` when the comment is unchanged), to run
    /// on another thread since it goes through the KDF. Never adds a passphrase to a key
    /// that has none.
    pub fn prepare_edit(&self, id: Uuid, comment: &str, passphrase: Option<String>) -> Result<Option<impl FnOnce() -> Result<Rewrite> + Send + 'static>> {
        let meta = self.get(id).ok_or_else(|| anyhow!("unknown key"))?;
        let comment = clean_label(comment, "");
        if comment == meta.comment {
            return Ok(None);
        }
        let keep = if meta.encrypted { passphrase } else { None };
        let path = self.key_path(id);
        Ok(Some(move || prepare_rewrite(&path, id, keep.as_deref(), keep.as_deref(), Some(&comment))))
    }

    /// Changes the label (in memory; `save` persists it).
    pub fn rename(&mut self, id: Uuid, name: &str) {
        if let Some(k) = self.keys.iter_mut().find(|k| k.id == id) {
            k.name = clean_label(name, &k.name);
        }
    }

    /// Writes a prepared rewrite (fast: the crypto already happened) and updates the index.
    pub fn apply(&mut self, rewrite: Rewrite) -> Result<()> {
        let path = self.key_path(rewrite.id);
        let tmp = path.with_extension("tmp");
        let written = write_private(&tmp, rewrite.pem.as_bytes()).and_then(|()| std::fs::rename(&tmp, &path).context("replacing key file"));
        if let Err(e) = written {
            // Never leave a (possibly unencrypted) copy of the key behind.
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        if let Some(k) = self.keys.iter_mut().find(|k| k.id == rewrite.id) {
            k.encrypted = rewrite.encrypted;
            k.comment = rewrite.comment;
            if let Some(public) = rewrite.public {
                k.public = public;
            }
        }
        self.save()
    }

    /// Copies the private key file (exactly as stored) to `dest`.
    pub fn export_private(&self, id: Uuid, dest: &Path) -> Result<()> {
        let bytes = Zeroizing::new(std::fs::read(self.key_path(id)).context("reading key file")?);
        write_private(dest, &bytes).with_context(|| format!("writing {}", dest.display()))
    }

    pub fn remove(&mut self, id: Uuid) -> Result<()> {
        for path in [self.key_path(id), self.key_path(id).with_extension("tmp")] {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).context("deleting key file"),
            }
        }
        let _ = secret::delete(id);
        self.keys.retain(|k| k.id != id);
        self.save()
    }

    /// Deletes `*.tmp` leftovers of interrupted rewrites.
    fn sweep_temp_files(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return };
        for e in entries.flatten() {
            if e.path().extension().is_some_and(|x| x == "tmp") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// A re-encoded private key file, prepared off the UI thread by [`prepare_rewrite`].
pub struct Rewrite {
    id: Uuid,
    pem: Zeroizing<String>,
    comment: String,
    public: Option<String>,
    encrypted: bool,
}

/// Unlocks the key file with `unlock`, optionally changes its comment, and encodes it
/// again, encrypted with `new_pass` (none when empty). Runs the KDF: call off-thread.
pub fn prepare_rewrite(path: &Path, id: Uuid, unlock: Option<&str>, new_pass: Option<&str>, comment: Option<&str>) -> Result<Rewrite> {
    let mut key = load_file(path, id, unlock)?;
    if let Some(c) = comment {
        key.set_comment(c);
    }
    // Read these before encrypting: an encrypted key only carries its comment inside
    // the encrypted section.
    let comment = key.comment().as_str_lossy().to_string();
    let public = key.public_key().to_openssh().ok();
    let key = encrypt(key, new_pass)?;
    let pem = key.to_openssh(LineEnding::LF).map_err(|e| anyhow!("encoding key: {e}"))?;
    Ok(Rewrite { id, pem, comment, public, encrypted: key.is_encrypted() })
}

/// `key` encrypted with `passphrase`, or unchanged when that is empty. Runs the KDF.
pub fn encrypt(key: PrivateKey, passphrase: Option<&str>) -> Result<PrivateKey> {
    match passphrase.filter(|p| !p.is_empty()) {
        Some(p) => key.encrypt(&mut SysRng, p).map_err(|e| anyhow!("encrypting key: {e}")),
        None => Ok(key),
    }
}

fn load_file(path: &Path, id: Uuid, passphrase: Option<&str>) -> Result<PrivateKey> {
    let bytes = Zeroizing::new(std::fs::read(path).context("reading managed key file (was it deleted?)")?);
    let key = PrivateKey::from_openssh(&*bytes).map_err(|e| anyhow!("managed key is corrupt ({e})"))?;
    if !key.is_encrypted() {
        return Ok(key);
    }
    let pass = passphrase.filter(|p| !p.is_empty()).ok_or_else(|| anyhow!("Passphrase required"))?;
    key.decrypt(pass).map_err(|_| BadPassphrase(id).into())
}

/// Writes private key material: owner-only on Unix, flushed to disk.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // mode() only applies when creating; exports may overwrite an existing file.
        file.set_permissions(std::fs::Permissions::from_mode(0o600)).context("protecting private key file")?;
    }
    file.write_all(bytes).context("writing key file")?;
    file.sync_all().context("flushing key file")
}

// Generates a new key. RSA 4096 takes seconds: call off the UI thread.
pub fn generate(kind: KeyKind, comment: &str) -> Result<PrivateKey> {
    let mut rng = UnwrapErr(SysRng);
    let comment = clean_label(comment, "");
    match kind.rsa_bits() {
        Some(bits) => {
            let pair = RsaKeypair::random(&mut rng, bits).map_err(|e| anyhow!("generating RSA key: {e}"))?;
            PrivateKey::new(KeypairData::from(pair), comment).map_err(|e| anyhow!("building key: {e}"))
        }
        None => {
            let mut key = PrivateKey::random(&mut rng, kind.algorithm()).map_err(|e| anyhow!("generating key: {e}"))?;
            key.set_comment(comment.as_str());
            Ok(key)
        }
    }
}

/// `user@COMPUTER`, the conventional default comment.
pub fn default_comment() -> String {
    let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "me".into());
    let host = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "tether".into());
    format!("{}@{}", user, host.to_lowercase())
}

fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

fn algorithm_label(key: &PublicKey) -> String {
    match key.key_data() {
        ssh_key::public::KeyData::Rsa(rsa) => {
            let bits = rsa.key_size();
            format!("RSA {bits}")
        }
        ssh_key::public::KeyData::Ed25519(_) => "Ed25519".into(),
        ssh_key::public::KeyData::Ecdsa(e) => format!("ECDSA {}", e.curve().as_str().trim_start_matches("nist")),
        other => other.algorithm().as_str().to_string(),
    }
}

/// Single line, trimmed, length-capped.
fn clean_label(raw: &str, fallback: &str) -> String {
    let one_line: String = raw.chars().filter(|c| !c.is_control()).take(MAX_LABEL).collect();
    let trimmed = one_line.trim();
    if trimmed.is_empty() { fallback.to_string() } else { trimmed.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, KeyStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::load(dir.path().join("keys")).unwrap();
        (dir, store)
    }

    #[cfg(unix)]
    #[test]
    fn private_key_exports_restrict_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&path, b"private").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read(&path).unwrap(), b"private");
    }

    #[test]
    fn generate_store_and_reload_ed25519() {
        let (dir, mut s) = store();
        let key = generate(KeyKind::Ed25519, "ana@box").unwrap();
        let meta = s.add_generated("Laptop", key, None).unwrap();
        assert_eq!((meta.algorithm.as_str(), meta.comment.as_str(), meta.encrypted), ("Ed25519", "ana@box", false));
        assert!(meta.public.starts_with("ssh-ed25519 "));
        assert!(meta.fingerprint.starts_with("SHA256:"));
        let again = KeyStore::load(dir.path().join("keys")).unwrap();
        assert_eq!(again.keys, vec![meta.clone()]);
        assert!(again.load_private(meta.id, None).is_ok());
        assert!(meta.randomart().unwrap().contains("[SHA256]"));
    }

    #[test]
    fn encrypted_keys_need_the_right_passphrase() {
        let (_dir, mut s) = store();
        let meta = s.add_generated("k", generate(KeyKind::EcdsaP256, "").unwrap(), Some("hunter2")).unwrap();
        assert!(meta.encrypted);
        assert!(meta.algorithm.starts_with("ECDSA"));
        assert!(s.load_private(meta.id, None).is_err());
        let err = s.load_private(meta.id, Some("nope")).unwrap_err();
        assert!(err.downcast_ref::<BadPassphrase>().is_some());
        assert!(s.load_private(meta.id, Some("hunter2")).is_ok());

        s.change_passphrase(meta.id, Some("hunter2"), None).unwrap();
        assert!(!s.get(meta.id).unwrap().encrypted);
        assert!(s.load_private(meta.id, None).is_ok());
        s.change_passphrase(meta.id, None, Some("new")).unwrap();
        assert!(s.get(meta.id).unwrap().encrypted);
        assert!(s.load_private(meta.id, Some("new")).is_ok());
    }

    #[test]
    fn edit_renames_and_rewrites_comment() {
        let (_dir, mut s) = store();
        let meta = s.add_generated("old", generate(KeyKind::Ed25519, "a@b").unwrap(), Some("pw")).unwrap();
        assert!(s.edit(meta.id, "x", "changed", Some("bad")).is_err(), "comment needs the passphrase");
        s.edit(meta.id, "  New name \n", "c@d", Some("pw")).unwrap();
        let k = s.get(meta.id).unwrap();
        assert_eq!((k.name.as_str(), k.comment.as_str()), ("New name", "c@d"));
        assert!(k.public.ends_with(" c@d"));
        assert_eq!(k.fingerprint, meta.fingerprint, "same key material");
        s.edit(meta.id, "Only rename", "c@d", None).unwrap();
        assert_eq!(s.get(meta.id).unwrap().name, "Only rename");
    }

    #[test]
    fn import_keeps_encrypted_files_and_rejects_duplicates() {
        let (dir, mut s) = store();
        let key = generate(KeyKind::Ed25519, "x").unwrap().encrypt(&mut SysRng, "pw").unwrap();
        let file = dir.path().join("id_test");
        std::fs::write(&file, key.to_openssh(LineEnding::LF).unwrap().as_bytes()).unwrap();
        let meta = s.import_file(&file).unwrap();
        assert!(meta.encrypted);
        assert_eq!(meta.name, "id_test");
        assert!(s.import_file(&file).is_err(), "duplicate fingerprint");
        assert!(s.import_file(&dir.path().join("missing")).is_err());
        std::fs::write(dir.path().join("junk"), "hello").unwrap();
        assert!(s.import_file(&dir.path().join("junk")).is_err());
    }

    #[test]
    fn remove_deletes_file_and_entry() {
        let (_dir, mut s) = store();
        let meta = s.add_generated("k", generate(KeyKind::Ed25519, "").unwrap(), None).unwrap();
        s.remove(meta.id).unwrap();
        assert!(s.keys.is_empty());
        assert!(s.load_private(meta.id, None).is_err());
    }

    #[test]
    fn public_line_without_comment_is_shell_safe() {
        let (_dir, mut s) = store();
        let meta = s.add_generated("k", generate(KeyKind::Ed25519, "it's me; rm -rf").unwrap(), None).unwrap();
        let line = meta.public_without_comment();
        assert!(line.starts_with("ssh-ed25519 "));
        assert!(line.chars().all(|c| c.is_ascii_alphanumeric() || "+/= -".contains(c)), "{line}");
    }

    #[test]
    fn install_command_is_quoted_and_idempotent() {
        let (_dir, mut s) = store();
        let meta = s.add_generated("k", generate(KeyKind::Ed25519, "ana@laptop").unwrap(), None).unwrap();
        let cmd = meta.install_command().unwrap();
        assert!(cmd.starts_with("sh -c '") && cmd.ends_with('\''));
        assert_eq!(cmd.matches('\'').count(), 2);
        assert!(cmd.contains("grep -qF"), "skips keys already present");
        assert!(cmd.contains(" ana@laptop\""), "safe comment kept");
        let odd = s.add_generated("k2", generate(KeyKind::Ed25519, "it's $(me)").unwrap(), None).unwrap();
        let cmd = odd.install_command().unwrap();
        assert!(!cmd.contains("$(me)"), "unsafe comment dropped");
        let mut bad = meta.clone();
        bad.public = "ssh-ed25519 AAAA\"x".into();
        assert!(bad.install_command().is_err());
    }

    #[test]
    fn labels_are_cleaned() {
        assert_eq!(clean_label("  a\tb\n ", "x"), "ab");
        assert_eq!(clean_label("   ", "fallback"), "fallback");
        assert_eq!(clean_label(&"z".repeat(500), "").len(), MAX_LABEL);
    }
}

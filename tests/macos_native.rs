//! Explicit native Keychain smoke test. Never run unattended on locked CI keychains.
#![cfg(target_os = "macos")]

#[test]
#[ignore = "requires an unlocked macOS login Keychain; run explicitly on a developer Mac"]
fn macos_keychain_persists_updates_and_deletes_a_secret() {
    use tether::profile::secret;
    use uuid::Uuid;
    struct Cleanup(Uuid);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = secret::delete(self.0);
        }
    }
    let id = Uuid::new_v4();
    let _cleanup = Cleanup(id);
    secret::set(id, "tether-qa-first").unwrap();
    assert_eq!(secret::get(id).as_deref(), Some("tether-qa-first"));
    secret::set(id, "tether-qa-updated").unwrap();
    assert_eq!(secret::get(id).as_deref(), Some("tether-qa-updated"));
    secret::delete(id).unwrap();
    assert!(secret::get(id).is_none());
}

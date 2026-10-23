//! End-to-end tests against a real SSH server. Skipped unless `TETHER_E2E` is set:
//!
//! ```sh
//! docker run -d --name tether-e2e -p 127.0.0.1:2222:2222 -e PASSWORD_ACCESS=true \
//!   -e USER_NAME=tester -e USER_PASSWORD=tetherpass lscr.io/linuxserver/openssh-server
//! TETHER_E2E=1 cargo test --test e2e_ssh -- --nocapture --test-threads=1
//! ```

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tether::known_hosts::KnownHosts;
use tether::profile::Profile;
use tether::ssh::pool::Pool;
use tether::ssh::sftp::Sftp;
use tether::ssh::terminal::{LinkState, TerminalHandle, run_shell};
use tether::ssh::transfer::{self, Direction, Progress};

const PASSWORD: &str = "tetherpass";

fn enabled() -> bool {
    std::env::var_os("TETHER_E2E").is_some()
}

fn profile() -> Profile {
    let mut p = Profile::parse_quick(&std::env::var("TETHER_E2E_TARGET").unwrap_or("tester@127.0.0.1:2222".into())).unwrap();
    p.name = "e2e".into();
    p
}

fn pool(dir: &tempfile::TempDir) -> Pool {
    Pool::new(KnownHosts::load(dir.path().join("kh.json")))
}

/// Deterministic pseudo-random bytes (xorshift) so corruption can't hide in zeros.
fn payload(len: usize) -> Vec<u8> {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

#[tokio::test]
async fn connection_is_shared_and_host_key_learned() {
    if !enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(&dir);
    let p = profile();
    let a = pool.get(&p, Some(PASSWORD.into())).await.expect("connect");
    assert!(a.learned_fingerprint.lock().unwrap().as_deref().is_some_and(|f| f.starts_with("SHA256:")));
    let b = pool.get(&p, None).await.expect("reuse");
    assert!(Arc::ptr_eq(&a, &b), "second open must reuse the transport");
    assert!(pool.has_live(p.id));
}

#[tokio::test]
async fn wrong_password_is_rejected() {
    if !enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let err = pool(&dir).get(&profile(), Some("nope".into())).await.err().expect("must fail");
    assert!(format!("{err:#}").contains("Authentication failed"), "{err:#}");
}

async fn wait_for(term: &TerminalHandle, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let text = term.shared.lock().unwrap().parser.screen().contents();
        if text.contains(needle) {
            return;
        }
        assert!(Instant::now() < deadline, "never saw {needle:?}:\n{text}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn shell_runs_commands_and_resizes() {
    if !enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let conn = pool(&dir).get(&profile(), Some(PASSWORD.into())).await.unwrap();
    let (term, rx) = TerminalHandle::new(24, 100);
    let task = tokio::spawn(run_shell(conn, term.clone(), rx, || {}));
    term.send(b"stty size; echo tether-$((6*7))\r".to_vec());
    wait_for(&term, "tether-42").await;
    wait_for(&term, "24 100").await;
    term.resize(30, 120);
    term.send(b"clear; stty size\r".to_vec());
    wait_for(&term, "30 120").await;
    term.close();
    tokio::time::timeout(Duration::from_secs(5), task).await.expect("shell must stop after close").unwrap().unwrap();
    assert!(matches!(term.shared.lock().unwrap().state, LinkState::Closed(_)));
}

async fn roundtrip(sftp: &Arc<Sftp>, data: &[u8], pipeline_kb: u32, tag: &str) -> (f64, f64) {
    let dir = tempfile::tempdir().unwrap();
    let up_src = dir.path().join("up.bin");
    let down_dst = dir.path().join("down.bin");
    std::fs::write(&up_src, data).unwrap();
    let remote = format!("/tmp/tether-e2e-{tag}.bin");

    let p = Arc::new(Progress::default());
    let t = Instant::now();
    transfer::run(sftp.clone(), Direction::Upload, up_src, remote.clone(), pipeline_kb, p.clone()).await.unwrap();
    let up = data.len() as f64 / t.elapsed().as_secs_f64();
    assert_eq!(p.done.load(Ordering::Relaxed), data.len() as u64);

    let p = Arc::new(Progress::default());
    let t = Instant::now();
    transfer::run(sftp.clone(), Direction::Download, down_dst.clone(), remote.clone(), pipeline_kb, p).await.unwrap();
    let down = data.len() as f64 / t.elapsed().as_secs_f64();

    assert!(std::fs::read(&down_dst).unwrap() == data, "downloaded bytes differ ({tag})");
    sftp.remove(&remote, false).await.unwrap();
    (up / 1e6, down / 1e6)
}

#[tokio::test]
async fn sftp_pipelined_roundtrip_is_byte_exact() {
    if !enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let conn = pool(&dir).get(&profile(), Some(PASSWORD.into())).await.unwrap();
    let sftp = Sftp::open(&conn).await.unwrap();
    println!("server chunk sizes: read {} / write {}", sftp.read_chunk, sftp.write_chunk);

    // Odd size exercises the final partial chunk.
    let data = payload(48 * 1024 * 1024 + 12_345);
    let (su, sd) = roundtrip(&sftp, &data, 16, "serial").await;
    let (pu, pd) = roundtrip(&sftp, &data, 4096, "pipelined").await;
    println!("pipeline 16 KB  : up {su:7.1} MB/s  down {sd:7.1} MB/s");
    println!("pipeline 4096 KB: up {pu:7.1} MB/s  down {pd:7.1} MB/s");

    // Empty file edge case.
    roundtrip(&sftp, &[], 2048, "empty").await;
}

#[tokio::test]
async fn sftp_directory_tree_transfer_and_ops() {
    if !enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let conn = pool(&dir).get(&profile(), Some(PASSWORD.into())).await.unwrap();
    let sftp = Sftp::open(&conn).await.unwrap();

    let local = dir.path().join("tree");
    std::fs::create_dir_all(local.join("a/b")).unwrap();
    for i in 0..40 {
        std::fs::write(local.join(format!("a/f{i}.txt")), format!("file {i}")).unwrap();
    }
    std::fs::write(local.join("a/b/deep.bin"), payload(300_000)).unwrap();

    let remote = "/tmp/tether-tree";
    let _ = sftp.remove(remote, true).await;
    let p = Arc::new(Progress::default());
    transfer::run(sftp.clone(), Direction::Upload, local.clone(), remote.into(), 2048, p.clone()).await.unwrap();
    assert_eq!(p.files_done.load(Ordering::Relaxed), 41);

    let listing = sftp.list("/tmp/tether-tree/a").await.unwrap();
    assert_eq!(listing.first().map(|e| (e.name.as_str(), e.is_dir)), Some(("b", true)), "dirs sort first");
    assert_eq!(listing.len(), 41);

    sftp.rename("/tmp/tether-tree/a/f0.txt", "/tmp/tether-tree/a/renamed.txt").await.unwrap();
    sftp.mkdir("/tmp/tether-tree/new").await.unwrap();

    let back = dir.path().join("back");
    transfer::run(sftp.clone(), Direction::Download, back.clone(), remote.into(), 2048, Arc::new(Progress::default())).await.unwrap();
    assert_eq!(std::fs::read(back.join("a/b/deep.bin")).unwrap(), payload(300_000));
    assert_eq!(std::fs::read_to_string(back.join("a/renamed.txt")).unwrap(), "file 0");
    assert!(back.join("new").is_dir());

    sftp.remove(remote, true).await.unwrap();
    assert!(sftp.stat(remote).await.is_err(), "recursive delete left files behind");
}

#[tokio::test]
async fn hostile_remote_names_are_refused_and_nothing_is_clobbered() {
    if !enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let conn = pool(&dir).get(&profile(), Some(PASSWORD.into())).await.unwrap();
    let sftp = Sftp::open(&conn).await.unwrap();
    let remote = "/tmp/tether-hostile";
    let _ = sftp.remove(remote, true).await;
    sftp.mkdir(remote).await.unwrap();
    // Legal on Linux, a path escape on Windows.
    sftp.raw.write_file_for_test(&format!("{remote}/..\\..\\escaped.txt")).await;

    let dest = dir.path().join("dl");
    let err = transfer::run(sftp.clone(), Direction::Download, dest.clone(), remote.into(), 512, Arc::new(Progress::default())).await.expect_err("must refuse");
    assert!(format!("{err:#}").contains("unsafe remote file name"), "{err:#}");
    assert!(!dir.path().join("escaped.txt").exists());

    // A failed download must not truncate an existing local file.
    let keep = dir.path().join("keep.txt");
    std::fs::write(&keep, b"precious").unwrap();
    let missing = transfer::run(sftp.clone(), Direction::Download, keep.clone(), "/tmp/does-not-exist".into(), 512, Arc::new(Progress::default())).await;
    assert!(missing.is_err());
    assert_eq!(std::fs::read(&keep).unwrap(), b"precious");
    sftp.remove(remote, true).await.unwrap();
}

trait WriteForTest {
    async fn write_file_for_test(&self, path: &str);
}

impl WriteForTest for russh_sftp::client::RawSftpSession {
    async fn write_file_for_test(&self, path: &str) {
        use russh_sftp::protocol::{FileAttributes, OpenFlags};
        let h = self.open(path, OpenFlags::CREATE | OpenFlags::WRITE, FileAttributes::empty()).await.unwrap().handle;
        self.write(h.as_str(), 0, b"pwned".to_vec()).await.unwrap();
        self.close(h).await.unwrap();
    }
}

/// Throughput across pipeline sizes. Add latency first to see why pipelining matters:
/// `docker exec tether-e2e tc qdisc add dev eth0 root netem delay 20ms`
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bench_pipeline_sizes() {
    if std::env::var_os("TETHER_BENCH").is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let conn = pool(&dir).get(&profile(), Some(PASSWORD.into())).await.unwrap();
    let sftp = Sftp::open(&conn).await.unwrap();
    let mb: usize = std::env::var("TETHER_BENCH_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(32);
    let data = payload(mb * 1024 * 1024);
    for kb in [16, 512, 2048, 4096, 8192] {
        let (u, d) = roundtrip(&sftp, &data, kb, &format!("bench{kb}")).await;
        println!("pipeline {kb:5} KB: up {u:7.1} MB/s  down {d:7.1} MB/s");
    }
}

#[tokio::test]
async fn finds_the_terminal_working_directory() {
    if !enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let conn = pool(&dir).get(&profile(), Some(PASSWORD.into())).await.unwrap();
    let (term, rx) = TerminalHandle::new(24, 100);
    let task = tokio::spawn(run_shell(conn.clone(), term.clone(), rx, || {}));
    term.send(b"cd /etc && echo moved-ok\r".to_vec());
    wait_for(&term, "moved-ok").await;
    let (age_secs, rows, cols) = term.probe_facts();
    let probe = tether::ssh::cwd::Probe { tag: &term.tag, age_secs, rows, cols };
    let cwd = tether::ssh::cwd::terminal_cwd(&conn, probe).await.expect("lookup");
    assert_eq!(cwd, "/etc");
    term.close();
    tokio::time::timeout(Duration::from_secs(5), task).await.expect("shell must stop after close").unwrap().unwrap();
}

#[tokio::test]
async fn reuses_the_transport_when_only_metadata_changed() {
    if !enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(&dir);
    let p = profile();
    let a = pool.get(&p, Some(PASSWORD.into())).await.expect("connect");
    let mut touched = p.clone();
    touched.last_used += 1;
    touched.name = "renamed".into();
    let b = pool.get(&touched, None).await.expect("reuse without a password");
    assert!(Arc::ptr_eq(&a, &b), "labels / last_used must not force a new handshake");
}

#[tokio::test]
async fn managed_key_installs_and_authenticates() {
    if !enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: e2e tests run with --test-threads=1.
    unsafe { std::env::set_var("TETHER_CONFIG_DIR", dir.path()) };
    let mut keys = tether::keystore::KeyStore::load(tether::keystore::default_dir()).unwrap();
    let key = tether::keystore::generate(tether::keystore::KeyKind::Ed25519, "e2e@tether").unwrap();
    let meta = keys.add_generated("e2e", key, Some("keypass")).unwrap();

    let conn = pool(&dir).get(&profile(), Some(PASSWORD.into())).await.unwrap();
    let (_, status) = tether::ssh::connection::exec(&conn, meta.install_command().unwrap(), 4096).await.unwrap();
    assert_eq!(status, Some(0));
    // Installing twice must not duplicate the line.
    let (_, status) = tether::ssh::connection::exec(&conn, meta.install_command().unwrap(), 4096).await.unwrap();
    assert_eq!(status, Some(0));
    let count = format!("grep -cF '{}' ~/.ssh/authorized_keys", meta.public_without_comment());
    let (out, _) = tether::ssh::connection::exec(&conn, count, 64).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&out).trim(), "1");

    let mut p = profile();
    p.auth = tether::profile::AuthMethod::Managed { key: meta.id };
    let bad = pool(&dir).get(&p, Some("wrong".into())).await.err().expect("wrong passphrase");
    assert!(bad.downcast_ref::<tether::keystore::BadPassphrase>().is_some(), "{bad:#}");
    let ok = pool(&dir).get(&p, Some("keypass".into())).await.expect("key auth");
    assert!(!ok.is_closed());
}

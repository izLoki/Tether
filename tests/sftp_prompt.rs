//! A real loopback SSH/SFTP transport with a virtual filesystem. No Docker, user
//! credentials, external hosts or files from the user's configuration are needed.
use russh::server::{Auth, ChannelOpenHandle, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{Attrs, File, FileAttributes, Handle, Name, Status, StatusCode, Version};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tether::ssh::{cwd, pool::Pool};

const HOME: &str = "/srv/custom homes/prisma";
const TARGET: &str = "/srv/custom homes/prisma/servers/practice-servers/na-practice-01";

#[derive(Clone)]
struct Server(Arc<Mutex<Vec<String>>>);
impl russh::server::Server for Server {
    type Handler = Ssh;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Ssh {
        Ssh { channels: HashMap::new(), seen: self.0.clone() }
    }
}
struct Ssh {
    channels: HashMap<ChannelId, Channel<Msg>>,
    seen: Arc<Mutex<Vec<String>>>,
}
impl russh::server::Handler for Ssh {
    type Error = anyhow::Error;
    async fn auth_password(&mut self, user: &str, password: &str) -> anyhow::Result<Auth> {
        assert_eq!(user, "root");
        assert_eq!(password, "fixture-only");
        Ok(Auth::Accept)
    }
    async fn channel_open_session(&mut self, channel: Channel<Msg>, reply: ChannelOpenHandle, _: &mut Session) -> anyhow::Result<()> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }
    async fn exec_request(&mut self, channel: ChannelId, command: &[u8], session: &mut Session) -> anyhow::Result<()> {
        // The home lookup must name the account printed in the prompt, not root.
        assert_eq!(std::str::from_utf8(command)?, cwd::user_home_command("prisma")?);
        self.seen.lock().unwrap().push("home:prisma".into());
        session.channel_success(channel)?;
        session.data(channel, format!("prisma:x:1001:1001::{HOME}:/bin/bash\n").into_bytes())?;
        session.exit_status_request(channel, 0)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }
    async fn subsystem_request(&mut self, channel: ChannelId, name: &str, session: &mut Session) -> anyhow::Result<()> {
        assert_eq!(name, "sftp");
        let channel_stream = self.channels.remove(&channel).unwrap().into_stream();
        let sftp = Files { read: HashSet::new(), seen: self.seen.clone() };
        session.channel_success(channel)?;
        tokio::spawn(async move {
            russh_sftp::server::run(channel_stream, sftp).await;
        });
        Ok(())
    }
}
struct Files {
    read: HashSet<String>,
    seen: Arc<Mutex<Vec<String>>>,
}
fn dir_attrs() -> FileAttributes {
    FileAttributes { permissions: Some(0o040755), ..FileAttributes::empty() }
}
fn exists(path: &str) -> bool {
    matches!(path, "/root" | TARGET | "/srv/second folder")
}
impl russh_sftp::server::Handler for Files {
    type Error = StatusCode;
    fn unimplemented(&self) -> StatusCode {
        StatusCode::OpUnsupported
    }
    async fn init(&mut self, _: u32, _: HashMap<String, String>) -> Result<Version, StatusCode> {
        Ok(Version::new())
    }
    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
        let path = if path == "." { "/root".into() } else { path };
        if !exists(&path) {
            return Err(StatusCode::NoSuchFile);
        }
        Ok(Name { id, files: vec![File::new(path, dir_attrs())] })
    }
    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        if !exists(&path) {
            return Err(StatusCode::NoSuchFile);
        }
        Ok(Attrs { id, attrs: dir_attrs() })
    }
    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusCode> {
        if !exists(&path) {
            return Err(StatusCode::NoSuchFile);
        }
        self.seen.lock().unwrap().push(path.clone());
        self.read.remove(&path);
        Ok(Handle { id, handle: path })
    }
    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, StatusCode> {
        if !self.read.insert(handle.clone()) {
            return Err(StatusCode::Eof);
        }
        let marker = if handle == TARGET { "clicked-directory-marker" } else { "other-directory" };
        Ok(Name { id, files: vec![File::new(marker, dir_attrs())] })
    }
    async fn close(&mut self, id: u32, _: String) -> Result<Status, StatusCode> {
        Ok(Status { id, status_code: StatusCode::Ok, error_message: String::new(), language_tag: String::new() })
    }
}

#[test]
fn root_connection_browses_the_switched_users_prompt_directory() {
    let config = tempfile::tempdir().unwrap();
    // One test owns this binary; its history and settings never touch the user's config.
    unsafe {
        std::env::set_var("TETHER_CONFIG_DIR", config.path());
    }
    tether::settings::init(config.path().join("settings.json"));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let listener = rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = listener.local_addr().unwrap().port();
    let key = tether::keystore::generate(tether::keystore::KeyKind::Ed25519, "loopback test").unwrap();
    let server_config = Arc::new(russh::server::Config { keys: vec![key], ..Default::default() });
    let server_seen = seen.clone();
    let server = rt.spawn(async move {
        Server(server_seen).run_on_socket(server_config, &listener).await.unwrap();
    });
    let profile = tether::profile::Profile::parse_quick(&format!("root@127.0.0.1:{port}")).unwrap();
    let pool = Pool::new(tether::known_hosts::KnownHosts::load(config.path().join("known_hosts.json")));
    rt.block_on(pool.get(&profile, Some("fixture-only".into()))).unwrap();
    let prompt = "prisma@na-prisma-02:~/servers/practice-servers/na-practice-01$";
    let (_, target, _) = tether::ui::term_links::find_link_in_str(prompt, prompt.find("servers").unwrap(), None, false, &mut Default::default()).unwrap();
    let tether::ui::term_links::LinkTarget::RemotePath { path, .. } = target else { panic!("not a remote path") };
    let request = Arc::new(Mutex::new(Some(path)));
    let requests = request.clone();
    let runtime = rt.handle().clone();
    let mut view = None;
    let mut h = egui_kittest::Harness::builder().with_size(eframe::egui::vec2(1000.0, 650.0)).build_ui(move |ui| {
        let view = view.get_or_insert_with(|| {
            let view = tether::ui::sftp_view::SftpView::new(profile.clone(), runtime.clone(), ui.ctx().clone());
            view.connect(pool.clone(), None, Default::default());
            view
        });
        if let Some(path) = requests.lock().unwrap().take() {
            view.browse_path(path);
        }
        view.ui(ui);
    });
    for expected in [TARGET, "/srv/second folder"] {
        let until = Instant::now() + Duration::from_secs(15);
        loop {
            h.run_steps(3);
            if seen.lock().unwrap().iter().any(|p| p == expected) {
                break;
            }
            assert!(Instant::now() < until, "SFTP didn't navigate to {expected}; requests: {:?}", seen.lock().unwrap());
            std::thread::sleep(Duration::from_millis(20));
        }
        *request.lock().unwrap() = Some("/srv/second folder".into());
    }
    assert!(seen.lock().unwrap().iter().any(|p| p == "home:prisma"));
    assert!(!seen.lock().unwrap().iter().any(|p| p.starts_with("/root/servers")));
    drop(h);
    server.abort();
    rt.shutdown_timeout(Duration::from_secs(1));
}

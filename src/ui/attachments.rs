//! Image attachments shared by clipboard, file picker and drop. The terminal receives
//! a real accessible image path; the UI retains a colored label and a small preview.
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use crate::profile::Profile;
use crate::ssh::pool::Pool;
use crate::theme;
use anyhow::{Context, Result, ensure};
use eframe::egui::{self, RichText};

pub struct RemoteTarget {
    pub pool: Pool,
    pub profile: Profile,
    pub runtime: tokio::runtime::Handle,
}

struct Attachment {
    number: usize,
    texture: egui::TextureHandle,
}

struct Prepared {
    pixels: image::RgbaImage,
    local: PathBuf,
    path: String,
}

#[derive(Default)]
pub struct Attachments {
    pub remote: Option<RemoteTarget>,
    items: Vec<Attachment>,
    pending: Vec<Receiver<Result<Prepared>>>,
    pub notice: Option<String>,
    next: usize,
    cache: Option<std::sync::Arc<tempfile::TempDir>>,
}

const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

pub fn is_image(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()).is_some_and(|s| ["png", "jpg", "jpeg"].iter().any(|e| s.eq_ignore_ascii_case(e)))
}

fn snapshot(path: &Path, dir: &Path) -> Result<PathBuf> {
    ensure!(std::fs::metadata(path)?.len() <= MAX_IMAGE_BYTES, "Image exceeds 20 MiB");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("png");
    let dest = dir.join(format!("{}.{}", uuid::Uuid::new_v4(), ext));
    std::fs::copy(path, &dest)?;
    Ok(dest)
}

fn preview(path: &Path) -> Result<image::RgbaImage> {
    let mut reader = image::ImageReader::open(path)?.with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader.decode()?.thumbnail(240, 160).to_rgba8())
}

impl Attachments {
    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }
    fn cache(&mut self) -> Result<std::sync::Arc<tempfile::TempDir>> {
        if self.cache.is_none() {
            self.cache = Some(std::sync::Arc::new(tempfile::Builder::new().prefix("tether-images-").tempdir()?));
        }
        Ok(self.cache.as_ref().unwrap().clone())
    }
    pub fn add(&mut self, paths: Vec<PathBuf>, ctx: &egui::Context) {
        for path in paths.into_iter().filter(|p| is_image(p)).take(16) {
            let cache = match self.cache() {
                Ok(cache) => cache,
                Err(e) => {
                    self.notice = Some(e.to_string());
                    break;
                }
            };
            if self.pending.len() + self.items.len() >= 32 {
                self.notice = Some("Up to 32 image previews per terminal. Clear previews to add more.".into());
                break;
            }
            let (tx, rx) = mpsc::channel();
            self.pending.push(rx);
            let ctx = ctx.clone();
            let remote = self.remote.as_ref().map(|r| (r.pool.clone(), r.profile.clone(), r.runtime.clone()));
            // Decode/copy off the UI thread; uploads use the existing SSH transport.
            std::thread::spawn(move || {
                let result = snapshot(&path, cache.path()).and_then(|local| {
                    let pixels = preview(&local)?;
                    let inserted = if let Some((pool, profile, runtime)) = remote {
                        runtime.block_on(upload(&pool, &profile, &local))?
                    } else {
                        local.display().to_string()
                    };
                    Ok(Prepared { pixels, local, path: inserted })
                });
                let _ = tx.send(result);
                ctx.request_repaint();
            });
        }
    }

    pub fn paste_image(&mut self, ctx: &egui::Context) -> bool {
        let result = (|| -> Result<PathBuf> {
            let mut clipboard = arboard::Clipboard::new()?;
            let data = clipboard.get_image()?;
            ensure!(data.bytes.len() as u64 <= 128 * 1024 * 1024, "Clipboard image is too large");
            let image = image::RgbaImage::from_raw(u32::try_from(data.width)?, u32::try_from(data.height)?, data.bytes.into_owned())
                .context("Invalid clipboard image")?;
            let dir = self.cache()?;
            let path = dir.path().join(format!("{}.png", uuid::Uuid::new_v4()));
            image.save(&path)?;
            Ok(path)
        })();
        match result {
            Ok(path) => {
                self.add(vec![path], ctx);
                true
            }
            Err(e) => {
                self.notice = Some(format!("Could not paste image: {e:#}"));
                false
            }
        }
    }

    /// Returns paths only after successful preparation/upload, never a local path to SSH.
    pub fn poll(&mut self, ctx: &egui::Context) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let mut i = 0;
        while i < self.pending.len() {
            let result = match self.pending[i].try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => {
                    i += 1;
                    continue;
                }
                Err(_) => Err(anyhow::anyhow!("Image worker stopped")),
            };
            self.pending.remove(i);
            match result {
                Ok(Prepared { pixels, local, path }) => {
                    self.next += 1;
                    let size = [pixels.width() as usize, pixels.height() as usize];
                    let image = egui::ColorImage::from_rgba_unmultiplied(size, pixels.as_raw());
                    let texture = ctx.load_texture(format!("attachment-{}", local.display()), image, egui::TextureOptions::LINEAR);
                    self.items.push(Attachment { number: self.next, texture });
                    paths.push(PathBuf::from(path));
                }
                Err(e) => self.notice = Some(format!("Image attachment failed: {e:#}")),
            }
        }
        if !self.pending.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        } else if !paths.is_empty() {
            self.notice = None;
        }
        paths
    }

    pub fn bar(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
            ui.painter().rect_filled(rect, egui::CornerRadius::same(6), theme::surface());
            if ui.button("+ Image").on_hover_text("Attach PNG or JPEG · You can also paste or drop an image").clicked() {
                if let Some(paths) = rfd::FileDialog::new().set_title("Attach images").add_filter("Images", &["png", "jpg", "jpeg"]).pick_files() {
                    self.add(paths, ui.ctx());
                }
            }
            if !self.pending.is_empty() {
                ui.spinner();
            }
            egui::ScrollArea::horizontal().id_salt("image_previews").show(ui, |ui| {
                ui.horizontal(|ui| {
                    for item in &self.items {
                        let text = RichText::new(format!("[Image {}]", item.number)).color(theme::accent()).strong();
                        ui.add(egui::Button::new(text).fill(theme::accent_soft())).on_hover_ui(|ui| {
                            ui.set_max_width(240.0);
                            ui.image((item.texture.id(), item.texture.size_vec2()));
                        });
                    }
                    if !self.items.is_empty()
                        && ui.small_button("Clear previews").on_hover_text("Hide previews; paths already inserted remain in the terminal").clicked()
                    {
                        self.items.clear();
                    }
                    if let Some(notice) = &self.notice {
                        ui.label(RichText::new(notice).small().color(theme::warn()));
                    }
                });
            });
        });
    }
}

async fn upload(pool: &Pool, profile: &Profile, local: &Path) -> Result<String> {
    use russh_sftp::protocol::{FileAttributes, OpenFlags};
    let conn = pool.get(profile, crate::profile::secret::get(profile.secret_id())).await?;
    let sftp = crate::ssh::sftp::Sftp::open(&conn).await?;
    let home = sftp.canonicalize(".").await?;
    let dir = crate::ssh::sftp::join(&home, &format!(".tether-image-{}", uuid::Uuid::new_v4()));
    sftp.raw.mkdir(&dir, FileAttributes { permissions: Some(0o700), ..FileAttributes::empty() }).await?;
    let remote = crate::ssh::sftp::join(&dir, local.file_name().unwrap_or_default().to_string_lossy().as_ref());
    let handle = sftp
        .raw
        .open(&remote, OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE, FileAttributes { permissions: Some(0o600), ..FileAttributes::empty() })
        .await?
        .handle;
    let bytes = tokio::fs::read(local).await?;
    let result = async {
        for (i, chunk) in bytes.chunks(sftp.write_chunk as usize).enumerate() {
            sftp.raw.write(&handle, i as u64 * u64::from(sftp.write_chunk), chunk.to_vec()).await?;
        }
        anyhow::Ok(())
    }
    .await;
    let closed = sftp.raw.close(handle).await;
    result?;
    closed?;
    Ok(remote)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(dir: &Path) -> PathBuf {
        let path = dir.join("reference.png");
        image::RgbaImage::from_fn(480, 320, |x, y| image::Rgba([(x % 255) as u8, (y % 255) as u8, 120, 255])).save(&path).unwrap();
        path
    }

    fn finish(images: &mut Attachments, ctx: &egui::Context) -> Vec<PathBuf> {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut paths = Vec::new();
        while images.is_pending() {
            paths.extend(images.poll(ctx));
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        paths
    }

    #[test]
    fn attachment_preserves_pixels_and_cleans_local_files_when_terminal_closes() {
        let dir = tempfile::tempdir().unwrap();
        let source = fixture(dir.path());
        let mut images = Attachments::default();
        let ctx = egui::Context::default();
        images.add(vec![source.clone()], &ctx);
        let ready = finish(&mut images, &ctx);
        assert_eq!(ready.len(), 1);
        assert_eq!(std::fs::read(&source).unwrap(), std::fs::read(&ready[0]).unwrap());
        assert_eq!(images.items[0].texture.size(), [240, 160]);
        drop(images);
        assert!(!ready[0].exists());
    }

    #[test]
    fn corrupt_image_never_reaches_the_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("bad.png");
        std::fs::write(&source, "not a png").unwrap();
        let ctx = egui::Context::default();
        let mut images = Attachments::default();
        images.add(vec![source], &ctx);
        assert!(finish(&mut images, &ctx).is_empty());
        assert!(images.items.is_empty());
        assert!(images.notice.as_deref().unwrap().contains("failed"));
    }

    #[tokio::test]
    #[ignore = "requires TETHER_IMAGE_E2E_TARGET pointing at an isolated SFTP test server"]
    async fn remote_image_roundtrip_and_permissions() {
        let target = std::env::var("TETHER_IMAGE_E2E_TARGET").expect("test server target");
        let profile = Profile::parse_quick(&target).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::new(crate::known_hosts::KnownHosts::load(dir.path().join("known-hosts.json")));
        let conn = pool.get(&profile, Some("tetherpass".into())).await.unwrap();
        let local = fixture(dir.path());
        let remote = upload(&pool, &profile, &local).await.unwrap();
        assert!(remote.contains("/.tether-image-"));
        assert!(!remote.contains("Temp"));
        let sftp = crate::ssh::sftp::Sftp::open(&conn).await.unwrap();
        let bytes = crate::remote_edit::read_capped(&sftp, &remote, MAX_IMAGE_BYTES).await.unwrap();
        assert_eq!(bytes, std::fs::read(local).unwrap());
        assert_eq!(sftp.stat(&remote).await.unwrap().permissions.unwrap() & 0o777, 0o600);
        let parent = crate::ssh::sftp::parent(&remote);
        assert_eq!(sftp.stat(&parent).await.unwrap().permissions.unwrap() & 0o777, 0o700);
        sftp.raw.remove(&remote).await.unwrap();
        sftp.raw.rmdir(parent).await.unwrap();
    }

    #[test]
    #[ignore = "renders attachment and hover preview with a GPU"]
    fn image_preview_screenshot() {
        use egui_kittest::kittest::Queryable;
        let dir = tempfile::tempdir().unwrap();
        let source = fixture(dir.path());
        let mut images = Attachments::default();
        let ctx = egui::Context::default();
        images.add(vec![source], &ctx);
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(760.0, 320.0)).wgpu().build_ui(move |ui| {
            ui.ctx().all_styles_mut(|s| s.interaction.tooltip_delay = 0.0);
            images.poll(ui.ctx());
            ui.heading("Image attachments");
            ui.label("Paste, drop or select an image. Hover its label to preview it.");
            let rect = egui::Rect::from_min_size(egui::pos2(10.0, 260.0), egui::vec2(740.0, 34.0));
            images.bar(ui, rect);
        });
        for _ in 0..60 {
            harness.run_steps(2);
            if harness.query_by_label("[Image 1]").is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        harness.get_by_label("[Image 1]").hover();
        harness.run_steps(15);
        let output = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/ui-shots");
        std::fs::create_dir_all(&output).unwrap();
        harness.render().unwrap().save(output.join("general-images.png")).unwrap();
    }
}

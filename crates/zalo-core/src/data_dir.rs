//! Cross-platform user data directory resolution and atomic private file persistence.
//!
//! Unlike `ctdozlite` (which defaulted to `cwd` on Windows and failed inside `Program Files`),
//! `litechat` always resolves Windows data to `%LOCALAPPDATA%\litechat` unless overridden.

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct StoragePaths {
    pub root: PathBuf,
}

impl StoragePaths {
    /// Resolve the default OS-appropriate writable directory for `litechat`.
    pub fn resolve_default(profile: Option<&str>) -> Result<Self> {
        let base = default_app_data_dir()?;
        let root = match profile {
            Some(raw) => {
                let safe = sanitize_profile_name(raw)?;
                base.join("profiles").join(safe)
            }
            None => base,
        };
        ensure_private_dir(&root)?;
        Ok(Self { root })
    }

    /// Resolve an explicit directory path (used by `--data-dir` and tests).
    pub fn from_dir(dir: impl AsRef<Path>) -> Result<Self> {
        let root = dir.as_ref().to_path_buf();
        ensure_private_dir(&root)?;
        Ok(Self { root })
    }

    pub fn session_file(&self) -> PathBuf {
        self.root.join("session.json")
    }

    pub fn translate_file(&self) -> PathBuf {
        self.root.join("translate.json")
    }

    pub fn history_db(&self, uin: Option<&str>) -> PathBuf {
        match uin.filter(|u| !u.trim().is_empty()) {
            Some(id) => {
                let clean: String = id.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
                if clean.is_empty() {
                    self.root.join("history.db")
                } else {
                    self.root.join("accounts").join(format!("{clean}_history.db"))
                }
            }
            None => self.root.join("history.db"),
        }
    }

    pub fn sticker_cache_dir(&self) -> PathBuf {
        self.root.join("stickers_cache")
    }
}

pub fn sanitize_profile_name(raw: &str) -> Result<String> {
    let safe: String = raw
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if safe.is_empty() {
        bail!("Tên profile chỉ chấp nhận chữ, số, '-' hoặc '_'");
    }
    Ok(safe)
}

fn default_app_data_dir() -> Result<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let p = PathBuf::from(local);
            if !p.as_os_str().is_empty() {
                return Ok(p.join("litechat"));
            }
        }
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            return Ok(PathBuf::from(profile).join("AppData/Local/litechat"));
        }
        bail!("Không xác định được thư mục %LOCALAPPDATA% trên Windows");
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").context("Thiếu biến môi trường HOME")?;
        return Ok(PathBuf::from(home).join("Library/Application Support/litechat"));
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            let p = PathBuf::from(xdg);
            if !p.as_os_str().is_empty() {
                return Ok(p.join("litechat"));
            }
        }
        let home = std::env::var_os("HOME").context("Thiếu biến môi trường HOME")?;
        Ok(PathBuf::from(home).join(".local/share/litechat"))
    }
}

pub fn ensure_private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .with_context(|| format!("Không tạo được thư mục {}", path.display()))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
            .with_context(|| format!("Không tạo được thư mục {}", path.display()))?;
    }
    Ok(())
}

/// Write bytes atomically via a sibling temp file and rename.
pub fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp_name = format!(
        ".tmp-{}-{}-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id(),
        NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed)
    );
    let tmp_path = parent.join(tmp_name);

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }

    let mut f = opts
        .open(&tmp_path)
        .with_context(|| format!("Không tạo được tệp tạm {}", tmp_path.display()))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);

    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err).with_context(|| format!("Không ghi đè được {}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_default_outside_program_files() {
        let paths = StoragePaths::resolve_default(Some("work_1")).unwrap();
        let s = paths.root.to_string_lossy().to_lowercase();
        assert!(!s.contains("program files"));
        assert!(s.contains("litechat"));
        assert!(s.contains("work_1"));
    }

    #[test]
    fn atomic_write_replaces_existing_file() {
        let dir = std::env::temp_dir().join(format!("litechat-dir-test-{}", std::process::id()));
        let paths = StoragePaths::from_dir(&dir).unwrap();
        let f = paths.session_file();
        write_private_atomic(&f, b"first").unwrap();
        write_private_atomic(&f, b"second").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"second");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

use std::path::{Path, PathBuf};

/// Resolves the directory holding the database, message blobs and secrets.
///
/// `MAILBOX_DATA_DIR` wins; otherwise `~/Library/Application Support/Mailbox AI` on macOS and
/// `$XDG_DATA_HOME/mailbox` elsewhere.
pub fn default_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("MAILBOX_DATA_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Mailbox AI")
    } else if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        PathBuf::from(xdg).join("mailbox-ai")
    } else {
        home.join(".local/share/mailbox")
    }
}

#[derive(Clone, Debug)]
pub struct Paths {
    pub root: PathBuf,
}

impl Paths {
    pub fn new(root: impl AsRef<Path>) -> std::io::Result<Paths> {
        let root = root.as_ref().to_path_buf();
        for sub in ["blobs", "attachments", "uploads", "logs", "tmp"] {
            std::fs::create_dir_all(root.join(sub))?;
        }
        restrict(&root)?;
        Ok(Paths { root })
    }

    pub fn db(&self) -> PathBuf {
        self.root.join("mailbox.db")
    }

    pub fn daemon_info(&self) -> PathBuf {
        self.root.join("daemon.json")
    }

    pub fn secrets(&self) -> PathBuf {
        self.root.join("secrets.bin")
    }

    pub fn master_key_file(&self) -> PathBuf {
        self.root.join("master.key")
    }

    pub fn blob(&self, hash: &str) -> PathBuf {
        let (a, b) = hash.split_at(2.min(hash.len()));
        self.root.join("blobs").join(a).join(format!("{b}.eml"))
    }

    pub fn tmp(&self) -> PathBuf {
        self.root.join("tmp")
    }

    pub fn attachments(&self) -> PathBuf {
        self.root.join("attachments")
    }

    pub fn uploads(&self) -> PathBuf {
        self.root.join("uploads")
    }
}

/// Makes a file or directory readable by the owner only.
pub fn restrict(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if path.is_dir() { 0o700 } else { 0o600 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// Writes a file atomically with owner-only permissions.
pub fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    restrict(&tmp)?;
    std::fs::rename(&tmp, path)
}

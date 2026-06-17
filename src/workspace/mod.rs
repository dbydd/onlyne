use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
    onlyne: PathBuf,
}

impl Workspace {
    pub fn resolve(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            onlyne: root.join(".onlyne"),
            root,
        }
    }

    pub fn current() -> anyhow::Result<Self> {
        Ok(Self::resolve(std::env::current_dir()?))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn dir(&self) -> &Path {
        &self.onlyne
    }
    pub fn config_path(&self) -> PathBuf {
        self.onlyne.join("config.toml")
    }
    pub fn dotenv_path(&self) -> PathBuf {
        self.onlyne.join(".env")
    }
    pub fn db_path(&self) -> PathBuf {
        self.onlyne.join("state.db")
    }
    pub fn socket_path(&self) -> PathBuf {
        self.onlyne.join("run/onlyne.sock")
    }
    pub fn log_path(&self) -> PathBuf {
        self.onlyne.join("logs/daemon.log")
    }
    pub fn media_dir(&self) -> PathBuf {
        self.onlyne.join("cache/media")
    }
    pub fn adapter_dir(&self) -> PathBuf {
        self.onlyne.join("adapters")
    }

    pub fn bootstrap(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(self.onlyne.join("run"))?;
        std::fs::create_dir_all(self.onlyne.join("logs"))?;
        std::fs::create_dir_all(self.media_dir())?;
        std::fs::create_dir_all(self.adapter_dir())?;
        if !self.config_path().exists() {
            std::fs::write(self.config_path(), crate::config::DEFAULT_CONFIG)?;
        }
        if !self.dotenv_path().exists() {
            std::fs::write(self.dotenv_path(), crate::config::DEFAULT_DOTENV)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workspace_paths_are_local() {
        let ws = Workspace::resolve(PathBuf::from("/tmp/x"));
        assert_eq!(
            ws.config_path(),
            PathBuf::from("/tmp/x/.onlyne/config.toml")
        );
        assert_eq!(
            ws.socket_path(),
            PathBuf::from("/tmp/x/.onlyne/run/onlyne.sock")
        );
    }
    #[test]
    fn bootstrap_creates_layout() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::resolve(dir.path());
        ws.bootstrap().unwrap();
        assert!(ws.config_path().exists());
        assert!(ws.db_path().parent().unwrap().exists());
        assert!(ws.media_dir().exists());
    }
}

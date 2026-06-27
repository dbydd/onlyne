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

    pub fn discover(start: impl AsRef<Path>) -> anyhow::Result<Self> {
        let start = start.as_ref().to_path_buf();
        for dir in start.ancestors() {
            if dir.join(".onlyne").is_dir() {
                return Ok(Self::resolve(dir));
            }
        }
        Ok(Self::resolve(start))
    }

    pub fn current() -> anyhow::Result<Self> {
        Self::discover(std::env::current_dir()?)
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
    pub fn rendered_dir(&self) -> PathBuf {
        self.onlyne.join("cache/rendered")
    }
    pub fn adapter_dir(&self) -> PathBuf {
        self.onlyne.join("adapters")
    }
    pub fn channels_dir(&self) -> PathBuf {
        self.onlyne.join("channels")
    }
    pub fn channel_dir(&self, channel: &str) -> PathBuf {
        self.channels_dir().join(channel)
    }

    pub fn bootstrap(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(self.onlyne.join("run"))?;
        std::fs::create_dir_all(self.onlyne.join("logs"))?;
        std::fs::create_dir_all(self.media_dir())?;
        std::fs::create_dir_all(self.rendered_dir())?;
        std::fs::create_dir_all(self.adapter_dir())?;
        std::fs::create_dir_all(self.channels_dir())?;
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

    #[test]
    fn discovers_nearest_parent_onlyne() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let child = root.join("a/b");
        std::fs::create_dir_all(child.clone()).unwrap();
        Workspace::resolve(&root).bootstrap().unwrap();

        let ws = Workspace::discover(&child).unwrap();

        assert_eq!(ws.root(), root.as_path());
    }

    #[test]
    fn discover_falls_back_to_start_when_no_onlyne_exists() {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("a/b");
        std::fs::create_dir_all(child.clone()).unwrap();

        let ws = Workspace::discover(&child).unwrap();

        assert_eq!(ws.root(), child.as_path());
    }
}

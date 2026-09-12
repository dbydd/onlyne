pub mod error;
pub mod markdown;
pub mod media;
pub mod onboarding;

pub use error::KitError;

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    fn walk_rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in fs::read_dir(dir).expect("read kit dir") {
            let path = entry.expect("kit dir entry").path();
            if path.is_dir() {
                walk_rs_files(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn kit_sources_keep_workspace_and_platform_dependency_firewall() {
        let kit_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/kit");
        let mut files = Vec::new();
        walk_rs_files(&kit_dir, &mut files);
        let banned = [
            ["onlyne", "_proto"].concat(),
            ["onlyne", "_adapter"].concat(),
            ["onlyne", "_store"].concat(),
            ["tel", "oxide"].concat(),
            ["open", "lark"].concat(),
            ["wechat", "_ilink"].concat(),
            ["tokio", "_tungstenite"].concat(),
        ];
        for file in files {
            let text = fs::read_to_string(&file).expect("read kit source");
            for needle in &banned {
                assert!(
                    !text.contains(needle),
                    "{} contains forbidden dependency marker {needle}",
                    file.display()
                );
            }
        }
    }
}

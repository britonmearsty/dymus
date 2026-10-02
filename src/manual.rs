//! Bundled offline manual, also available to Cargo installations.
use anyhow::{Context, Result};
use std::{
    env,
    io::{self, Write},
    path::{Path, PathBuf},
};

pub fn render() -> String {
    include_str!("../docs/dymus.1").replace("@VERSION@", env!("CARGO_PKG_VERSION"))
}

pub fn run(install: bool, directory: Option<&Path>) -> Result<()> {
    if !install {
        io::stdout().write_all(render().as_bytes())?;
        return Ok(());
    }
    let root = match directory {
        Some(directory) => directory.to_owned(),
        None => user_data_dir()?.join("man"),
    };
    let path = install_to(&root)?;
    println!("Installed {}", path.display());
    println!(
        "Read it with `man dymus` (or `man -l {}` if your man search path does not include this directory).",
        path.display()
    );
    Ok(())
}

fn user_data_dir() -> Result<PathBuf> {
    if let Some(path) = env::var_os("XDG_DATA_HOME").filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    Ok(PathBuf::from(
        env::var_os("HOME").context("Cannot locate your data directory; pass --directory")?,
    )
    .join(".local/share"))
}

fn install_to(root: &Path) -> Result<PathBuf> {
    let directory = root.join("man1");
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("Cannot create {}", directory.display()))?;
    let path = directory.join("dymus.1");
    let mut file = tempfile::NamedTempFile::new_in(&directory)?;
    file.write_all(render().as_bytes())?;
    use std::os::unix::fs::PermissionsExt;
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o644))?;
    file.persist(&path)
        .with_context(|| format!("Cannot install {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_bundled_manual_atomically_into_section_one() {
        let root = tempfile::tempdir().unwrap();
        let path = install_to(root.path()).unwrap();
        assert_eq!(path, root.path().join("man1/dymus.1"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(".TH DYMUS 1"));
        assert!(text.contains(env!("CARGO_PKG_VERSION")));
        assert!(!text.contains("@VERSION@"));
        std::fs::write(&path, "old version").unwrap();
        install_to(root.path()).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), text);
    }
}

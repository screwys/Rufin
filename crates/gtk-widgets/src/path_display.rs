use std::path::{Component, Path, Prefix};

/// Format a native path for a label, without changing the path used for I/O.
pub fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    match path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::VerbatimDisk(_) => text.replacen(r"\\?\", "", 1),
            Prefix::VerbatimUNC(_, _) => text.replacen(r"\\?\UNC\", r"\\", 1),
            _ => text.into_owned(),
        },
        _ => text.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_native_paths() {
        let path = Path::new("Music").join("Björk");
        assert_eq!(display_path(&path), path.to_string_lossy());
    }

    #[cfg(windows)]
    #[test]
    fn displays_extended_drive_and_network_paths() {
        for (input, expected) in [
            (r"\\?\C:\Users\ad\Downloads", r"C:\Users\ad\Downloads"),
            (r"\\?\C:\", r"C:\"),
            (r"\\?\UNC\server\share\Music", r"\\server\share\Music"),
            (r"\\?\Volume{example}\Music", r"\\?\Volume{example}\Music"),
        ] {
            assert_eq!(display_path(Path::new(input)), expected);
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn preserves_windows_like_names_on_other_platforms() {
        let path = Path::new(r"\\?\C:\Music");
        assert_eq!(display_path(path), path.to_string_lossy());
    }
}

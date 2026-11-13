use std::fs;
use std::path::{Path, PathBuf};

/// Icons only Android draws, and the notes beside the icons.
const NOT_DESKTOP_ICONS: [&str; 3] = [
    "source-icons.md",
    "hicolor/scalable/actions/rufin-dots-horizontal-symbolic.svg",
    "hicolor/scalable/actions/rufin-selection-mode-symbolic.svg",
];

fn main() {
    // Embed the checkout path so Cargo rebuilds this script when worktrees share a target directory.
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let resources = manifest_dir.join("resources");
    let icons = manifest_dir.join("../../resources/icons");
    for path in [&resources, &icons] {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let mut interfaces = Vec::new();
    collect(&resources, &resources, &mut interfaces);
    let mut icon_files = Vec::new();
    collect(&icons, &icons, &mut icon_files);
    icon_files.retain(|path| !NOT_DESKTOP_ICONS.contains(&path.as_str()));

    let mut manifest = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gresources>\n  \
         <gresource prefix=\"/io/github/screwys/Rufin/icons\">\n",
    );
    for icon in &icon_files {
        if icon.ends_with("-symbolic.svg") {
            let svg = fs::read_to_string(icons.join(icon)).expect("read Rufin symbolic icon");
            assert!(
                !svg.contains("<image") && !svg.contains("data:image"),
                "symbolic icon embeds raster artwork: {icon}"
            );
        }
        manifest.push_str(&format!("    <file>{icon}</file>\n"));
    }
    manifest.push_str("  </gresource>\n  <gresource prefix=\"/io/github/screwys/Rufin/ui\">\n");
    for interface in &interfaces {
        if interface.ends_with(".ui") {
            let definition = fs::read_to_string(resources.join(interface))
                .unwrap_or_else(|_| panic!("read interface resource: {interface}"));
            assert!(
                !definition.contains("<template")
                    || !definition.lines().any(|line| line.starts_with("  <object")),
                "interface resource mixes Builder objects and a template: {interface}"
            );
        }
        manifest.push_str(&format!(
            "    <file compressed=\"true\">{interface}</file>\n"
        ));
    }
    manifest.push_str("  </gresource>\n</gresources>\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let manifest_path = out.join("rufin.gresource.xml");
    // compile_resources makes Cargo watch this file, so rewriting it would rerun this script on every build.
    if fs::read_to_string(&manifest_path).ok().as_deref() != Some(manifest.as_str()) {
        fs::write(&manifest_path, manifest).expect("write the resource manifest");
    }
    glib_build_tools::compile_resources(
        &[resources.as_path(), icons.as_path()],
        manifest_path.to_str().expect("UTF-8 OUT_DIR"),
        "rufin.gresource",
    );
}

/// Every file below `directory`, relative to `root`, in a stable order.
fn collect(root: &Path, directory: &Path, files: &mut Vec<String>) {
    let mut entries = fs::read_dir(directory)
        .expect("read a Rufin resource directory")
        .map(|entry| entry.expect("read a Rufin resource entry").path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect(root, &path, files);
        } else {
            files.push(
                path.strip_prefix(root)
                    .expect("resource below its root")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}

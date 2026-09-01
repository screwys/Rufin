use std::fs;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=resources");
    println!("cargo:rerun-if-changed=resources/rufin.gresource.xml");
    println!("cargo:rerun-if-changed=../../data/icons");
    verify_ui_resources();
    verify_symbolic_icons(Path::new("../../data/icons"));
    glib_build_tools::compile_resources(
        &["../../data/icons", "resources"],
        "resources/rufin.gresource.xml",
        "rufin.gresource",
    );
}

fn verify_symbolic_icons(directory: &Path) {
    for entry in fs::read_dir(directory).expect("read Rufin icon directory") {
        let path = entry.expect("read Rufin icon entry").path();
        if path.is_dir() {
            verify_symbolic_icons(&path);
            continue;
        }
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("-symbolic.svg"))
        {
            continue;
        }
        let svg = fs::read_to_string(&path).expect("read Rufin symbolic icon");
        assert!(
            !svg.contains("<image") && !svg.contains("data:image"),
            "symbolic icon embeds raster artwork: {}",
            path.display()
        );
    }
}

fn verify_ui_resources() {
    let root = Path::new("resources");
    let manifest = fs::read_to_string("resources/rufin.gresource.xml")
        .expect("read Rufin interface resource manifest");
    let mut sources = Vec::new();
    collect_ui_sources(root, root, &mut sources);
    sources.sort();
    for source in sources {
        let definition = fs::read_to_string(root.join(&source))
            .unwrap_or_else(|_| panic!("read interface resource: {source}"));
        assert!(
            !definition.contains("<template")
                || !definition.lines().any(|line| line.starts_with("  <object")),
            "interface resource mixes Builder objects and a template: {source}"
        );
        let entry = format!("<file compressed=\"true\">{source}</file>");
        assert!(
            manifest.contains(&entry),
            "interface resource is missing from the manifest: {source}"
        );
    }
}

fn collect_ui_sources(root: &Path, directory: &Path, sources: &mut Vec<String>) {
    for entry in fs::read_dir(directory).expect("read Rufin interface resource directory") {
        let path = entry.expect("read Rufin interface resource entry").path();
        if path.is_dir() {
            collect_ui_sources(root, &path, sources);
        } else if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("ui" | "css")
        ) {
            sources.push(
                path.strip_prefix(root)
                    .expect("interface resource below its root")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}

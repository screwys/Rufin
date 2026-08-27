use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{self, Command},
};

#[test]
fn i18n_files_omit_source_references() {
    let root = repo_root();
    let mut files = po_files(&root.join("locales"));
    files.push(root.join("locales/rufin.pot"));
    for file in files {
        let content = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
        assert!(
            !content.lines().any(|line| line.starts_with("#:")),
            "{} should omit source references",
            file.display()
        );
    }
}

#[test]
fn i18n_source_msgids_use_ascii_ellipsis() {
    let root = repo_root();
    let template = root.join("locales/rufin.pot");
    let content = fs::read_to_string(&template)
        .unwrap_or_else(|error| panic!("read {}: {error}", template.display()));
    assert_active_msgids_use_ascii_ellipsis(&template, &content);
}

#[test]
fn i18n_catalogs_pass_msgfmt_check() {
    let root = repo_root();
    let catalogs = po_files(&root.join("locales"));
    assert!(!catalogs.is_empty(), "expected at least one .po catalog");

    for catalog in catalogs {
        let stem = catalog
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("catalog");
        let output = env::temp_dir().join(format!("rufin-catalog-{}-{stem}.mo", process::id()));
        let status = Command::new("msgfmt")
            .arg("--check")
            .arg(&catalog)
            .arg("-o")
            .arg(&output)
            .status()
            .unwrap_or_else(|error| panic!("run msgfmt for {}: {error}", catalog.display()));
        let _ = fs::remove_file(output);
        assert!(
            status.success(),
            "msgfmt --check failed for {}",
            catalog.display()
        );
    }
}

fn assert_active_msgids_use_ascii_ellipsis(file: &Path, content: &str) {
    let mut current = String::new();
    let mut collecting = false;

    for line in content.lines().chain(std::iter::once("")) {
        if collecting && !line.starts_with('"') {
            assert!(
                !current.contains('…') && !current.contains(r"\342\200\246"),
                "{} contains a non-ASCII ellipsis in active source msgid: {}",
                file.display(),
                current
            );
            current.clear();
            collecting = false;
        }

        if line.starts_with("msgid ") || line.starts_with("msgid_plural ") {
            current.push_str(line);
            collecting = true;
        } else if collecting && line.starts_with('"') {
            current.push_str(line);
        }
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn po_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("po"))
        .collect::<Vec<_>>();
    files.sort();
    files
}

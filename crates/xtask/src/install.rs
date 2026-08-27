use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Result;
use crate::process::{collect_files_relative, ensure_command};

const USAGE: &str = "Usage: cargo run --locked -p xtask -- install linux --binary PATH --destdir PATH [--prefix PATH]";
const COPIED_PAYLOAD: &[(&str, &str)] = &[
    (
        "data/japanese-readings.dic",
        "share/rufin/japanese-readings.dic",
    ),
    (
        "data/japanese-readings.LICENSE",
        "share/licenses/rufin/japanese-readings.LICENSE",
    ),
    (
        "data/io.github.screwys.Rufin.desktop",
        "share/applications/io.github.screwys.Rufin.desktop",
    ),
    (
        "data/io.github.screwys.Rufin.metainfo.xml",
        "share/metainfo/io.github.screwys.Rufin.metainfo.xml",
    ),
];

pub(crate) struct PayloadFile {
    source: PathBuf,
    destination: PathBuf,
    compile_locale: bool,
}

impl PayloadFile {
    pub(crate) fn destination(&self) -> &Path {
        &self.destination
    }
}

pub(crate) fn run(mut args: Vec<String>) -> Result<()> {
    if args.first().map(String::as_str) != Some("linux") {
        return Err("install requires the linux subcommand".into());
    }
    args.remove(0);
    if matches!(args.as_slice(), [arg] if arg == "-h" || arg == "--help") {
        eprintln!("{USAGE}");
        return Ok(());
    }
    if cfg!(not(target_os = "linux")) {
        return Err("install linux is only supported on Linux".into());
    }

    let (binary, destdir, prefix) = parse_options(args)?;
    let source = env::current_dir()?;
    let install_root = destination_root(&destdir, &prefix)?;
    ensure_command("msgfmt")?;

    copy_file(&binary, &install_root.join("bin/rufin"), 0o755)?;
    for file in linux_payload(&source)? {
        let destination = install_root.join(file.destination());
        if file.compile_locale {
            create_parent(&destination)?;
            let status = Command::new("msgfmt")
                .arg(&file.source)
                .arg("-o")
                .arg(&destination)
                .status()?;
            if !status.success() {
                return Err(format!(
                    "msgfmt failed for {} with status {status}",
                    file.source.display()
                )
                .into());
            }
            set_mode(&destination, 0o644)?;
        } else {
            copy_file(&file.source, &destination, 0o644)?;
        }
    }

    eprintln!(
        "Installed Rufin Linux payload in {}",
        install_root.display()
    );
    Ok(())
}

fn parse_options(args: Vec<String>) -> Result<(PathBuf, PathBuf, String)> {
    let mut binary = None;
    let mut destdir = None;
    let mut prefix = "/usr".to_owned();
    let mut args = args.into_iter();

    while let Some(option) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("{option} requires a path"))?;
        match option.as_str() {
            "--binary" if binary.is_none() => binary = Some(PathBuf::from(value)),
            "--destdir" if destdir.is_none() => destdir = Some(PathBuf::from(value)),
            "--prefix" => prefix = value,
            "--binary" | "--destdir" => {
                return Err(format!("{option} may only be passed once").into());
            }
            _ => return Err(format!("unexpected argument: {option}").into()),
        }
    }

    Ok((
        binary.ok_or("install linux requires --binary PATH")?,
        destdir.ok_or("install linux requires --destdir PATH")?,
        prefix,
    ))
}

fn destination_root(destdir: &Path, prefix: &str) -> Result<PathBuf> {
    let Some(prefix) = prefix.strip_prefix('/') else {
        return Err("--prefix must be an absolute path".into());
    };
    let mut relative = PathBuf::new();
    for component in prefix.split('/') {
        match component {
            "" | "." => {}
            ".." => return Err("--prefix may not contain '..'".into()),
            component => relative.push(component),
        }
    }
    Ok(destdir.join(relative))
}

pub(crate) fn linux_payload(source: &Path) -> Result<Vec<PayloadFile>> {
    let mut files = COPIED_PAYLOAD
        .iter()
        .map(|(source_path, destination)| PayloadFile {
            source: source.join(source_path),
            destination: PathBuf::from(destination),
            compile_locale: false,
        })
        .collect::<Vec<_>>();

    let icon_root = source.join("data/icons/hicolor");
    let mut icons = Vec::new();
    collect_files_relative(&icon_root, &icon_root, &mut icons)?;
    icons.sort();
    files.extend(icons.into_iter().map(|icon| PayloadFile {
        source: icon_root.join(&icon),
        destination: PathBuf::from("share/icons/hicolor").join(icon),
        compile_locale: false,
    }));

    let mut po_files = fs::read_dir(source.join("locales"))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("po"))
        .collect::<Vec<_>>();
    po_files.sort();
    for po_file in po_files {
        let language = po_file
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| format!("invalid locale filename: {}", po_file.display()))?
            .to_owned();
        files.push(PayloadFile {
            source: po_file,
            destination: PathBuf::from("share/locale")
                .join(language)
                .join("LC_MESSAGES/rufin.mo"),
            compile_locale: true,
        });
    }
    Ok(files)
}

fn copy_file(source: &Path, destination: &Path, mode: u32) -> Result<()> {
    if !source.is_file() {
        return Err(format!("package input is missing: {}", source.display()).into());
    }
    create_parent(destination)?;
    fs::copy(source, destination)?;
    set_mode(destination, mode)
}

fn create_parent(path: &Path) -> Result<()> {
    fs::create_dir_all(
        path.parent()
            .ok_or_else(|| format!("package path has no parent: {}", path.display()))?,
    )?;
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Err("Linux package permissions require a Unix host".into())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::destination_root;

    #[test]
    fn destination_stays_below_destdir() {
        assert_eq!(
            destination_root(Path::new("/stage"), "/usr").unwrap(),
            PathBuf::from("/stage/usr")
        );
        assert_eq!(
            destination_root(Path::new("/stage"), "/usr//./local").unwrap(),
            PathBuf::from("/stage/usr/local")
        );
        assert!(destination_root(Path::new("/stage"), "usr").is_err());
        assert!(destination_root(Path::new("/stage"), "C:/usr").is_err());
        assert!(destination_root(Path::new("/stage"), "/usr/../opt").is_err());
    }
}

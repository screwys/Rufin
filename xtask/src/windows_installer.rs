use std::fs;
use std::path::{self, MAIN_SEPARATOR, Path};

use crate::Result;
use crate::process::{repo_root, run_command, temp_path, write_string};

const USAGE: &str = "Usage: cargo run --locked -p xtask -- generate windows-installer STAGE_DIR LOCALE_DIR MAKENSIS OUTPUT_DIR VERSION PROJECT_NAME DISPLAY_NAME APP_ID";

struct Installer<'a> {
    stage: &'a Path,
    locale_dir: &'a Path,
    compiler: &'a str,
    output_dir: &'a Path,
    version: &'a str,
    project_name: &'a str,
    display_name: &'a str,
    app_id: &'a str,
}

pub(crate) fn command(args: Vec<String>) -> Result<()> {
    let [
        stage,
        locale_dir,
        compiler,
        output_dir,
        version,
        project_name,
        display_name,
        app_id,
    ] = args.as_slice()
    else {
        return Err(USAGE.into());
    };
    let installer = Installer {
        stage: Path::new(stage),
        locale_dir: Path::new(locale_dir),
        compiler,
        output_dir: Path::new(output_dir),
        version,
        project_name,
        display_name,
        app_id,
    };
    let work = temp_path("windows-installer");
    fs::create_dir_all(&work)?;
    let built = installer.build(&work);
    let removed = fs::remove_dir_all(&work);
    built?;
    Ok(removed?)
}

/// Prefixes a makensis option the way the host's makensis parses it.
pub(crate) fn makensis_option(name: &str) -> String {
    let prefix = if cfg!(windows) { '/' } else { '-' };
    format!("{prefix}{name}")
}

impl Installer<'_> {
    fn build(&self, work: &Path) -> Result<()> {
        let root = repo_root()?;
        let languages = work.join("languages.nsh");
        let inventory = work.join("install-files.txt");
        write_string(
            &languages,
            &crate::windows_i18n::languages(self.locale_dir, self.compiler)?,
        )?;
        write_inventory(self.stage, &inventory)?;
        fs::create_dir_all(self.output_dir)?;
        let stage = native(self.stage)?;
        let defines = [
            ("RUFIN_STAGE_FILES", format!("{stage}{MAIN_SEPARATOR}*")),
            ("RUFIN_STAGE_DIR", stage),
            ("RUFIN_OUTPUT_DIR", native(self.output_dir)?),
            (
                "RUFIN_ASSET_DIR",
                native(&root.join("packaging/windows/assets"))?,
            ),
            ("RUFIN_LANGUAGES_FILE", native(&languages)?),
            ("RUFIN_FILES_FILE", native(&inventory)?),
            ("RUFIN_APP_ID", self.app_id.to_owned()),
            ("RUFIN_DISPLAY_NAME", self.display_name.to_owned()),
            ("RUFIN_PROJECT_NAME", self.project_name.to_owned()),
            ("RUFIN_VERSION", self.version.to_owned()),
            ("RUFIN_VERSION_QUAD", version_quad(self.version)?),
        ];
        // Warnings are errors: an unknown define stays literal text in the installer.
        let mut args = vec![makensis_option("WX")];
        args.extend(
            defines
                .into_iter()
                .map(|(name, value)| makensis_option(&format!("D{name}={value}"))),
        );
        args.push(native(&root.join("packaging/windows/rufin.nsi"))?);
        run_command(self.compiler, args)
    }
}

/// makensis splits paths only at backslashes on Windows, so it gets native absolute paths.
fn native(path: &Path) -> Result<String> {
    let path = path::absolute(path)?;
    let path = path
        .to_str()
        .ok_or_else(|| format!("{} is not valid Unicode", path.display()))?;
    Ok(if cfg!(windows) {
        path.replace('/', "\\")
    } else {
        path.to_owned()
    })
}

/// Windows file versions take four numbers: MAJOR.MINOR.PATCH.0.
fn version_quad(version: &str) -> Result<String> {
    let invalid = || format!("{version} does not start with MAJOR.MINOR.PATCH");
    let mut parts = version.splitn(3, '.');
    let (Some(major), Some(minor), Some(rest)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(invalid().into());
    };
    let patch = &rest[..rest.bytes().take_while(u8::is_ascii_digit).count()];
    if [major, minor, patch]
        .iter()
        .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(invalid().into());
    }
    Ok(format!("{major}.{minor}.{patch}.0"))
}

fn write_inventory(root: &Path, output: &Path) -> Result<()> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    let mut directories = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let relative =
                crate::process::path_to_slash(path.strip_prefix(root)?).replace('/', "\\");
            if entry.file_type()?.is_dir() {
                directories.push(relative);
                pending.push(path);
            } else {
                files.push(relative);
            }
        }
    }
    files.sort();
    directories.sort();
    // Delete files first, then attempt child directories before their parents.
    // UTF-16LE matches NSIS FileReadUTF16LE, including non-ASCII filenames.
    let inventory = files
        .into_iter()
        .map(|path| format!("F{path}\r\n"))
        .chain(
            directories
                .into_iter()
                .rev()
                .map(|path| format!("D{path}\r\n")),
        )
        .collect::<String>();
    fs::write(
        output,
        inventory
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    )?;
    Ok(())
}

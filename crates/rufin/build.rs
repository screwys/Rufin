use std::{
    env,
    error::Error,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

type BuildResult<T> = Result<T, Box<dyn Error>>;

fn main() -> BuildResult<()> {
    let manifest_dir = cargo_env_path("CARGO_MANIFEST_DIR")?;
    let icon_path = manifest_dir.join("../../packaging/windows/assets/rufin.ico");

    println!("cargo:rerun-if-changed={}", icon_path.display());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_DEVELOPMENT");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }

    compile_windows_resource(&icon_path)
}

fn cargo_env_path(name: &str) -> BuildResult<PathBuf> {
    let value = env::var_os(name).ok_or_else(|| io::Error::other(format!("{name} is not set")))?;
    Ok(PathBuf::from(value))
}

fn compile_windows_resource(icon_path: &Path) -> BuildResult<()> {
    let out_dir = cargo_env_path("OUT_DIR")?;
    let resource_icon_path = out_dir.join("rufin.ico");
    let resource_script_path = out_dir.join("rufin.rc");
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let compiled_resource_path = if target_env == "msvc" {
        out_dir.join("rufin.res")
    } else {
        out_dir.join("rufin-resource.o")
    };

    fs::copy(icon_path, &resource_icon_path)?;
    fs::write(&resource_script_path, windows_resource_script()?)?;

    if target_env == "msvc" {
        compile_with_msvc_resource_compiler(&resource_script_path, &compiled_resource_path)?;
    } else {
        compile_with_windres(&resource_script_path, &compiled_resource_path)?;
    }

    println!(
        "cargo:rustc-link-arg-bin=rufin={}",
        compiled_resource_path.display()
    );
    Ok(())
}

fn windows_resource_script() -> BuildResult<String> {
    let package_version = env::var("CARGO_PKG_VERSION")?;
    let display_name = if env::var_os("CARGO_FEATURE_DEVELOPMENT").is_some() {
        "Rufin (Development)"
    } else {
        "Rufin"
    };
    let [major, minor, patch, build] = windows_version_numbers(&package_version);

    Ok(format!(
        r#"#define APP_ICON 1
#define VER_FILEVERSION {major},{minor},{patch},{build}
#define VER_FILEVERSION_STR "{package_version}\0"

APP_ICON ICON "rufin.ico"

1 VERSIONINFO
FILEVERSION VER_FILEVERSION
PRODUCTVERSION VER_FILEVERSION
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904B0"
        BEGIN
            VALUE "FileDescription", "{display_name}\0"
            VALUE "FileVersion", VER_FILEVERSION_STR
            VALUE "InternalName", "rufin\0"
            VALUE "OriginalFilename", "rufin.exe\0"
            VALUE "ProductName", "{display_name}\0"
            VALUE "ProductVersion", VER_FILEVERSION_STR
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0409, 1200
    END
END
"#
    ))
}

fn windows_version_numbers(version: &str) -> [u16; 4] {
    let mut numbers = [0, 0, 0, 0];
    for (index, part) in version
        .split(['.', '-', '+'])
        .take(numbers.len())
        .enumerate()
    {
        if let Some(number) = numbers.get_mut(index) {
            *number = part.parse::<u16>().unwrap_or(0);
        }
    }
    numbers
}

fn compile_with_windres(
    resource_script_path: &Path,
    compiled_resource_path: &Path,
) -> BuildResult<()> {
    let mut last_error = None;
    let mut compilers = Vec::new();
    if let Some(compiler) = env::var_os("WINDRES") {
        compilers.push(PathBuf::from(compiler));
    }
    compilers.extend(["windres", "llvm-windres"].map(PathBuf::from));
    let resource_dir = resource_script_path
        .parent()
        .ok_or_else(|| io::Error::other("Windows resource script has no parent directory"))?;
    let resource_file = resource_script_path
        .file_name()
        .ok_or_else(|| io::Error::other("Windows resource script has no file name"))?;
    let compiled_resource_file = compiled_resource_path
        .file_name()
        .ok_or_else(|| io::Error::other("compiled Windows resource has no file name"))?;
    for compiler in compilers {
        let result = Command::new(&compiler)
            .current_dir(resource_dir)
            .arg("-i")
            .arg(resource_file)
            .arg("-O")
            .arg("coff")
            .arg("-o")
            .arg(compiled_resource_file)
            .status();
        match result {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => {
                return Err(io::Error::other(format!(
                    "{} failed with status {status}",
                    compiler.display()
                ))
                .into());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => last_error = Some(error),
            Err(error) => {
                return Err(io::Error::other(format!(
                    "failed to run {}: {error}",
                    compiler.display()
                ))
                .into());
            }
        }
    }

    Err(io::Error::other(format!(
        "failed to compile Windows resources: windres was not found{}",
        last_error
            .map(|error| format!(" ({error})"))
            .unwrap_or_default()
    ))
    .into())
}

fn compile_with_msvc_resource_compiler(
    resource_script_path: &Path,
    compiled_resource_path: &Path,
) -> BuildResult<()> {
    let mut last_error = None;
    let resource_dir = resource_script_path
        .parent()
        .ok_or_else(|| io::Error::other("Windows resource script has no parent directory"))?;
    let resource_file = resource_script_path
        .file_name()
        .ok_or_else(|| io::Error::other("Windows resource script has no file name"))?;
    let compiled_resource_file = compiled_resource_path
        .file_name()
        .ok_or_else(|| io::Error::other("compiled Windows resource has no file name"))?;
    for compiler in ["rc", "rc.exe", "llvm-rc", "llvm-rc.exe"] {
        let result = Command::new(compiler)
            .current_dir(resource_dir)
            .arg("/nologo")
            .arg(format!("/fo{}", compiled_resource_file.to_string_lossy()))
            .arg(resource_file)
            .status();
        match result {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => {
                return Err(
                    io::Error::other(format!("{compiler} failed with status {status}")).into(),
                );
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => last_error = Some(error),
            Err(error) => {
                return Err(io::Error::other(format!("failed to run {compiler}: {error}")).into());
            }
        }
    }

    Err(io::Error::other(format!(
        "failed to compile Windows resources: rc was not found{}",
        last_error
            .map(|error| format!(" ({error})"))
            .unwrap_or_default()
    ))
    .into())
}

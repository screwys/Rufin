use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Result;
use crate::process::{command_stdout, repo_root};

const GSTREAMER_VERSION: &str = "1.28.7";
const GSTREAMER_SHA256: &str = "fa914bcf8695af1c6fa1f81db2e8da019a0306a89c2c3e48659438e1654023da";

pub(crate) fn run(args: Vec<String>) -> Result<()> {
    let root = repo_root()?;
    env::set_current_dir(&root)?;
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["metadata"] => metadata(),
        ["build", distribution @ ("stable" | "development")] => build(&root, distribution),
        ["native", variant, distribution @ ("stable" | "development"), abis] => {
            native(&root, variant, distribution, abis)
        }
        ["install"] => install(&root),
        ["clippy"] => clippy(&root),
        _ => Err("Usage: xtask android build stable|development\n       xtask android install\n       xtask android clippy\n       xtask android native VARIANT stable|development ABIS".into()),
    }
}

fn metadata() -> Result<()> {
    let cargo: serde_json::Value = serde_json::from_str(&command_stdout(
        "cargo",
        [
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--filter-platform",
            "aarch64-linux-android",
        ],
    )?)?;
    let verifier = cargo["packages"]
        .as_array()
        .and_then(|packages| {
            packages
                .iter()
                .find(|package| package["name"] == "rustls-platform-verifier-android")
        })
        .ok_or("Cargo metadata is missing rustls-platform-verifier-android")?;
    println!(
        "{}",
        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "stableId": app_identity::STABLE_APP_ID,
            "stableName": app_identity::STABLE_DISPLAY_NAME,
            "developmentId": app_identity::DEVELOPMENT_APP_ID,
            "developmentName": app_identity::DEVELOPMENT_DISPLAY_NAME,
            "verifierManifest": verifier["manifest_path"],
            "verifierVersion": verifier["version"],
            "cargoTarget": cargo["target_directory"],
        })
    );
    Ok(())
}

fn execute(command: &mut Command) -> Result<()> {
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "{} failed with {status}",
            command.get_program().to_string_lossy()
        )
        .into())
    }
}

fn build(root: &Path, distribution: &str) -> Result<()> {
    let android = root.join("android");
    let (variant, build_type, name) = if distribution == "development" {
        ("DevelopmentDebug", "debug", "Rufin.Devel.apk")
    } else {
        ("StableRelease", "release", "Rufin.apk")
    };
    let output = android.join(format!("app/build/outputs/apk/{distribution}/{build_type}"));
    // Incremental APK replacement can leave old native libraries as unused space.
    // Rebuild the archive while keeping the native compilation caches.
    for suffix in ["", "-unsigned"] {
        let archive = output.join(format!("app-{distribution}-{build_type}{suffix}.apk"));
        if archive.exists() {
            fs::remove_file(archive)?;
        }
    }
    execute(
        Command::new(android.join("gradlew"))
            .arg("-p")
            .arg(&android)
            .arg(format!("assemble{variant}")),
    )?;
    let metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("output-metadata.json"))?)?;
    let filename = metadata["elements"][0]["outputFile"]
        .as_str()
        .ok_or("Gradle did not produce an APK")?;
    let source = output.join(filename);
    let destination = root.join(".local/artifacts").join(name);
    copy_file(&source, &destination)?;
    fs::File::options()
        .write(true)
        .open(&destination)?
        .set_modified(std::time::SystemTime::now())?;
    println!("Created {}", destination.display());
    Ok(())
}

fn download_gstreamer(destination: &Path, architectures: &[&str]) -> Result<()> {
    println!("Downloading GStreamer {GSTREAMER_VERSION} Android SDK…");
    fs::create_dir_all(destination)?;
    let temporary = tempfile::tempdir_in(destination)?;
    let archive = temporary.path().join("gstreamer.tar.xz");
    execute(
        Command::new("curl")
            .args(["--fail", "--location", "--progress-bar"])
            .arg(format!(
                "https://gstreamer.freedesktop.org/data/pkg/android/{GSTREAMER_VERSION}/gstreamer-1.0-android-universal-{GSTREAMER_VERSION}.tar.xz"
            ))
            .arg("--output")
            .arg(&archive),
    )?;
    let checksum = if env::consts::OS == "macos" {
        command_stdout(
            "shasum",
            [OsStr::new("-a"), OsStr::new("256"), archive.as_os_str()],
        )?
    } else {
        command_stdout("sha256sum", [&archive])?
    };
    if checksum.split_whitespace().next() != Some(GSTREAMER_SHA256) {
        return Err("GStreamer Android SDK checksum does not match the pinned release".into());
    }
    execute(
        Command::new("tar")
            .arg("-xf")
            .arg(&archive)
            .arg("-C")
            .arg(temporary.path())
            .args(architectures),
    )?;
    for arch in architectures {
        let target = destination.join(arch);
        if target.exists() {
            fs::remove_dir_all(&target)?;
        }
        fs::rename(temporary.path().join(arch), target)?;
    }
    Ok(())
}

struct Toolchain {
    android: PathBuf,
    ndk: PathBuf,
    compiler_dir: PathBuf,
    gst: PathBuf,
}

struct Target<'a> {
    abi: &'a str,
    rust: &'static str,
    sdk_arch: &'static str,
    compiler: &'static str,
}

fn toolchain<'a>(root: &Path, abis: &'a str) -> Result<(Toolchain, Vec<Target<'a>>)> {
    let android = root.join("android");
    let mut sdk = env::var_os("ANDROID_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            directories::BaseDirs::new()
                .expect("home directory")
                .home_dir()
                .join("Android/Sdk")
        });
    let mut custom_gst = env::var_os("GSTREAMER_ROOT_ANDROID").map(PathBuf::from);
    let properties = android.join("local.properties");
    if properties.exists() {
        for line in fs::read_to_string(properties)?.lines() {
            if let Some(path) = line.strip_prefix("sdk.dir=") {
                sdk = path.into();
            }
            if let Some(path) = line.strip_prefix("gstreamer.dir=") {
                custom_gst = Some(path.into());
            }
        }
    }
    let host = match env::consts::OS {
        "linux" => "linux-x86_64",
        "macos" => "darwin-x86_64",
        _ => return Err("Android native builds currently require Linux or macOS.".into()),
    };
    let gst = custom_gst
        .clone()
        .unwrap_or_else(|| android.join(format!(".toolchains/gstreamer-{GSTREAMER_VERSION}")));
    let targets = abis
        .split(',')
        .map(|abi| {
            let (rust, sdk_arch, compiler) = match abi {
                "arm64-v8a" => ("aarch64-linux-android", "arm64", "aarch64-linux-android"),
                "armeabi-v7a" => (
                    "armv7-linux-androideabi",
                    "armv7",
                    "armv7a-linux-androideabi",
                ),
                "x86_64" => ("x86_64-linux-android", "x86_64", "x86_64-linux-android"),
                "x86" => ("i686-linux-android", "x86", "i686-linux-android"),
                _ => return Err(format!("Unknown Android ABI: {abi}")),
            };
            Ok(Target {
                abi,
                rust,
                sdk_arch,
                compiler,
            })
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut missing = targets
        .iter()
        .map(|target| target.sdk_arch)
        .filter(|arch| {
            !gst.join(arch)
                .join("lib/pkgconfig/gstreamer-1.0.pc")
                .is_file()
        })
        .collect::<Vec<_>>();
    missing.sort_unstable();
    missing.dedup();
    if !missing.is_empty() {
        if custom_gst.is_some() {
            return Err(format!(
                "GStreamer SDK at {} is missing architectures: {}",
                gst.display(),
                missing.join(", ")
            )
            .into());
        }
        download_gstreamer(&gst, &missing)?;
    }
    let ndk = sdk.join("ndk/29.0.14206865");
    let compiler_dir = ndk.join("toolchains/llvm/prebuilt").join(host).join("bin");
    Ok((
        Toolchain {
            android,
            ndk,
            compiler_dir,
            gst,
        },
        targets,
    ))
}

impl Toolchain {
    fn cargo(&self, target: &Target, subcommand: &str) -> Command {
        let suffix = target.rust.replace('-', "_");
        let compiler = self
            .compiler_dir
            .join(format!("{}24-clang", target.compiler));
        let mut cargo = Command::new("cargo");
        cargo
            .args([
                subcommand,
                "--locked",
                "--target",
                target.rust,
                "--jobs",
                "4",
            ])
            .env("CARGO_TARGET_DIR", self.android.join("build/rust"))
            .env(
                format!("CARGO_TARGET_{}_LINKER", suffix.to_uppercase()),
                &compiler,
            )
            .env(format!("CC_{suffix}"), &compiler)
            .env(
                format!("CXX_{suffix}"),
                self.compiler_dir
                    .join(format!("{}24-clang++", target.compiler)),
            )
            .env(format!("AR_{suffix}"), self.compiler_dir.join("llvm-ar"))
            .env(
                format!("PKG_CONFIG_{suffix}"),
                self.android.join("native/pkg-config"),
            )
            .env(
                "PKG_CONFIG_LIBDIR",
                self.gst.join(target.sdk_arch).join("lib/pkgconfig"),
            )
            .env("PKG_CONFIG_PATH", "")
            .env("PKG_CONFIG_ALLOW_CROSS", "1")
            .env("RUFIN_GSTREAMER_LINK_DIR", self.native(target).join("libs"))
            .env(
                format!("CARGO_TARGET_{}_RUSTFLAGS", suffix.to_uppercase()),
                "-C link-arg=-Wl,--no-undefined",
            );
        cargo
    }

    fn native(&self, target: &Target) -> PathBuf {
        self.android.join("build/native").join(target.abi)
    }
}

// Lints the Android build of every crate the desktop build skips or compiles differently.
fn clippy(root: &Path) -> Result<()> {
    let (toolchain, targets) = toolchain(root, "arm64-v8a")?;
    execute(
        toolchain
            .cargo(&targets[0], "clippy")
            .args([
                "--workspace",
                "--all-targets",
                "--features",
                "rufin-android/development",
            ])
            .args([
                "--exclude",
                "gtk-*",
                "--exclude",
                "rufin",
                "--exclude",
                "rufin-controller",
                "--exclude",
                "desktop-integration",
                "--exclude",
                "windows-updater",
                "--exclude",
                "xtask",
            ]),
    )
}

fn native(root: &Path, variant: &str, distribution: &str, abis: &str) -> Result<()> {
    let (toolchain, targets) = toolchain(root, abis)?;
    let android = &toolchain.android;
    let generated = android.join("app/build/generated/rufin").join(variant);
    let profile = if variant.ends_with("Release") {
        "android-release"
    } else {
        "android"
    };
    for target in &targets {
        let gst_arch = toolchain.gst.join(target.sdk_arch);
        let native = toolchain.native(target);
        execute(
            Command::new("cmake")
                .arg("-S")
                .arg(android.join("native"))
                .arg("-B")
                .arg(&native)
                .arg(format!(
                    "-DCMAKE_TOOLCHAIN_FILE={}",
                    toolchain
                        .ndk
                        .join("build/cmake/android.toolchain.cmake")
                        .display()
                ))
                .arg(format!("-DANDROID_ABI={}", target.abi))
                .args([
                    "-DANDROID_PLATFORM=android-24",
                    "-DANDROID_STL=c++_shared",
                    "-DCMAKE_BUILD_TYPE=Release",
                ])
                .arg(format!("-DGStreamer_ROOT_DIR={}", gst_arch.display())),
        )?;
        execute(
            Command::new("cmake")
                .arg("--build")
                .arg(&native)
                .args(["--parallel", "4"]),
        )?;
        let mut cargo = toolchain.cargo(target, "build");
        cargo.args(["-p", "rufin-android", "--lib", "--profile", profile]);
        if distribution == "development" {
            cargo.args(["--features", "development"]);
        }
        execute(&mut cargo)?;
        let library = android
            .join("build/rust")
            .join(target.rust)
            .join(profile)
            .join("librufin_android.so");
        let output = generated.join("jniLibs").join(target.abi);
        let cxx = toolchain
            .compiler_dir
            .join("../sysroot/usr/lib")
            .join(target.rust.replace("armv7", "arm"))
            .join("libc++_shared.so");
        for source in [&library, &native.join("libs/libgstreamer_android.so"), &cxx] {
            copy_file(source, &output.join(source.file_name().unwrap()))?;
        }
        copy_tree(&native.join("java"), &generated.join("java"))?;
        copy_tree(&native.join("assets"), &generated.join("assets"))?;
    }
    execute(Command::new("cargo").args([
        "build",
        "--locked",
        "-p",
        "rufin-android",
        "--features",
        "bindgen",
        "--bin",
        "rufin-bindgen",
    ]))?;
    copy_file(
        &root.join("resources/icons/hicolor/512x512/apps/io.github.screwys.Rufin.png"),
        &generated.join("res/drawable-nodpi/app_icon.png"),
    )?;
    for provider in [
        "jellyfin",
        "emby",
        "navidrome",
        "opensubsonic",
        "plex",
        "webdav",
        "smb",
    ] {
        copy_file(
            &root.join(format!(
                "resources/icons/hicolor/64x64/apps/io.github.screwys.Rufin.source.{provider}.png"
            )),
            &generated.join(format!("res/drawable-nodpi/source_{provider}.png")),
        )?;
    }
    for service in ["lastfm", "musicbrainz"] {
        copy_file(
            &root.join(format!(
                "resources/icons/hicolor/64x64/apps/io.github.screwys.Rufin.external.{service}.png"
            )),
            &generated.join(format!("res/drawable-nodpi/external_{service}.png")),
        )?;
    }
    icons(root, &generated)
}

fn icons(root: &Path, generated: &Path) -> Result<()> {
    let mut paths = fs::read_dir(root.join("resources/icons/hicolor/scalable/actions"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort();
    paths.push(
        root.join("resources/icons/hicolor/symbolic/apps/io.github.screwys.Rufin-symbolic.svg"),
    );
    let mut mapping = String::from(
        "package io.github.screwys.rufin\n\ninternal data class RufinIconResource(val drawable: Int, val glyphScale: Float, val glyphX: Float, val glyphY: Float)\n\ninternal val rufinIconResources = mapOf(\n",
    );
    for path in paths {
        if path.extension().is_none_or(|extension| extension != "svg") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or("Invalid icon filename")?;
        let name = if name == "io.github.screwys.Rufin-symbolic" {
            "rufin-cover-fallback-symbolic"
        } else {
            name
        };
        let resource = name.replace('-', "_");
        let output = generated
            .join("res/drawable-nodpi")
            .join(format!("{resource}.png"));
        let modified = fs::metadata(&path)?.modified()?;
        if fs::metadata(&output)
            .and_then(|metadata| metadata.modified())
            .ok()
            != Some(modified)
        {
            let tree =
                resvg::usvg::Tree::from_data(&fs::read(&path)?, &resvg::usvg::Options::default())?;
            let size = tree.size();
            let mut image =
                resvg::tiny_skia::Pixmap::new(96, 96).ok_or("Could not allocate icon")?;
            let scale = 96.0 / size.width().max(size.height());
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::from_scale(scale, scale).post_translate(
                    (96.0 - size.width() * scale) / 2.0,
                    (96.0 - size.height() * scale) / 2.0,
                ),
                &mut image.as_mut(),
            );
            fs::create_dir_all(output.parent().ok_or("Icon output directory is missing")?)?;
            image.save_png(&output)?;
            fs::File::options()
                .write(true)
                .open(&output)?
                .set_modified(modified)?;
        }
        let image = resvg::tiny_skia::Pixmap::load_png(&output)?;
        let width = image.width();
        let height = image.height();
        let (left, top, right, bottom) = image
            .pixels()
            .iter()
            .enumerate()
            .filter(|(_, pixel)| pixel.alpha() != 0)
            .map(|(index, _)| {
                let x = index as u32 % width;
                let y = index as u32 / width;
                (x, y, x + 1, y + 1)
            })
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
            .unwrap_or((0, 0, width, height));
        let span = (right - left).max(bottom - top) as f32;
        let scale = width as f32 / span;
        // Bounding box centering shifts the triangle's center of mass.
        let (x, y) = if name == "rufin-media-playback-start-symbolic" {
            let (weight, horizontal, vertical) = image.pixels().iter().enumerate().fold(
                (0.0f64, 0.0, 0.0),
                |(weight, horizontal, vertical), (index, pixel)| {
                    let alpha = f64::from(pixel.alpha());
                    (
                        weight + alpha,
                        horizontal + alpha * (f64::from(index as u32 % width) + 0.5),
                        vertical + alpha * (f64::from(index as u32 / width) + 0.5),
                    )
                },
            );
            (
                (width as f64 / 2.0 - horizontal / weight) as f32 / span,
                (height as f64 / 2.0 - vertical / weight) as f32 / span,
            )
        } else {
            (
                (width as f32 - (left + right) as f32) / (2.0 * span),
                (height as f32 - (top + bottom) as f32) / (2.0 * span),
            )
        };
        mapping.push_str(&format!(
            "    \"{name}\" to RufinIconResource(R.drawable.{resource}, {scale:.6}f, {x:.6}f, {y:.6}f),\n"
        ));
    }
    mapping.push_str(")\n");
    let output = generated.join("icons/io/github/screwys/rufin/IconResources.kt");
    fs::create_dir_all(output.parent().ok_or("Icon mapping directory is missing")?)?;
    if fs::read_to_string(&output).ok().as_deref() != Some(mapping.as_str()) {
        fs::write(output, mapping)?;
    }
    Ok(())
}

fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    let metadata = source.metadata()?;
    if let Ok(existing) = destination.metadata()
        && existing.len() == metadata.len()
        && existing.modified()? == metadata.modified()?
    {
        return Ok(());
    }
    fs::create_dir_all(destination.parent().unwrap())?;
    fs::copy(source, destination)?;
    fs::File::options()
        .write(true)
        .open(destination)?
        .set_modified(metadata.modified()?)?;
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            copy_file(&entry.path(), &target)?;
        }
    }
    Ok(())
}

fn install(root: &Path) -> Result<()> {
    let artifacts = root.join(".local/artifacts");
    let apk = [
        artifacts.join("Rufin.apk"),
        artifacts.join("Rufin.Devel.apk"),
    ]
    .into_iter()
    .filter(|path| path.is_file())
    .map(|path| path.metadata()?.modified().map(|modified| (modified, path)))
    .collect::<std::io::Result<Vec<_>>>()?
    .into_iter()
    .max_by_key(|(modified, _)| *modified)
    .map(|(_, path)| path)
    .ok_or("Build an APK first with just build apk or just build apk-dev.")?;
    let devices = command_stdout("adb", ["devices"])?;
    let online = devices
        .lines()
        .filter_map(|line| {
            let (id, status) = line.split_once('\t')?;
            (status.trim() == "device").then_some(id)
        })
        .collect::<Vec<_>>();
    let requested = env::var("ANDROID_SERIAL").ok();
    let serial = if let Some(serial) = requested.as_deref() {
        if !online.contains(&serial) {
            return Err(
                "ANDROID_SERIAL does not identify an authorized, online ADB connection.".into(),
            );
        }
        serial
    } else {
        *online
            .first()
            .ok_or("No authorized, online ADB connection is available.")?
    };
    execute(
        Command::new("adb")
            .args(["-s", serial, "install", "--user", "0", "-r"])
            .arg(apk),
    )
}

set shell := ["bash", "-euc"]

default:
    @just --list

build target="" architecture="":
    @if [[ "{{ target }}" == "arch" && -z "{{ architecture }}" ]]; then \
        scripts/container run default none just _build-arch; \
    elif [[ "{{ target }}" == "dmg" && -z "{{ architecture }}" ]]; then \
        just _build-dmg; \
    elif [[ "{{ target }}" == "rpm" ]]; then \
        scripts/container run packaging engine \
            just _build-rpm "{{ architecture }}"; \
    elif [[ "{{ target }}" == "flatpak" && -z "{{ architecture }}" ]]; then \
        scripts/container run packaging sandbox env FLATPAK_BWRAP=/usr/bin/bwrap \
            just _build-flatpak; \
    elif [[ "{{ target }}" == "windows" && -z "{{ architecture }}" ]]; then \
        just _build-windows; \
    elif [[ -z "{{ target }}" && -z "{{ architecture }}" ]]; then \
        scripts/container run default none just _build; \
    else \
        echo "usage: just build [arch|dmg|flatpak|windows|rpm [arm]]" >&2; \
        exit 2; \
    fi

_build:
    @target_dir="${CARGO_TARGET_DIR:-$PWD/target}"; \
    artifact_root="${RUFIN_ARTIFACT_ROOT:-$PWD/.local/artifacts}"; \
    missing_gstreamer=(); \
    if command -v gst-inspect-1.0 >/dev/null 2>&1; then \
        if ! gst-inspect-1.0 --exists playbin3 >/dev/null 2>&1 \
            && ! gst-inspect-1.0 --exists playbin >/dev/null 2>&1; then \
            missing_gstreamer+=("playbin3 or playbin"); \
        fi; \
        audio_sink=autoaudiosink; \
        if [[ "$(uname -s)" == Darwin ]]; then audio_sink=osxaudiosink; fi; \
        for element in audioconvert audioresample "$audio_sink" equalizer-nbands scaletempo souphttpsrc; do \
            if ! gst-inspect-1.0 --exists "$element" >/dev/null 2>&1; then \
                missing_gstreamer+=("$element"); \
            fi; \
        done; \
    else \
        missing_gstreamer+=(gst-inspect-1.0); \
    fi; \
    if (( ${#missing_gstreamer[@]} > 0 )); then \
        printf -v missing_gstreamer_list '%s, ' "${missing_gstreamer[@]}"; \
        missing_gstreamer_list="${missing_gstreamer_list%, }"; \
        echo "Warning: You are missing one or more basic GStreamer dependencies: $missing_gstreamer_list." >&2; \
        printf 'Continue with the build? [Y/n] ' >&2; \
        if ! IFS= read -r reply; then \
            reply=n; \
        fi; \
        case "$reply" in \
            ""|y|Y|yes|YES|Yes) ;; \
            *) echo "Build cancelled." >&2; exit 1 ;; \
        esac; \
    fi; \
    executable=rufin; \
    if [[ "$(rustc -vV | sed -n 's/^host: //p')" == *-windows-* ]]; then \
        executable=rufin.exe; \
    fi; \
    artifact="$artifact_root/$executable"; \
    mkdir -p "$artifact_root"; \
    CARGO_TARGET_DIR="$target_dir" cargo build --locked -p rufin --features development; \
    cp "$target_dir/debug/$executable" "$artifact"

_build-arch:
    #!/usr/bin/env bash
    set -euo pipefail

    artifact_root="${RUFIN_ARTIFACT_ROOT:-$PWD/.local/artifacts}"
    work_dir="$PWD/.local/build/arch"
    source_dir="$work_dir/source"
    package_dir="$work_dir/package"

    for command in bsdtar cargo fakeroot git makepkg msgfmt pkg-config rustc zstd; do
        if ! command -v "$command" >/dev/null 2>&1; then
            echo "$command is required to build the Arch package." >&2
            exit 1
        fi
    done

    mkdir -p "$artifact_root"
    rm -rf "$work_dir"
    mkdir -p "$source_dir" "$package_dir"

    bsdtar \
        --exclude='./.flatpak-builder' \
        --exclude='./.git' \
        --exclude='./.local' \
        --exclude='./.ruff_cache' \
        --exclude='./build-dir' \
        --exclude='./target' \
        -cf - \
        -C "$PWD" \
        . \
        | bsdtar -xf - -C "$source_dir"

    git -C "$source_dir" init --quiet
    git -C "$source_dir" add .
    git -C "$source_dir" \
        -c user.name='Rufin package build' \
        -c user.email='rufin@localhost' \
        commit --quiet --message='Build local package'

    sed \
        "s|git+https://github.com/screwys/Rufin.git|git+file://$source_dir|" \
        packaging/aur/rufin-git/PKGBUILD \
        > "$package_dir/PKGBUILD"

    makepkg_config=/etc/makepkg.conf
    if [[ ! -r "$makepkg_config" ]]; then
        if ! command -v nix >/dev/null 2>&1 || ! command -v jq >/dev/null 2>&1; then
            echo "makepkg.conf was not found." >&2
            exit 1
        fi
        makepkg_config="$(
            nix profile list --json \
                | jq -r '[.elements[] | select(.active and (.attrPath | endswith(".pacman"))) | .storePaths[0]][0] // empty'
        )/etc/makepkg.conf"
    fi

    (
        cd "$package_dir"
        PKGEXT='.pkg.tar.zst' makepkg \
            --config "$makepkg_config" \
            --cleanbuild \
            --clean \
            --nodeps \
            --noconfirm \
            --noprogressbar
    )

    mapfile -t packages < <(
        find "$package_dir" \
            -maxdepth 1 \
            -type f \
            -name 'rufin-git-*.pkg.tar.zst' \
            ! -name '*-debug-*' \
            -print
    )
    if [[ ${#packages[@]} -eq 0 ]]; then
        echo "The Arch build did not produce an artifact." >&2
        exit 1
    fi
    bsdtar -tf "${packages[0]}" | grep -qx 'usr/bin/rufin'
    find "$artifact_root" \
        -maxdepth 1 \
        -type f \
        -name 'rufin-git-*.pkg.tar.zst' \
        -delete
    cp "${packages[@]}" "$artifact_root/"
    echo "Created the Arch package in $artifact_root"

_build-rpm requested_arch="":
    #!/usr/bin/env bash
    set -euo pipefail

    requested_arch="{{ requested_arch }}"
    case "$requested_arch" in
        ""|x86|x86_64)
            rpm_arch=x86_64
            container_arch=amd64
            ;;
        arm|arm64|aarch64)
            rpm_arch=aarch64
            container_arch=arm64
            ;;
        *)
            echo "usage: just build rpm [arm]" >&2
            exit 2
            ;;
    esac

    for command in cargo git; do
        if ! command -v "$command" >/dev/null 2>&1; then
            echo "$command is required to build an RPM." >&2
            exit 1
        fi
    done

    declare -a engine_command platform_args
    if [[ "${RUFIN_CONTAINER:-0}" == "1" ]]; then
        if [[ "${RUFIN_CONTAINER_HOST_ENGINE:-0}" != "1" ]]; then
            echo "The RPM build needs command-scoped access to the host container engine. Run 'just build rpm' from the host." >&2
            exit 1
        fi
        if ! command -v docker >/dev/null 2>&1; then
            echo "docker is required to use the host container engine from the development container." >&2
            exit 1
        fi
        engine_command=(docker)
        platform_args=(--platform "linux/$container_arch")
    elif command -v podman >/dev/null 2>&1; then
        engine_command=(podman)
        platform_args=(--arch "$container_arch")
    elif command -v docker >/dev/null 2>&1; then
        engine_command=(docker)
        platform_args=(--platform "linux/$container_arch")
    else
        echo "Podman or Docker is required to build an RPM." >&2
        exit 1
    fi

    tag="${RUFIN_RPM_TAG:-$(git describe --tags --abbrev=0 --match 'v[0-9]*')}"
    fedora_version="${RUFIN_RPM_FEDORA_VERSION:-44}"
    if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([.-][A-Za-z0-9.]+)?$ ]]; then
        echo "Invalid RPM release tag: $tag" >&2
        exit 1
    fi
    if [[ ! "$fedora_version" =~ ^[0-9]+$ ]]; then
        echo "RUFIN_RPM_FEDORA_VERSION must be a Fedora release number." >&2
        exit 1
    fi

    artifact_root="${RUFIN_ARTIFACT_ROOT:-$PWD/.local/artifacts}"
    artifact_dir="$PWD/.local/build/rpm/$tag/$rpm_arch"
    artifact_dir_for_engine="$artifact_dir"
    image="registry.fedoraproject.org/fedora-minimal:$fedora_version"
    rpm_build_command='
        dnf -y --setopt=install_weak_deps=False install dnf5-plugins rpm-build
        dnf -y --setopt=install_weak_deps=False builddep /work/*.src.rpm
        rpm_dir="$(rpm --eval "%{_rpmdir}")"
        rpmbuild --rebuild /work/*.src.rpm
        cp "$rpm_dir"/*/rufin-*.rpm /work/
    '

    case "$(uname -s)" in
        CYGWIN*|MINGW*|MSYS*)
            engine_command=(env MSYS2_ARG_CONV_EXCL='*' MSYS_NO_PATHCONV=1 "${engine_command[@]}")
            artifact_dir_for_engine="$(cygpath -am "$artifact_dir")"
            ;;
    esac

    mkdir -p "$artifact_root"
    rm -rf "$artifact_dir"
    mkdir -p "$artifact_dir"

    cargo run --locked -p xtask -- generate rpm-srpm "$tag" --output "$artifact_dir"

    rpm_container="$("${engine_command[@]}" create \
        "${platform_args[@]}" \
        --security-opt label=disable \
        "$image" \
        bash -euc "$rpm_build_command")"
    cleanup_rpm_container() {
        "${engine_command[@]}" rm --force "$rpm_container" >/dev/null 2>&1 || true
    }
    trap cleanup_rpm_container EXIT
    "${engine_command[@]}" cp "$artifact_dir_for_engine" "$rpm_container:/work"
    "${engine_command[@]}" start "$rpm_container" >/dev/null
    "${engine_command[@]}" logs --follow "$rpm_container"
    builder_status="$("${engine_command[@]}" wait "$rpm_container")"
    builder_status="${builder_status##*$'\n'}"
    builder_status="${builder_status//$'\r'/}"
    if [[ "$builder_status" != "0" ]]; then
        echo "The Fedora RPM builder exited with status $builder_status." >&2
        exit 1
    fi
    "${engine_command[@]}" cp "$rpm_container:/work/." "$artifact_dir_for_engine"
    cleanup_rpm_container
    trap - EXIT

    mapfile -t rpms < <(find "$artifact_dir" -maxdepth 1 -type f -name 'rufin-*.rpm' -print)
    if [[ ${#rpms[@]} -eq 0 ]]; then
        echo "The RPM build did not produce an artifact." >&2
        exit 1
    fi
    shopt -s nullglob
    declare -a previous_rpms=(
        "$artifact_root"/rufin-*.src.rpm
        "$artifact_root"/rufin-*."$rpm_arch".rpm
    )
    shopt -u nullglob
    if [[ ${#previous_rpms[@]} -gt 0 ]]; then
        rm -f -- "${previous_rpms[@]}"
    fi
    cp "${rpms[@]}" "$artifact_root/"
    echo "Created $rpm_arch RPMs in $artifact_root"

clean:
    @scripts/container clean

# Run all checks, or only Linux dependency checks with `just check deps`.
check target="":
    @if [[ -z "{{ target }}" ]]; then \
        scripts/container run default none just _check-all; \
    elif [[ "{{ target }}" == "deps" ]]; then \
        scripts/container run default none just _check-deps; \
    else \
        echo "usage: just check [deps]" >&2; \
        exit 2; \
    fi

_check-deps:
    @cargo run --locked -p xtask -- generate linux-packaging --check

_check-all:
    @cargo run --locked -p xtask -- generate flatpak-sources --check
    @cargo run --locked -p xtask -- generate i18n-template --check
    @cargo run --locked -p xtask -- generate linux-packaging --check
    @cargo fmt --all -- --check
    @if command -v ast-grep >/dev/null 2>&1; then \
        just _ast-grep; \
    else \
        echo "ast-grep is unavailable; skipping RefCell checks."; \
    fi
    @just _lint
    @just _test
    @cargo deny --locked check -D unmatched-skip

setup-macos-signing:
    #!/usr/bin/env bash
    set -euo pipefail

    if [[ "$(uname -s)" != Darwin ]]; then
        echo "macOS signing setup must run on macOS." >&2
        exit 1
    fi
    signing_identity="Rufin Development"
    if security find-identity -v -p codesigning \
        | grep -F "$signing_identity" >/dev/null; then
        echo "$signing_identity is already available."
        exit 0
    fi

    openssl_command="$(brew --prefix openssl@3)/bin/openssl"
    keychain_path="$(security default-keychain -d user | tr -d '"')"
    work_dir="$(mktemp -d)"
    trap 'rm -rf "$work_dir"' EXIT
    private_key_path="$work_dir/rufin-development.key.pem"
    certificate_path="$work_dir/rufin-development.cert.pem"

    "$openssl_command" req \
        -newkey rsa:3072 \
        -nodes \
        -x509 \
        -sha256 \
        -days 3650 \
        -subj '/CN=Rufin Development/O=Rufin' \
        -addext 'basicConstraints=critical,CA:TRUE' \
        -addext 'keyUsage=critical,digitalSignature' \
        -addext 'extendedKeyUsage=critical,codeSigning' \
        -keyout "$private_key_path" \
        -out "$certificate_path"
    security import "$private_key_path" \
        -k "$keychain_path" \
        -T /usr/bin/codesign
    security import "$certificate_path" \
        -k "$keychain_path" \
        -T /usr/bin/codesign
    security add-trusted-cert \
        -r trustRoot \
        -p codeSign \
        -k "$keychain_path" \
        "$certificate_path"
    security find-identity -v -p codesigning "$keychain_path" \
        | grep -F "$signing_identity" >/dev/null
    echo "Created $signing_identity."

debug *args:
    @if [[ "${RUFIN_CONTAINER:-0}" == "1" ]]; then \
        echo "Run 'just debug' on the host." >&2; \
        exit 1; \
    fi
    @set -- {{ args }}; \
    if [[ "${1:-}" == "flatpak" ]]; then \
        shift; \
        flatpak run --env=RUST_LOG="${RUST_LOG:-debug}" io.github.screwys.Rufin "$@" 2>&1; \
    else \
        if [[ "$(uname -s)" == Darwin ]]; then \
            brew_prefix="$(brew --prefix)"; \
            export GIO_MODULE_DIR="${brew_prefix}/lib/gio/modules"; \
            export GSETTINGS_SCHEMA_DIR="${brew_prefix}/share/glib-2.0/schemas"; \
            export XDG_DATA_DIRS="${brew_prefix}/share${XDG_DATA_DIRS:+:${XDG_DATA_DIRS}}"; \
            if [[ -z "${RUFIN_MACOS_SIGN_IDENTITY:-}" ]]; then \
                just setup-macos-signing; \
            fi; \
            target_dir="${CARGO_TARGET_DIR:-$PWD/target}"; \
            cargo build --locked -p rufin --features development; \
            signing_args=( \
                --force \
                --sign "${RUFIN_MACOS_SIGN_IDENTITY:-Rufin Development}" \
            ); \
            if [[ -n "${RUFIN_MACOS_SIGN_KEYCHAIN:-}" ]]; then \
                signing_args+=(--keychain "$RUFIN_MACOS_SIGN_KEYCHAIN"); \
            fi; \
            codesign "${signing_args[@]}" \
                --identifier io.github.screwys.Rufin.Devel \
                "$target_dir/debug/rufin"; \
            RUST_LOG="${RUST_LOG:-debug}" "$target_dir/debug/rufin" "$@"; \
        else \
            RUST_LOG="${RUST_LOG:-debug}" \
                cargo run --locked -p rufin --features development -- "$@"; \
        fi; \
    fi

fmt:
    @scripts/container run default none cargo fmt --all

test *args:
    @scripts/container run default none just _test {{ args }}

_test *args:
    @if command -v cargo-nextest >/dev/null 2>&1; then \
        nextest_jobs="${NEXTEST_JOBS:-4}"; \
        if [[ ! "$nextest_jobs" =~ ^[1-9][0-9]*$ ]]; then \
            echo "NEXTEST_JOBS must be a positive integer." >&2; \
            exit 1; \
        fi; \
        cargo nextest run --locked --test-threads "$nextest_jobs" {{ args }}; \
    else \
        cargo_args=(--locked); \
        if [[ -z "{{ args }}" ]]; then \
            cargo_args+=(--lib --bins --tests --benches --examples); \
        fi; \
        echo "cargo-nextest is unavailable; falling back to cargo test." >&2; \
        cargo test "${cargo_args[@]}" {{ args }}; \
    fi

container action="status":
    @scripts/container {{ action }}

_ast-grep:
    @ast-grep test --skip-snapshot-tests
    @ast-grep scan --error crates

_lint:
    @cargo clippy --workspace --all-targets --locked

# Regenerate Linux package dependency metadata.
deps:
    @scripts/container run default none just _deps

_deps:
    @cargo run --locked -p xtask -- generate linux-packaging

_build-dmg identity="development":
    #!/usr/bin/env bash
    set -euo pipefail

    repo_root="$PWD"
    build_identity="{{ identity }}"
    case "$build_identity" in
        development)
            app_id="io.github.screwys.Rufin.Devel"
            bundle_name="Rufin.Devel"
            cargo_features=(--features development)
            ;;
        stable)
            app_id="io.github.screwys.Rufin"
            bundle_name="Rufin"
            cargo_features=()
            ;;
        *)
            echo "macOS build identity must be 'development' or 'stable'." >&2
            exit 2
            ;;
    esac
    if [[ "$build_identity" == stable \
        && ( -z "${RUFIN_MACOS_SIGN_IDENTITY:-}" \
            || "${RUFIN_MACOS_SIGN_IDENTITY}" == "-" ) ]]; then
        echo "Stable macOS builds require RUFIN_MACOS_SIGN_IDENTITY." >&2
        exit 1
    fi
    if [[ "$build_identity" == development \
        && -z "${RUFIN_MACOS_SIGN_IDENTITY:-}" ]]; then
        just setup-macos-signing
    fi
    signing_args=(
        --force
        --sign "${RUFIN_MACOS_SIGN_IDENTITY:-Rufin Development}"
    )
    if [[ -n "${RUFIN_MACOS_SIGN_KEYCHAIN:-}" ]]; then
        signing_args+=(--keychain "$RUFIN_MACOS_SIGN_KEYCHAIN")
    fi
    artifact_root="${RUFIN_ARTIFACT_ROOT:-${repo_root}/.local/artifacts}"
    work_root="${repo_root}/.local/build/macos"
    target_dir="${CARGO_TARGET_DIR:-${work_root}/target}"
    app_path="${work_root}/${bundle_name}.app"
    dmg_root="${work_root}/dmg"
    dmg_path="${RUFIN_DMG_ARTIFACT:-${artifact_root}/${bundle_name}.dmg}"

    mkdir -p "$work_root"
    mkdir -p "$(dirname "$dmg_path")"

    mach_o_dependencies() {
        otool -L "$1" \
            | sed -n 's/^[[:space:]][[:space:]]*\([^[:space:]]*\).*/\1/p'
    }

    mach_o_install_name() {
        otool -D "$1" 2>/dev/null \
            | sed -n '2s/^[[:space:]]*//p'
    }

    mach_o_rpaths() {
        otool -l "$1" \
            | awk '
                $1 == "cmd" && $2 == "LC_RPATH" {
                    reading_rpath = 1
                    next
                }
                reading_rpath && $1 == "path" {
                    print $2
                    reading_rpath = 0
                }
            '
    }

    prepare_mach_o_for_bundle() {
        local bundled_file="$1"
        local architectures
        local bundled_mode
        local thin_file
        local rpath

        architectures="$(lipo -archs "$bundled_file")"
        if [[ "$architectures" != "$package_arch" ]]; then
            thin_file="${bundled_file}.rufin-thin"
            bundled_mode="$(stat -f '%Lp' "$bundled_file")"
            lipo "$bundled_file" -thin "$package_arch" -output "$thin_file"
            chmod "$bundled_mode" "$thin_file"
            mv "$thin_file" "$bundled_file"
        fi

        while IFS= read -r rpath; do
            while mach_o_rpaths "$bundled_file" | grep -Fx "$rpath" >/dev/null; do
                install_name_tool -delete_rpath "$rpath" "$bundled_file"
            done
            install_name_tool -add_rpath "$rpath" "$bundled_file"
        done < <(mach_o_rpaths "$bundled_file" | LC_ALL=C sort -u)
    }

    brew_prefix="$(brew --prefix)"
    gstreamer_plugins="$(pkg-config --variable=pluginsdir gstreamer-1.0)"
    pixbuf_loaders="$(pkg-config --variable=gdk_pixbuf_moduledir gdk-pixbuf-2.0)"
    plugin_scanner_dir="$(pkg-config --variable=pluginscannerdir gstreamer-1.0)"
    plugin_scanner="${plugin_scanner_dir}/gst-plugin-scanner"
    libsoup_libdir="$(pkg-config --variable=libdir libsoup-3.0)"
    libsoup_library="${libsoup_libdir}/libsoup-3.0.0.dylib"
    libsoup_bundle_input="${work_root}/libsoup-3.0.0.dylib"

    gstreamer_version="$(pkg-config --modversion gstreamer-1.0)"
    wavpack_source_archive="$work_root/gst-plugins-good-${gstreamer_version}.tar.xz"
    wavpack_source_checksum="${wavpack_source_archive}.sha256sum"
    wavpack_source_dir="$work_root/gst-plugins-good-${gstreamer_version}"
    wavpack_build_dir="$work_root/gst-plugins-good-build"
    wavpack_source_url="https://gstreamer.freedesktop.org/src/gst-plugins-good"

    curl --fail --location --silent --show-error --retry 3 \
        "$wavpack_source_url/$(basename "$wavpack_source_archive")" \
        --output "$wavpack_source_archive"
    curl --fail --location --silent --show-error --retry 3 \
        "$wavpack_source_url/$(basename "$wavpack_source_checksum")" \
        --output "$wavpack_source_checksum"
    (
        cd "$work_root"
        shasum -a 256 --check "$(basename "$wavpack_source_checksum")"
    )
    rm -rf "$wavpack_source_dir" "$wavpack_build_dir"
    tar -xJf "$wavpack_source_archive" -C "$work_root"
    meson setup \
        "$wavpack_build_dir" \
        "$wavpack_source_dir" \
        -Dauto_features=disabled \
        -Dwavpack=enabled \
        --buildtype=release
    meson compile -C "$wavpack_build_dir" gstwavpack
    wavpack_plugin="$wavpack_build_dir/ext/wavpack/libgstwavpack.dylib"

    extra_audio_source_archive="$work_root/gst-plugins-bad-${gstreamer_version}.tar.xz"
    extra_audio_source_checksum="${extra_audio_source_archive}.sha256sum"
    extra_audio_source_dir="$work_root/gst-plugins-bad-${gstreamer_version}"
    extra_audio_build_dir="$work_root/gst-plugins-bad-build"
    extra_audio_source_url="https://gstreamer.freedesktop.org/src/gst-plugins-bad"

    curl --fail --location --silent --show-error --retry 3 \
        "$extra_audio_source_url/$(basename "$extra_audio_source_archive")" \
        --output "$extra_audio_source_archive"
    curl --fail --location --silent --show-error --retry 3 \
        "$extra_audio_source_url/$(basename "$extra_audio_source_checksum")" \
        --output "$extra_audio_source_checksum"
    (
        cd "$work_root"
        shasum -a 256 --check "$(basename "$extra_audio_source_checksum")"
    )
    rm -rf "$extra_audio_source_dir" "$extra_audio_build_dir"
    tar -xJf "$extra_audio_source_archive" -C "$work_root"
    gme_prefix="$(brew --prefix game-music-emu)"
    openmpt_prefix="$(brew --prefix libopenmpt)"
    CFLAGS="-I${gme_prefix}/include ${CFLAGS:-}" \
    LDFLAGS="-L${gme_prefix}/lib ${LDFLAGS:-}" \
    PKG_CONFIG_PATH="${openmpt_prefix}/lib/pkgconfig:${PKG_CONFIG_PATH:-}" \
        meson setup \
        "$extra_audio_build_dir" \
        "$extra_audio_source_dir" \
        -Dauto_features=disabled \
        -Dgme=enabled \
        -Dopenmpt=enabled \
        --buildtype=release
    meson compile -C "$extra_audio_build_dir" gstgme gstopenmpt
    gme_plugin="$extra_audio_build_dir/ext/gme/libgstgme.dylib"
    openmpt_plugin="$extra_audio_build_dir/ext/openmpt/libgstopenmpt.dylib"

    version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "${repo_root}/Cargo.toml" | head -n 1)"
    deployment_target="${MACOSX_DEPLOYMENT_TARGET:-15.0}"
    package_arch="$(uname -m)"

    rm -rf "$app_path" "$dmg_root"
    mkdir -p \
        "$app_path/Contents/Frameworks" \
        "$app_path/Contents/MacOS" \
        "$app_path/Contents/Resources/lib/gdk-pixbuf-2.0/loaders" \
        "$app_path/Contents/Resources/lib/gio/modules" \
        "$app_path/Contents/Resources/lib/gstreamer-1.0" \
        "$app_path/Contents/Resources/share"

    MACOSX_DEPLOYMENT_TARGET="$deployment_target" \
        CARGO_TARGET_DIR="$target_dir" \
        cargo build --locked --release -p rufin "${cargo_features[@]}"
    cp "$target_dir/release/rufin" "$app_path/Contents/MacOS/rufin"
    cp "$plugin_scanner" "$app_path/Contents/MacOS/gst-plugin-scanner"
    cp "$(brew --prefix gdk-pixbuf)/bin/gdk-pixbuf-query-loaders" \
        "$app_path/Contents/MacOS/gdk-pixbuf-query-loaders"
    chmod +x "$app_path/Contents/MacOS/"*
    sed \
        -e "s/@VERSION@/${version}/g" \
        -e "s/@MINIMUM_SYSTEM_VERSION@/${deployment_target}/g" \
        -e "s/@BUNDLE_IDENTIFIER@/${app_id}/g" \
        -e "s/@BUNDLE_NAME@/${bundle_name}/g" \
        "${repo_root}/packaging/macos/Info.plist.in" \
        >"$app_path/Contents/Info.plist"

    copy_directory() {
        local source_path="$1"
        local destination_path="$2"
        if [[ -d "$source_path" ]]; then
            mkdir -p "$destination_path"
            cp -R -L "$source_path"/. "$destination_path"/
        fi
    }

    gstreamer_plugin_names=(
        libgstaiff.dylib
        libgstalaw.dylib
        libgstapetag.dylib
        libgstasf.dylib
        libgstaudioconvert.dylib
        libgstaudiofx.dylib
        libgstaudioparsers.dylib
        libgstaudioresample.dylib
        libgstautodetect.dylib
        libgstcoreelements.dylib
        libgstequalizer.dylib
        libgstfdkaac.dylib
        libgstflac.dylib
        libgstid3demux.dylib
        libgstisomp4.dylib
        libgstlevel.dylib
        libgstlibav.dylib
        libgstmatroska.dylib
        libgstmpg123.dylib
        libgstmusepack.dylib
        libgstmulaw.dylib
        libgstogg.dylib
        libgstopus.dylib
        libgstopusparse.dylib
        libgstosxaudio.dylib
        libgstpbtypes.dylib
        libgstplayback.dylib
        libgstreplaygain.dylib
        libgstsoup.dylib
        libgstspeex.dylib
        libgsttypefindfunctions.dylib
        libgstvolume.dylib
        libgstvorbis.dylib
        libgstwavparse.dylib
    )
    for plugin_name in "${gstreamer_plugin_names[@]}"; do
        plugin_path="$gstreamer_plugins/$plugin_name"
        cp -L "$plugin_path" \
            "$app_path/Contents/Resources/lib/gstreamer-1.0/$plugin_name"
    done
    cp -L "$wavpack_plugin" \
        "$app_path/Contents/Resources/lib/gstreamer-1.0/libgstwavpack.dylib"
    cp -L "$gme_plugin" \
        "$app_path/Contents/Resources/lib/gstreamer-1.0/libgstgme.dylib"
    cp -L "$openmpt_plugin" \
        "$app_path/Contents/Resources/lib/gstreamer-1.0/libgstopenmpt.dylib"
    cp -L "$libsoup_library" "$libsoup_bundle_input"
    copy_directory "$pixbuf_loaders" \
        "$app_path/Contents/Resources/lib/gdk-pixbuf-2.0/loaders"
    copy_directory "${brew_prefix}/lib/gio/modules" \
        "$app_path/Contents/Resources/lib/gio/modules"
    copy_directory "${brew_prefix}/share/glib-2.0/schemas" \
        "$app_path/Contents/Resources/share/glib-2.0/schemas"
    copy_directory "${brew_prefix}/share/gstreamer-1.0" \
        "$app_path/Contents/Resources/share/gstreamer-1.0"
    copy_directory "${brew_prefix}/share/gtk-4.0" \
        "$app_path/Contents/Resources/share/gtk-4.0"
    copy_directory "${brew_prefix}/share/icons/Adwaita" \
        "$app_path/Contents/Resources/share/icons/Adwaita"
    copy_directory "${brew_prefix}/share/icons/AdwaitaLegacy" \
        "$app_path/Contents/Resources/share/icons/AdwaitaLegacy"
    copy_directory "${brew_prefix}/share/icons/hicolor" \
        "$app_path/Contents/Resources/share/icons/hicolor"
    copy_directory "${brew_prefix}/share/mime" \
        "$app_path/Contents/Resources/share/mime"

    mkdir -p \
        "$app_path/Contents/Resources/share/rufin" \
        "$app_path/Contents/Resources/share/licenses/rufin"
    cp "${repo_root}/data/japanese-readings.dic" \
        "$app_path/Contents/Resources/share/rufin/japanese-readings.dic"
    cp "${repo_root}/data/japanese-readings.LICENSE" \
        "$app_path/Contents/Resources/share/licenses/rufin/japanese-readings.LICENSE"
    cp "${repo_root}/LICENSE" "$app_path/Contents/Resources/LICENSE"

    for po_file in "${repo_root}"/crates/localization/locales/*.po; do
        language="$(basename "$po_file" .po)"
        copy_directory \
            "${brew_prefix}/share/locale/${language}" \
            "$app_path/Contents/Resources/share/locale/${language}"
        locale_dir="$app_path/Contents/Resources/share/locale/${language}/LC_MESSAGES"
        mkdir -p "$locale_dir"
        msgfmt --check "$po_file" -o "$locale_dir/rufin.mo"
    done

    glib-compile-schemas "$app_path/Contents/Resources/share/glib-2.0/schemas"
    gio-querymodules "$app_path/Contents/Resources/lib/gio/modules"

    iconset="$work_root/Rufin.iconset"
    rm -rf "$iconset"
    mkdir -p "$iconset"
    icon_source="${repo_root}/data/icons/hicolor/scalable/apps/io.github.screwys.Rufin.svg"
    for icon_size in 16 32 128 256 512; do
        rsvg-convert -w "$icon_size" -h "$icon_size" "$icon_source" \
            >"$iconset/icon_${icon_size}x${icon_size}.png"
        doubled_size=$((icon_size * 2))
        rsvg-convert -w "$doubled_size" -h "$doubled_size" "$icon_source" \
            >"$iconset/icon_${icon_size}x${icon_size}@2x.png"
    done
    iconutil -c icns "$iconset" -o "$app_path/Contents/Resources/Rufin.icns"

    files_to_fix=(
        "$libsoup_bundle_input"
        "$app_path/Contents/MacOS/rufin"
        "$app_path/Contents/MacOS/gdk-pixbuf-query-loaders"
        "$app_path/Contents/MacOS/gst-plugin-scanner"
    )
    while IFS= read -r -d '' module_path; do
        files_to_fix+=("$module_path")
    done < <(
        find \
            "$app_path/Contents/Resources/lib/gdk-pixbuf-2.0/loaders" \
            "$app_path/Contents/Resources/lib/gio/modules" \
            "$app_path/Contents/Resources/lib/gstreamer-1.0" \
            -type f \( -name '*.dylib' -o -name '*.so' \) -print0
    )

    dependency_sources="$work_root/dependency-sources"
    rm -rf "$dependency_sources"
    mkdir -p "$dependency_sources"
    while IFS= read -r -d '' dependency; do
        ln -s "$dependency" "$dependency_sources/$(basename "$dependency")"
    done < <(find -L "${brew_prefix}/lib" -maxdepth 1 -type f -name '*.dylib' -print0)

    dylib_args=(-od -b -d "$app_path/Contents/Frameworks" -p '@executable_path/../Frameworks/')
    for file_to_fix in "${files_to_fix[@]}"; do
        dylib_args+=(-x "$file_to_fix")
    done
    dylibbundler "${dylib_args[@]}" -s "$dependency_sources" -ns
    cp "$libsoup_bundle_input" \
        "$app_path/Contents/Frameworks/libsoup-3.0.0.dylib"

    bundled_rufin_binary="$app_path/Contents/MacOS/rufin"
    if ! mach_o_rpaths "$bundled_rufin_binary" | grep -Fx '/usr/lib/swift' >/dev/null; then
        install_name_tool -add_rpath '/usr/lib/swift' "$bundled_rufin_binary"
    fi

    while IFS= read -r -d '' bundled_file; do
        if file "$bundled_file" | grep -q 'Mach-O'; then
            prepare_mach_o_for_bundle "$bundled_file"
            codesign "${signing_args[@]}" "$bundled_file"
            install_name="$(mach_o_install_name "$bundled_file" || true)"
            external_dependencies="$(mach_o_dependencies "$bundled_file" \
                | grep -vFx "$install_name" \
                | grep '^/' \
                | grep -Ev '^(/usr/lib/|/System/Library/)' || true)"
            if [[ -n "$external_dependencies" ]]; then
                echo "$external_dependencies" >&2
                echo "External library path remains in ${bundled_file}." >&2
                exit 1
            fi
        fi
    done < <(find "$app_path/Contents" -type f -print0)
    codesign "${signing_args[@]}" --identifier "$app_id" "$app_path"
    codesign --verify --deep --strict "$app_path"

    mkdir -p "$dmg_root"
    ditto "$app_path" "$dmg_root/${bundle_name}.app"
    ln -s /Applications "$dmg_root/Applications"
    hdiutil create -volname "$bundle_name" -srcfolder "$dmg_root" -ov -format UDZO "$dmg_path"

    echo "Built ${dmg_path}"

_build-flatpak:
    #!/usr/bin/env bash
    set -euo pipefail

    repo_root="$PWD"
    artifact_root="${RUFIN_ARTIFACT_ROOT:-${repo_root}/.local/artifacts}"
    work_root="${repo_root}/.local/build/flatpak"
    build_dir="${work_root}/build"
    repository_dir="${work_root}/repo"
    state_dir="${work_root}/state"
    bundle_path="${artifact_root}/io.github.screwys.Rufin.flatpak"
    temporary_bundle_path="${bundle_path}.new"
    work_bundle_path="${work_root}/io.github.screwys.Rufin.flatpak"
    manifest="${repo_root}/packaging/flatpak/io.github.screwys.Rufin.json"
    runtime_repo_url=https://flathub.org/repo/flathub.flatpakrepo
    declare -a builder_args=()

    if [[ "${RUFIN_CONTAINER:-0}" == "1" ]]; then
        if [[ "${RUFIN_CONTAINER_NESTED_SANDBOX:-0}" != "1" ]]; then
            echo "The Flatpak build needs the command-scoped nested sandbox profile. Run 'just build flatpak' from the host." >&2
            exit 1
        fi
        if [[ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]]; then
            exec dbus-run-session -- "$0" "$@"
        fi

        # Flatpak 1.16 otherwise probes the unavailable system bus for parental controls.
        export FLATPAK_SYSTEM_HELPER_ON_SESSION=container
        builder_args+=(--disable-rofiles-fuse)
    fi

    mkdir -p "$artifact_root" "$work_root"
    rm -f "$temporary_bundle_path" "$work_bundle_path"

    flatpak remote-add --user --if-not-exists flathub "$runtime_repo_url"
    flatpak-builder \
        "${builder_args[@]}" \
        --user \
        --install-deps-from=flathub \
        --repo="$repository_dir" \
        --state-dir="$state_dir" \
        --force-clean \
        "$build_dir" \
        "$manifest"
    flatpak build-update-repo "$repository_dir"
    flatpak build-bundle \
        --runtime-repo="$runtime_repo_url" \
        "$repository_dir" \
        "$work_bundle_path" \
        io.github.screwys.Rufin \
        master

    cp "$work_bundle_path" "$temporary_bundle_path"
    mv -f "$temporary_bundle_path" "$bundle_path"
    echo "Built ${bundle_path}"

_build-windows:
    #!/usr/bin/env bash
    set -euo pipefail

    repo_root="$PWD"
    artifact_root="${RUFIN_ARTIFACT_ROOT:-${repo_root}/.local/artifacts}"
    work_root="${repo_root}/.local/build/windows"
    target_dir="${CARGO_TARGET_DIR:-${work_root}/target}"
    stage_dir="${work_root}/Rufin"
    output_dir="${work_root}/output"

    version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$repo_root/Cargo.toml" | head -n 1)"
    version_base="${version%%-*}"
    IFS=. read -r version_major version_minor version_patch <<<"$version_base"
    version_quad="${version_major:-0}.${version_minor:-0}.${version_patch:-0}.0"

    mkdir -p "$artifact_root" "$output_dir"
    installer="$artifact_root/Rufin-${version}-setup.exe"
    work_installer="$output_dir/Rufin-${version}-setup.exe"
    rm -f "$work_installer"
    CARGO_TARGET_DIR="$target_dir" cargo build --locked --release -p rufin --bin rufin
    CARGO_TARGET_DIR="$target_dir" \
        cargo build --locked --release -p windows-updater --bin rufin-update-helper

    runtime_prefix="$MINGW_PREFIX"
    binary="$target_dir/release/rufin.exe"
    update_helper="$target_dir/release/rufin-update-helper.exe"

    copy_file() {
        local source_path="$1"
        local destination_path="$2"
        if [[ -f "$source_path" || -L "$source_path" ]]; then
            mkdir -p "$(dirname "$destination_path")"
            cp -L "$source_path" "$destination_path"
        fi
    }

    copy_directory() {
        local source_path="$1"
        local destination_path="$2"
        if [[ -d "$source_path" ]]; then
            mkdir -p "$destination_path"
            cp -R -L "$source_path"/. "$destination_path"/
        fi
    }

    objdump_command="$runtime_prefix/bin/objdump.exe"
    msgfmt_command="$runtime_prefix/bin/msgfmt.exe"
    schema_command="$runtime_prefix/bin/glib-compile-schemas.exe"
    icon_cache_command="$runtime_prefix/bin/gtk4-update-icon-cache.exe"

    copy_dependency_closure() {
        local dependency_root="$1"
        local dependency_destination="$2"
        local dependency_owner dependency_output dependency_name dependency_source
        local dependency_index=0
        local -a dependency_queue=()
        mapfile -d '' dependency_queue < <(
            find "$dependency_root" -type f \( -iname '*.dll' -o -iname '*.exe' \) -print0
        )
        while (( dependency_index < ${#dependency_queue[@]} )); do
            dependency_owner="${dependency_queue[$dependency_index]}"
            dependency_index=$((dependency_index + 1))
            dependency_output="$("$objdump_command" -p "$dependency_owner")"
            while IFS= read -r dependency_name; do
                [[ -n "$dependency_name" ]] || continue
                if find "$dependency_destination" \
                    -maxdepth 1 \
                    -type f \
                    -iname "$dependency_name" \
                    -print \
                    -quit | grep -q .; then
                    continue
                fi
                dependency_source="$(
                    find "$runtime_prefix/bin" \
                        -maxdepth 1 \
                        -type f \
                        -iname "$dependency_name" \
                        -print \
                        -quit 2>/dev/null || true
                )"
                [[ -n "$dependency_source" ]] || continue
                cp -L "$dependency_source" "$dependency_destination/$(basename "$dependency_source")"
                dependency_queue+=("$dependency_destination/$(basename "$dependency_source")")
            done < <(
                sed -n 's/^[[:space:]]*DLL Name:[[:space:]]*//p' <<<"$dependency_output" \
                    | tr -d '\r'
            )
        done
    }

    rm -rf "$stage_dir"
    app_bin="$stage_dir/bin"
    app_share="$stage_dir/share"
    mkdir -p "$app_bin"
    cp "$binary" "$app_bin/rufin.exe"
    copy_file "$repo_root/LICENSE" "$stage_dir/LICENSE"
    copy_file "$repo_root/packaging/windows/assets/rufin.ico" "$stage_dir/rufin.ico"
    copy_file \
        "$repo_root/data/japanese-readings.dic" \
        "$app_share/rufin/japanese-readings.dic"
    copy_file \
        "$repo_root/data/japanese-readings.LICENSE" \
        "$app_share/licenses/rufin/japanese-readings.LICENSE"
    copy_file \
        "$repo_root/data/io.github.screwys.Rufin.desktop" \
        "$app_share/applications/io.github.screwys.Rufin.desktop"
    copy_file \
        "$repo_root/data/io.github.screwys.Rufin.metainfo.xml" \
        "$app_share/metainfo/io.github.screwys.Rufin.metainfo.xml"

    for helper in gspawn-win64-helper.exe gspawn-win64-helper-console.exe; do
        copy_file "$runtime_prefix/bin/$helper" "$app_bin/$helper"
    done

    gstreamer_plugins="$stage_dir/lib/gstreamer-1.0"
    mkdir -p "$gstreamer_plugins"
    gstreamer_plugin_names=(
        libgstaiff.dll
        libgstalaw.dll
        libgstapetag.dll
        libgstasf.dll
        libgstaudioconvert.dll
        libgstaudiofx.dll
        libgstaudioparsers.dll
        libgstaudioresample.dll
        libgstautodetect.dll
        libgstcoreelements.dll
        libgstdirectsound.dll
        libgstequalizer.dll
        libgstfdkaac.dll
        libgstflac.dll
        libgstgme.dll
        libgstid3demux.dll
        libgstisomp4.dll
        libgstlevel.dll
        libgstlibav.dll
        libgstmatroska.dll
        libgstmpg123.dll
        libgstmusepack.dll
        libgstmulaw.dll
        libgstogg.dll
        libgstopenmpt.dll
        libgstopus.dll
        libgstopusparse.dll
        libgstpbtypes.dll
        libgstplayback.dll
        libgstreplaygain.dll
        libgstsoup.dll
        libgstspeex.dll
        libgsttypefindfunctions.dll
        libgstvolume.dll
        libgstvorbis.dll
        libgstwasapi.dll
        libgstwavpack.dll
        libgstwavparse.dll
    )
    for plugin_name in "${gstreamer_plugin_names[@]}"; do
        cp -L \
            "$runtime_prefix/lib/gstreamer-1.0/$plugin_name" \
            "$gstreamer_plugins/$plugin_name"
    done

    copy_directory "$runtime_prefix/lib/gdk-pixbuf-2.0" "$stage_dir/lib/gdk-pixbuf-2.0"
    copy_directory "$runtime_prefix/lib/gio/modules" "$stage_dir/lib/gio/modules"
    copy_directory \
        "$runtime_prefix/libexec/gstreamer-1.0" \
        "$stage_dir/libexec/gstreamer-1.0"
    find "$stage_dir/lib" -type f -name '*.dll.a' -delete
    copy_dependency_closure "$stage_dir" "$app_bin"

    updater_dir="$stage_dir/updater/$version"
    mkdir -p "$updater_dir"
    cp "$update_helper" "$updater_dir/rufin-update-helper.exe"
    printf 'rufin-update-helper:%s\n' "$version" \
        >"$updater_dir/rufin-update-helper.complete"
    copy_dependency_closure "$updater_dir" "$updater_dir"

    copy_directory "$runtime_prefix/share/glib-2.0/schemas" "$app_share/glib-2.0/schemas"
    copy_directory "$runtime_prefix/share/gstreamer-1.0" "$app_share/gstreamer-1.0"
    copy_directory "$runtime_prefix/share/gtk-4.0" "$app_share/gtk-4.0"
    copy_directory "$runtime_prefix/share/icons/Adwaita" "$app_share/icons/Adwaita"
    copy_directory "$runtime_prefix/share/icons/AdwaitaLegacy" "$app_share/icons/AdwaitaLegacy"
    copy_directory "$runtime_prefix/share/icons/hicolor" "$app_share/icons/hicolor"
    copy_directory "$repo_root/data/icons/hicolor" "$app_share/icons/hicolor"
    copy_directory "$runtime_prefix/share/mime" "$app_share/mime"
    copy_directory "$runtime_prefix/share/themes" "$app_share/themes"
    copy_directory "$runtime_prefix/share/licenses" "$app_share/licenses"

    for po_file in "$repo_root"/crates/localization/locales/*.po; do
        language="$(basename "$po_file" .po)"
        copy_directory \
            "$runtime_prefix/share/locale/$language" \
            "$app_share/locale/$language"
        locale_dir="$app_share/locale/$language/LC_MESSAGES"
        mkdir -p "$locale_dir"
        "$msgfmt_command" --check "$po_file" -o "$locale_dir/rufin.mo"
    done
    "$schema_command" "$app_share/glib-2.0/schemas"

    hicolor_dir="$app_share/icons/hicolor"
    rm -f "$hicolor_dir/icon-theme.cache"
    "$icon_cache_command" -q -t -f "$hicolor_dir"

    settings_dir="$stage_dir/etc/gtk-4.0"
    mkdir -p "$settings_dir"
    printf '%s\n' '[Settings]' 'gtk-font-name=Segoe UI 9' >"$settings_dir/settings.ini"

    stage_argument="$(cygpath -w "$stage_dir")"
    stage_files_argument="${stage_argument}\\*"
    output_argument="$(cygpath -w "$output_dir")"
    asset_argument="$(cygpath -w "$repo_root/packaging/windows/assets")"

    MSYS2_ARG_CONV_EXCL='/D' makensis \
        "/DRUFIN_STAGE_DIR=${stage_argument}" \
        "/DRUFIN_STAGE_FILES=${stage_files_argument}" \
        "/DRUFIN_OUTPUT_DIR=${output_argument}" \
        "/DRUFIN_ASSET_DIR=${asset_argument}" \
        "/DRUFIN_VERSION=${version}" \
        "/DRUFIN_VERSION_QUAD=${version_quad}" \
        "$repo_root/packaging/windows/rufin.nsi"

    cp "$work_installer" "$installer"
    echo "Built ${installer}"

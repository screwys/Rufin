set shell := ["bash", "-euc"]

default:
    @just --list

build target="" architecture="":
    @if [[ -z "{{ target }}" && -z "{{ architecture }}" ]]; then \
        case "$(uname -s)" in \
            Darwin|CYGWIN*|MINGW*|MSYS*) just _build-native-package ;; \
            *) scripts/container run default none just _build ;; \
        esac; \
    elif [[ "{{ target }}" == "arch" && -z "{{ architecture }}" ]]; then \
        scripts/container run default none just _build-arch; \
    elif [[ "{{ target }}" == "rpm" ]]; then \
        scripts/container run packaging engine \
            just _build-rpm "{{ architecture }}"; \
    elif [[ "{{ target }}" == "flatpak" && -z "{{ architecture }}" ]]; then \
        scripts/container run packaging sandbox env FLATPAK_BWRAP=/usr/bin/bwrap \
            just _build-flatpak; \
    else \
        echo "usage: just build [arch|flatpak|rpm [arm]]" >&2; \
        exit 2; \
    fi

_build:
    @preset=development; \
    if [[ "${RUFIN_CONTAINER:-0}" == "1" ]]; then preset=development-container; fi; \
    cmake --preset "$preset"; \
    cmake --build --preset "$preset"

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
    @just _check-cmake
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

_check-cmake:
    @preset=development; \
    if [[ "${RUFIN_CONTAINER:-0}" == "1" ]]; then preset=development-container; fi; \
    cmake --preset "$preset"

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
        -T /usr/bin/codesign
    security import "$certificate_path" \
        -T /usr/bin/codesign
    security add-trusted-cert \
        -r trustRoot \
        -p codeSign \
        "$certificate_path"
    security find-identity -v -p codesigning \
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
        cmake --preset development; \
        cmake --build --preset development; \
        executable="$PWD/.local/build/cmake/development/bin/rufin"; \
        if [[ "$(rustc -vV | sed -n 's/^host: //p')" == *-windows-* ]]; then \
            executable="${executable}.exe"; \
        fi; \
        if [[ "$(uname -s)" == Darwin ]]; then \
            brew_prefix="$(brew --prefix)"; \
            export GIO_MODULE_DIR="${brew_prefix}/lib/gio/modules"; \
            export GSETTINGS_SCHEMA_DIR="${brew_prefix}/share/glib-2.0/schemas"; \
            export XDG_DATA_DIRS="${brew_prefix}/share${XDG_DATA_DIRS:+:${XDG_DATA_DIRS}}"; \
            if [[ -z "${RUFIN_MACOS_SIGN_IDENTITY:-}" ]]; then \
                just setup-macos-signing; \
            fi; \
            signing_args=( \
                --force \
                --sign "${RUFIN_MACOS_SIGN_IDENTITY:-Rufin Development}" \
            ); \
            if [[ -n "${RUFIN_MACOS_SIGN_KEYCHAIN:-}" ]]; then \
                signing_args+=(--keychain "$RUFIN_MACOS_SIGN_KEYCHAIN"); \
            fi; \
            codesign "${signing_args[@]}" \
                --identifier io.github.screwys.Rufin.Devel \
                "$executable"; \
            RUST_LOG="${RUST_LOG:-debug}" "$executable" "$@"; \
        elif [[ "$(uname -s)" == Linux ]]; then \
            data_home="${XDG_DATA_HOME:-${HOME:?}/.local/share}"; \
            mkdir -p "$data_home/applications"; \
            desktop-file-install \
                --dir="$data_home/applications" \
                --set-key=Exec \
                --set-value="$executable" \
                data/io.github.screwys.Rufin.Devel.desktop; \
            install -Dm0644 \
                data/icons/hicolor/scalable/apps/io.github.screwys.Rufin.svg \
                "$data_home/icons/hicolor/scalable/apps/io.github.screwys.Rufin.Devel.svg"; \
            if command -v update-desktop-database >/dev/null 2>&1; then \
                update-desktop-database "$data_home/applications"; \
            fi; \
            RUST_LOG="${RUST_LOG:-debug}" "$executable" "$@"; \
        else \
            RUST_LOG="${RUST_LOG:-debug}" "$executable" "$@"; \
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

_build-native-package identity="development":
    @build_identity="{{ identity }}"; \
    case "$build_identity" in \
        development) preset=development-package ;; \
        stable) preset=release-package ;; \
        *) echo "Native package identity must be 'development' or 'stable'." >&2; exit 2 ;; \
    esac; \
    if [[ "$(uname -s)" == Darwin && "$build_identity" == development \
        && -z "${RUFIN_MACOS_SIGN_IDENTITY:-}" ]]; then \
        just setup-macos-signing; \
    fi; \
    cmake --preset "$preset"; \
    cmake --build --preset "$preset" --target rufin-native-package
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

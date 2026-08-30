Name:           rufin
Version:        0.14.0
Release:        1%{?dist}
Summary:        Native GTK4/libadwaita music player written in Rust

License:        GPL-3.0-or-later AND Apache-2.0 AND BSD-3-Clause AND CC0-1.0 AND CDLA-Permissive-2.0 AND ISC AND MIT AND MPL-2.0 AND NAIST-2003 AND Unicode-3.0 AND Unlicense AND Zlib
URL:            https://github.com/screwys/Rufin
Source0:        Rufin-%{version}.tar.xz
Source1:        Rufin-%{version}-vendor.tar.xz

ExclusiveArch:  x86_64 aarch64

BuildRequires:  appstream
BuildRequires:  cargo-rpm-macros
BuildRequires:  cmake
BuildRequires:  desktop-file-utils
BuildRequires:  gettext
BuildRequires:  ninja-build
BuildRequires:  perl-interpreter
BuildRequires:  pkgconfig(gdk-pixbuf-2.0)
BuildRequires:  pkgconfig(glib-2.0)
BuildRequires:  pkgconfig(gstreamer-1.0)
BuildRequires:  pkgconfig(gstreamer-app-1.0)
BuildRequires:  pkgconfig(gstreamer-audio-1.0)
BuildRequires:  pkgconfig(gstreamer-pbutils-1.0)
BuildRequires:  pkgconfig(gtk4)
BuildRequires:  pkgconfig(libadwaita-1) >= 1.9
BuildRequires:  rust >= 1.95.0

Requires:       hicolor-icon-theme
Recommends:     gstreamer1-plugins-base
Recommends:     gstreamer1-plugins-good
Recommends:     gstreamer1-plugins-bad-free
Recommends:     gstreamer1-plugins-bad-free-extras
Recommends:     gstreamer1-plugins-ugly-free
Recommends:     gstreamer1-plugin-libav

%description
Rufin is a native GTK4/libadwaita music player for Jellyfin, Subsonic,
Navidrome, and local music libraries.

%prep
%autosetup -n Rufin-%{version} -a1
%cargo_prep -v vendor

%build
# aws-lc-sys omits CFLAGS from one compiler probe, so pair Fedora's hardened
# linker flags with the PIE compile flag that probe still needs.
LDFLAGS="%{build_ldflags} -fPIE" \
  %cmake -G Ninja \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX=/usr \
    -DRUFIN_BUILD_IDENTITY=stable \
    -DRUFIN_CARGO_FROZEN=ON \
    -DRUFIN_CARGO_PROFILE=rpm
LDFLAGS="%{build_ldflags} -fPIE" %cmake_build --target rufin
CARGO_HOME=.cargo RUSTC_BOOTSTRAP=1 cargo tree \
  -Zavoid-dev-deps \
  --package rufin \
  --offline \
  --edges=no-build,no-dev,no-proc-macro \
  --target=all \
  --prefix=none \
  --format='{l}: {p}' > LICENSE.dependencies
%cargo_vendor_manifest

%install
DESTDIR=%{buildroot} %cmake_install

%find_lang rufin
find "%{buildroot}%{_datadir}/icons/hicolor" -type f -print \
  | sed "s#^%{buildroot}##" \
  | LC_ALL=C sort >> rufin.lang

%check
desktop-file-validate data/io.github.screwys.Rufin.desktop
appstreamcli validate --no-net data/io.github.screwys.Rufin.metainfo.xml

%files -f rufin.lang
%license LICENSE
%license LICENSE.dependencies
%license cargo-vendor.txt
%license %{_datadir}/licenses/rufin/japanese-readings.LICENSE
%doc README.md
%{_bindir}/rufin
%{_datadir}/rufin/japanese-readings.dic
%{_datadir}/applications/io.github.screwys.Rufin.desktop
%{_metainfodir}/io.github.screwys.Rufin.metainfo.xml

%changelog
* Fri Jul 17 2026 screwy <screwygit@proton.me> - 0.9.0-1
- Initial RPM package

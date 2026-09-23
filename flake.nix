{
  description = "Native music player for Jellyfin, Navidrome/OpenSubsonic, Plex, and Emby servers;  local folders,  WebDAV including a direct Nextcloud browser login path, Samba and NAS shares.";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.crane.url = "github:ipetkov/crane";

  outputs =
    {
      self,
      nixpkgs,
      crane,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          inherit (pkgs) lib;
          craneLib = crane.mkLib pkgs;
          workspaceManifest = builtins.fromTOML (builtins.readFile ./Cargo.toml);
          commonArgs = {
            pname = "rufin";
            version = workspaceManifest.workspace.package.version;

            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [
                ./.cargo
                ./Cargo.lock
                ./Cargo.toml
                ./CMakeLists.txt
                ./LICENSE
                ./README.md
                ./cmake
                ./crates
                ./vendor
                ./resources/icons/hicolor
                ./resources/io.github.screwys.Rufin.desktop
                ./resources/io.github.screwys.Rufin.metainfo.xml
                ./resources/showcase.css
                ./resources/themes
                ./locales
                ./web
              ];
            };

            strictDeps = true;

            nativeBuildInputs = with pkgs; [
              cmake
              gettext
              ninja
              pkg-config
              wrapGAppsHook4
            ];

            buildInputs =
              with pkgs;
              [
                glib
                gtk4
                libadwaita
                glib-networking
              ]
              ++ (with gst_all_1; [
                gstreamer
                gst-plugins-base
                gst-plugins-good
                gst-plugins-bad
                gst-plugins-ugly
                gst-libav
              ]);

            doCheck = false;
            CMAKE_GENERATOR = "Ninja";

            SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
          };

          cargoArtifacts = craneLib.buildDepsOnly (
            commonArgs
            // {
              buildPhaseCargoCommand = "cargo build --release --locked --package rufin";
            }
          );
        in
        rec {
          rufin = craneLib.mkCargoDerivation (
            commonArgs
            // {
              inherit cargoArtifacts;
              doInstallCargoArtifacts = false;
              nativeBuildInputs = commonArgs.nativeBuildInputs ++ [
                craneLib.removeReferencesToRustToolchainHook
                craneLib.removeReferencesToVendoredSourcesHook
              ];

              configurePhase = "cmakeConfigurePhase";
              buildPhaseCargoCommand = "cmake --build . --parallel $NIX_BUILD_CORES";
              installPhaseCommand = "cmake --install .";

              cmakeFlags = [
                (lib.cmakeFeature "RUFIN_BUILD_IDENTITY" "stable")
                (lib.cmakeBool "RUFIN_CARGO_FROZEN" true)
              ];

              postInstall = ''
                substituteInPlace "$out/share/applications/io.github.screwys.Rufin.desktop" \
                  --replace-fail "Exec=rufin" "Exec=$out/bin/rufin"
              '';

              preFixup = ''
                gappsWrapperArgs+=(
                  --set-default RUFIN_LOCALEDIR "$out/share/locale"
                  --set-default SSL_CERT_FILE "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
                )
              '';

              meta = {
                description = "Native music player for Jellyfin, Navidrome/OpenSubsonic, Plex, and Emby servers;  local folders,  WebDAV including a direct Nextcloud browser login path, Samba and NAS shares.";
                homepage = "https://github.com/screwys/Rufin";
                license = lib.licenses.gpl3Plus;
                mainProgram = "rufin";
                platforms = lib.platforms.linux;
              };
            }
          );

          default = rufin;
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/rufin";
          meta.description = self.packages.${system}.default.meta.description;
        };
      });

      checks = forAllSystems (system: {
        default = self.packages.${system}.default;
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          defaultShell = pkgs.mkShell {
            packages =
              with pkgs;
              [
                ast-grep
                cargo
                cargo-deny
                cargo-nextest
                clippy
                cmake
                debugedit
                desktop-file-utils
                fakeroot
                file
                gettext
                git
                just
                jq
                libarchive
                ninja
                pacman
                pkg-config
                rustc
                rustfmt
                wavpack
                zstd
              ]
              ++ (with pkgs; [
                glib
                gtk4
                libadwaita
                glib-networking
              ])
              ++ (with pkgs.gst_all_1; [
                gstreamer
                gst-plugins-base
                gst-plugins-good
                gst-plugins-bad
                gst-plugins-ugly
                gst-libav
              ]);

            GIO_EXTRA_MODULES = "${pkgs.glib-networking}/lib/gio/modules";
          };
        in
        {
          default = defaultShell;
          checks = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              rustfmt
              ast-grep
              just
            ];
          };
          packaging = pkgs.mkShell {
            inputsFrom = [ defaultShell ];
            packages = with pkgs; [
              bubblewrap
              dbus
              docker-client
              flatpak
              flatpak-builder
            ];
          };
        }
      );

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt);
    };
}

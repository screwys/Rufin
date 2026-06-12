{
  description = "Native GTK4 Jellyfin music client";

  nixConfig = {
    extra-substituters = [ "https://screwys-rufin.cachix.org" ];
    extra-trusted-public-keys = [
      "screwys-rufin.cachix.org-1:BOMRVJbl30p0hFwsumnUXNjf88hLOysCSAhNpLhljqA="
    ];
  };

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
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
          workspaceManifest = builtins.fromTOML (builtins.readFile ./Cargo.toml);
          gstRuntimePlugins = with pkgs.gst_all_1; [
            gst-plugins-base
            gst-plugins-good
            gst-plugins-bad
            gst-plugins-ugly
            gst-libav
          ];
        in
        rec {
          rufin = pkgs.rustPlatform.buildRustPackage {
            pname = "rufin";
            version = workspaceManifest.workspace.package.version;

            src = lib.cleanSourceWith {
              src = ./.;
              filter =
                path: type:
                lib.cleanSourceFilter path type
                && !(lib.elem (baseNameOf path) [
                  ".flatpak-builder"
                  ".local"
                  "result"
                  "target"
                ]);
            };

            cargoHash = "sha256-JDsuSG3QRC9Pz4nd65EJ4SoXo3lYMIj7vmgwGI9K9rE=";

            strictDeps = true;

            nativeBuildInputs = with pkgs; [
              gettext
              pkg-config
              wrapGAppsHook4
            ];

            buildInputs =
              with pkgs;
              [
                gdk-pixbuf
                gettext
                gtk4
                libadwaita
              ]
              ++ (with gst_all_1; [
                gstreamer
                gst-plugins-base
              ])
              ++ gstRuntimePlugins;

            cargoBuildFlags = [
              "-p"
              "rufin-app"
            ];

            doCheck = false;

            SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";

            postInstall = ''
              install -Dm644 data/io.github.screwys.Rufin.desktop \
                "$out/share/applications/io.github.screwys.Rufin.desktop"
              substituteInPlace "$out/share/applications/io.github.screwys.Rufin.desktop" \
                --replace-fail "Exec=rufin" "Exec=$out/bin/rufin"
              install -Dm644 data/io.github.screwys.Rufin.metainfo.xml \
                "$out/share/metainfo/io.github.screwys.Rufin.metainfo.xml"
              install -Dm644 data/icons/hicolor/scalable/apps/io.github.screwys.Rufin.svg \
                "$out/share/icons/hicolor/scalable/apps/io.github.screwys.Rufin.svg"
              install -Dm644 -t "$out/share/icons/hicolor/scalable/actions" \
                data/icons/hicolor/scalable/actions/*.svg
              install -Dm644 -t "$out/share/icons/hicolor/scalable/status" \
                data/icons/hicolor/scalable/status/*.svg
              install -Dm644 -t "$out/share/icons/hicolor/512x512/apps" \
                data/icons/hicolor/512x512/apps/*.png
              install -Dm644 -t "$out/share/icons/hicolor/64x64/apps" \
                data/icons/hicolor/64x64/apps/*.png

              for po_file in locales/*.po; do
                if [ -f "$po_file" ]; then
                  lang="$(basename "$po_file" .po)"
                  mkdir -p "$out/share/locale/$lang/LC_MESSAGES"
                  msgfmt "$po_file" -o "$out/share/locale/$lang/LC_MESSAGES/rufin.mo"
                fi
              done
            '';

            preFixup = ''
              gappsWrapperArgs+=(
                --set-default RUFIN_LOCALEDIR "$out/share/locale"
                --set-default SSL_CERT_FILE "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
                --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : "$GST_PLUGIN_SYSTEM_PATH_1_0"
              )
            '';

            meta = {
              description = "Native GTK music client for Jellyfin";
              homepage = "https://github.com/screwys/Rufin";
              license = lib.licenses.gpl3Plus;
              mainProgram = "rufin";
              platforms = lib.platforms.linux;
            };
          };

          default = rufin;
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/rufin";
        };
      });

      checks = forAllSystems (system: {
        default = self.packages.${system}.default;
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        {
          default = pkgs.mkShell {
            packages =
              with pkgs;
              [
                cargo
                cargo-nextest
                clippy
                desktop-file-utils
                gettext
                jq
                pkg-config
                rust-analyzer
                rustc
                rustfmt
              ]
              ++ (with pkgs; [
                gdk-pixbuf
                gtk4
                libadwaita
              ])
              ++ (with pkgs.gst_all_1; [
                gstreamer
                gst-plugins-base
                gst-plugins-good
                gst-plugins-bad
                gst-plugins-ugly
                gst-libav
              ]);

            RUFIN_LOCALEDIR = "locales";
          };
        }
      );

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt-rfc-style);
    };
}

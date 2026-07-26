{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    inputs@{ flake-parts, crane, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } (
      { moduleWithSystem, ... }:
      {
        systems = [
          "x86_64-linux"
          "aarch64-linux"
        ];

        perSystem =
          {
            config,
            pkgs,
            lib,
            ...
          }:
          let
            version = "0.1.0"; # x-release-please-version

            craneLib = crane.mkLib pkgs;
            src = craneLib.cleanCargoSource ./.;

            # Everything the windowing and rendering stack opens at runtime
            # rather than links against, so it has to be in the binary's rpath
            # or the first frame fails with a missing library.
            guiLibraries = with pkgs; [
              libxkbcommon
              vulkan-loader
              wayland
              libxcursor
              libxi
              libx11
              libxcb
            ];

            common = {
              inherit src;
              strictDeps = true;
              nativeBuildInputs = [ pkgs.mold ];
              RUSTFLAGS = "-C link-arg=-fuse-ld=mold";
            };

            daemonArgs = common // {
              pname = "flydigictl";
              inherit version;
            };

            guiArgs = common // {
              pname = "flydigictl-gui";
              inherit version;
              cargoExtraArgs = "--features gui --bin flydigictl-gui";
              nativeBuildInputs = common.nativeBuildInputs ++ [
                pkgs.pkg-config
                pkgs.copyDesktopItems
              ];
              buildInputs = guiLibraries;
              doCheck = false;
            };
          in
          {
            packages.default = craneLib.buildPackage (
              daemonArgs
              // {
                cargoArtifacts = craneLib.buildDepsOnly daemonArgs;

                meta = {
                  description = "Control Flydigi BS series laptop coolers on Linux";
                  license = lib.licenses.mit;
                  mainProgram = "flydigictl";
                  platforms = lib.platforms.linux;
                };
              }
            );

            packages.gui = craneLib.buildPackage (
              guiArgs
              // {
                cargoArtifacts = craneLib.buildDepsOnly guiArgs;

                postFixup = ''
                  patchelf --add-rpath "${lib.makeLibraryPath guiLibraries}" \
                    $out/bin/flydigictl-gui
                '';

                desktopItems = [
                  (pkgs.makeDesktopItem {
                    name = "flydigictl-gui";
                    desktopName = "Flydigi Cooler";
                    comment = "Fan curves and lighting for a Flydigi BS series cooler";
                    exec = "flydigictl-gui";
                    icon = "preferences-system";
                    terminal = false;
                    categories = [
                      "System"
                      "Settings"
                      "HardwareSettings"
                    ];
                  })
                ];

                meta = {
                  description = "Desktop interface for the Flydigi cooler daemon";
                  license = lib.licenses.mit;
                  mainProgram = "flydigictl-gui";
                  platforms = lib.platforms.linux;
                };
              }
            );

            devShells.default = pkgs.mkShell {
              inputsFrom = [
                config.packages.default
                config.packages.gui
              ];
              packages = with pkgs; [
                rust-analyzer
                clippy
                rustfmt
                mold
              ];

              RUSTFLAGS = "-C link-arg=-fuse-ld=mold";

              LD_LIBRARY_PATH = lib.makeLibraryPath guiLibraries;
            };
          };

        flake.nixosModules.default = moduleWithSystem (
          { config, ... }:
          _: {
            imports = [ (import ./nix/module.nix { inherit (config.packages) default gui; }) ];
          }
        );

        flake.homeModules.default = moduleWithSystem (
          { config, ... }: _: { imports = [ (import ./nix/home.nix config.packages.gui) ]; }
        );
      }
    );
}

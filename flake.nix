{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{ flake-parts, ... }:
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
          in
          {
            packages.default = pkgs.rustPlatform.buildRustPackage {
              pname = "flydigictl";
              inherit version;
              src = ./.;
              cargoLock.lockFile = ./Cargo.lock;
              meta = {
                description = "Control Flydigi BS series laptop coolers on Linux";
                license = lib.licenses.mit;
                mainProgram = "flydigictl";
                platforms = lib.platforms.linux;
              };
            };

            # Built apart from the daemon and the CLI: the interface drags in
            # wgpu and a windowing stack, and a headless install should not
            # have to carry either.
            packages.gui = pkgs.rustPlatform.buildRustPackage {
              pname = "flydigictl-gui";
              inherit version;
              src = ./.;
              cargoLock.lockFile = ./Cargo.lock;

              buildFeatures = [ "gui" ];
              cargoBuildFlags = [
                "--bin"
                "flydigictl-gui"
              ];

              # The library tests belong to the plain package, which runs them.
              doCheck = false;

              nativeBuildInputs = with pkgs; [
                pkg-config
                copyDesktopItems
              ];
              buildInputs = guiLibraries;

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
            };

            devShells.default = pkgs.mkShell {
              inputsFrom = [
                config.packages.default
                config.packages.gui
              ];
              packages = with pkgs; [
                rust-analyzer
                clippy
                rustfmt
              ];

              # cargo run inside the shell links against these but has no rpath
              # of its own to fall back on.
              LD_LIBRARY_PATH = lib.makeLibraryPath guiLibraries;
            };
          };

        flake.nixosModules.default = moduleWithSystem (
          { config, ... }:
          _: {
            imports = [ (import ./nix/module.nix { inherit (config.packages) default gui; }) ];
          }
        );
      }
    );
}

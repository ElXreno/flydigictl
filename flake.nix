{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    # Keeps compiled dependencies in the store instead of rebuilding four
    # hundred crates every time a line of this one changes.
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

            # mold links the interface in about a second; the default linker
            # spends ten on it, which is most of a rebuild.
            common = {
              inherit src;
              strictDeps = true;
              nativeBuildInputs = [ pkgs.mold ];
              RUSTFLAGS = "-C link-arg=-fuse-ld=mold";
            };

            # No target filter here: crane hands the same arguments to the
            # test run, and narrowing it to the binaries meant the library's
            # tests silently stopped running in the sandbox.
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
              # The library tests belong to the plain package, which runs them.
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

            # Built apart from the daemon and the CLI: the interface drags in
            # wgpu and a windowing stack, and a headless install should not
            # have to carry either.
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

        # The interface is a desktop application, so its own settings belong to
        # the user rather than to the machine.
        flake.homeModules.default = moduleWithSystem (
          { config, ... }: _: { imports = [ (import ./nix/home.nix config.packages.gui) ]; }
        );
      }
    );
}

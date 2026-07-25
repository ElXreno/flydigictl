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
          {
            packages.default = pkgs.rustPlatform.buildRustPackage {
              pname = "flydigictl";
              version = "0.1.0"; # x-release-please-version
              src = ./.;
              cargoLock.lockFile = ./Cargo.lock;
              meta = {
                description = "Control Flydigi BS series laptop coolers on Linux";
                license = lib.licenses.mit;
                mainProgram = "flydigictl";
                platforms = lib.platforms.linux;
              };
            };

            devShells.default = pkgs.mkShell {
              inputsFrom = [ config.packages.default ];
              packages = with pkgs; [
                rust-analyzer
                clippy
                rustfmt
              ];
            };
          };

        flake.nixosModules.default = moduleWithSystem (
          { config, ... }: _: { imports = [ (import ./nix/module.nix config.packages.default) ]; }
        );
      }
    );
}

flydigictl:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.flydigictl;
in
{
  options.programs.flydigictl = {
    enable = lib.mkEnableOption "Flydigi BS series cooler control";

    package = lib.mkOption {
      type = lib.types.package;
      default = flydigictl;
      description = "The flydigictl package to use.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    services.udev.packages = [
      (pkgs.writeTextFile {
        name = "flydigi-udev-rules";
        destination = "/lib/udev/rules.d/70-flydigi-cooler.rules";
        text = builtins.readFile ./70-flydigi-cooler.rules;
      })
    ];
  };
}

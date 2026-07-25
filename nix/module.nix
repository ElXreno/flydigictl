flydigictl:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.flydigictl;
  daemon = config.services.flydigictl;
  format = pkgs.formats.toml { };
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

  options.services.flydigictl = {
    enable = lib.mkEnableOption "the Flydigi cooler fan curve daemon";

    settings = lib.mkOption {
      type = format.type;
      default = { };
      example = lib.literalExpression ''
        {
          interval_secs = 3;
          hysteresis_rpm = 100;
          sensor = {
            hwmon = "k10temp";
            label = "Tctl";
          };
          curve = [
            { temp_c = 45; rpm = 0; }
            { temp_c = 60; rpm = 1300; }
            { temp_c = 75; rpm = 2400; }
            { temp_c = 85; rpm = 3300; }
          ];
        }
      '';
      description = ''
        Contents of {file}`/etc/flydigictl/config.toml`.

        Because this is generated into the store it is read-only: the daemon
        still accepts changes over its socket and applies them immediately, but
        logs a warning that they are lost on restart.
      '';
    };
  };

  config = lib.mkMerge [
    (lib.mkIf (cfg.enable || daemon.enable) {
      environment.systemPackages = [ cfg.package ];

      services.udev.packages = [
        (pkgs.writeTextFile {
          name = "flydigi-udev-rules";
          destination = "/lib/udev/rules.d/70-flydigi-cooler.rules";
          text = builtins.readFile ./70-flydigi-cooler.rules;
        })
      ];
    })

    (lib.mkIf daemon.enable {
      environment.etc."flydigictl/config.toml".source =
        format.generate "flydigictl-config.toml" daemon.settings;

      systemd.services.flydigictld = {
        description = "Flydigi cooler fan curve daemon";
        wantedBy = [ "multi-user.target" ];
        after = [ "bluetooth.target" ];

        # The cooler comes and goes with its Bluetooth link, so the daemon
        # waits for it rather than failing at startup.
        serviceConfig = {
          ExecStart = "${lib.getExe' cfg.package "flydigictld"}";
          Restart = "on-failure";
          RestartSec = 5;
          RuntimeDirectory = "flydigictl";
          RuntimeDirectoryMode = "0755";

          DevicePolicy = "closed";
          DeviceAllow = [ "char-hidraw rw" ];
          ProtectSystem = "strict";
          ProtectHome = true;
          PrivateTmp = true;
          PrivateNetwork = true;
          NoNewPrivileges = true;
          RestrictAddressFamilies = [ "AF_UNIX" ];
          SystemCallFilter = [ "@system-service" ];
          MemoryDenyWriteExecute = true;
        };
      };
    })
  ];
}

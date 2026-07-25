packages:
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
      default = packages.default;
      description = "The flydigictl package to use.";
    };

    gui = {
      enable = lib.mkEnableOption "the desktop interface";

      package = lib.mkOption {
        type = lib.types.package;
        default = packages.gui;
        description = "The flydigictl-gui package to use.";
      };
    };
  };

  options.services.flydigictl = {
    enable = lib.mkEnableOption "the Flydigi cooler fan curve daemon";

    socketGroup = lib.mkOption {
      type = lib.types.str;
      default = "users";
      description = ''
        Group allowed to talk to the control socket.

        systemd creates the socket with this group and mode 0660, so a desktop
        client can reach a daemon that holds no privileges of its own.
      '';
    };

    settings = lib.mkOption {
      type = format.type;
      default = { };
      example = lib.literalExpression ''
        {
          interval_secs = 3;
          hysteresis_rpm = 100;
          standby = "delayed";
          curves = [
            {
              name = "ram";
              sensor.hwmon = "spd5118";
              panic_c = 80;
              points = [
                { temp_c = 45; rpm = 500; }
                { temp_c = 65; rpm = 2600; }
                { temp_c = 75; rpm = 4000; }
              ];
            }
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
    (lib.mkIf cfg.gui.enable {
      # The interface talks to the daemon and never to the cooler, so it needs
      # nothing beyond the socket group its user is already in.
      environment.systemPackages = [ cfg.gui.package ];
    })

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

      # The daemon reaches the cooler through this group rather than through
      # root, which is what lets it run as a dynamic user.
      users.groups.flydigi = { };

      services.udev.extraRules = ''
        SUBSYSTEM=="hidraw", KERNELS=="*:37D7:*", GROUP="flydigi", MODE="0660"
      '';

      systemd.sockets.flydigictld = {
        description = "Flydigi cooler daemon control socket";
        wantedBy = [ "sockets.target" ];
        socketConfig = {
          ListenStream = "/run/flydigictl/flydigictl.sock";
          SocketMode = "0660";
          SocketGroup = daemon.socketGroup;
          RemoveOnStop = true;
        };
      };

      systemd.services.flydigictld = {
        description = "Flydigi cooler fan curve daemon";
        wantedBy = [ "multi-user.target" ];

        # The socket is not merely an entry point, it is the only one: systemd
        # creates it in a directory the dynamic user cannot write to, so a
        # daemon started without that descriptor has nowhere to listen.
        requires = [ "flydigictld.socket" ];
        after = [
          "flydigictld.socket"
          "bluetooth.target"
        ];

        serviceConfig = {
          ExecStart = lib.getExe' cfg.package "flydigictld";
          Restart = "on-failure";
          RestartSec = 5;

          # No account of its own, no home, no privileges beyond the group that
          # opens the cooler.
          DynamicUser = true;
          SupplementaryGroups = [ "flydigi" ];

          DevicePolicy = "closed";
          DeviceAllow = [ "char-hidraw rw" ];
          ProtectSystem = "strict";
          ProtectHome = true;
          ProtectKernelTunables = true;
          ProtectKernelModules = true;
          ProtectControlGroups = true;
          PrivateTmp = true;
          PrivateNetwork = true;
          NoNewPrivileges = true;
          RestrictAddressFamilies = [ "AF_UNIX" ];
          RestrictNamespaces = true;
          RestrictRealtime = true;
          LockPersonality = true;
          SystemCallFilter = [ "@system-service" ];
          SystemCallArchitectures = "native";
          MemoryDenyWriteExecute = true;
          CapabilityBoundingSet = [ "" ];
        };
      };
    })
  ];
}

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

    nvidia = {
      enable = lib.mkEnableOption ''
        reading an NVIDIA GPU's temperature.

        The reading is taken off the card's own registers, mapped read-only
        through sysfs, because the vendor tool cannot be used for this: opening
        the driver's device nodes takes a runtime power reference, so a card
        polled every few seconds never suspends again. Mapping a BAR bypasses
        the driver entirely, and a suspended card answers with all ones instead
        of waking up.

        This grants group `flydigi` read access to the register aperture of
        every NVIDIA display controller, and needs `iomem=relaxed` on the kernel
        command line, since the driver claims the aperture and the mapping is
        refused otherwise
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
          lights_follow_screens = true;
          lighting = {
            mode = { mode = "effect"; effect = 3; };
            brightness = 60;
            indicators = true;
          };
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
        they last only as long as it runs. Anything that should come back after
        a restart belongs here, `lighting` included - nothing can be read back
        out of the cooler, so what is not declared is not known.
      '';
    };
  };

  config = lib.mkMerge [
    (lib.mkIf cfg.gui.enable {
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

      users.groups.flydigi = { };

      warnings = lib.optional (daemon.nvidia.enable && !(lib.elem "iomem=relaxed" config.boot.kernelParams)) ''
        services.flydigictl.nvidia is on, but the kernel command line has no
        iomem=relaxed. The driver claims the card's registers, so mapping them
        is refused and the GPU curve will never read anything.
      '';

      services.udev.extraRules = ''
        SUBSYSTEM=="hidraw", KERNELS=="*:37D7:*", GROUP="flydigi", MODE="0660"
      ''
      # resource0 is a sysfs attribute, created root-owned and 0600 by the
      # kernel with no way to ask for anything else, so its mode is changed
      # after the fact rather than declared.
      + lib.optionalString daemon.nvidia.enable ''
        SUBSYSTEM=="pci", ATTR{vendor}=="0x10de", ATTR{class}=="0x030000", RUN+="${pkgs.coreutils}/bin/chgrp flydigi /sys$devpath/resource0", RUN+="${pkgs.coreutils}/bin/chmod 0640 /sys$devpath/resource0"
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

        # systemd creates the socket in a directory the dynamic user cannot
        # write to, so a daemon started without that descriptor cannot listen.
        requires = [ "flydigictld.socket" ];
        after = [
          "flydigictld.socket"
          "bluetooth.target"
        ];

        serviceConfig = {
          ExecStart = lib.getExe' cfg.package "flydigictld";
          Restart = "on-failure";
          RestartSec = 5;

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
          NoNewPrivileges = true;

          # No PrivateNetwork: sysfs is tagged by network namespace, so a
          # private one hides every hwmon behind a network device, a Wi-Fi
          # card's temperature among them.
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

# flydigictl

Control Flydigi BS series laptop coolers on Linux.

> **WARNING:** This project is fully vibe-coded with [Claude Opus 5](https://docs.anthropic.com/en/docs/about-claude/models).
> It writes directly to HID devices. Use at your own risk.

## Tested Hardware

| Cooler          | Connection            |
|-----------------|-----------------------|
| Flydigi BS3 Pro | Bluetooth (PID `1004`)|

BS2, BS2 Pro and BS3 share the same protocol and should work over both
Bluetooth and USB, but are untested. BS1 speaks BLE instead of HID and is not
supported. Open an issue with your model either way.

## Requirements

- Linux with `hidraw` support
- The cooler paired through your system's Bluetooth settings
- Read/write access to the hidraw device (via udev rule or root)

## Usage

```console
$ flydigictl list
/dev/hidraw8  BS3 Pro

$ flydigictl status
current 1700 rpm   target 1700 rpm   mode gear   gear quiet (max overclock)

$ flydigictl set 2600
target set to 2600 rpm (firmware ramps at ~60 rpm/s)

$ flydigictl watch -n 3
current 1800 rpm   target 2600 rpm   mode realtime   gear quiet (max overclock)
current 2100 rpm   target 2600 rpm   mode realtime   gear quiet (max overclock)
current 2400 rpm   target 2600 rpm   mode realtime   gear quiet (max overclock)

$ flydigictl auto
released to gear mode
```

`set` holds a fixed speed until you call `auto`, power-cycle the cooler, or
change the gear with the physical button. While it is active the gear LEDs
blink.

## Installation

### NixOS (module)

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flydigictl = {
      url = "github:ElXreno/flydigictl";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, flydigictl, ... }:
    {
      nixosConfigurations."hostname" = nixpkgs.lib.nixosSystem {
        modules = [
          flydigictl.nixosModules.default
          {
            programs.flydigictl.enable = true;
          }
        ];
      };
    };
}
```

The module installs the binary and the udev rules that grant your session
access to the cooler.

### Other distributions

Grab a `.deb`, `.rpm` or tarball from [releases](https://github.com/ElXreno/flydigictl/releases),
or build it yourself:

```console
$ cargo build --release
```

Then install the udev rules manually:

```console
$ sudo tee /etc/udev/rules.d/70-flydigi-cooler.rules <<'EOF'
SUBSYSTEM=="hidraw", ATTRS{idVendor}=="37d7", MODE="0660", TAG+="uaccess"
SUBSYSTEM=="hidraw", KERNELS=="*:37D7:*", MODE="0660", TAG+="uaccess"
EOF
$ sudo udevadm control --reload-rules
$ sudo udevadm trigger --subsystem-match=hidraw
```

The `70-` prefix is not cosmetic: systemd's `73-seat-late.rules` runs the
`uaccess` builtin, so a rule sorting after it tags the device too late to
receive an ACL. The second line matters for Bluetooth: those coolers hang off
`uhid` and have no USB parent to carry `idVendor`.

## Protocol

See [docs/PROTOCOL.md](docs/PROTOCOL.md) for the frame layout, checksum and
command codes, including which ones are verified against hardware.

## Credits

Command codes were taken from [THRM](https://github.com/TIANLI0/THRM) by
TIANLI0 (MIT), which does the same job on Windows with a much larger feature
set.

## License

MIT

## Daemon

`flydigictld` runs a fan curve against the cooler and exposes a socket for
other tools.

```nix
services.flydigictl = {
  enable = true;
  settings = {
    interval_secs = 3;
    hysteresis_rpm = 100;
    sensor = { hwmon = "k10temp"; label = "Tctl"; };
    curve = [
      { temp_c = 45; rpm = 0; }
      { temp_c = 60; rpm = 1300; }
      { temp_c = 75; rpm = 2400; }
      { temp_c = 85; rpm = 3300; }
    ];
  };
};
```

The module writes `/etc/flydigictl/config.toml`. Speeds are interpolated
between curve points, `rpm = 0` stops the fan, and the daemon re-applies the
target whenever the cooler drops back to gear mode - which it does after every
reconnect.

Because a declarative config lives in the store, it cannot be written to. The
daemon notices, keeps runtime changes in memory and says so:

```text
[WARN ] /etc/flydigictl/config.toml is read-only (a NixOS store path, most
        likely) - changes made at runtime apply immediately but are lost when
        the daemon restarts
```

Outside NixOS the same file is writable and changes are saved. Either way the
config is reloaded live: the daemon watches the *directory*, so replacing the
symlink during `nixos-rebuild switch` is picked up, as is a plain editor save.

### Socket

Newline-delimited JSON on `/run/flydigictl/flydigictl.sock`:

```console
$ echo '{"request":"status"}' | socat - UNIX-CONNECT:/run/flydigictl/flydigictl.sock
{"reply":"status","model":"BS3 Pro","connected":true,"temp_c":47,"current_rpm":1100,"target_rpm":900,"manual":false}
```

| Request | Effect |
|---------|--------|
| `{"request":"status"}` | current temperature, speed and mode |
| `{"request":"get_config"}` | config in force, plus whether it can be saved |
| `{"request":"set_config","config":{...}}` | replace the config |
| `{"request":"set_manual","rpm":1500}` | hold a speed; `"rpm":null` returns to the curve |

### Standby

```console
$ flydigictl standby delayed
standby delayed
```

`off`, `instant` and `delayed` decide what the cooler does when the host goes
away - shutting the laptop down, for instance. This is the firmware's own
feature: it stops the fan, blanks both light sources and wakes back up with the
gear it had when the host returns. The setting is stored in the cooler, so it
keeps working with the daemon stopped.

The daemon re-applies `standby` from the config whenever it connects.

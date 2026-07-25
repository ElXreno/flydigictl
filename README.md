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

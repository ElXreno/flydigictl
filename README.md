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
target 2600 rpm

$ flydigictl watch -n 3
current 1800 rpm   target 2600 rpm   mode realtime   gear quiet (max overclock)
current 2100 rpm   target 2600 rpm   mode realtime   gear quiet (max overclock)
current 2400 rpm   target 2600 rpm   mode realtime   gear quiet (max overclock)

$ flydigictl auto
released to gear mode

$ flydigictl sensors
nvidia   0000:01:00.0 (core)     active       46 C
nvidia   0000:01:00.0 (memory)   active       48 C
k10temp  0000:00:18.3            Tctl         59 C
amdgpu   0000:66:00.0            edge         41 C
nvme     0000:05:00.0 (nvme0)    Composite    33 C
spd5118  0000:00:14.0/0050       -            41 C
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
            programs.flydigictl.gui.enable = true;
          }
        ];
      };
    };
}
```

The module installs the binary and the udev rules that grant your session
access to the cooler. `gui.enable` adds the desktop interface, which is built
as a separate package: it drags in wgpu and a windowing stack that a headless
install has no use for.

The interface draws itself rather than through GTK or Qt, so no desktop theme
reaches it: `org.freedesktop.appearance` offers a light or dark preference, an
accent colour and a contrast flag, and no palette at all. Left alone it follows
that preference.

Given colours, it uses them. It reads the first of these it can parse:

| Path | |
|------|--|
| `$FLYDIGICTL_PALETTE` | wherever you point it |
| `~/.config/flydigictl/palette.json` | this application's own |
| `~/.cache/wallust/colors.json` | whatever wallust last generated |
| `~/.cache/wal/colors.json` | the same from pywal |

Its own file names the six colours it uses:

```json
{
  "background": "#1f2430",
  "text":       "#cccac2",
  "primary":    "#73d0ff",
  "success":    "#d5ff80",
  "warning":    "#ffd173",
  "danger":     "#f28779"
}
```

A base16 scheme in JSON (`base00` through `base0F`) works too. The wallust and
pywal caches are read for the sake of desktops that already generate one -
nothing needs setting up there.

On Nix, `homeModules.default` fills the file in:

```nix
programs.flydigictl = {
  enable = true;
  palette = with config.lib.stylix.colors.withHashtag; {
    background = base00;
    text = base05;
    primary = base0D;
    success = base0B;
    warning = base0A;
    danger = base08;
  };
};
```

Deciding which of sixteen scheme colours plays which of six roles is a
judgement call, which is why the module takes the answer rather than making
it.

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

[docs/FIRMWARE.md](docs/FIRMWARE.md) is the reference: the complete command
surface read out of the firmware itself, with argument ranges, what is clamped
against what is refused, the acknowledgement semantics, the state machine and
per-command acceptance criteria to test against.
[docs/PROTOCOL.md](docs/PROTOCOL.md) is the earlier map, written from black-box
testing; where the two disagree the firmware wins, and section 11 of the
former lists the corrections.

**Do not send command `0xDF`.** It erases the firmware's first flash sector and
reboots into the ROM bootloader, with no authentication and no payload gate, and
the cooler will not run again until it is reflashed over USB. It is reachable
from any unpaired Bluetooth peer, so it is worth knowing about even though
nothing here sends it. `0x06` is a factory reset and `0x08` with an out-of-range
gear corrupts the stored gear table; both are also unused here. If you fuzz this
device, exclude those three.

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
    standby = "delayed";

    smoothing = {
      rise_secs = 10.0;
      fall_secs = 60.0;
      panic_c = 90;
    };

    curves = [
      {
        name = "ram";
        sensor.hwmon = "spd5118";     # both DIMMs, hottest one wins
        panic_c = 78;
        points = [
          { temp_c = 46; rpm = 0; }   # below this the fan stops entirely
          { temp_c = 47; rpm = 500; } # one degree later, the slowest it holds
          { temp_c = 58; rpm = 800; }
          { temp_c = 70; rpm = 2200; }
          { temp_c = 78; rpm = 4000; }
        ];
      }
      {
        name = "cpu";
        sensor = { hwmon = "k10temp"; label = "Tctl"; };
        panic_c = 92;
        points = [
          { temp_c = 52; rpm = 0; }
          { temp_c = 53; rpm = 600; }
          { temp_c = 72; rpm = 1800; }
          { temp_c = 92; rpm = 4000; }
        ];
      }
    ];
  };
};
```

The module writes `/etc/flydigictl/config.toml`.

Each curve converts its own sensor into a speed and the highest demand wins.
That matters because temperatures are not comparable across subsystems: 60 C is
idle for a CPU and hot for a stick of RAM, so averaging them would let a warm
drive hide behind a cool processor. `flydigictl sensors` lists what is available
to point a curve at; an empty `label` matches every input of that hwmon and
takes the hottest, which covers both DIMMs or both drives in one curve.

Speeds are interpolated between points and the target is re-applied whenever the
cooler falls back to gear mode, which it does after every reconnect.

`rpm = 0` stops the fan, and stopping is not the mirror image of starting.
Anything between 1 and 500 rpm is a stall band - the blades barely turn and the
tachometer flips between 0 and 400 - so a point or an interpolated value landing
in it is rounded up to 500. A curve that means to stop should step from 0 to its
first working speed across a single degree. Speeding up happens the moment a
curve asks for it, while stopping waits for every curve to agree for a minute,
because a stopped fan needs some twenty seconds of stall-and-retry to turn
again. A speed set by hand is applied immediately either way.

Readings are smoothed before a curve sees them, with a shorter time constant
going up than coming down. A CPU can spike thirty degrees and fall back inside
ten seconds; smoothing the input keeps a real climb responsive while a burst
barely registers. A raw reading at or above `panic_c` bypasses smoothing, and
that threshold belongs per curve, since 85 C is a working load for a CPU and
long past trouble for an SSD.

Because a declarative config lives in the store, it cannot be written to. The
daemon notices, keeps runtime changes in memory and says so:

```text
[WARN ] /etc/flydigictl/config.toml is read-only, runtime changes are lost on restart
```

Outside NixOS the same file is writable and changes are saved. Either way the
config is reloaded live: the daemon watches the *directory*, so replacing the
symlink during `nixos-rebuild switch` is picked up, as is a plain editor save.

### Socket

Newline-delimited JSON on `/run/flydigictl/flydigictl.sock`:

```console
$ echo '{"request":"status"}' | socat - UNIX-CONNECT:/run/flydigictl/flydigictl.sock
{"reply":"status","model":"BS3 Pro","connected":true,"temp_c":49,"current_rpm":1100,"target_rpm":2826,
 "manual":false,"leading":"ram","demands":[{"name":"ram","temp_c":49,"smoothed_c":49,"rpm":2826,"panic":false},
 {"name":"cpu","temp_c":51,"smoothed_c":51,"rpm":500,"panic":false}]}
```

| Request | Effect |
|---------|--------|
| `{"request":"status"}` | speed, mode, and every curve's reading plus which one leads |
| `{"request":"subscribe"}` | turn the connection into a stream of status updates |
| `{"request":"get_config"}` | config in force, plus whether it can be saved |
| `{"request":"set_config","config":{...}}` | replace the config |
| `{"request":"set_manual","rpm":1500}` | hold a speed; `"rpm":null` returns to the curves |
| `{"request":"sensors"}` | temperature inputs the daemon can read, with their current readings |
| `{"request":"gears"}` | the four speeds stored in the cooler, and whether the supply allows each |
| `{"request":"set_gear","gear":"quiet","rpm":1500}` | rewrite one of them |
| `{"request":"set_lighting","lighting":{"mode":{"mode":"effect","effect":3},"brightness":60,"indicators":true}}` | the whole lighting state at once |
| `{"request":"set_standby","standby":"delayed"}` | what the cooler does once the host goes away |

A curve names its sensor by hwmon, device and label, and an empty field matches
anything - no label takes the hottest input of that chip, which is how one curve
covers both DIMMs. The device is a **stable address** rather than a kernel name:

```console
$ flydigictl sensors
nvme       0000:05:00.0 (nvme0)        Composite   37 C
nvme       0000:02:00.0 (nvme1)        Composite   39 C
spd5118    0000:00:14.0/0050 (21-0050) -           49 C
```

`nvme0` and `nvme1` are handed out in probe order and do swap between boots, so
a config written against them can end up watching the other drive. The address
is the PCI slot the chip sits in, plus its i2c address where several chips share
one bus - two memory sticks on the same SMBus differ only by that. A hand-written
`device = "nvme0"` still matches, it is simply not dependable.

### NVIDIA

A curve can follow an NVIDIA GPU, which the kernel publishes no hwmon for:

```nix
services.flydigictl.nvidia.enable = true;

services.flydigictl.settings.curves = [
  {
    name = "gpu";
    sensor = { kind = "nvidia"; label = "core"; };  # or "memory", or empty for the hotter
    panic_c = 87;
    points = [
      { temp_c = 52; rpm = 0; }
      { temp_c = 53; rpm = 600; }
      { temp_c = 80; rpm = 2800; }
      { temp_c = 87; rpm = 4000; }
    ];
  }
];
```

The reading does **not** come from `nvidia-smi`, and that is the whole point.
Opening any of the driver's device nodes takes a runtime power reference and
forces the card to D0, and the driver then wants several idle seconds before it
will suspend again - so a curve polling every few seconds pins a laptop card
awake for as long as the daemon runs. Checking the power state first only avoids
waking a sleeping card; it does nothing about keeping an awake one from ever
sleeping again.

Instead the daemon maps the card's BAR0 read-only through sysfs and reads the
registers itself. That path never enters the driver and takes no power
reference, and a suspended card answers with all ones rather than waking up,
which the daemon reads as "no temperature, and none needed". Verified on an
RTX 4060 Laptop: read every three seconds, the card still suspends on schedule.

Two temperatures are exposed. `label = "memory"` is the memory junction, at
BAR0 `0xE2A8`, twelve bits in thirty-seconds of a degree - the only way to get it
at all on Ada, where `nvidia-smi` reports `N/A`. `label = "core"` is the die, at
BAR0 `0x20400`, whole degrees in the low byte; nobody documents that one, so it
was found by dumping the therm aperture at known temperatures and keeping what
tracked them, then checked against `nvidia-smi` over a cooling run. An empty
label follows whichever of the two is hotter. Both matter: under a
bandwidth-bound load the memory runs far ahead of the die, and it has no fan of
its own.

This needs two things from the system. The kernel command line must carry
`iomem=relaxed`, because the driver claims the aperture and `iomem_is_exclusive`
otherwise refuses the mapping; the NixOS module warns if it is missing. And the
daemon needs read access to `/sys/bus/pci/devices/<address>/resource0`, which
the kernel creates root-owned and `0600` with no way to ask for anything else -
the module ships a udev rule that gives group `flydigi` read access to it on
NVIDIA display controllers. A curve that cannot read its card says so rather
than quietly going missing: it is listed as unreadable in the status and shown
as `cannot read` in the interface.

Ask the daemon for the sensor list rather than reading `/sys/class/hwmon`
directly. The two can disagree: systemd's `PrivateNetwork=` gives a service its
own network namespace, sysfs is tagged by namespace, and every hwmon belonging
to a network device - a Wi-Fi card's temperature, for instance - vanishes from
the service's view while remaining plainly visible to everyone else. A curve
built on one of those never reads anything. The unit here does not use
`PrivateNetwork=` for exactly that reason, and `RestrictAddressFamilies=AF_UNIX`
already stops the daemon opening a network socket.

Everything below `set_manual` reaches the cooler through the daemon rather than
around it: it owns the device, and a second process writing to the same hidraw
node would have its acknowledgements stolen. That is also why the interface
ships no device access of its own.

A subscription is the way to follow the cooler rather than interrogate it: the
daemon writes a status whenever its picture changes, which is twice a second
because that is how often the cooler reports itself. Curves are
still evaluated on `interval_secs`, so the temperatures in those updates move
at their own slower pace. A cooler that goes away arrives as a status with
`"connected":false`, which is how a client tells an unplugged cooler from a
dead daemon. Nothing else is read on that connection, so open a second one for
requests.

Warnings carry a stable `code` alongside their text, so a client that shows each
one once can dedupe on that rather than on the message, which names a config
path that changes on every rebuild.

### Lighting

Nothing in the cooler reports what the strip is playing, so the daemon knows
only what it was told. Changes over the socket last as long as it runs; declare
them in the config to have them restored on every connection:

```toml
[lighting]
brightness = 60
indicators = true

[lighting.mode]
mode = "effect"   # or "off", or "static" with a colour
effect = 3
```

Brightness applies to animations as well as to a plain colour: it is the same
header byte either way, so dimming an effect keeps it running.

`lights_follow_screens = true` puts the strip and the gear indicators out while
every connected display is off, and brings them back with the first one that
lights up. The daemon reads this from `/sys/class/drm`, where a compositor
switching monitors off leaves either a disabled connector or a DPMS property
that says so - no session, no desktop portal and no bus involved, which is what
lets a system service know. The choice made meanwhile is remembered rather than
applied, so nothing lights an empty room.

### Undoing

Every change the interface sends is a snapshot, so `Ctrl+Z` walks back through
them and `Ctrl+Shift+Z` or `Ctrl+Y` walks forward again. Snapshots are taken
when a change goes out rather than while it is being made: dragging a point
produces one, not one per pixel.

Only the configuration is remembered this way. Lighting and a held speed are
not: the cooler is showing them, and stepping back through those would mean
telling it to change again.

### Exporting

The daemon holds the running configuration, and on NixOS the file behind it is
a store path nobody can edit. **Export** in the interface copies it as TOML, so
a curve dragged into shape by hand can be pasted into a config file or turned
into Nix.

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

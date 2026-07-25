# Flydigi BS series HID protocol

Reverse-engineered against a **BS3 Pro** paired over Bluetooth. Command codes
come from [THRM](https://github.com/TIANLI0/THRM) (MIT); frame layout, the
checksum and the mode byte values below were confirmed by hand against the
device.

## Transport

The cooler is a plain HID device. Over Bluetooth it hangs off `uhid`, so it has
no USB parent in sysfs:

```text
/devices/virtual/misc/uhid/0005:37D7:1004.003A/hidraw/hidraw8
```

Match it through `HID_ID` (`bus:vendor:product`) in the hid parent's uevent
rather than `idVendor`, which only exists for USB-attached devices.

| Device  | VID    | PID    |
|---------|--------|--------|
| BS1     | `37D7` | `1001` |
| BS2 Pro | `37D7` | `1002` |
| BS3     | `37D7` | `1003` |
| BS3 Pro | `37D7` | `1004` |

BS1 speaks BLE rather than HID and is not covered here.

## Frames

Both directions use 25-byte reports:

```text
<report id> 5A A5 <cmd> <len> <payload...> <checksum> <padding...>
```

- Report ID is `0x01` for device → host, `0x02` for host → device.
- `len` is `2 + payload length`.
- `checksum = (cmd + len + sum(payload)) & 0xFF`.

That length is not negotiable. THRM pads lighting commands to 65 bytes, and a
BS3 Pro drops those on the floor - no effect, no error, no reply. The same
command in a 25-byte report works.

Commands are acknowledged with `01 5A A5 <cmd> 03 01 <checksum>`, so a missing
reply means the device did not accept the report at all. Verified both ways: a
made-up command (`0x7E`) and a 65-byte report both go unanswered, while every
command below is acknowledged.

Queries answer with data in the same shape. Captured from a BS3 Pro:

| Query  | Payload             | Reading                                     |
|--------|---------------------|---------------------------------------------|
| `0x01` | `00 00 02 04`       | device info, likely firmware 2.4            |
| `0x02` | `01`                | config flag                                 |
| `0x04` | `xx xx xx xx xx xx` | config snapshot, not decoded                |
| `0x25` | `01`               | work mode                                    |
| `0x27` | `a4 06 60 09 b8 0b 74 0e` | gear speeds: 1700, 2400, 3000, 3700 |

Note that `0x27` returns exactly four values, one per gear. The low/medium/high
split inside each gear exists only in the vendor app.

## Status notification (`0xEF`)

The device pushes these unprompted, roughly four times a second:

```text
01 5A A5 EF 0D 68 03 05 6C 07 6C 07 01 01 B4 23 2B 00 ...
```

| Offset | Size | Field                                                       |
|--------|------|-------------------------------------------------------------|
| 0      | 1    | Report ID `0x01`                                             |
| 1-2    | 2    | Magic `5A A5`                                                |
| 3      | 1    | Command `0xEF`                                               |
| 4      | 1    | Length (`0x0D` = 13)                                         |
| 5      | 1    | High nibble: highest allowed gear; low nibble: selected gear |
| 6      | 1    | Mode: `0x02` gear, `0x03` realtime override                  |
| 7      | 1    | Unknown, `0x05` in every capture                             |
| 8-9    | 2    | Current fan speed, RPM, little-endian                        |
| 10-11  | 2    | Target fan speed, RPM, little-endian                         |
| 12-13  | 2    | Unknown, `01 01` in every capture                            |
| 14-15  | 2    | Sequence counter, little-endian, wraps                       |
| 16     | 1    | Checksum                                                     |

The BS2 Pro notes in THRM document the mode byte as `0x04`/`0x05`. A BS3 Pro
reports `0x02`/`0x03` instead, so treat the value as model-specific.

## Commands

| Code   | Name                | Payload            | Verified |
|--------|---------------------|--------------------|----------|
| `0x21` | Set realtime RPM    | RPM, little-endian | yes      |
| `0x23` | Enter realtime mode | none               | yes      |
| `0x24` | Exit realtime mode  | none               | yes      |
| `0x26` | Set gear RPM        | gear, RPM LE       | no       |
| `0x41` | Effect upload init  | none               | no       |
| `0x42` | Effect block write  | data               | no       |
| `0x43` | Play user buffer    | `01`               | yes      |
| `0x44` | Select effect       | mode `00`-`05`     | yes      |
| `0x45` | Select strip        | none, then `01`    | yes      |
| `0x46` | Strip power         | `00` off, `01` on  | yes      |
| `0x47` | Write effect frame  | index + 10 bytes   | yes      |
| `0x48` | Gear indicator LED  | `00` off, `01` on  | yes      |
| `0x01` | Query device info   | none               | yes      |
| `0x04` | Query config        | none               | yes      |
| `0x25` | Query work mode     | none               | yes      |
| `0x27` | Query gear RPM table| none               | yes      |

Setting a fixed speed is `0x23` followed by `0x21`:

```text
02 5A A5 23 02 25                 enter realtime
02 5A A5 21 04 28 0A 57           target 2600 rpm (0x0A28)
02 5A A5 24 02 26                 back to gear mode
```

Realtime mode is an override layered on the selected gear, not a separate
mode: the gear LEDs blink while it is active and return to showing the selected
gear afterwards. The firmware ramps the fan itself at roughly 60 RPM/s, so a
new target is reached over several seconds.

Speed limits are enforced by the application, not the firmware. A BS3 Pro is
rated for 4000 RPM at its top gear, and its four gears are roughly idle, 2700,
3300 and 4000 RPM. Reaching the top two needs a 9V/3A PD adapter in the side
USB-C port; powered from a laptop USB port the cooler stays at 2700 RPM.

Losing power even briefly - a PD renegotiation on a shared GaN charger will do
it - drops the Bluetooth link and takes the hidraw node with it.

What survives that, and what does not:

- The selected gear is stored in the cooler and comes back with it.
- A realtime target does **not**. The device returns in gear mode (`0x02`) at
  the gear's own speed, so anything holding a custom RPM has to re-send `0x23`
  and `0x21` after every reconnect.

Reconnecting creates a *new* HID device, so an open descriptor is dead even
when the node reuses its old name. The device may also enumerate more than once
before it settles - the first node can appear and never deliver a frame - so
recovery should keep retrying rather than assume the first reopen won.

### Low end, measured

Stepping a BS3 Pro down through realtime targets, ten seconds per step:

| Target | Tachometer                          |
|--------|-------------------------------------|
| 700    | follows down, steady                |
| 500    | steady 500                          |
| 300    | stalls, reports flip between 0-200  |
| 100    | stalls, reports flip between 0-400  |
| 0      | fan stops                           |

So 500 RPM is the practical floor, 0 is a genuine passive mode, and anything in
between is worse than useless - the fan cannot hold a speed there.

## Lighting, as implemented in the firmware

Confirmed by disassembling `CH591_For_BS3PRO_Ver0.0.2.4` (RISC-V, base 0). The
dispatcher lives at `0x23fa` and looks handlers up in a table at `gp - 0x58C`
(`gp = 0x20002F18`), filled at runtime by `register_handler(cmd, fn)` at
`0x2818`.

`0x44` is `set_effect(mode)`:

| mode | Effect |
|------|--------|
| `0` | play the buffer uploaded through `0x47`/`0x42` |
| `1`-`5` | presets with palettes hardcoded in the firmware |

The presets are gated. The handler starts with

```c
if (mode != 0 && DAT_20003d14 == 0) return;
```

and that flag is set by `0x23` (enter realtime) and cleared by `0x24` (exit
realtime). **Built-in effects therefore only play while the fan is in realtime
mode.** The command is acknowledged either way, so a no-op looks exactly like
success on the wire - check the fan mode first.

`0x43` is not a generic commit either: it clears the upload counters, persists
the buffer and calls `set_effect(0)`. Sending it after `0x44 01` replaces the
preset with the user buffer, which is why a `0x43` tail blanks the strip when
nothing has been uploaded.

Frame indices are limited: `0x00` is the header, `0x01`-`0x11` carry 10 bytes,
`0x12` carries 6, and higher indices are acknowledged but discarded. The vendor
app uploads 30 frames; everything past `0x12` never reaches the strip.

### Presets cannot be left running

`0x24` (exit realtime) calls `0x658c(0)`, which runs `set_effect(0)` - leaving
realtime always drops the strip back to the user buffer. A preset selected with
`0x44` therefore only plays until the fan leaves realtime mode, and there is no
command that restores the factory animation.

What does survive is the buffer itself, so a preset can be kept by uploading
its own palette through `0x47` instead of asking for it by number. The palettes
live in `FUN_ram_00005bdc` as a 180-byte buffer - exactly 18 frames of 10 bytes
- with the usual header in the first six bytes and RGB triplets after it.

Two independent light sources, easy to confuse:

| Source | Command | Setting | Rendering |
|--------|---------|---------|-----------|
| Gear indicators (1-4) | `0x48` | 3 | lit up to the selected gear, or pulsing while in realtime |
| Side RGB strip (6 LEDs) | `0x46`, content via `0x43`/`0x44`/`0x47` | 4 | plays the palette regardless of fan mode |

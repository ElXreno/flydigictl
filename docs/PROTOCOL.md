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
| `0x44` | Temperature effect  | `01`               | yes      |
| `0x45` | Select strip        | none, then `01`    | yes      |
| `0x46` | Strip power         | `00` off, `01` on  | yes      |
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

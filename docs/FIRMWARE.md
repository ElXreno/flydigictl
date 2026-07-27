# Flydigi BS3 Pro cooler firmware

Full reverse of `CH591_For_BS3PRO_Ver0.0.2.4`, extracted as `bs3pro.bin` (raw binary,
`RISCV:LE:32:RV32IMC`, image base `0x00000000`, 188424 bytes, 1085 functions). The SoC is
a WCH CH591/CH592-class RISC-V BLE microcontroller running the WCH TMOS scheduler and BLE
stack; the application is a thin layer on top.

This document is written against the firmware, not against black-box observation. Every
statement carries a Ghidra address so it can be re-checked. Each claim is marked **PROVEN**
(an instruction was read that does exactly this) or **INFERRED** (the plausible reading of
proven surrounding facts). Where two independent reads disagreed, the disagreement is stated
rather than silently resolved.

`PROTOCOL.md` remains in place as the black-box-plus-partial-reverse record. A dedicated
section near the end lists every point on which the firmware corrects it.

Global pointer `gp = 0x20002F18`. A decompiler expression `*(x *)(gp - N)` is the global at
`0x20002F18 - N`; all such forms below are resolved to absolute addresses.

Conventions for RAM regions, from the startup copy loop at `ram:0000216c` (PROVEN):
- `.data` block 1: flash `0x4..0x2168` copied to RAM `0x20000000..0x20002168` (WCH
  RAM-resident library and vector table).
- `.data` block 2: flash `0x299D8..` copied to RAM `0x20002168..0x20002798`. **Initialised
  globals in this range have non-zero power-on values** taken from flash.
- BSS: RAM `0x20002798..0x200035A8`, zeroed at boot.

---

## 1. Transport and framing

### 1.1 Physical channels

The device presents itself over BLE with two independent GATT services that both carry the
same 25-byte command protocol (`ram:000028a8` registers the one RX callback
`proto_rx_frame_cb` on both, and both drain the same queues):

- **HID-over-GATT service `0x1812`** (`GATTServApp_RegisterService` at `ram:000080E0`). This
  is what Linux binds as `hidraw`. Report characteristics (attribute table imaged at
  `ram:00029D00`, PROVEN byte values):
  - Report ID 1, **Input**, `0x2A4D` at `ram:00029D8C`, properties `0x12` = Read+Notify,
    permission `0x40` (encrypted read). Device -> host.
  - Report ID 2, **Output**, `0x2A4D` at `ram:00029DCC`, properties `0x0E` =
    Read+Write+WriteNoRsp, permission `0xC0` (**encrypted read+write**). Host -> device.
  - Report Map `0x2A4B` value at flash `ram:000291D4`, decoded in 1.4.
  - A third Report characteristic (`ram:00029DEC`, Feature-shaped) exists in the table but
    is absent from the descriptor and is dead.
- **Battery Service `0x180F`** with Battery Level `0x2A19` (attribute table imaged at
  `ram:00029b24`). Advertised in the scan response alongside HID. It reports a hard-coded
  94% that is unrelated to any measurement — see 4.4.
- **Device Information `0x180A`** and **Scan Parameters `0x1813`** are also present in the
  UUID pool and the HID init cluster (`0x180A` at `ram:00029948`, `0x1813` at `ram:00029988`).
- **Vendor service `0xFFF0`** (`ram:000038E2`, attribute table imaged at `ram:00029A14`):
  - `0xFFF2` at `ram:00029A34`, permission **`0x03` = plain READ|WRITE, no encryption**.
    Host -> device commands.
  - `0xFFF1` at `ram:00029A64`, properties `0x10` = Notify only. Device -> host.
  This is the WCH `SimplePeripheral` demo profile (the leftover "Characteristic 1/2" user
  descriptions give it away) repurposed as the real command channel that the vendor mobile
  app uses.

**Security consequence (PROVEN at the permission-byte and callback level):** the HID Output
report requires an **encrypted** link (permission `0xC0`), so the `hidraw` path is gated
behind pairing/bonding. The vendor `0xFFF2` characteristic is **plain READ|WRITE with no
encryption and no authentication** (`ram:00029A34`), and its write callback
(`vendor_fff0_app_cb` at `ram:0000305E`, event 3) forwards straight to `proto_rx_frame_cb`
and `cmd_dispatch`. So any BLE central in range can connect without pairing, write `0xFFF2`,
and issue the full command set — set RPM, change gears, upload lighting, sleep the device,
and (see 9) the destructive `0xDF`. Pairing itself is Just-Works, unauthenticated, bonded
(`GAPBondMgr` params at `ram:00003396`: MITM off `0x401=0`, IO cap NoInputNoOutput
`0x402=3`, bonding on `0x405=1`). INFERRED param-ID names, PROVEN values.

### 1.2 Frame layout

Both directions use a 25-byte HID report:

```
<report id> 5A A5 <cmd> <len> <payload...> <checksum> <padding...>
```

- Report ID `0x01` device -> host, `0x02` host -> device. PROVEN against the report
  descriptor (1.4). Directions match PROTOCOL.md.
- `len = 2 + payload_length` (the length byte covers `cmd`, itself, and the payload). Built
  by `proto_build_frame` at `ram:00002356` as `len_field = payload_len + 2`. PROVEN.
- `checksum = (cmd + len + sum(payload)) & 0xFF`. `proto_checksum` at `ram:000022c2` is a
  plain byte-sum masked to 8 bits, computed over `cmd, len, payload...`. PROTOCOL.md is
  correct. PROVEN.
- The 25-byte total is `1 (report id) + 24 (report data)`; the meaningful frame is
  `5A A5 cmd len payload cksum` and the rest is zero padding.

### 1.3 The 20-byte / 15-payload ceiling — RESOLVED

The ceiling is a firmware bound, **not** the BLE ATT MTU, contradicting PROTOCOL.md's
inference. In the HID Output write callback `hid_output_report_cb` (`ram:0000309e`), PROVEN:

- `ram:000030be`: the ATT write length must be **exactly 24** (`li a4,0x18; bne`), else the
  write is rejected with `ATT_ERR_INVALID_VALUE_SIZE` and never reaches the parser. This is
  why a 65-byte report is dropped on the floor.
- `ram:000030ca`: `li a4,0x11; bltu a4,a1` on the frame's `len` byte — **accept iff
  `len <= 0x11` (17)**, i.e. payload <= 15, i.e. total frame <= 20 bytes. Out-of-range is
  rejected: the handler returns without calling the parser, so no reply and no error. This
  exactly matches the measured "15 bytes answered, 16 met with silence". Unsigned compare
  (`bltu`), no sign trap.

Would a larger negotiated MTU lift the ceiling? **No** — the check is on the frame's own
length byte, independent of the MTU. In fact the link already runs above the default MTU:
the callback demands a 24-byte write, which at ATT_MTU 23 cannot fit (a Write Request
carries only MTU-3 = 20 value bytes), so the fact that the device works on Linux proves the
central negotiated MTU >= 27. Server fallback MTU is `0x17` = 23 (`ram:0000A83A`).

**Caveat:** the length checks above live only on the *HID* Output path. The vendor `0xFFF2`
write path (`vendor_fff0_app_cb` -> `proto_rx_frame_cb`) has **no 24-byte check and no
`len <= 0x11` check**; it calls `ring_enqueue(rx_ring, buf+2, buf[3])` with the
attacker-controlled declared length `buf[3]` up to 255 copied into a 64-byte ring slot
(`proto_rx_frame_cb` at `ram:00002340`). Over the vendor channel the ceiling does not apply
and a long declared length over-reads/over-writes the ring slot. PROVEN. INFERRED that no
production client uses this, but it is reachable by any unpaired peer.

There is **no fragmentation or reassembly**: one ATT write = one 24-byte report = one frame.
The only length consumed is `pData[3]` from the single write. PROVEN.

### 1.4 HID report descriptor

Located at flash `ram:000291D4`, 38 bytes, vendor usage page `0xFFA0` (so a `05 01` / `06 xx
FF` pattern search misses it — it was found through the Report Map attribute's value
pointer). Decoded (PROVEN):

```
06 A0 FF   Usage Page (Vendor 0xFFA0)
09 FF      Usage (0xFF)
A1 FF      Collection (Vendor)
  85 01      Report ID 1
  75 08 95 18  Report Size 8, Report Count 24
  81 02      Input  (Data,Var,Abs)     -> Report ID 1, 24 input bytes
  85 02      Report ID 2
  75 08 95 18  Report Size 8, Report Count 24
  91 02      Output (Data,Var,Abs)     -> Report ID 2, 24 output bytes
C0         End Collection
```

`1 + 24 = 25` is exactly the 25-byte report length. Report ID `0x01` = Input (device->host),
`0x02` = Output (host->device), confirming PROTOCOL.md's direction assignment.

### 1.5 Receive path, dispatch, and pacing

```
BLE write (HID Output 0x2A4D, or vendor 0xFFF2)
  -> proto_rx_frame_cb (ram:000022e4)   magic + len>3 + checksum(if enforced) check
  -> ring_enqueue(g_rx_ring 0x20002D8C, buf+2, buf[3])
  -> proto_poll_task (ram:0000282e), run from poll_task_5ms (ram:00004a2c, every 5 ms):
       ring_dequeue one RX frame -> cmd_dispatch (ram:000023fa)
       ring_dequeue one TX frame -> USB HID send + BLE notify
```

`proto_poll_task` is reached by a tail `j` through the module poll-list walked by
`poll_task_5ms` (`ram:00004a2c` -> `ram:00004a08` -> `ram:00002d00`), which is a TMOS task
scheduled every **5.000 ms** (period 8 units, 625 us/unit — see 2). PROVEN. (One primary
reader could not locate the caller and marked the poll rate UNRESOLVED; the transport agent
found it via the poll-list walk. Resolved: 5 ms.)

**Ring geometry** (`ring_enqueue` `ram:000028f8`, `ring_dequeue` `ram:00002952`,
`ring_is_full` `ram:000028de`, both rings memset at `ram:000028a8`): header
`{u16 read; u16 write}`, 10 slots of 64 bytes at `+4`, per-slot length array at `+0x2C4`,
indices mod 10, full when `(write+1)%10 == read` -> **9 usable entries**. RX ring at
`0x20002D8C`, TX ring at `0x2000315C`.

**Pacing, derived from the firmware:** `proto_poll_task` dequeues exactly one RX and one TX
frame per 5 ms tick. A burst of up to 9 frames is absorbed by the ring; sustained traffic
faster than one frame per 5 ms overflows the ring and `ring_enqueue` returns 1, **dropping
the frame silently** (`ram:000028de`), with no reply and no error — the ATT layer has already
acknowledged the write, so the loss is invisible at the transport level. **The
firmware-derived minimum safe sustained inter-report interval is 5.000 ms.** This is the
mechanism behind the empirically-found "~5 ms gap"; the number comes straight from the poll
period, not from trial and error. PROVEN.

Two secondary stalls: the `0xFFF1` notify pump (`ram:0000310a`) accumulates up to MTU-3
bytes, retrying every 2.5 ms up to 11 times (~27.5 ms worst case) before flushing — affects
the vendor notify channel only. And synchronous flash writes (see 6.3) stall the task loop
during `0x0A`, `0x26`, `0x43`, `0x06`; do not pipeline immediately behind those. The render
tick disables all interrupts for ~30 us per LED (six per frame, ~10 ms cadence); an RX IRQ
landing there is deferred, not lost (Cortex-class pending latch). INFERRED contribution.

### 1.6 Transmit path

```
handler -> proto_reply_send (ram:000023ca): build into g_reply_scratch (0x2000305C),
           ring_enqueue(g_tx_ring 0x2000315C)   <- returns 1=FULL=dropped silently
proto_poll_task dequeues one TX frame per 5 ms:
  USB HID: memset 0x1F @ 0x20002285, copy, send 0x20 (32) bytes gated on 0x200027E4 (USB up)
  frame[2]==0xEF -> hid input report (ram:00003028) + direct 0xFFF1 notify (if 0x200027B8)
  else           -> push into 0xFFF1 byte-FIFO + hid input report
```

Replies are **queued, not synchronous**: `proto_reply_send` returns after enqueue; the actual
send happens on a later poll tick. If the TX ring is full the reply is dropped silently.
PROVEN. `hid_send_input_report` (`ram:00003028`) always emits a 24-byte zero-padded Report
ID 1 input report via `HidDev_Report`. The USB path sends 32 bytes; the BLE notify carries 24
value bytes.

### 1.7 Checksum enforcement — settled

`proto_rx_frame_cb` drops a checksum-mismatched frame **only if `g_checksum_enforce`
(`0x20002718`) != 0** (`ram:00002324`-`2330`, gate polarity PROVEN: non-zero = enforce).

Two agents disagreed on the power-on default. Settled directly by the orchestrator:
`0x20002718` is **not** BSS — it sits at `0x20002718`, inside `.data` block 2
(`0x20002168..0x20002798`), whose LMA is flash `0x299D8`. The source byte at flash
`0x299D8 + (0x20002718-0x20002168) = 0x29F88` reads **`0x01`** (`read_memory ram:00029f88`
-> `01 ff 00 00`). Therefore **checksum enforcement is ON by default and a bad checksum is
rejected (dropped silently)**. The earlier "BSS-zero, accepted by default" reading was wrong.

The undocumented commands `0xF1` and `0xF2` are the toggle: `0xF1` writes 0 (disable
checksum validation), `0xF2` writes 1 (re-enable). They are inline in the dispatcher
(`ram:0000280e` / `ram:000025d8`), reply nothing, and `0x20002718` is RAM-only so it resets
to enforced on every boot. This is the sole purpose of the flag (only reader is the parser).
PROVEN.

---

## 2. Task and timing framework

The application runs on the WCH TMOS cooperative scheduler.

- `tmos_register_task(taskFn) -> taskId` at `ram:00009B96` (`0xFF` if the table is full).
- `tmos_start_task(taskId, event, period)` at `ram:0000951E`.
- `tmos_set_event(taskId, event)` at `ram:0000931C` (fire immediately).

**The `period` unit is 625 us exactly.** `tmos_start_task` computes
`ticks = round(F_sys * period / 1600)` (`ram:00009552`), and `main` sets
`SetSysClock(0x48) = CLK_SOURCE_PLL_60MHz` so `F_sys = 60 MHz`; `60e6/1600 = 37500` counts =
625 us. The division by 1600 makes the result 1/1600 s per unit regardless of `F_sys`.
PROVEN (the 625 us is unit-independent; the 60 MHz cross-check is INFERRED from the SDK enum
value `0x48`).

Complete task table (PROVEN periods; ms via 625 us/unit):

| task | addr | event | period | ms | role |
|------|------|-------|--------|-----|------|
| `poll_task_5ms` | `ram:00004A2C` | 0x80 | 8 | **5.0** | button FSM + `proto_poll_task` (command dispatch) |
| fan control loop | `ram:0000507A` body, loop `ram:00004DD6` | 0x40 | 160 | **100** | closed-loop fan regulation |
| `status_ef_task_500ms` | `ram:00004A68` | 1 | 800 | **500.0** | builds + queues the `0xEF` status frame |
| `led_task_10ms` | `ram:0000624C` body, `ram:00005FE8` | 0x100 | 16 | **10.0** | strip + gear-LED render tick |
| `power_supply_detect_task` | `ram:00006820` | 1 | 16 | **10.0** | supply-level detection |
| `settings_flush_task_1s` | `ram:0000533A` | 0x200 | 1600 | **1000** | deferred settings flash write |
| standby countdown | `ram:00006BD8` | 1 | 160 | **100** | delayed-standby tick |
| auto-start on power | `ram:00006BD8` | 2 | 320 | **200** | one-shot wake if setting 1 == 1 |
| conn-param update | `ram:000032B4` | 4 | 12800 | **8000** | requests 10 ms interval, once, after connect |

The main loop is owned by the WCH library (`TMOS_SystemProcess`); there is no application
`while(1)`. Boot: reset vector `j 0x216C` -> startup (copy .data, zero BSS, `mtvec`,
`mepc = main`) -> `main` (`ram:00006C28`): clock, BLE stack, GATT/HID init, then
`ram:000062CE` runs all application module inits in order (HW, protocol, store, power,
status/telemetry, fan-driver, fan-gear, supply, lighting, telemetry-B). All handlers are
registered synchronously within a few ms of reset; the real gate on the command surface is
BLE connection or USB enumeration.

---

## 3. Command reference

### 3.1 Dispatcher contract

`cmd_dispatch(cmd, payload_ptr, payload_len)` at `ram:000023fa` is a chained unsigned
compare on `cmd`. It builds a small stack struct and passes its address to
`handler_table[cmd]` (`0x2000298C + 4*cmd`, base `gp-0x58C`), calling the handler only if
the slot is non-NULL. The struct is **not cleared between calls**. Struct layout per command
(offset from struct base): byte 0 = command code; byte 1 = the single inline payload byte for
commands that take one; bytes 2..3 = a u16 for `0x21`/`0x26`; for `0x42` a payload pointer at
+4 and a length byte at +8; for `0x47` an index at +1 and up to 10 bytes copied to +2. Inline
handling for `0x01`/`0x04`/`0x0B`/`0xF0`/`0xF1`/`0xF2`. PROVEN, full offset table in
`01-dispatcher-transport.md`.

**The only length validation in the entire dispatcher is `0x44`'s `payload_len == 1` check**
(`ram:0000254c`). Every other command reads `payload[0..2]` unchecked. Because the struct is
not zeroed, a frame too short for a command makes its handler read **stale** stack bytes from
the previous dispatch (the RX-ring slot and the stack mirror are both uncleared). Over the
HID path the 20-byte ceiling and the `len == 24` requirement bound reachability; over the
vendor path they do not. PROVEN. This is a latent memory-safety issue, not known to be
weaponisable from the HID path.

`0x47` index handling in the dispatcher (unsigned `bgeu`): index `<= 0x11` copies 10 bytes,
`== 0x12` copies 6, `>= 0x13` copies nothing but still invokes the handler on stale struct
bytes. The handler itself (3.5) re-checks the index and writes nothing for `>= 0x13`, so no
stale data reaches the animation buffer — safe in the end. PROVEN.

### 3.2 Reply / acknowledgement semantics

**There is no dispatcher-level auto-ACK.** Every reply comes from a handler explicitly
calling `proto_reply_send`. The generic acknowledgement is
`01 5A A5 <cmd> 03 <status> <cksum>`, where `<status>` is a **per-handler byte**, not a
constant `0x01`. Many handlers put meaning there (`0x21` -> 01/02, `0x26` -> 01/00,
`0x0C` -> 01/00); many put a constant `0x01` that is not a status at all (`0x0A`, `0x0D`,
`0x46`, `0x48`, `0x05`, `0x2A`, `0x41`, `0x43`, `0x44`, `0x47`); queries put data there.
PROVEN.

**Commands that reply nothing (total silence):** `0xDF`, `0xED`, `0xEE`, `0xF1`, `0xF2`, and
any unrecognised command byte. For these, silence is not a rejection signal — it is the
normal outcome, and a valid silent command is indistinguishable on the wire from a rejected
unknown byte. Every command in ranges `0x01`-`0x0D`, `0x21`-`0x2A`, `0x41`-`0x48` replies.
PROVEN (cross-checked against the `proto_reply_send` caller set).

**Failure shapes at the transport/dispatch level** (deciding address in brackets):

| class | host observes | where |
|-------|---------------|-------|
| bad magic | silence | `ram:000022f2`/`2fe` |
| frame too short (`len<=3`) | silence | `ram:000022e6` |
| bad checksum | silence (**enforced by default**; accepted only after `0xF1`) | `ram:00002324` |
| unknown command byte | silence | fallthrough `ram:0000242e` |
| wrong length for `0x44` (!=1) | silence (rejected) | `ram:0000254c` |
| short frame for any other cmd | no rejection — handler reads stale payload | dispatcher reads `payload[n]` unchecked |
| frame > 20 bytes (HID path) | silence (rejected) | `ram:000030ca` |

No failure class produces a distinct error frame, a clamp, or a reset at this level.

### 3.3 Command table (complete)

Status column: reply status byte / meaning. "silent" = no reply. Payload byte order is
on-the-wire after `cmd len`. Danger and preconditions in the notes. Handler addresses in 3.4+.

| cmd | name | payload | reply | status meaning | notes |
|-----|------|---------|-------|----------------|-------|
| `0x01` | query firmware version | none | 4 bytes | `00 00 02 04` = 0.0.2.4 | inline `ram:000025e4` |
| `0x02` | query power state | none | 1 byte | `01` awake / `02` asleep | `ram:0000699a` |
| `0x03` | **enter demo mode** (undoc) | none | 1 byte | `01` entered / `05` already | latching, no exit but power cycle; forces gear 1, white strip, fast ramp. `ram:00006538` |
| `0x04` | query MAC | none | 6 bytes | factory MAC from `0x7F018` | inline; keep out of logs |
| `0x05` | **wake** (undoc) | none | 1 byte | constant `01` | starts a device that booted asleep; non-destructive. `ram:00006aa4` |
| `0x06` | **factory reset** (undoc) | none | 1 byte | constant `01` | wipes 3 of 4 flash blocks + sleeps device; no confirmation. `ram:00006738` |
| `0x07` | query supply level | none | 1 byte | `00`..`03` (`00`=undecided) | `ram:000067fe` |
| `0x08` | **select gear** (undoc) | 1 byte gear `01`..`04` | 1 byte | `01` applied / `05` refused by supply | out-of-range at high supply corrupts stored gear (3.4). `ram:000066d0` |
| `0x09` | **query scratch byte** (undoc) | none | 1 byte | the stored byte | pure host-owned scratch, firmware never reads it. `ram:00005128` |
| `0x0A` | **set scratch byte** (undoc) | 1 byte | 1 byte | constant `01` | synchronous flash write, no dedup. `ram:00005698` |
| `0x0B` | query preamble+MAC | none | 13 bytes | `19 31 F0 F1 F2 F3 F4` + 6 MAC | inline `ram:00002670` |
| `0x0C` | auto-start on power | 1 byte `01`=on `02`=off | 1 byte | `01` accepted / `00` refused | note polarity: `00` is rejected. `ram:00005500` |
| `0x0D` | standby mode | 1 byte `00`/`01`/`02` | 1 byte | constant `01` | `>=03` stored verbatim, behaves like `00`. `ram:000054d4` |
| `0x21` | set realtime RPM | u16 LE | 1 byte | `01` applied / `02` not in realtime | no upper bound; clamped only in loop at level 1/2. `ram:000063c4` |
| `0x22` | query fan RPM (undoc) | none | 4 bytes | `curr_rpm_LE` \|\| `target_rpm_LE` | `ram:00004ca0` |
| `0x23` | enter realtime | none | 1 byte | constant `01` | sets realtime flag + light gate. `ram:000066a2` |
| `0x24` | exit realtime | none | 1 byte | constant `01` | restores gear RPM + re-applies user light buffer. `ram:00006676` |
| `0x25` | query work mode | none | 1 byte | `01`-`04` effective gear / `05` realtime / `04` asleep | not a bitfield. `ram:00006400` |
| `0x26` | set gear RPM | 1 byte gear `00`..`03`, u16 LE | 1 byte | `01` stored / `00` index out of range | synchronous flash; also switches to that gear. `ram:000065f0` |
| `0x27` | query gear table | none | 8 bytes | 4x u16 LE gear RPMs | `ram:00006384` |
| `0x29` | query ramp profile (undoc) | none | 1 byte | `00`..`03` | `ram:00004db2` |
| `0x2A` | set ramp profile (undoc) | 1 byte | 1 byte | constant `01` | clamped 0..3, deferred flash. `ram:000050ca` |
| `0x41` | lighting upload begin | none | 1 byte | constant `01` | arms upload, resets cursor. `ram:00005980` |
| `0x42` | lighting upload block | data (<=15) | 1 byte on success | silence = refused | needs prior `0x41`, bounds-checked. `ram:0000591a` |
| `0x43` | lighting commit | 1 byte (ignored) | 1 byte | constant `01` | persists buffer to flash `0x2000`, blocks. `ram:00005f24` |
| `0x44` | select effect | 1 byte mode | 1 byte | constant `01` | modes 1-5 gated on realtime; `>=6` renders uninit stack. `ram:00005ef8` |
| `0x45` | query strip power | none | 1 byte | strip flag | payload ignored. `ram:00005100` |
| `0x46` | strip power | 1 byte `00`/`01` | 1 byte | constant `01` | `>=02` silent no-op with success ack. `ram:00005468` |
| `0x47` | write light frame | index + up to 10 bytes | 1 byte | constant `01` | index `>=0x13` acked, discarded. `ram:000058b4` |
| `0x48` | gear indicator LEDs | 1 byte `00`/`01` | 1 byte | constant `01` | `>=02` silent no-op with success ack. `ram:0000549e` |
| `0xDF` | **ERASE FIRMWARE + REBOOT** (undoc) | none | silent | — | **DESTRUCTIVE, no auth. See 9. DO NOT SEND.** `ram:00004c24` |
| `0xED` | disable `0xEF` stream (undoc) | none | silent | — | clears `g_status_enable`. `ram:00004a5a` |
| `0xEE` | enable `0xEF` stream (undoc) | none | silent | — | sets `g_status_enable`. `ram:00004a60` |
| `0xEF` | (device -> host status push) | 11-byte payload | — | — | unsolicited every 500 ms. See 5. |
| `0xF0` | query MAC (alt) | none | 6 bytes | factory MAC | inline `ram:000027f0` |
| `0xF1` | **disable checksum check** (undoc) | none | silent | — | writes `g_checksum_enforce=0`. `ram:0000280e` |
| `0xF2` | **enable checksum check** (undoc) | none | silent | — | writes `g_checksum_enforce=1` (boot default). `ram:000025d8` |

Any command byte not in this table falls through the dispatcher and does nothing, silently.

### 3.4 Fan and gear commands (details and bounds)

Module init `fan_gear_module_init` at `ram:0000643e`. All bounds below were read from the
disassembly and confirmed by an independent second pass (`09-crosscheck-fan.md`); verdicts
noted.

**`0x21` set realtime RPM** (`ram:000063c4`). u16 LE loaded with `lhu` (zero-extended) and
passed straight to `fan_set_target_rpm` (`ram:00004d3e`) with **no compare between them** —
the entire `0..65535` range is ACCEPTED, including 0, and stored verbatim in
`g_fan_target_rpm` (`0x20003b74`). Precondition: `g_realtime_mode` (`0x20003e80`) != 0, else
reply `02` and nothing happens. Reply `01` applied / `02` refused. No flash, no mode change.
CONFIRMED. Any ceiling is applied later, in the control loop, on a local copy (3.6) — so
`0x22` and the `0xEF` target field report the raw commanded value even while the fan holds a
lower speed.

**`0x26` set gear RPM** (`ram:000065f0`). The dispatcher pre-increments the index byte-wide
(`payload[0]+1`, `ram:00002712`), so the handler's `bltu 3, (idx-1)` (`ram:0000660c`,
unsigned) accepts internal 1..4 = **host bytes `0x00`..`0x03` only**; host `0x04` -> internal
5 -> rejected, host `0xFF` -> internal 0 -> rejected. Out-of-range: reply `00`, no write, no
flash. The RPM value is **unchecked** — any u16 stored and persisted. Persist is
**synchronous** to flash `0x4000` "2PSM" inside the handler. On a successful store within the
supply gate the firmware also **exits realtime and switches to the edited gear**
(`ram:00006644`-`6656`) — it does not merely "apply if selected". If the supply gate blocks
the apply (level 1 with idx>2, level 2 with idx==4) the value is still stored and the reply
is still `01`. Reply `01` stored / `00` out of range. CONFIRMED.

**`0x08` select gear** (`ram:000066d0`, undocumented). One raw byte (NOT pre-incremented), so
host sends `01`..`04`. Supply gate: level 1 requires gear <= 2, level 2 refuses gear 4,
level 3 no gate; refusal replies `05`. On accept: exits realtime, `fan_select_gear`, persists
settings index 0 (deferred flash). **Danger:** at a permissive supply level an index of 0 or
>= 5 passes the supply gate; `fan_select_gear` then rejects it and returns -1 which the
handler ignores, but `settings_set(0, bogus)` runs anyway and realtime has already been
dropped. Reply is still `01`. The corrupted "selected gear" then fails the `(u8)(v-1) < 4`
check on the next wake, so **the fan will not spin up until a valid gear is set**. CONFIRMED,
with a refinement: index `0x00` is accepted at every supply level, not only level 3.

**`0x03` enter demo mode** (`ram:00006538`, undocumented). No payload. Sets
`g_fan_fast_ramp_demo` (`0x20003b82`) -> the loop uses +-50 duty/tick for `|err|>300`; sets
`DAT_20003d17` -> render forces all gear LEDs to full and the whole strip to white; selects
gear 1. Reply `01` entered / `05` already in demo. **Latching: no code anywhere clears either
flag** (exhaustive xref, cleared only by cold-boot init `fan_driver_init`; sleep, wake and
factory reset all verified not to touch them). Only a power cycle leaves demo mode. CONFIRMED.

**`0x06` factory reset** (`ram:00006738`, undocumented). No payload, always replies `01`, no
failure path. Rewrites the settings, lighting and gear-table blocks to defaults and flushes
each **immediately** to flash; then re-applies the (now default) gear and light mode, then
**puts the device to sleep** (`power_sleep_enter` at `ram:00006790`). The "1VPI" scratch
block survives (3 of 4 blocks wiped); BLE bonding survives (the data-flash write is
range-limited to `0x1000..0x6000`, bonding lives below `0x1000`). After it, `0x27` shows
default RPMs and `0x25` returns `04` (asleep) until woken. CONFIRMED (correcting a primary
that said all four blocks).

**`0x23` / `0x24` enter/exit realtime** (`ram:000066a2` / `ram:00006676`). `0x23` sets
`g_realtime_mode = 1` and the lighting gate `DAT_20003d14 = 1`; does not touch the fan target
(the fan keeps the gear speed until a `0x21` arrives). `0x24` clears both, re-applies the user
light buffer (restarting the animation from frame 0), restores the gear RPM, and persists the
realtime flag as settings index 6. Both always reply `01`; the reply carries no accept/refuse
information, so assert on `0xEF` instead. `fan_set_realtime_mode` at `ram:0000658c`. CONFIRMED.

**`0x25` query work mode** (`ram:00006400`). Returns effective gear `01`..`04`; `05` if in
realtime; `04` if asleep (ambiguous with gear 4 — cross-check with `0x02` or `0xEF` byte 0
bit 0). Not a bitfield, not the `0xEF` mode byte. Priority asleep > realtime > gear; reports
the *effective* (supply-clamped) gear. CONFIRMED.

**`0x27` query gear table** (`ram:00006384`). 8 bytes, four u16 LE from `0x20003b96`,
`0x20003b98`, `0x20003b9a`, `0x20003b9c`. Defaults 1700 / 2400 / 3000 / 3700
(`06A4 / 0960 / 0BB8 / 0E74`), applied as immediate stores by `store_gear_table_defaults`
(`ram:0000529a`), not from a const blob. Matches the capture `a4 06 60 09 b8 0b 74 0e`.

**`0x22` / `0x29` / `0x2A` fan telemetry/config** (`ram:00004ca0` / `ram:00004db2` /
`ram:000050ca`, undocumented). `0x22` replies 4 bytes `current_rpm_LE || target_rpm_LE`.
`0x29` replies the ramp-profile index (0..3). `0x2A` sets it: payload clamped to 0..3
(`min(payload,3)` then a second clamp in `settings_set` case 5), deferred flash, reply `01`.
CONFIRMED.

### 3.5 Lighting commands (details and bounds)

Module init `led_module_init` at `ram:00005f64`. Two distinct buffers, which PROTOCOL.md
conflated:

- `g_anim_stage_buf` at **`0x20003d64`**, bound **256** bytes: the write target for
  `0x41`/`0x42`/`0x47` staging.
- `g_anim_flash_mirror` at **`0x20003ba4`**, 186 bytes used / flashed as 192: the render
  source (mode 0) and the flash copy.

**`0x41` upload begin** (`ram:00005980`): resets cursor to 0, sets the arm flag, replies `01`.
The stage-buffer pointer is `.data`-initialised (never NULL) so the guard always passes.

**`0x42` upload block** (`ram:0000591a`): payload is a pointer+length (up to 15 bytes). Logic:
`if (armed) if (cursor + len <= 256) { memcpy(stage+cursor, data, len); cursor += len; reply
01; }`. Bound is unsigned `<=` (a block ending exactly at 256 is accepted). Overrun -> silent
(no copy, no reply). Not armed (no prior `0x41`) -> silent. The reply lives inside the bound
check, so **silence is the only refusal signal**. Fast: a <=15-byte memcpy, no flash, no IRQ
mask. PROVEN.

**`0x43` commit** (`ram:00005f24`): ignores its payload byte; clears arm+cursor, copies 186
bytes stage->mirror, writes magic "2BGR" + 192 bytes to flash `0x2000` (`led_persist_flash`
at `ram:000052e4`), calls `led_build_effect(0)`, replies `01`. No validation — persists
whatever is staged, even zeros. **Blocks synchronously during the flash write** (single-page
RMW, INFERRED ~2-4 ms; ROM timing not in the image). Wait for its ACK before the next report.

**`0x44` select effect** (`ram:00005ef8`): dispatcher rejects unless payload length == 1.
`led_build_effect(mode)`: mode 0 = play the user buffer (no realtime gate); mode != 0 =
`if (g_realtime_flag == 0) return;` (built-in presets only render in realtime); then
`index = (mode-1) & 0xff`, `if (4 < index) default`. Modes 1-5 build hardcoded presets;
**mode >= 6 falls to a default that renders an uninitialised stack buffer** (garbage
colours/speed) — ACCEPTED with consequences, ACK sent, only manifests in realtime. It is an
uninitialised-stack read bounded to the frame plus a write to the fixed palette at
`0x20003c60` — no OOB write, no state corruption. Always replies `01` regardless of outcome,
so a preset selected outside realtime is a silent no-op that looks like success. PROVEN.

**`0x47` write light frame** (`ram:000058b4`): index at struct+1, data at struct+2. Handler:
`i<0x12` -> memcpy 10 bytes at `stage + i*10` (max end 180); `i==0x12` -> memcpy 6 bytes at
`stage+0xB4` (end 186); `i>=0x13` -> no copy, still replies `01`. Max reachable write offset
186 < 256 -> no overflow, no stale-stack leak into the buffer. "Acknowledged but discarded"
confirmed and safe. PROVEN.

Preset palettes (built inline by code, not a const table): mode 1 green breathing, mode 2
yellow, mode 3 red, mode 4 static red, mode 5 multicolour. Header layout in the buffer:
`[0-2 reserved][3 frameCount-1][4 speed][5 brightness]`, then LED-major RGB triplets at
`buf[6 + led*30 + frame*3]` (6 LEDs x up to 10 frames x 3). There is no mode byte in the
buffer — mode is the `0x44` argument. Driver `led_output_strip` (`ram:00002f46`) is a
WS2812/SK6812 GRB bit-bang for 6 LEDs, all interrupts disabled per LED (NVIC ICER), linear
brightness (channel * brightness / 100), no gamma. Speed byte: larger = more interpolation
steps = slower. PROVEN.

**Read-back:** none for the animation. Every accessor of both buffers is a write or a render
read; no command returns the pattern. The only lighting read-backs are the strip flag (`0x45`
and `0xEF` byte 7 bit 2) and the gear-indicator flag (`0xEF` byte 7 bit 0). A client must
remember what it uploaded/selected. PROVEN.

### 3.6 Fan control loop, tachometer, PWM

Loop `fan_control_loop_100ms` at `ram:00004dd6`, 100 ms tick. It is a closed-loop bang-bang
controller on measured RPM with a fixed duty step — no PI term, no duty<->RPM table, no
temperature input (there is no temperature sensor). PROVEN.

**Ramp:** slews PWM duty (not RPM) toward the target. Step source
`g_fan_ramp_step_table` at `ram:00029910` = `{2, 5, 10, 20}` indexed by settings index 5
(default 1 -> step 5). Deadband `|err| <= 50` -> 0; `51..300` -> min(5, table[idx]);
`> 300` -> table[idx]; demo mode -> +-50; a brownout limiter allows only +2 when current RPM
>= 2100 and the supply ADC reads <= 7. Default slew ~5/2400 duty per 100 ms. The "~60 RPM/s"
in PROTOCOL.md is an observed consequence, not a firmware constant. PROVEN.

**State machine** `g_fan_state` at `0x20003b7c`: 0 off (boot / sleep) -> 5 arm (seed kick
threshold 240 for gear 1, 400 for gears 2-4) -> 2 kick (+50/tick until threshold) -> 1
regulate -> 4 stall-latched. Stall watchdog: 100 consecutive zero-RPM ticks (10.0 s) with a
non-zero target -> state 4, duty 0, recoverable only by a sleep/wake cycle. PROVEN.

**Tachometer:** TMR2 edge count (`ram:00003a4e`), converted `pulses * 600 >> 1` over a 100 ms
window. A 3-sample rolling mean feeds `g_fan_current_rpm` (`0x20003b76`). The raw sample is a
multiple of 300; averaging three of them gives the **0/100/200/300 ladder** — so the
**host-visible RPM quantum is 100** (a cross-check corrected the primary's "300"). 2
pulses/rev is INFERRED (2 pulses/rev * 100 ms window is consistent with `*300`). A stalled
fan reports 0. PROVEN except pulses/rev.

**PWM:** TMR1, period 2400 counts, duty capped at 2400 (twice). ~25 kHz assuming 60 MHz Fsys
(INFERRED; period constant PROVEN). No feed-forward; duty is purely the loop's integrator
output.

### 3.7 Power, supply and standby commands

Details in 4. `0x02` query power state (`ram:0000699a`): reads `g_power_sleep_flag`
(`0x2000283c`), replies `01` awake / `02` asleep. `0x05` wake (`ram:00006aa4`, undocumented):
`power_wake()` then constant `01`; non-destructive; the only way to start a device that
booted asleep with both auto-start and standby disabled. `0x07` query supply level
(`ram:000067fe`): reads `g_supply_level` (`0x20002834`), replies `00`..`03` — **`00` is a
real answer** during the ~1-3.5 s before detection completes.

### 3.8 Storage-backed commands

`0x09`/`0x0A` (`ram:00005128` / `ram:00005698`): a one-byte host-owned persistent scratch
register in the "1VPI" flash block; the firmware never reads it. `0x0A` writes flash
synchronously with no dedup and discards the write result. `0x0C` auto-start
(`ram:00005500`): payload `01`=on / `02`=off (note: `00` is rejected with status `00`),
deferred flash. `0x0D` standby mode (`ram:000054d4`): payload passed to `settings_set(2)`
with no validation; `03`..`0xFF` are stored verbatim and behave like `00`; always replies
`01`, deferred flash. `0x45`/`0x46`/`0x48` strip and gear-indicator (details in 6). `0x46`
and `0x48` reject payload >= 2 silently while still replying `01`.

---

## 4. State machine and power model

### 4.1 Sleep / wake

`g_power_sleep_flag` at `0x2000283c` (0 awake, 1 asleep). Writers: `power_module_init` sets 1
at every boot (`ram:000069f8`), `power_wake` clears it (`ram:00006a74`), `power_sleep_enter`
sets it (`ram:00006ad4`). **The device always boots asleep**: fan stopped, LEDs dark, until a
wake trigger fires.

Wake triggers (PROVEN, complete): (1) command `0x05`, unconditional; (2) USB host configured
or resumed, only if standby setting != 0; (3) BLE link established, only if standby setting
!= 0; (4) auto-start task 200 ms after boot, only if auto-start setting == 1; (5) physical
button single click while asleep.

Sleep triggers: instant standby on last-host-loss (`power_on_host_link_change`), delayed
standby countdown expiry, factory reset `0x06`, physical button long press while awake.

Sleeping is purely logical: `power_sleep_enter` stops the fan (state 0) and the render tick
then blanks the strip and gear LEDs. **No WFI, no clock gating, no BLE/USB stop** in the
application path — the BLE stack stays up while "asleep", which is how `0x02` can answer `02`.
On loss of all hosts, realtime mode is dropped unconditionally
(`power_on_all_hosts_gone_exit_realtime` at `ram:000069c0`) regardless of the standby
setting — this is why a realtime target never survives a disconnect. PROVEN.

**Dead-end to know:** auto-start (setting 1) == 0 and standby (setting 2) == 0 means the
cooler boots asleep and the connect callbacks are gated off (`ram:00006af8`), so it stays
dark with a live link until `0x05` arrives. PROVEN.

### 4.2 Standby timing

Standby mode (settings index 2): 0 keep-off / 1 instant / 2 delayed. Delayed uses a 600-tick
countdown at 100 ms/tick = **60.0 s exactly** (`li a5,0x258` at `ram:00006b30`, task re-arms
`0xa0`=160 units=100 ms at `ram:00006bd8`). Every PROTOCOL.md number here is confirmed.

### 4.3 Supply detection and enforcement

`g_supply_level` at `0x20002834`, values 0/1/2/3, written only by `power_supply_detect_task`
(`ram:00006820`). It measures VBUS on ADC channel 0 (PA4, scale `raw*6>>11` ~ volts on a
~12 V-full-scale divider), a PB15 discrete high-power sense, and the USB-enumerated flag.
Classification thresholds (all unsigned):

| condition | level |
|-----------|-------|
| USB enumerated, volts <= 7 | 1 |
| USB enumerated, volts >= 8, PB15 high | 2 |
| USB enumerated, volts >= 8, PB15 low | 3 |
| no USB after ~1 s, volts >= 8, PB15 high/low | 2 / 3 |
| charger-probe fallback end, volts <= 6 | 1 |
| charger-probe fallback end, volts >= 7 | 2 (level 3 unreachable on this path) |

**No hysteresis, no debounce beyond the fixed delays. The level is decided once per boot and
never changes** — the task stops re-arming after it writes a level, and nothing restarts it.
Worst case time to a decision ~3.46 s (fallback probe chain), best case ~10 ms after USB
enumerates. On supply loss the MCU loses power and reboots, so detection reruns from scratch.
PROVEN.

Enforcement is two-part and both parts are silent clamps, not rejections:

- **RPM ceiling** in the control loop, on a local copy per tick: level 1 -> 2700
  (`0x1000-0x574`, `ram:00004f94`), level 2 -> 3300 (`0x1000-0x31C`, `ram:00004fac`),
  **level 3 (and level 0) -> no clamp at all**. A firmware-wide constant search found no
  `0xFA0`/4000. So PROTOCOL.md's "level 3 -> 4000 RPM" ceiling **does not exist in the
  firmware** — 4000 is the fan's hardware rating, and at level 3 the only limit is the physical
  duty cap. Because the clamp is on a local copy, `g_fan_target_rpm` (and thus `0x22` and the
  `0xEF` target field) keeps reporting the raw commanded value. PROVEN.
- **Highest usable gear** in `fan_clamp_gear_to_supply` (`ram:000062de`): max gear **2 / 3 /
  4** for levels 1 / 2 / 3 (the *direction* matches PROTOCOL.md; the numbers are 2/3/4, not
  1/2/3). Silent clamp: the raw choice stays in `g_selected_gear` and takes effect if the
  supply improves via `fan_on_supply_level_change` (`ram:00006350`), which — note — overwrites
  a realtime target with the gear speed without checking the realtime flag. PROVEN.

The device has **no physical battery**: the only ADC channels used are 0 (the VBUS rail) and
15 (the WCH library's internal temperature sensor), there is no charge controller
transaction, no coulomb counter and no charge-status GPIO, and losing VBUS resets the MCU.
PROVEN by exhaustive absence.

**However, the firmware does expose a BLE Battery Service that reports a hard-coded 94%.**
This is not a contradiction of the paragraph above but it is a correction to an earlier
version of this document, which claimed there was "no BLE Battery Service" — that claim was
wrong. See 4.4.

There is **no watchdog** (`R8_WDOG_COUNT` has zero xrefs). Any CPU fault vectors to a
software reset (`ram:00000500`), so an unexpected reboot is indistinguishable on the wire
from a power cycle. PROVEN.

### 4.4 The BLE Battery Service and the phantom 94%

**Erratum.** An earlier revision of this document stated flatly that there is no BLE Battery
Service. That was wrong, and the error came from a search for `0x180F` as an *instruction
operand*; the UUID is stored as a **data constant** in a flash-resident pool, so the search
could never have matched. Hosts really do see a battery percentage, and it really does come
from this firmware.

**It is a GATT Battery Service, not HID battery reporting.** Both candidate mechanisms were
checked:

- *Ruled out — HID.* The report descriptor at `ram:000291D4` (decoded in full in 1.4) is 38
  bytes and declares only Usage Page `0xFFA0` (vendor), one 24-byte Input report (ID 1) and
  one 24-byte Output report (ID 2). It contains **no** Generic Device Controls page (`05 06`)
  and **no** Battery Strength usage (`09 20`), so the kernel's `hid-input` has nothing to turn
  into a power_supply device. PROVEN.
- *Confirmed — GATT.* UUID `0x2A19` (Battery Level) is at `ram:0002991c` and `0x180F`
  (Battery Service) at `ram:00029920`, with the `{len=2, pUUID}` descriptor at `ram:00029924`.
  `0x180F` is also advertised in the scan response (`05 02 12 18 0F 18` = HID + Battery). The
  attribute table is imaged at `ram:00029b24` (copied to RAM at boot with the rest of `.data`
  block 2):

  | attr | type | perms | pValue | meaning |
  |------|------|-------|--------|---------|
  | `ram:00029b24` | `0x2800` primary service | `0x01` R | -> `0x29924` (`0x180F`) | Battery Service declaration |
  | `ram:00029b34` | `0x2803` char decl | `0x01` R | `0x20002738` = `0x12` | properties Read+Notify |
  | `ram:00029b44` | **`0x2A19` Battery Level** | **`0x01` plain READ** | **`0x20002737`** | **the percentage byte** |
  | `ram:00029b54` | `0x2902` CCCD | `0x03` R+W | `0x20005684` | notification enable |
  | `ram:00029b64` | `0x2908` report ref | `0x01` R | `0x20002740` | |

  Note the value's permission is `0x01` = plain READ with **no encryption**, so an unpaired
  peer can read it, consistent with the unauthenticated vendor channel in 1.1.

**Where the number comes from.** The level byte at `0x20002737` lives in `.data` block 2, so
it has a flash initial value: LMA `0x29FA7` = **`0x64` = 100**. It is updated only by
`Batt_MeasLevel` at `ram:00007ec8`, which is the stock TI/WCH `battservice.c` routine:

```c
level = battMeasure();                    // ram:00007c36
if (level < battLevel) {                  // monotonic: only ever decreases
    battLevel = level;                    // sb a0, -0x7e1(gp)  @ ram:00007ed8
    battNotifyLevel();                    // ram:000131da -> GATT notify
}
```

`battMeasure` (`ram:00007c36`) does **not read the ADC**. It reads two u16s and applies the
stock SDK formula:

```
maxLevel = *(u16*)0x2000273A   (gp-0x7de)
measured = *(u16*)0x2000273C   (gp-0x7dc)
if (maxLevel < 401) return 100;
if (measured >= 400) return 0;
n     = ((maxLevel + 1) - measured) >> 2
level = ((400 - measured) * 25 + (n - 1)) / n
```

Both inputs are `.data` constants from LMA `0x29FAA` / `0x29FAC`: **`maxLevel = 409`** and
**`measured = 273`**. Substituting:

```
n     = ((409 + 1) - 273) >> 2 = 137 >> 2 = 34
level = ((400 - 273) * 25 + 33) / 34 = (3175 + 33) / 34 = 3208 / 34 = 94   (integer division)
```

**= 94, exactly the value observed on the host.** PROVEN.

**Is it constant?** Yes, permanently. Verified by exhaustive gp-relative operand search:

- `gp-0x7dc` (`0x2000273C`, the "measured" input): **one instruction touches it in the entire
  image, and it is a load** (`lhu` at `ram:00007c5a`, inside `battMeasure`). Nothing ever
  writes it.
- `gp-0x7de` (`0x2000273A`, maxLevel): likewise only a load, at `ram:00007c4e`.
- The three optional SDK hooks that could have made it dynamic — setup `gp-0x6b4`, teardown
  `gp-0x6b0`, and a custom percentage callback `gp-0x6b8` — are **never assigned anywhere**.
  The only instructions mentioning them are the `addi` address computations inside
  `battMeasure` itself that test them for NULL; they are in BSS, so they are zero and the
  built-in formula always runs. (A `sw ..., -0x6b8, a4` does appear in `cmd_42_upload_block`,
  but that is `a4`-relative, not `gp`-relative, and is the animation cursor — unrelated.)

So the value is **not** derived from the VBUS ADC, not a table indexed by supply level, and
not uninitialised memory. It is a compile-time constant that the stock WCH SDK's arithmetic
happens to turn into 94.

**Does it change?** Only once, and only downward. The HID-over-GATT task `ram:00008344`
fires event 2, which calls `Batt_MeasLevel` and re-arms with
`tmos_start_task(taskId, 2, 15000)` = 15000 x 625 us = **9.375 s**. So roughly every 9.4
seconds the firmware recomputes 94. The first measurement (94) is less than the initial 100,
so it writes 94 and sends a notification; on every subsequent run `94 < 94` is false, so the
value is frozen at 94 for the rest of the device's life. A host that subscribes early may
observe a single 100 -> 94 transition; thereafter it never moves. **It will never track the
supply, so it is not even a crude power-quality indicator — it carries no information at
all.** PROVEN.

**Is it exposed anywhere else?** No. `0x20002737` is not part of the `0xEF` status payload
(which is `0x20003B68`..`0x20003B72`), and no command handler reads it — the only accessors
are the `battservice.c` internals in the `ram:00007d20`..`ram:00007ed8` range (the GATT
read/notify callbacks and the SDK's own get/set parameter helpers), none of which is reachable
from `cmd_dispatch`. The percentage is readable **only** over GATT characteristic `0x2A19`.
PROVEN.

**Client guidance:** ignore the battery percentage entirely, and if `flydigictl` ever surfaces
device status it should suppress it rather than pass it through — a desktop Bluetooth panel
showing "94%" for a mains-powered cooler is a stock-SDK artefact, not a reading.

---

## 5. The `0xEF` status notification

Built by `status_ef_task_500ms` at `ram:00004a68`, period 800 units = **500.0 ms exactly**,
gated on `g_status_enable` at `0x2000280b` (power-on default 1; `0xED` clears, `0xEE` sets;
RAM-only so it re-enables each boot). Payload is **11 bytes** into `g_status_frame_buf`
(`0x20003b68`), sent as `proto_reply_send(0xEF, buf, 0x0B)`; the wire `len` byte is `0x0D`
= 13 = payload + 2; the meaningful frame is 16 bytes; the HID report is 25 bytes with 8 zero
pad. The captured checksum `0x2B` verifies. Three agents converged on this; the "13-byte
payload" reading was a naming error (len field vs payload). PROVEN.

Offsets given as report offsets (report ID at offset 0), matching PROTOCOL.md's numbering.
Class: COMMANDED (changes on the next tick after a successful command) vs MEASURED (a sensor
value that follows over time). All rows PROVEN.

| report off | bits | field | source global | class |
|------------|------|-------|---------------|-------|
| 5 | b0 | sleep (1 = asleep) | `0x2000283C` | COMMANDED |
| 5 | b1-2 | **effective** gear - 1 (0..3) | `0x20003E78` | COMMANDED |
| 5 | b3 | BLE link up | `0x2000283E` | MEASURED |
| 5 | b4 | USB host configured | `0x2000283D` | MEASURED |
| 5 | b5-6 | supply level (0 = undecided, else 1..3) | `0x20002834` | MEASURED, latched once/boot |
| 5 | b7 | demo mode (latching) | `0x20003B82` | COMMANDED (`0x03`) |
| 6 | b0 | realtime mode | `0x20003E80` | COMMANDED |
| 6 | b1 | setting 1 auto-start (defaults 1) | `0x20003B89` | COMMANDED (`0x0C`) |
| 6 | b2-3 | setting 2 standby, 2-bit `& 3` | `0x20003B8A` | COMMANDED (`0x0D`) |
| 6 | b4-7 | constant 0 | - | - |
| 7 | b0 | setting 3 gear-indicator enable | `0x20003B8B` | COMMANDED (`0x48`) |
| 7 | b2 | setting 4 strip power | `0x20003B8C` | COMMANDED (`0x46`) |
| 7 | b1, b3-7 | constant 0 | - | - |
| 8-9 | - | current RPM u16 LE | `0x20003B76` | **MEASURED** (tach, 100 RPM quantum) |
| 10-11 | - | target RPM u16 LE | `0x20003B74` | **COMMANDED**, stored unclamped |
| 12 | - | setting 5 ramp profile 0..3 | `0x20003B8D` | COMMANDED (`0x2A`) |
| 13 | - | setting 6 persisted realtime flag | `0x20003B8E` | COMMANDED |
| 14-15 | - | sequence counter u16 LE | `0x20003B71` | free-running, +1/frame, wraps |
| 16 | - | checksum | - | - |

The captured byte 5 = `0x68` decodes as awake / gear 1 / BLE up / USB down / supply level 3 /
demo off — consistent with a Bluetooth capture on a PD adapter. A change to any field appears
in the next frame, <= 500 ms after the handler returns; current RPM (bytes 8-9) is the only
field that lags over seconds as the fan ramps. The sequence counter advances even while
telemetry is suppressed and while nothing is connected. The frame is built and queued
unconditionally when enabled; it reaches the air only after a host writes a notify CCCD
(latch `0x200027B7`, never cleared once set), and is suppressed entirely by `0xED`.

---

## 6. Storage subsystem

### 6.1 Flash blocks

Four fixed data-flash blocks, restored at boot by `store_init` (`ram:000056c2`) with a
per-block magic check; on mismatch or read error the defaults are applied and re-flushed.

| offset | size | magic | contents |
|--------|------|-------|----------|
| `0x1000` | 12 | `2SDB` (`0x42443253`) | settings 0..6 (see 6.2) |
| `0x2000` | 0xC0 | `2BGR` (`0x52474232`) | lighting/animation buffer (write-only via commands) |
| `0x3000` | 8 | `1VPI` (`0x49505631`) | one opaque host scratch byte (`0x09`/`0x0A`) |
| `0x4000` | 16 | `2PSM` (`0x4D535032`) | gear RPM table, 4x u16 (`0x27`/`0x26`) |

These are data-flash offsets (passed unmodified to the ROM EEPROM selectors, bounded to
`< 0x6000`), not CPU addresses. The window below `0x1000` is refused by
`dataflash_range_check` (`ram:00002d62`) and is INFERRED to hold BLE bonding. PROVEN.

### 6.2 Settings block

RAM mirror at `0x20003b88`, seven bytes, defaults `02 01 00 01 01 01 00`:

| idx | RAM | meaning | value handling | default |
|-----|-----|---------|----------------|---------|
| 0 | `0x20003b88` | selected gear | raw byte, no clamp | 2 |
| 1 | `0x20003b89` | auto-start on power (`0x0C`) | normalised 0/1 | 1 |
| 2 | `0x20003b8a` | standby mode (`0x0D`) | **raw byte, no clamp** | 0 |
| 3 | `0x20003b8b` | gear-indicator LEDs (`0x48`) | normalised 0/1 | 1 |
| 4 | `0x20003b8c` | side strip power (`0x46`, read by `0x45`) | normalised 0/1 | 1 |
| 5 | `0x20003b8d` | fan ramp profile (`0x29`/`0x2A`) | **clamped to 3** | 1 |
| 6 | `0x20003b8e` | realtime light mode / persisted realtime | normalised 0/1 | 0 |

Accessors: `settings_get(index, out)` at `ram:0000555c` (index 0..6 = one byte, 7 = bulk copy
of all 7, >7 rejected); `settings_set(index, pval)` at `ram:00005376` (rejects index > 6,
never rejects a *value* — clamps idx 5, boolean-squashes idx 1/3/4/6, accepts raw idx 0/2,
write-if-changed). PROVEN.

### 6.3 Flash layer, deferral, and wear

Write path: the four `*_persist` helpers -> `dataflash_write_rmw` (`ram:00002db6`) ->
`rom_eeprom_read`/`rom_eeprom_erase_write` -> ROM entry `0x200006EC` (selectors 0x0B read /
0x09 erase / 0x0A program). `dataflash_write_rmw` does a **full 256-byte page
read-modify-erase-write**; each of the four call sites touches exactly one page. No read-back
verify, no CRC, no A/B banking — the boot-time magic is the only integrity gate. PROVEN.

**The settings block is the only one that is deferred and coalesced.** `settings_set` sets a
dirty flag at `0x20002821`; `settings_flush_task_1s` (`ram:0000533a`) flushes within 1.000 s
and retries on failure. So `0x0C`/`0x0D`/`0x46`/`0x48`/gear-select changes land within 1 s and
a power cut in that window loses them. **This refutes the flash-blocking theory for those
commands** — they do no flash work in the handler.

**The other three blocks write synchronously, in the handler, with no dedup:**
`0x0A` (page `0x3000`), `0x26` (page `0x4000`), `0x43` (page `0x2000`), and `0x06` (all
three). A client that re-sends `0x26` in a loop burns one erase/program cycle of the same
physical page per report — there is no wear levelling (four fixed offsets) and no
write-if-changed. **Client rule: never re-send `0x26`, `0x43` or `0x0A` with an unchanged
value; the firmware will not deduplicate for you.** Absolute erase/program times are
UNRESOLVED (the ROM routine body is not in the image). The wrappers provably do **not**
disable interrupts; whether the ROM routine does internally is UNRESOLVED. PROVEN except the
timing.

### 6.4 Persistence: what survives what

| state | flash block | survives reconnect | survives power loss | readback |
|-------|-------------|--------------------|--------------------|----------|
| settings 0..6 | 2SDB `0x1000` | yes | yes | `0xEF`; strip also `0x45`, ramp also `0x29` |
| gear RPM table | 2PSM `0x4000` | yes | yes | `0x27` |
| animation buffer | 2BGR `0x2000` | yes | yes | **none — write-only** |
| scratch byte | 1VPI `0x3000` | yes | yes | `0x09` |
| realtime target RPM | none (RAM) | no | no | `0xEF` bytes 10-11 |
| realtime mode | none (RAM) | no | no | `0xEF` byte 6 bit 0; persisted copy byte 13 |
| sleep flag | none (RAM, boots asleep) | no | no | `0x02`, `0xEF` byte 5 bit 0 |
| supply level | none (recomputed each boot) | recomputed | recomputed | `0x07`, `0xEF` byte 5 bits 5-6 |

Every persistent item except the 2BGR animation buffer is readable back. PROTOCOL.md's "a
client must remember what it set" is true only for the animation.

---

## 7. Physical button and indicator LEDs

### 7.1 Button

One button on GPIO **PB22**, input pull-up, active low, polled by `button_fsm_poll`
(`ram:00002a1c`) once per 5 ms tick. Debounce 3 samples (15 ms). All actions are gated behind
the 1-second boot-grace flag at `0x20002808` (a report/press before that is ignored). Timings
(5 ms/tick): click window 61 ticks (305 ms), long press 201 ticks (1005 ms), ultra-long 2001
ticks (10005 ms). PROVEN.

- **Single click** (`btn_on_single_click`, `ram:00004910`): if asleep -> `power_wake`; else
  cycle to the next gear (`fan_button_next_gear` at `ram:000067a8` — advances gear, wraps to
  1 above the supply-clamped max, or if in realtime it *exits realtime* instead of cycling)
  and persist the resulting gear to settings index 0.
- **Double click** (`btn_on_double_click`, `ram:0000494a`): if setting 6 (realtime light
  mode) == 0, set it to 1. One-way (only sets, never clears). This is the only writer of
  setting 6 besides the realtime enter/exit path — it is why `0xEF` byte 13 can change with no
  command sent.
- **Long press** (`btn_on_long_press`, `ram:00004980`): if awake -> `power_sleep_enter`.
- **Ultra-long press** (~10 s hold then release, `btn_on_ultralong_press` `ram:000048ec` and
  the release callback `ram:000048f8`): triggers a **factory BLE unpair / bond reset**
  (`ble_unpair_bump_addr_reset`) — it clears bonding and reboots the link so the device can be
  paired to a new host. PROVEN. Not destructive to firmware or settings, but it drops the
  current bond.

A button-driven gear change is observable to a connected host via `0xEF` byte 5 bits 1-2 (and
target RPM) — this is the "the button on the case changed the target" case PROTOCOL.md notes.

### 7.2 Gear indicator LEDs

Four indicators, driven as 4-channel soft-PWM (100 brightness steps) by a TMR0 interrupt
handler at `ram:000001F4`, with brightness targets set by the 10 ms render tick
(`ram:00005fe8`) into `g_gear_led_buf` at `0x20003e64`. Lit up to the selected gear; pulse
while in realtime; forced full-on in demo mode (`0x03`); blanked while asleep or when setting
3 == 0. Enable flag is settings index 3, set by `0x48` and readable via `0xEF` byte 7 bit 0
(so the gear-indicator state **is** readable, contrary to PROTOCOL.md). The side strip is a
separate light source: 6 WS2812/SK6812 GRB LEDs bit-banged by `led_output_strip`, gated by
setting 4 (`0x46`). PROVEN.

---

## 8. Memory map of the state globals that matter

| address | name | meaning |
|---------|------|---------|
| `0x20002718` | `g_checksum_enforce` | 1 = enforce RX checksum (boot default 1); `0xF1`/`0xF2` toggle |
| `0x2000280B` | `g_status_enable` | `0xEF` push enable (boot default 1); `0xED`/`0xEE` toggle |
| `0x20002808` | boot-grace flag | set 1s after boot; gates button + first frames |
| `0x20002737` | `g_ble_battery_level_pct` | BLE Battery Level `0x2A19` value; init 100, becomes 94 once, then frozen |
| `0x2000273A` / `0x2000273C` | `g_batt_maxlevel_const_409` / `g_batt_measured_const_273` | `.data` constants that produce the fake 94%; never written |
| `0x20002821` | `g_settings_dirty` | settings block needs flushing |
| `0x20002824` / `0x20002828` | 1VPI magic / scratch byte | `0x09`/`0x0A` host scratch |
| `0x20002834` | `g_supply_level` | 0/1/2/3, decided once per boot |
| `0x2000283C` | `g_power_sleep_flag` | 0 awake / 1 asleep (boots 1) |
| `0x2000283D` / `0x2000283E` | USB host / BLE link presence | |
| `0x20002840` | standby countdown | 100 ms units, delayed standby |
| `0x20002D8C` / `0x2000315C` | RX ring / TX ring | 10 slots x 64 B each |
| `0x2000305C` | reply scratch | reply frame build buffer |
| `0x20003B68` | `g_status_frame_buf` | 11-byte `0xEF` payload |
| `0x20003B71` | `0xEF` sequence counter | u16, +1/frame |
| `0x20003B74` | `g_fan_target_rpm` | commanded target, raw/unclamped |
| `0x20003B76` | `g_fan_current_rpm` | measured, 3-sample mean |
| `0x20003B78` | `g_fan_pwm_duty` | 0..2400 |
| `0x20003B7C` | `g_fan_state` | 0 off / 5 arm / 2 kick / 1 regulate / 4 stall |
| `0x20003B82` | `g_fan_fast_ramp_demo` | demo flag (`0x03`), never cleared |
| `0x20003B84` | settings block 2SDB | magic + settings 0..6 at `0x20003B88` |
| `0x20003B90` | gear table 2PSM | magic + 4x u16 at `0x20003B94` |
| `0x20003BA0` / `0x20003BA4` | lighting 2BGR magic / mirror | render source + flash copy, 186 B |
| `0x20003C60` | decoded frame palette | expanded RGB the tick interpolates |
| `0x20003D14` | lighting realtime gate | set by `0x23`, cleared by `0x24` |
| `0x20003D17` | demo LED override | set by `0x03`, never cleared |
| `0x20003D64` | `g_anim_stage_buf` | upload staging (256 B) |
| `0x20003E64` | `g_gear_led_buf` | gear indicator LEDs |
| `0x20003E78` | `g_effective_gear` | supply-clamped gear (reported by `0x25`, `0xEF`) |
| `0x20003E7C` | `g_selected_gear` | raw selected gear 1..4 |
| `0x20003E80` | `g_realtime_mode` | 0/1 |

---

## 9. Danger: `0xDF` erases the firmware

**`0xDF` (`ram:00004c24`) is destructive and must never be sent.** Two independent reads
initially disagreed (one called it a harmless status re-init); the disagreement was settled
from disassembly, twice, and the destructive read is correct.

The 4-instruction handler calls a RAM-resident routine at `0x2000027e`, which the C-runtime
copies from flash `0x282` at boot (the copy math `0x20000000 + (0x27e - 0x4)` lands exactly
at flash `0x282`, PROVEN). That routine:

1. `FUN_ram_000006f0(op=1, addr=0, len=0x1000)` — the flash engine's erase path (op 1 takes
   the code-flash branch, not the `+0x70000` data-flash branch), SPI sector-erase opcode
   `0x20`, erasing **one 4 KB sector at internal-flash address 0 — the reset vector / firmware
   start**. PROVEN (opcode, address literal 0, length 0x1000).
2. `FUN_ram_000006f0(op=4)` — flash chip reset.
3. SAFE_ACCESS `0x57`/`0xA8` + set the MCU software-reset bit + infinite spin — the routine
   never returns; the handler's tail is dead code. PROVEN (reset by the SAFE_ACCESS + spin
   idiom; the exact register bit is INFERRED but immaterial — the erase alone bricks).

There is **no authentication, no unlock sequence, no payload gate**. A bare `0xDF` byte over
either channel (and, since `0xFFF2` is unencrypted, from any unpaired peer) erases the running
firmware and reboots into the WCH ROM ISP bootloader; the device will not run its firmware
again until reflashed over USB (WCHISPTool / wchisp). It is effectively the OTA/DFU
bootloader-entry primitive but, standalone, a one-way firmware erase. **There is no safe way
to test it.** The observable, if it were ever sent, is the device vanishing from the HID/BLE
bus and re-enumerating as a WCH ISP device.

Every other undocumented command is safe: `0x03` (demo, latching but reversible by power
cycle), `0x05` (wake), `0x06` (factory reset — destroys user state but not the device),
`0x08`/`0x22`/`0x29`/`0x2A` (fan), `0x09`/`0x0A` (scratch), `0xED`/`0xEE` (telemetry toggle),
`0xF1`/`0xF2` (checksum toggle).

---

## 10. Acceptance criteria for automated validation

Framing: send with report ID `0x02`; expect a reply with report ID `0x01`, command byte
echoed, and `checksum = (cmd + len + sum(payload)) & 0xFF`. Replies are queued — allow one
5 ms poll period before declaring a timeout. **A missing reply for a command that should
reply means the frame was dropped by transport (bad magic/length/checksum, or ring overflow),
not refused by a handler.** For the silent commands (`0xDF`/`0xED`/`0xEE`/`0xF1`/`0xF2`),
absence of a reply is normal.

Per-command "send this, expect that" (all derived from the firmware; addresses in sections 3-6):

- `0x01` -> reply `00 00 02 04`. `0x04`/`0xF0` -> 6 MAC bytes. `0x0B` -> preamble + 6 MAC.
- `0x02` -> `01`/`02`. `0x07` -> `00`..`03` (treat `00` as "not yet decided"; poll for up to
  ~3.5 s after connect; do not cache across a reconnect, which implies a reboot).
- `0x21 <rpm>` -> `01` if in realtime, `02` otherwise. On `01`, `0xEF` bytes 10-11 (target)
  show the value on the next tick; bytes 8-9 (current) converge over seconds. **Trap:** above
  the level 1/2 ceiling the reply is `01`, target shows the request, current settles at
  2700/3300. Detect by reading `0x07` first, or by comparing current vs target after
  convergence. Also `0x22` returns `current || target` immediately, no wait.
- `0x26 <gear 00-03> <rpm LE>` -> `01` stored / `00` out of range. **Strongest assertion in
  the whole surface:** `0x27` returns the updated table immediately. If the supply gate blocks
  the apply, `0x27` still changes but `0xEF` gear/target do not.
- `0x08 <gear 01-04>` -> `01` applied / `05` refused by supply. `0x25` and `0xEF` gear change
  on the next tick. **Trap:** index 0 or >=5 at a permissive level -> `01` with nothing
  visible changing, realtime dropped, and a corrupted stored gear that breaks the next wake.
- `0x23`/`0x24` -> `01` always (reply carries no info). Assert on `0xEF` byte 6 bit 0 or
  `0x25` == `05`.
- `0x25` -> `01`-`05`. `0x27` -> 8 bytes.
- `0x05` -> `01` always. Observe `0x02` flip `02`->`01`, `0xEF` byte 5 bit 0 clear. To reach a
  genuinely asleep device without hardware: `0x0C 02`, `0x0D 00`, power cycle; it boots asleep
  with the connect wake gated off, and only `0x05` starts it.
- `0x0C 01`/`0x0C 02` -> `01`; `0x0C 00` -> `00` (refused). `0x0D 00/01/02` -> `01`; `>=03`
  stored but inert. Readback via `0xEF` byte 6.
- `0x46`/`0x48 00`/`01` -> `01`, readback `0x45` / `0xEF` byte 7. `>=02` -> `01` but no change
  (silent no-op).
- `0x2A 00-03` -> `01`, readback `0x29` / `0xEF` byte 12.
- `0x41` -> `01`. `0x42 <=15B` -> `01` on success, **silence = refused** (overrun or not
  armed). `0x43 01` -> `01` (wait for it; it blocks on flash). `0x44 <mode>` -> `01` always
  (no-op if a preset is selected outside realtime — assert fan mode first). `0x47` -> `01`
  always (bad index acked and discarded). No lighting state is in `0xEF` except the strip flag.
- `0x09` -> stored byte; `0x0A <b>` -> `01`, read back with `0x09`.

**Commands whose ACK says success while nothing observable changes** (the traps): `0x21` above
the supply ceiling; `0x08` out-of-range at a permissive level; `0x26` blocked by the supply
gate; `0x23`/`0x24` when already in that mode; `0x03` while in realtime; `0x44` preset outside
realtime; `0x46`/`0x48`/`0x0D` with out-of-range payload; `0x0A` when the flash write fails
(result discarded); `0x21` racing a supply-level change (the target is silently overwritten
with the gear RPM while the mode bit stays set).

---

## 11. Corrections to PROTOCOL.md

Firmware-authoritative corrections. Each carries the deciding address above.

1. **Checksum is enforced by default, not always and not never.** Enforcement is gated by
   `g_checksum_enforce` (`0x20002718`), which is `.data`-initialised to `0x01` (flash LMA
   `0x29f88`), so bad checksums are rejected by default. The undocumented `0xF1`/`0xF2`
   disable/enable it (RAM-only, resets to enforced each boot).
2. **The 20-byte ceiling is a firmware bound, not the ATT MTU.** `hid_output_report_cb`
   requires a 24-byte ATT write and `len <= 0x11` (`ram:000030be`/`030ca`). A larger MTU would
   not lift it. (The vendor `0xFFF2` path has no such check.)
3. **The ACK status byte is per-handler, and there is no auto-ACK.** A missing reply means the
   handler chose not to reply (or the frame was dropped), not "report rejected". `0xDF`,
   `0xED`, `0xEE`, `0xF1`, `0xF2` and unknown bytes are silent.
4. **`0xEF` field decode was substantially wrong.** Byte 5 is a bitfield (bit 0 sleep, bits
   1-2 effective gear, bit 3 BLE, bit 4 USB, bits 5-6 supply level, bit 7 demo), not two
   nibbles. Byte 6 bit 1 is auto-start (not "always set"), bits 2-3 are a 2-bit standby field.
   Byte 7 is the two LED-enable bits (not "always 0x05"), which makes the **gear-indicator
   state readable**. Bytes 12-13 are settings 5 and 6 (not "always 01 01"). See section 5.
5. **`0x25` returns an effective-gear enum (`01`-`05`, `04`=asleep), not the mode bitfield.**
6. **Supply level 3 has no RPM ceiling in the firmware.** Level 1/2 clamp to 2700/3300; the
   "level 3 -> 4000" row is the fan's rating, not a constant. Highest usable gear is 2/3/4 for
   levels 1/2/3, not 1/2/3. Supply level is decided once per boot and never changes; `0x07`
   can legitimately answer `00`.
7. **`0x21` has no upper bound** and does not "refuse whenever not in realtime" by rejecting
   the value — it stores nothing and replies `02` when not in realtime; when in realtime it
   accepts any u16 and the loop clamps a local copy.
8. **`0x26` also switches to the edited gear and exits realtime** on a successful store within
   the supply gate; it is not "apply only if selected".
9. **New commands to document:** `0x03` demo mode (latching), `0x05` wake, `0x06` factory reset
   (destructive to user state), `0x08` select gear, `0x09`/`0x0A` host scratch byte,
   `0x22`/`0x29`/`0x2A` fan telemetry/ramp-profile, `0x0C` payload polarity (`01`=on/`02`=off,
   `00` rejected), `0xED`/`0xEE` telemetry toggle, `0xF1`/`0xF2` checksum toggle, and
   **`0xDF` = destructive firmware erase (DO NOT SEND)**.
10. **Two buffers, not one, for lighting.** Staging (`0x20003d64`, bound 256) vs render/flash
    mirror (`0x20003ba4`, 186 B, flashed as 192). The header has no mode byte. `0x44` mode
    >= 6 renders uninitialised stack (garbage, only in realtime).
11. **The tach RPM quantum is 100, not 300.** Three 300-multiples averaged give the
    0/100/200/300 ladder.
12. **`0x0D` does not validate**; `03`..`0xFF` are stored verbatim and behave like `00`.
    `0x46`/`0x48` reject payload >= 2 silently while replying `01`.
13. **Realtime is dropped on host loss by a path independent of standby**
    (`ram:000069c0`), and the device **always boots asleep**.
14. **`0x0D`/`0x0C`/`0x46`/`0x48`/gear-select are deferred to flash (1 s window)**; `0x26`,
    `0x43`, `0x0A`, `0x06` write flash synchronously with no dedup.

---

## 12. Open questions and inferred-not-proven items

- **Absolute flash erase/program time** (and whether the ROM routine masks interrupts) —
  UNRESOLVED. The ROM routine at `0x200006EC` is not in the image. The firmware-side wrappers
  provably do not mask interrupts; the ~2-4 ms figure for `0x43`'s stall is INFERRED.
- **Fsys = 60 MHz** is INFERRED from the SetSysClock argument `0x48`; the 25 kHz PWM and the
  625 us tick both depend on it (the 625 us is unit-independent of the exact frequency, so it
  is safe; the 25 kHz is not verified).
- **VBUS divider ratio on PA4** and **what drives PB15 low** — UNRESOLVED (off-chip). The
  supply thresholds (7/8 V fast path, 6/7 V probe path) are proven as integers; their physical
  unit is INFERRED as volts.
- **Tach pulses-per-rev = 2** is INFERRED from the `*300` conversion; the count itself and the
  100 ms window are PROVEN.
- **The `0x0007F010` chip-id byte** (which gates the 4 KB vs 256 B erase granularity) is
  outside the image; the 256-byte page conclusion is INFERRED from the firmware working.
- **The 1VPI scratch byte's intended meaning** — PROVEN that the firmware never reads it;
  INFERRED that it is a vendor-app profile/UI tag.
- **`0x44` mode-5 multicolour palette** — the dominant character is decoded; the exact
  per-LED word-packed values are UNRESOLVED (low value).
- **The short-frame stale-payload read** is proven reachable in principle over the unbounded
  vendor `0xFFF2` path; no concrete exploit was constructed, and hardware testing is out of
  scope.
- **`0xDF` as the vendor updater's real DFU-entry mechanism** vs a USB control request — the
  erase+reboot is PROVEN; which path the official updater uses is UNRESOLVED (not in this
  image).

---

*Reversed in Ghidra against `bs3pro.bin`; the database is annotated (functions renamed,
globals labelled, plate/decompiler comments) to match this document. No physical hardware was
touched at any point.*

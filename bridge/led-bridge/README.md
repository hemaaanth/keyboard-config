# Go60 LED bridges

This crate builds two host-side bridges for the same Go60 GATT firmware:

- `bb-led-bridge`: active Linux/BlueZ daemon for bb on Omarchy.
- `paseo-led-bridge`: legacy Windows/WinRT daemon for Paseo.

Both write the same LED frames, so the keyboard firmware did not need a new
transport or a new pairing.

## Target device protocol

- Service UUID: `70617365-6f4c-4544-b0a0-000000000001`
- Write characteristic UUID: `70617365-6f4c-4544-b0a0-000000000002` (write-without-response)
- Frame: byte 0 = pixel count `n`, then `n` × 4 bytes `[logical_index, r, g, b]`.
  - Logical index 0-9 = number-row keys `1 2 3 4 5 6 7 8 9 0`.
  - Logical index 10-17 = `Y U J K L ; F D`.
  - Logical index 18-29 = full columns `= 1 2 3 4 5 6 7 8 9 0 -`.
  - Frames may contain up to 18 pixel entries.
  - Index byte `0xFE` is the **fill-all** op: sets all 30 LEDs on both
    halves to that r,g,b. Later entries in the same frame layer on top of
    it (so `[0xFE=red, 3=blue]` means "all red except key 4").

## Files

- `Cargo.toml` — crate manifest and both binary targets
- `src/bb_main.rs` — Linux BlueZ client, bb REST/realtime client, status mapping
- `src/main.rs` — everything: arg parsing, BLE connection logic, the WS
  client for `run`, the five subcommands, and a `#[cfg(test)]` self-check
  covering every pure decision the daemon makes (no BLE hardware or live
  Paseo daemon needed — `cargo test`)

## Dependencies

| crate       | why |
|-------------|-----|
| anyhow      | error context/propagation for a CLI |
| serde / serde_json | Paseo WS message parsing (`run`) |
| tungstenite | WebSocket client to the Paseo daemon (`run`) |
| windows     | WinRT Bluetooth LE (all commands) + Win32 global hotkey and foreground-window APIs (`run`, Windows-only) |
| bluer / tokio | BlueZ GATT and async runtime for `bb-led-bridge` on Linux |
| reqwest / tokio-tungstenite | bb thread snapshot and realtime change feed |

## Linux bb bridge

```bash
cargo build --release --bin bb-led-bridge
./target/release/bb-led-bridge frame 1=blue,F=white
./target/release/bb-led-bridge demo
./target/release/bb-led-bridge run
```

The default bb URL is `http://127.0.0.1:38886` (override with `BB_URL`,
`BB_SERVER_URL`, or `--bb-url`). The default paired-device name filter is
`Go60` (override with `--name`). The daemon fetches bb's
`/api/v1/sidebar-bootstrap` data and the status-sidebar `listLater` RPC. It
reproduces that plugin's section order, activity detection, pinned-first sort,
and environment grouping, then subscribes to bb thread-list changes and the
plugin's `later-threads` realtime signal.

Install the release binary and user service with `../install-omarchy.sh`; see
`../README.md` for the Hyprland bindings.

CLI parsing is hand-rolled in `main.rs` (five subcommands, `--name` and
`--ws-url` flags) — not enough surface to justify pulling in `clap`.

## Legacy Windows build

Built from WSL (Linux host), cross-compiled to a Windows x86_64 executable.

Toolchain (user-level, no sudo):
```
mise use -g zig@latest
cargo install cargo-zigbuild
rustup target add x86_64-pc-windows-gnu
```

Build:
```
cd bridge/led-bridge
cargo zigbuild --release --target x86_64-pc-windows-gnu
```

Exact artifact path:
```
bridge/led-bridge/target/x86_64-pc-windows-gnu/release/paseo-led-bridge.exe
```

### Linux-side `cargo check` / `cargo test`

Confirmed working, for fast iteration without touching Windows:
```
cargo check
cargo test    # pure-logic self-check: colors, blink/alarm schedule, usage
              # bar rendering/ordering/cycling, 90% crossing detection,
              # frame diff/debounce/repush, WS message parsing, spec
              # parsing, frame encoding
```
The WinRT/Win32 connection layer (`mod winrt`) is Windows-only and has no
meaningful unit-testable surface without real hardware; it stays thin and
calls into the pure functions above for every decision.

## Usage

```
paseo-led-bridge scan [--name FILTER]
paseo-led-bridge frame <key=color,...> [--name FILTER]
paseo-led-bridge demo [--name FILTER]
paseo-led-bridge run [--name FILTER] [--ws-url URL]
```

`--name` is a case-insensitive substring match on the paired BLE device
name, default `Go60`. `--ws-url` is the Paseo daemon WebSocket URL, default
`ws://127.0.0.1:6767/ws` (or `$PASEO_WS_URL`) — the daemon's listen address
defaults to `127.0.0.1:6767` (override with `PASEO_LISTEN`); `--ws-url`/
`PASEO_WS_URL` exists so a non-default deployment can be pointed at without
a rebuild.

### `scan` / `frame` / `demo` / `debug`

Unchanged BLE spike commands, proven on hardware — see inline `--help`
output (`paseo-led-bridge` with no args) for the short form. `frame` writes
one static frame (keys `1-9,0` only); `demo` cycles a rainbow for 15s;
`debug` dumps every GATT service the paired device exposes.

### `run` — the live-sync daemon

Connects to the Paseo daemon over WebSocket and mirrors state onto the
keyboard continuously, reconnecting both the WS and BLE links on failure.
Single main loop + `mpsc` channel architecture: the WS client, the global
hotkey listener, and the foreground-window poller are all senders into one
channel; the main loop is the only receiver, owns the BLE connection, and
is the only place frames get composed and written.

#### Autostart (background, at logon, with crash restarts)

From this folder, on Windows:

```powershell
powershell -ExecutionPolicy Bypass -File .\install-autostart.ps1
```

Copies the exe to `%LOCALAPPDATA%\PaseoLedBridge\`, wraps it in a hidden-
window launcher, and registers a **PaseoLedBridge** Scheduled Task: starts
at logon in your user session (required for BLE + foreground detection)
and restarts up to 3× a minute apart on a crash. The bridge already
reconnects forever on WS/BLE failures, so restarts only cover hard
crashes. Manage or remove it in Task Scheduler; to debug, stop the task
and run `paseo-led-bridge.exe run` in a terminal instead. After updating
the exe, re-run the installer; it stops the old bridge, replaces it, and
starts the new one.

#### Key map & color legend

| Key | Logical index | Meaning | Color |
|-----|----------------|---------|-------|
| `1`-`9`,`0` | 0-9 | pinned workspace slot status | see below |
| `Y` | 10 | any slotted workspace needs input | yellow `#FF5F00` / off |
| `U` | 11 | any slotted workspace needs input | red `#FF0000` / off |
| `J` | 12 | commit (Paseo foreground only) | `#00FF60` / off |
| `K` | 13 | push (Paseo foreground only) | `#0080FF` / off |
| `L` | 14 | open PR (Paseo foreground only) | `#A020F0` / off |
| `;` | 15 | merge (Paseo foreground only) | `#FF2000` / off |
| `F` | 16 | focus (Paseo foreground only) | `#202020` / off |
| `D` | 17 | focus chat input (Paseo foreground only) | `#FFE000` / off |

Slot derivation: slots 1-10 = workspaces with a non-null `pinnedAt` and a
null `archivingAt`, sorted by `pinnedAt` descending (newest pin = slot 1 =
logical LED 0), capped at 10. Slots are re-derived from the live workspace
map after every `workspace_update`.

Slot status colors (feature 1):

| Status | Color | Meaning |
|---|---|---|
| `done` | dim white `#0A0A0A` | idle |
| `needs_input` | yellow `#FF5F00` | waiting on you |
| `attention` | green `#00C800` | finished, unread (Paseo flips it to `done` once you view it) |
| `failed` | red `#FF0000` | |
| `running` | blue `#0033FF` | |
| *(empty slot)* | off | |

Blink-then-solid (feature 2): the instant a *workspace* (tracked by id,
not slot position — pin reordering doesn't retrigger this) transitions
into `attention` from a different previously-known status, it blinks
green 400ms on / 400ms off for 3 full cycles (2.4s), then holds solid
green. A workspace that simply *appears* already at `attention` (a fresh
snapshot, a newly-pinned workspace, a reconnect) renders solid immediately
— no blink.

Permission glow (feature 4): `Y`/`U` light together only when a slotted
agent has a pending permission that is not a question. They stay off for
agent questions, even though those also set the workspace to `needs_input`.

Foreground action indicators (feature 3): `J K L ; F` only light while the
foreground window's owning process image ends with `Paseo.exe`
(case-insensitive), polled via Win32 every 500ms.

#### Usage-bar mode (feature 5)

Global hotkey **Shift+F17**. First press fetches usage over the existing
WS connection (`provider.usage.list.request` / cached 60s) and clears the
keyboard for a 12-column display: `=` is a fixed green marker, `1..0` are
the ten usage columns, and `-` is a fixed red marker. Each used column has
its own color in a continuous green → yellow → orange → red gradient. All
columns stay solid. The normal Paseo display
returns six seconds after the last press. Tracked windows, in display order
(absent ones skipped): Claude `five_hour`, Claude `weekly`, Codex `weekly`,
then Claude `weekly_model*` windows. Each further press cycles to the next
tracked window and logs it, e.g.:
```
usage: claude five_hour 62% (resets 2026-08-10T18:00:00Z)
```
Auto-exits back to normal frames 6s after the last press. On fetch
failure, flashes the 10 number keys red once (1s), logs, and exits.

#### 90% usage alarm (feature 6)

A background poll every 5 minutes (sharing the same cache/fetch plumbing
as the hotkey mode) checks the same three tracked windows. The first poll
seeds a silent baseline. Any window crossing from <90% to ≥90% (upward
only) flashes the **entire keyboard** red 3× (fill-all `0xFE` red 500ms /
fill-all off 500ms) and logs the crossing, then repaints whatever the
normal frame currently is (status, or usage-bar mode if that happens to
still be active).

#### Debounce / dedupe / repush

The normal 17 status pixels go through one `FrameSender`: state changes are debounced
(~60ms) into a single write, unchanged pixels are dropped from that write,
and a full frame is force-resent every 30s regardless of change (in case
the firmware missed a write or reset). Usage-bar and alarm frames
temporarily override the status frame through the same engine — normal
state frames resume, with a full repaint, once the override ends.

### Paseo WS protocol

Verified against Paseo's protocol source (`packages/protocol/src/messages.ts`).
Connects to `ws://127.0.0.1:6767/ws` (blocking `tungstenite`):

1. On open, the **first** message sent must be `hello`:
   `{"type":"hello","clientId":"paseo-led-bridge","clientType":"cli","protocolVersion":1,"appVersion":"<cargo pkg version>"}`
   — within 15s or the server closes with code 4001; a wrong
   `protocolVersion` closes with 4003.
2. Then: `{"type":"fetch_workspaces_request","requestId":"<unique>","sort":[{"key":"activity_at","direction":"desc"}],"page":{"limit":200},"subscribe":{}}`.
3. Every server message arrives wrapped one level:
   `{"type":"session","message":{...inner...}}` — `run` unwraps exactly
   one level (`parse_ws_value` in `src/main.rs`) and dispatches on the
   inner `type`.
4. Inner `fetch_workspaces_response`: `payload.entries` is a
   `WorkspaceDescriptor[]` — a **full snapshot**, replacing the whole
   in-memory workspace map. (`payload.pageInfo.hasMore`, if true, is
   logged as a warning; pagination isn't implemented.)
5. Inner `workspace_update`: `payload` is tagged on `"kind"` —
   `{"kind":"upsert","workspace":<WorkspaceDescriptor>}` or
   `{"kind":"remove","id":"<workspaceId>"}` — streamed forever, per the
   `subscribe` above.
6. Inner `provider.usage.list.response`: as used by the usage-bar mode /
   alarm below.
7. A top-level (**not** session-wrapped) `{"type":"ping"}` is sent every
   10s; the server replies top-level `{"type":"pong"}`. Any successful
   read (including `pong`) counts as liveness; 15s of read silence is
   treated as dead — close, sleep 3s, and reconnect from step 1 (full
   state rebuild: the next `fetch_workspaces_response` replaces the map
   from scratch).

`WorkspaceDescriptor` fields (all tolerant of being absent — `#[serde(default)]`):
`id: string`, `name: string`, `pinnedAt: string|null` (ISO timestamp),
`archivingAt: string|null`, `status: string` (one of `needs_input|failed|
running|attention|done`).

## Error handling

Every step (scan/find, connect, service discovery, characteristic lookup,
write) has a distinct error message, e.g.:

- `no paired BLE device matching name filter 'Go60' ...`
- `failed to connect to Go60: ...`
- `Go60 does not expose the LED GATT service (70617365-...-001)`
- `LED service found on Go60 but write characteristic (70617365-...-002) is missing`
- `write failed: ...`

`run` additionally logs and retries indefinitely on both BLE write failure
(5s backoff) and WS disconnect (3s backoff, then a full reconnect + state
rebuild per the protocol above), rather than exiting.

## Windows runtime caveats (from WinRT/Win32 behavior)

- The Go60 must already be **paired/bonded in Windows Settings >
  Bluetooth & devices** before this tool can connect — WinRT's BLE APIs
  don't do pairing/bonding themselves, only connect to already-known
  devices.
- Since the Go60 is simultaneously bonded as a HID keyboard, expect it to
  show up as a single Bluetooth device in Windows that serves both the HID
  and the custom GATT service.
- This tool writes with `WriteWithoutResponse` first, falling back to
  `WriteWithResponse` once if the device rejects it (and remembers which
  mode worked for subsequent writes).
- `run`'s global hotkey (Shift+F17) and foreground-window polling use
  Win32 (`RegisterHotKey`, `GetForegroundWindow`,
  `QueryFullProcessImageNameW`) — no elevated/admin privileges required
  for a normal interactive session.

## Firmware compatibility

`run` assumes firmware that understands logical indices 0-29, frames up to
18 pixels, and the `0xFE` fill-all op. **The Go60 must be flashed with a
matching firmware build** — an older firmware that only understands 10
pixels (indices 0-9) will silently ignore or mishandle the extra bytes.

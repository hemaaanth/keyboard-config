# Keyboard Config

Canonical repository for my keyboard firmware and layouts.

## Active keyboard: MoErgo Go60

Wireless split Go60 running [MoErgo's ZMK fork](https://github.com/moergo-sc/zmk),
with **Paseo Deck** — a [Codex Micro](https://worklouder.cc/codex-micro)-style
integration that turns the keyboard into a control surface and status display
for [Paseo](https://paseo.sh) AI agents, fully wirelessly (BLE only, no cable).

![Go60 keymap](keymap-drawer/go60.svg)

### How Paseo Deck works

```
                        keystrokes (F13-F24)                agent state
  Go60 ────────────────────────────────────────▶ Windows ◀──────────────── Paseo daemon (WSL)
   ▲                                                │                        ▲
   │    LED frames (custom BLE GATT service)        │                        │
   └────────────────────────────────────────────────┤                        │
                                                    ├── paseo-led-bridge ────┘   WebSocket :6767
                                                    └── paseo-deck.ahk ──▶ paseo-deck.sh ──▶ paseo CLI
```

Three cooperating pieces:

1. **Firmware** (`config/paseo-leds/`, a ZMK module): exposes a custom BLE
   GATT service. The host writes per-key RGB frames to the left half
   (the BLE central), which paints its own LEDs and forwards right-half
   pixels over the wireless split link. A `paseo` layer (held via the left
   thumb key) emits F13–F24 — dead keys to every app except the bridge.
2. **`bridge/led-bridge/`** (Rust, single Windows exe, ~1.4 MB): subscribes
   to the Paseo daemon's WebSocket for live workspace state and mirrors it
   onto the LEDs; also renders the usage bar and alarms. Runs hidden at
   logon via a scheduled task (`install-autostart.ps1`).
3. **`bridge/paseo-deck.ahk` + `paseo-deck.sh`** (AutoHotkey v2 + WSL bash):
   catch the F13–F24 keys globally, focus/jump the Paseo window, and drive
   agent actions through the `paseo` CLI.

**Slots:** pin a workspace in Paseo and it gets a number-key LED and jump
slot — newest pin = key `1`. Colors: **white** idle · **yellow** needs
input · **blue** working · **green** finished, unread (blinks, then solid;
back to white once read) · **red** error.

**Paseo layer keys** (hold left thumb key):

| Keys | Action |
|---|---|
| `1`–`0` | Jump to workspace slot 1–10 |
| `Y` / `U` | Approve / deny a pending permission (they glow yellow/red when one is waiting) |
| `J` `K` `L` `;` | Commit / push / PR / merge on the active slot (lit in unique colors while Paseo is focused) |
| `F` | Focus the Paseo window (always-on white beacon) |
| `H` | Usage bar: number row shows Claude 5h → Claude weekly → Codex usage as a 10-segment green→red bar |
| `W` / `S` | Thinking effort up / down |
| `Q` / `A` | Permission mode up (toward bypass) / down (toward plan) |
| `T` | Toggle the LED sync on/off |

The whole keyboard also flashes red when a usage window crosses 90%, and
the status colors follow the regular RGB brightness/saturation keys on the
Magic layer. Deep details: [docs/go60-paseo-layer.md](docs/go60-paseo-layer.md)
(firmware/protocol), [bridge/README.md](bridge/README.md) (hotkeys + WSL
helper), [bridge/led-bridge/README.md](bridge/led-bridge/README.md) (LED
bridge internals).

### Files & builds

- `config/go60.keymap` — keymap incl. the `paseo` layer; `config/go60.conf` / `config/default.nix` — ZMK config and nix build
- `scripts/build-go60-local.sh` — local firmware build (podman/docker + nix) → `go60.uf2`; `.github/workflows/build-go60.yml` — same build on Actions
- `config/paseo-leds/` — the LED firmware module; `bridge/` — everything host-side

## Legacy keyboards

### Eyelash Corne (ZMK)

Previous daily driver: a wireless **Eyelash Peripherals Corne** (not
[foostan's Corne](https://github.com/foostan/crkbd) — it doesn't use
standard `corne` firmware). The config still builds: `config/eyelash_corne.*`
plus the custom board definition under `boards/arm/eyelash_corne/`, buildable
via the **Build ZMK firmware** Actions workflow (`build.yaml`), editable with
[Nick Coutsos' Keymap Editor](https://nickcoutsos.github.io/keymap-editor/),
keymap diagram at [`keymap-drawer/eyelash_corne.svg`](keymap-drawer/eyelash_corne.svg).

### QMK layouts

Historical QMK Configurator exports for the Redox Wireless and Idobo/XD75-era
setup are preserved under [`legacy/qmk/`](legacy/qmk/README.md). References
only; not part of any active build.

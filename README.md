# Keyboard Config

Canonical repository for my keyboard firmware and layouts.

## Active keyboard: MoErgo Go60

Wireless split Go60 running [MoErgo's ZMK fork](https://github.com/moergo-sc/zmk),
with **BB Deck** — a [Codex Micro](https://worklouder.cc/codex-micro)-style
integration that turns the keyboard into a control surface and status display
for bb AI agents on Omarchy, fully wirelessly (BLE only, no cable).

![Go60 keymap](keymap-drawer/go60.svg)

### How BB Deck works

```
                     keystrokes (F13-F24)               bb state/events
  Go60 ───────────────────────────────────────▶ Hyprland ◀──────────── bb :38886
   ▲                                               │                     │
   │    LED frames (custom BLE GATT via BlueZ)     │                     │
   └───────────────────────────────────────────────┴── bb-led-bridge ◀────┘
```

Three cooperating pieces:

1. **Firmware** (`config/paseo-leds/`, a ZMK module): exposes a custom BLE
   GATT service. The host writes per-key RGB frames to the left half
   (the BLE central), which paints its own LEDs and forwards right-half
   pixels over the wireless split link. The Agent Deck layer (held via the left
   thumb key) emits F13–F24 — dead keys to every app except the bridge.
2. **`bb-led-bridge`** (Rust): reads the Status Sidebar's exact visible shortcut
   slots, subscribes to bb's realtime WebSocket, and writes changed status
   frames through BlueZ. It runs as a user systemd service and reconnects to
   both bb and the keyboard.
3. **`bb-deck` + Omarchy bindings**: catch the deck's function-key events globally,
   focus bb's web-app window across workspaces, open status-sidebar thread slots,
   focus the composer, and queue commit/push/PR/merge instructions.

**Slots:** the first ten rows in the native **Status Sidebar** get the number-key
LEDs and jump slots, including its pinned section and saved drag order. Colors:
**white** idle/read ·
**yellow** asking a question or awaiting approval · **blue** working ·
**green** finished/unread · **red** error.

**Agent Deck keys** (hold left thumb key):

| Keys | Action |
|---|---|
| `1`–`0` | Jump to Status Sidebar row 1–10 |
| `Y` / `U` | Unassigned |
| `J` `K` `L` `;` | Queue commit / push / PR / merge on the active thread |
| `D` | Focus bb and its composer |
| `F` | Focus bb, switching to its Hyprland workspace (always-on white beacon) |
| `T` | Toggle the LED sync on/off |

the status colors follow the regular RGB brightness/saturation keys on the
Magic layer. Deep details: [docs/go60-paseo-layer.md](docs/go60-paseo-layer.md)
(firmware/protocol), [bridge/README.md](bridge/README.md) (Omarchy install and
hotkeys), [bridge/led-bridge/README.md](bridge/led-bridge/README.md) (bridge
internals). The former Windows/Paseo programs remain in `bridge/` as legacy
support.

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

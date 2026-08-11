# Go60 Paseo layer

The ZMK side of Phase 1 of the Go60 ↔ Paseo integration (see
`../bridge/README.md` for the Windows/WSL side). Implemented as the `paseo`
layer in `config/go60.keymap`.

## Access

Hold the **left thumb cluster's third key** (previously `&none` on the
Windows layer) — it's `&mo LAYER_Paseo`, so the layer is active only while
held. The Mac layer is untouched (Paseo runs on the Windows side).

## Bindings while held

| Key | Sends | Meaning |
|-----|-------|---------|
| `1`–`9`, `0` | F13–F22 | Jump to Paseo workspace slot 1–10 |
| `Y` | F23 | Approve pending permission |
| `U` | F24 | Deny pending permission |
| `H` | Shift+F17 | Usage-bar hotkey for the host bridge |
| `D` | Ctrl+L | Focus Paseo's chat input |
| `J` | Shift+F13 | Commit on the active slot's agent |
| `K` | Shift+F14 | Push |
| `L` | Shift+F15 | Open PR |
| `;` | Shift+F16 | Merge |
| `F` | Shift+F24 | Focus the Paseo window |
| `T` | `&plt` | Toggle Paseo LEDs sync (see below) |
| `W` / `S` | Shift+F18 / Shift+F19 | Thinking effort up / down on the active slot |
| `Q` / `A` | Shift+F20 / Shift+F21 | Mode up (toward bypass) / down (toward plan) |

`G`/`P`/`R`/`M` (the old commit/push/pr/merge positions) are now `&none`.
Everything else on the layer is `&none`, so a held layer key can't type
stray characters. F13–F24 are dead keys on Windows unless the AutoHotkey
bridge is running — same conflict-free idea as the Hyper-style
`Ctrl+Win+Alt+Shift+F5` key on the right thumb.

## Paseo LEDs (config/paseo-leds)

Per-key BLE LED control, added on top of the layer above. The host writes
RGB frames over a custom GATT service to the left half (central), which
paints its own LEDs directly and forwards right-half pixels to the
peripheral over the split link.

### Logical indices

| Logical index | Key | Half |
|---|---|---|
| 0–4 | 1 2 3 4 5 | left |
| 5–9 | 6 7 8 9 0 | right |
| 10 | Y | right |
| 11 | U | right |
| 12 | J | right |
| 13 | K | right |
| 14 | L | right |
| 15 | `;` | right |
| 16–17 | F D | left |
| 18–29 | `=` `1`–`5` `6`–`0` `-` | full keyboard columns, left to right |

`left-number-row-indices` / `right-number-row-indices` (config/go60.keymap's
`paseo_leds` node) are hardware-verified — do not change. `left-extra-indices`
(order: F D) and `right-extra-indices` (order: Y U J K L `;`) extend the same
lookup for the new logical indices 10–17; derivation (from the documented
left-half strip grid in `config/paseo-leds/src/paseo_leds.c`):

- **F** = row 2 ("mo A S D F G"), column 4 → strip index 9. **D** = row 2,
  column 3 → strip index 14. Both come directly from the hardware-verified
  left-half grid.
- **Y** = right row 1 ("Y U I O P ESC"), column 1 → mirrors left row 1
  column 5 (index 4). **U** = column 2 → mirrors left column 4 (index 8).
- **J** = right row 2 ("H J K L ; '"), column 2 → mirrors left row 2
  column 4 (index 9). **K** = column 3 → mirrors left column 3 (index 14).
  **L** = column 4 → mirrors left column 2 (index 19). **;** = column 5 →
  mirrors left column 1 (index 24).

The right-half mirror carries the same not-verified-on-hardware caveat as
`right-number-row-indices`; correct `right-extra-indices` in the keymap if
the wrong keys light up on real hardware.

### Fill-all frame op

A frame pixel entry (or forwarded behavior invocation) with index byte
`0xFE` means "set every LED on both halves to this r,g,b" — used for
full-keyboard alarm flashes. It isn't a logical index and doesn't consult
the tables above. Entries are applied in frame order, so a normal entry
listed after a fill-all in the same frame still layers its single pixel on
top.

### Paseo-mode toggle (`&plt`)

`T` on the Paseo layer toggles a module-global enabled flag on both halves
at once (global-locality behavior). Frames/behavior invocations always
keep updating the pixel buffers regardless of this flag; only the final
`led_strip` flush is gated on it. Disabling blanks the strip once;
re-enabling immediately flushes whatever was already buffered.

### Brightness/saturation

Underglow itself stays off (`CONFIG_ZMK_RGB_UNDERGLOW_ON_START=n`), but its
brightness/saturation settings — adjustable via the existing `rgb_ug` keys
on the Magic layer — still apply to Paseo LED pixels at flush time. There's
no live getter for those values in the MoErgo ZMK fork (only an on/off
getter), so the firmware reads the underglow's own persisted
`rgb/underglow/state` settings entry directly via
`settings_load_subtree_direct()` rather than duplicating or linking against
`rgb_underglow.c`'s internals — the least invasive option, documented in
`paseo_leds.c`. If that read fails or nothing was ever saved, it falls back
to brightness 40 / saturation 100. Every channel is unconditionally clamped
to 40% of 255, mirroring the board's `CONFIG_ZMK_RGB_UNDERGLOW_BRT_MAX=40`
hardware power ceiling (`go60_lh_defconfig`'s "DO NOT CHANGE ... ABOVE 40").
The boot self-test (dim green flash on the number row ~2s after boot) goes
through the same flush path, so it exercises this scaling too.

## Rebuild

```
./scripts/build-go60-local.sh   # podman/docker + nix, outputs go60.uf2
```

or push and run the **Build Go60 firmware** GitHub Actions workflow
(`.github/workflows/build-go60.yml`), then flash the `go60.uf2` via the
bootloader mass-storage device.

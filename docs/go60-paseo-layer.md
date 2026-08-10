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
| `G` | Shift+F13 | Commit on the active slot's agent |
| `P` | Shift+F14 | Push |
| `R` | Shift+F15 | Open PR |
| `M` | Shift+F16 | Merge |
| `F` | Shift+F24 | Focus the Paseo window |

Everything else on the layer is `&none`, so a held layer key can't type
stray characters. F13–F24 are dead keys on Windows unless the AutoHotkey
bridge is running — same conflict-free idea as the Hyper-style
`Ctrl+Win+Alt+Shift+F5` key on the right thumb.

## Rebuild

```
./scripts/build-go60-local.sh   # podman/docker + nix, outputs go60.uf2
```

or push and run the **Build Go60 firmware** GitHub Actions workflow
(`.github/workflows/build-go60.yml`), then flash the `go60.uf2` via the
bootloader mass-storage device.

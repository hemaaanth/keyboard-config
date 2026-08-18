# BB Deck on Omarchy

The Go60's Agent Deck layer emits F13–F24. Omarchy/Hyprland catches those
otherwise-unused keys and routes them through `bb-deck`, while
`bb-led-bridge` mirrors Status Sidebar thread state to the keyboard over the
existing custom Bluetooth GATT service. Slot order comes from the installed
`status-sidebar` plugin's status-first model.

## Install

The Go60 must already be paired in Linux Bluetooth settings and bb must be
running on its normal loopback address (`http://127.0.0.1:38886`). Then:

```bash
bridge/install-omarchy.sh
```

Add these bindings to `~/.config/hypr/bindings.lua`:

```lua
-- Go60 Agent Deck: raw XKB codes 191-202 are F13-F24. Raw codes are
-- intentional: symbolic F13-F24 bindings are unreliable for this BLE HID.
o.bind("code:191", "BB thread slot 1", "bb-deck slot 1")
o.bind("code:192", "BB thread slot 2", "bb-deck slot 2")
o.bind("code:193", "BB thread slot 3", "bb-deck slot 3")
o.bind("code:194", "BB thread slot 4", "bb-deck slot 4")
o.bind("code:195", "BB thread slot 5", "bb-deck slot 5")
o.bind("code:196", "BB thread slot 6", "bb-deck slot 6")
o.bind("code:197", "BB thread slot 7", "bb-deck slot 7")
o.bind("code:198", "BB thread slot 8", "bb-deck slot 8")
o.bind("code:199", "BB thread slot 9", "bb-deck slot 9")
o.bind("code:200", "BB thread slot 10", "bb-deck slot 10")
o.bind("SHIFT + code:191", "Ask BB to commit", "bb-deck action commit")
o.bind("SHIFT + code:192", "Ask BB to push", "bb-deck action push")
o.bind("SHIFT + code:193", "Ask BB to open a PR", "bb-deck action pr")
o.bind("SHIFT + code:194", "Ask BB to merge", "bb-deck action merge")
o.bind("SHIFT + code:201", "Focus BB composer", "bb-deck composer")
o.bind("SHIFT + code:202", "Focus BB", "bb-deck focus")
```

Hyprland switches to the workspace containing bb when `bb-deck focus` runs.
`bb-deck` reads bb's web-app URL from
`~/.local/share/applications/BB.desktop` and derives the Chromium window class
from it. Reinstalling the web app with a different URL therefore keeps focus
and slot shortcuts attached to the existing window. `BB_APP_URL`,
`BB_WINDOW_CLASS`, and `BB_DESKTOP_FILE` can override discovery when needed.
If the web app is closed, Omarchy launches it at the discovered URL.
After editing the bindings, validate them:

```bash
hyprctl reload
hyprctl configerrors
```

`D` changed from `Ctrl+L` to `Shift+F23`, so flash the current
`config/go60.keymap` build before expecting the composer shortcut to work.

## Slots and colors

The first ten Status Sidebar shortcut rows map directly to the number row and
the F13–F22 slot shortcuts. The sidebar publishes the exact client-visible
order, so Status/Projects mode, collapsed sections, search filtering, saved
drag order, worktree grouping, and attached-agent exclusion all match the
physical mapping. Older plugin builds fall back to the previous reconstructed
status-first projection. Run `bb-deck slots` to print the current mapping.

| Color | bb state |
|---|---|
| dim white | idle and read |
| blue | runtime, workflow, background agent/command, plan, or goal active |
| yellow | pending question, approval, or permission |
| green | finished attention that has not been read |
| red | error |

`Y` and `U` are unassigned. The `F` LED is a dim always-on locator for the
focus shortcut. `bb-led-bridge` subscribes to bb's `/ws` thread-list changes
and status-sidebar's `Later` and thread-order signals; a 30-second full refresh
recovers missed BLE writes.

## Commands and diagnostics

```bash
systemctl --user status bb-led-bridge
journalctl --user -u bb-led-bridge -f

bb-deck slots
bb-led-bridge frame 1=blue,2=question,F=white
bb-led-bridge demo
bb-led-bridge run --bb-url http://127.0.0.1:38886 --name Go60
```

The slot helper remembers the last selected thread under `$XDG_RUNTIME_DIR`.
Action keys target that thread. If none has been selected, it falls back to a
Status Sidebar row with a pending interaction, then slot 1.

## Legacy Windows/Paseo support

The previous implementation remains available:

- `paseo-deck.ahk` catches the same keys on Windows.
- `paseo-deck.sh` drives the Paseo CLI through WSL.
- `paseo-deck-test.sh` is its read-only smoke test.
- `led-bridge/src/main.rs` builds `paseo-led-bridge.exe` and consumes Paseo's
  WebSocket protocol through WinRT BLE.

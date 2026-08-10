# Paseo Deck (Phase 1)

Phase 1 of the Go60 ↔ Paseo integration: a MoErgo Go60 "Paseo layer" emits
F13-F24 as global hotkeys. A Windows AutoHotkey v2 script catches them,
focuses the Paseo desktop app, and delegates agent actions to a bash helper
running inside WSL that drives the `paseo` CLI.

```
Go60 (ZMK) --F13-F24--> Windows (AHK v2) --wsl.exe--> bash helper --> paseo CLI
```

## Hotkey table

| Key       | Action                                              |
|-----------|------------------------------------------------------|
| F13-F21   | Jump to Paseo workspace slot 1-9 (Ctrl+1..9)          |
| F22       | Focus Paseo, select slot 10 (no keystroke sent — Ctrl+0 is zoom reset) |
| F23       | Approve the pending permission for the active slot    |
| F24       | Deny the pending permission for the active slot       |
| Shift+F13 | Commit on the active slot's agent                     |
| Shift+F14 | Push on the active slot's agent                       |
| Shift+F15 | Open a PR on the active slot's agent                  |
| Shift+F16 | Merge the open PR on the active slot's agent           |
| Shift+F24 | Focus Paseo only                                       |
| Shift+F18/F19 | Thinking effort up / down on the active slot's agent |
| Shift+F20/F21 | Mode up (toward bypass) / down (toward plan)         |

"Active slot" is whichever F13-F22 key was pressed most recently.

## Install (Windows)

1. Install [AutoHotkey v2](https://www.autohotkey.com/).
2. Open `paseo-deck.ahk` and check the `HelperPath` variable at the top — it
   defaults to `/home/system/Documents/Development/keyboard-config/bridge/paseo-deck.sh`.
   Adjust it if this repo lives somewhere else inside WSL.
3. Run `paseo-deck.ahk` (double-click, or `AutoHotkey64.exe paseo-deck.ahk`).
4. For autostart: press Win+R, run `shell:startup`, and drop a shortcut to
   `paseo-deck.ahk` in the folder that opens.

## Test

Before wiring up the keyboard, run the smoke test inside WSL:

```
bridge/paseo-deck-test.sh
```

It only calls read-only `paseo` commands (`workspace ls`, `agent ls`,
`permit ls`, and `resolve`) and never touches `permit allow/deny` or
`agent send`. It should print `ALL PASS` and exit 0.

Once that passes, program F13-F24 on the Go60 (see
`../docs/go60-paseo-layer.md`) and press the keys — each press should show a
tray notification with the result.

## Troubleshooting

- Check `paseo-deck.log` next to the `.ahk` script for a timestamped history
  of every helper invocation and its result.
- `wsl.exe` must resolve on PATH and the default WSL distro must be the one
  with the `paseo` daemon and this repo.
- The Paseo daemon must be running (`paseo workspace ls` should work from a
  WSL shell).
- A `busy` tray tip means a previous key press is still in flight — actions
  are single-flight and dropped, not queued, so just press again.

## Phase 2

Phase 2 replaces the WSL-CLI hop with a native Windows bridge and adds BLE
status LEDs on the Go60 for agent state (running/idle/error/permission
pending) instead of relying on tray notifications alone.

#!/usr/bin/env bash
# Build and install the Linux bb bridge plus its user service and deck helper.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd "$script_dir/.." && pwd)

cargo build --release --bin bb-led-bridge --manifest-path "$script_dir/led-bridge/Cargo.toml"
install -Dm755 "$script_dir/led-bridge/target/release/bb-led-bridge" "$HOME/.local/bin/bb-led-bridge"
install -Dm755 "$script_dir/bb-deck.sh" "$HOME/.local/bin/bb-deck"
install -Dm644 "$script_dir/systemd/bb-led-bridge.service" "$HOME/.config/systemd/user/bb-led-bridge.service"

systemctl --user daemon-reload
systemctl --user enable bb-led-bridge.service
systemctl --user restart bb-led-bridge.service

echo "Installed bb-led-bridge and bb-deck from $repo_dir"
echo "Add the Hyprland bindings documented in bridge/README.md, then run:"
echo "  hyprctl reload && hyprctl configerrors"

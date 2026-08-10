{ pkgs ? import <nixpkgs> {}
, firmware ? import ../src {}
}:

let
  config = ./.;
  studioModules = [
    firmware.zephyr.modules.nanopb.modulePath
    firmware.zephyr.modules."zmk-studio-messages".modulePath
  ];

  # Phase 2 firmware spike: per-key BLE LED control (config/paseo-leds).
  # Needed on both halves -- the behavior it defines lives in the shared
  # keymap, which both go60_lh and go60_rh compile.
  extraModules = studioModules ++ [ "${config}/paseo-leds" ];

  go60_left = firmware.zmk.override {
    board = "go60_lh";
    keymap = "${config}/go60.keymap";
    kconfig = "${config}/go60.conf";
    snippets = [ "studio-rpc-usb-uart" ];
    extraModules = extraModules;
  };

  go60_right = firmware.zmk.override {
    board = "go60_rh";
    keymap = "${config}/go60.keymap";
    kconfig = "${config}/go60.conf";
    extraModules = extraModules;
  };

in firmware.combine_uf2 go60_left go60_right "go60"

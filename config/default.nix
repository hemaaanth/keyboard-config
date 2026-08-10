{ pkgs ? import <nixpkgs> {}
, firmware ? import ../src {}
}:

let
  config = ./.;
  studioModules = [
    firmware.zephyr.modules.nanopb.modulePath
    firmware.zephyr.modules."zmk-studio-messages".modulePath
  ];


  go60_left = firmware.zmk.override {
    board = "go60_lh";
    keymap = "${config}/go60.keymap";
    kconfig = "${config}/go60.conf";
    snippets = [ "studio-rpc-usb-uart" ];
    extraModules = studioModules;
  };

  go60_right = firmware.zmk.override {
    board = "go60_rh";
    keymap = "${config}/go60.keymap";
    kconfig = "${config}/go60.conf";
    extraModules = studioModules;
  };

in firmware.combine_uf2 go60_left go60_right "go60"

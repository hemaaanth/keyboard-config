# Paseo LEDs -- Phase 2 firmware spike (per-key RGB over BLE, number row)
# SPDX-License-Identifier: MIT

if(CONFIG_PASEO_LEDS)
  zephyr_library()
  zephyr_library_sources(src/paseo_leds.c)
  zephyr_library_sources(src/paseo_leds_mic.c)
  zephyr_library_sources(src/paseo_leds_toggle.c)

  # drivers/behavior.h, zmk/*.h etc. live in the main ZMK app's own
  # include/, which isn't inherited by an out-of-tree zephyr_library().
  zephyr_include_directories(${APPLICATION_SOURCE_DIR}/include)
endif()

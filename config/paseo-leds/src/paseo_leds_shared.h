/*
 * Paseo LEDs -- Phase 2 firmware spike: tiny cross-file interface between
 * paseo_leds.c (owns the strip buffer/device and the paseo_leds_enabled
 * flag) and paseo_leds_toggle.c (the &plt behavior driver, split into its
 * own file because it needs a second DT_DRV_COMPAT -- see the comment at
 * the top of paseo_leds_toggle.c).
 *
 * SPDX-License-Identifier: MIT
 */

#pragma once

/* Flips paseo_leds_enabled. On disable, blanks the strip once (writes off
 * pixels straight to hardware; the stored pixel buffer is untouched). On
 * re-enable, flushes the stored buffer immediately (through the normal
 * brightness/saturation-scaling flush path). Safe to call from either
 * split role. */
void paseo_leds_toggle(void);

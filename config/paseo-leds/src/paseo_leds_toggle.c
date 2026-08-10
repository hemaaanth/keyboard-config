/*
 * Paseo LEDs -- Phase 2 firmware spike: global Paseo-mode toggle behavior.
 *
 * SPDX-License-Identifier: MIT
 *
 * paseo_leds.c already does `#define DT_DRV_COMPAT paseo_leds_invoke` for
 * its own behavior driver; a second `#define DT_DRV_COMPAT` in the same
 * translation unit would just redefine that macro out from under the
 * DT_INST_ / BEHAVIOR_DT_INST_DEFINE boilerplate the first driver uses
 * (they all implicitly key off DT_DRV_COMPAT). Rather than hand-expand the
 * DT_INST macros with an explicit compat argument, this compat gets its
 * own tiny source file, which is the documented ZMK pattern for a module
 * that defines more than one behavior.
 *
 * Global locality, bound directly in the keymap as &plt (unlike &pled,
 * which is only ever invoked programmatically): ZMK runs a global-locality
 * behavior locally on the pressing half AND forwards it to the other half,
 * so both sides call paseo_leds_toggle() (defined in paseo_leds.c) and
 * flip their own paseo_leds_enabled flag independently -- there is no
 * shared state to coordinate here, just two halves reacting the same way
 * to the same keypress.
 */

#define DT_DRV_COMPAT paseo_leds_toggle

#include <zephyr/device.h>
#include <zephyr/kernel.h>

#include <drivers/behavior.h>
#include <zmk/behavior.h>

#include "paseo_leds_shared.h"

#if DT_HAS_COMPAT_STATUS_OKAY(DT_DRV_COMPAT)

static int paseo_leds_toggle_pressed(struct zmk_behavior_binding *binding,
                                     struct zmk_behavior_binding_event event) {
    ARG_UNUSED(binding);
    ARG_UNUSED(event);

    paseo_leds_toggle();

    return ZMK_BEHAVIOR_OPAQUE;
}

static int paseo_leds_toggle_released(struct zmk_behavior_binding *binding,
                                      struct zmk_behavior_binding_event event) {
    ARG_UNUSED(binding);
    ARG_UNUSED(event);
    return ZMK_BEHAVIOR_OPAQUE;
}

static const struct behavior_driver_api paseo_leds_toggle_driver_api = {
    .binding_pressed = paseo_leds_toggle_pressed,
    .binding_released = paseo_leds_toggle_released,
    .locality = BEHAVIOR_LOCALITY_GLOBAL,
#if IS_ENABLED(CONFIG_ZMK_BEHAVIOR_METADATA)
    .get_parameter_metadata = zmk_behavior_get_empty_param_metadata,
#endif
};

BEHAVIOR_DT_INST_DEFINE(0, NULL, NULL, NULL, NULL, POST_KERNEL, CONFIG_KERNEL_INIT_PRIORITY_DEFAULT,
                        &paseo_leds_toggle_driver_api);

#endif /* DT_HAS_COMPAT_STATUS_OKAY(DT_DRV_COMPAT) */

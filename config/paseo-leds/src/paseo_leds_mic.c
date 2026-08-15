/* SPDX-License-Identifier: MIT */

#define DT_DRV_COMPAT paseo_leds_mic

#include <zephyr/device.h>
#include <zephyr/kernel.h>

#include <drivers/behavior.h>
#include <zmk/behavior.h>

#include "paseo_leds_shared.h"

#if DT_HAS_COMPAT_STATUS_OKAY(DT_DRV_COMPAT)

static int paseo_leds_mic_pressed(struct zmk_behavior_binding *binding,
                                  struct zmk_behavior_binding_event event) {
    ARG_UNUSED(binding);
    ARG_UNUSED(event);
    paseo_leds_mic_set(true);
    return ZMK_BEHAVIOR_OPAQUE;
}

static int paseo_leds_mic_released(struct zmk_behavior_binding *binding,
                                   struct zmk_behavior_binding_event event) {
    ARG_UNUSED(binding);
    ARG_UNUSED(event);
    paseo_leds_mic_set(false);
    return ZMK_BEHAVIOR_OPAQUE;
}

static const struct behavior_driver_api paseo_leds_mic_driver_api = {
    .binding_pressed = paseo_leds_mic_pressed,
    .binding_released = paseo_leds_mic_released,
    .locality = BEHAVIOR_LOCALITY_GLOBAL,
#if IS_ENABLED(CONFIG_ZMK_BEHAVIOR_METADATA)
    .get_parameter_metadata = zmk_behavior_get_empty_param_metadata,
#endif
};

BEHAVIOR_DT_INST_DEFINE(0, NULL, NULL, NULL, NULL, POST_KERNEL, CONFIG_KERNEL_INIT_PRIORITY_DEFAULT,
                        &paseo_leds_mic_driver_api);

#endif

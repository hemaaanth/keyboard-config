/*
 * Paseo LEDs -- Phase 2 firmware spike: let a host set per-key RGB on the
 * number row and home-row action keys over BLE, fully wireless.
 *
 * SPDX-License-Identifier: MIT
 *
 * Central (left half):
 *   - hosts a custom GATT service (fixed UUIDs below, coordinated with the
 *     Rust host bridge in bridge/led-bridge -- do not regenerate them)
 *   - a write lands on `paseo_leds_write()`, which only copies bytes and
 *     defers to a work item (never touch BLE/strip APIs from a GATT
 *     callback)
 *   - the work item applies left-owned pixels (logical index 0-4, 16)
 *     straight to its own strip, and pushes right-owned pixels
 *     (5-9, 10-15) to the peripheral via the global-locality
 *     `paseo,leds-invoke` behavior
 *
 * Peripheral (right half):
 *   - receives the behavior invocation (global locality runs it on every
 *     peripheral, see app/src/split/peripheral.c) and applies the packed
 *     pixels to its own strip
 *
 * Fill-all op: a frame/behavior-invocation pixel entry whose index byte is
 * 0xFE (PASEO_LEDS_FILL_ALL) means "set every LED on BOTH halves to this
 * r,g,b" -- used by the host for full-keyboard alarm flashes. It is not a
 * logical index and never consults the strip-index tables. The central
 * fills its own strip_pixels buffer, flushes immediately, and forwards
 * exactly one packed 0xFE entry to the peripheral; the peripheral fills
 * and flushes its own buffer the same way on receipt. Pixel entries within
 * one frame are applied in order, so a normal entry later in the same
 * frame still layers its single pixel on top of an earlier fill-all, on
 * whichever half owns that pixel.
 *
 * Paseo-mode toggle (&plt, config/paseo-leds/src/paseo_leds_toggle.c):
 * frames/behavior invocations always keep updating the pixel buffers below
 * regardless of the toggle state; only the final led_strip flush is gated
 * on paseo_leds_enabled. Disabling blanks the strip once (writes off
 * pixels straight to hardware, buffer untouched); re-enabling flushes the
 * stored buffer immediately.
 *
 * Brightness/saturation: applied to every pixel at flush time (including
 * the boot self-test below), scaled from the *existing* rgb_ug brightness/
 * saturation settings even though underglow itself stays off -- see the
 * derivation comment above paseo_leds_get_brightness_saturation(). Final
 * channel values are always clamped to 40% of 255, mirroring the hardware
 * power ceiling documented as CONFIG_ZMK_RGB_UNDERGLOW_BRT_MAX=40 in
 * go60_lh_defconfig ("DO NOT CHANGE ... TO ABOVE 40").
 *
 * All LEDs are off at boot; nothing is painted until the first frame or
 * behavior invocation arrives (aside from the boot self-test).
 */

#define DT_DRV_COMPAT paseo_leds_invoke

#include <string.h>

#include <zephyr/device.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/util.h>
#include <zephyr/bluetooth/gatt.h>
#include <zephyr/bluetooth/uuid.h>
#include <zephyr/bluetooth/att.h>
#include <zephyr/drivers/led_strip.h>

#include <drivers/behavior.h>
#include <drivers/ext_power.h>
#include <zmk/behavior.h>

#include "paseo_leds_shared.h"

#include <zephyr/logging/log.h>
LOG_MODULE_REGISTER(paseo_leds, CONFIG_ZMK_LOG_LEVEL);

/*
 * Fixed GATT UUIDs. Coordinated with the host bridge (bridge/led-bridge),
 * which is built against these exact values -- do not regenerate.
 *   service:        70617365-6f4c-4544-b0a0-000000000001
 *   characteristic: 70617365-6f4c-4544-b0a0-000000000002
 */
#define PASEO_LEDS_SVC_UUID                                                                       \
    BT_UUID_128_ENCODE(0x70617365, 0x6f4c, 0x4544, 0xb0a0, 0x000000000001)
#define PASEO_LEDS_CHRC_UUID                                                                      \
    BT_UUID_128_ENCODE(0x70617365, 0x6f4c, 0x4544, 0xb0a0, 0x000000000002)

/* Logical indices 0-17: 0-4 left number row, 5-9 right number row, 10-15
 * right home-row action keys (Y U J K L SEMI), 16-17 left home-row F D.
 * Indices 18-29 are full keyboard columns, left to right. They are used by
 * the usage display: =, 1-5, 6-0, -. */
#define PASEO_LEDS_NUM_LOGICAL 30
#define PASEO_LEDS_USAGE_COLUMN_FIRST 18
#define PASEO_LEDS_USAGE_COLUMN_COUNT 12
#define PASEO_LEDS_USAGE_RIGHT_FIRST 24
#define PASEO_LEDS_UNUSED_PIXEL 0xFFu
/* A normal 17-key frame plus one optional fill-all entry per frame. */
#define PASEO_LEDS_MAX_PIXELS 18

/* Packed-pixel sentinel meaning "no second pixel this behavior call". */
#define PASEO_LEDS_NO_PIXEL 0xFFFFFFFFu

/* Not a logical index -- "set every LED on both halves to r,g,b". See the
 * module comment above. */
#define PASEO_LEDS_FILL_ALL 0xFEu

#define PASEO_LEDS_CFG DT_NODELABEL(paseo_leds)

#define STRIP_CHOSEN DT_CHOSEN(zmk_underglow)
#define STRIP_NUM_PIXELS DT_PROP(STRIP_CHOSEN, chain_length)

static const struct device *const led_strip = DEVICE_DT_GET(STRIP_CHOSEN);
static struct led_rgb strip_pixels[STRIP_NUM_PIXELS];

static void paseo_leds_set_local_pixel(uint8_t strip_idx, uint8_t r, uint8_t g, uint8_t b) {
    if (strip_idx >= STRIP_NUM_PIXELS) {
        LOG_WRN("Strip index %d out of range (%d pixels)", strip_idx, STRIP_NUM_PIXELS);
        return;
    }
    strip_pixels[strip_idx] = (struct led_rgb){.r = r, .g = g, .b = b};
}

static void paseo_leds_fill_local(uint8_t r, uint8_t g, uint8_t b) {
    for (int i = 0; i < STRIP_NUM_PIXELS; i++) {
        strip_pixels[i] = (struct led_rgb){.r = r, .g = g, .b = b};
    }
}

/* The left strip layout is documented by go60_lh.dts. The right half mirrors
 * this order, as the existing right number-row mapping does. */
static const uint8_t paseo_leds_column_pixels[6][6] = {
    {26, 27, 28, 29, PASEO_LEDS_UNUSED_PIXEL, PASEO_LEDS_UNUSED_PIXEL},
    {22, 23, 24, 25, PASEO_LEDS_UNUSED_PIXEL, PASEO_LEDS_UNUSED_PIXEL},
    {17, 18, 19, 20, 21, PASEO_LEDS_UNUSED_PIXEL},
    {12, 13, 14, 15, 16, PASEO_LEDS_UNUSED_PIXEL},
    {7, 8, 9, 10, 11, 0},
    {3, 4, 5, 6, 1, 2},
};

static void paseo_leds_set_local_column(uint8_t column, uint8_t r, uint8_t g, uint8_t b) {
    if (column >= ARRAY_SIZE(paseo_leds_column_pixels)) {
        return;
    }
    for (size_t i = 0; i < ARRAY_SIZE(paseo_leds_column_pixels[column]); i++) {
        uint8_t strip_idx = paseo_leds_column_pixels[column][i];
        if (strip_idx != PASEO_LEDS_UNUSED_PIXEL) {
            paseo_leds_set_local_pixel(strip_idx, r, g, b);
        }
    }
}

/* ZMK only powers the LED rail when its underglow subsystem is switched
 * on, and we keep that subsystem off -- so enable external power
 * ourselves or every strip write lands on an unpowered rail.
 * ponytail: rail stays on permanently for the spike; gate it on
 * LED activity if battery life measurably suffers. */
static void paseo_leds_power_on(void) {
#if IS_ENABLED(CONFIG_ZMK_RGB_UNDERGLOW_EXT_POWER)
    static const struct device *const ext_power =
        DEVICE_DT_GET(DT_INST(0, zmk_ext_power_generic));
    if (device_is_ready(ext_power)) {
        ext_power_enable(ext_power);
    } else {
        LOG_WRN("ext_power device not ready");
    }
#endif
}

/*
 * ---- Brightness/saturation, read from the (disabled) underglow's own
 * persisted settings. ----
 *
 * We deliberately keep rgb_ug's underglow output OFF (Kconfig
 * CONFIG_ZMK_RGB_UNDERGLOW_ON_START stays unset / user leaves it off) but
 * still want the user's brightness/saturation preference -- set via the
 * existing rgb_ug RGB_BRI/RGB_BRD/RGB_SAI/RGB_SAD keys on the Magic layer
 * -- to scale these per-key pixels.
 *
 * zmk/rgb_underglow.h (app/include/zmk/rgb_underglow.h in the MoErgo fork)
 * only exposes zmk_rgb_underglow_get_state(bool *on_off) -- on/off, not
 * the HSB values -- so there's no live getter to call. Rather than add one
 * upstream (out of scope for a config-only spike) or duplicate/link
 * against rgb_underglow.c's static `state`, the least invasive option is
 * to read the settings entry it already persists itself
 * ("rgb/underglow/state", saved via zmk_rgb_underglow_save_state() any
 * time brightness/saturation/hue/effect/on-off changes) directly with
 * settings_load_subtree_direct(), which -- unlike a normal
 * SETTINGS_STATIC_HANDLER_DEFINE -- lets us pull one on-demand read
 * without registering a second permanent handler for the same key.
 *
 * That means mirroring the layout of rgb_underglow.c's private
 * `struct rgb_underglow_state` (color.h/s/b, animation_speed,
 * current_effect, animation_step, on, status_active,
 * status_animation_step) well enough to read color.b (brightness, 0-100)
 * and color.s (saturation, 0-100) out of the same bytes. This is a
 * plain-data C struct with no explicit packing on either side, built by
 * the same compiler, so the layout matches; it's still fragile against
 * upstream reordering that struct, which is why the mirror -- and the
 * "why" above -- lives in one place, right here.
 *
 * If settings are disabled, nothing was ever saved, or the read fails,
 * fall back to brt=40 sat=100 (BRT_MAX, full saturation), which is also
 * what a factory-fresh board (no saved rgb_ug state at all) would read.
 */
#if IS_ENABLED(CONFIG_SETTINGS)
#include <zephyr/settings/settings.h>

struct paseo_leds_rgb_ug_state_mirror {
    struct {
        uint16_t h;
        uint8_t s;
        uint8_t b;
    } color;
    uint8_t animation_speed;
    uint8_t current_effect;
    uint16_t animation_step;
    bool on;
    bool status_active;
    uint16_t status_animation_step;
};

struct paseo_leds_rgb_ug_read {
    struct paseo_leds_rgb_ug_state_mirror state;
    bool found;
};

static int paseo_leds_rgb_ug_state_cb(const char *key, size_t len, settings_read_cb read_cb,
                                      void *cb_arg, void *param) {
    ARG_UNUSED(key);
    struct paseo_leds_rgb_ug_read *out = param;

    if (len != sizeof(out->state)) {
        return -EINVAL;
    }
    if (read_cb(cb_arg, &out->state, sizeof(out->state)) < 0) {
        return -EIO;
    }
    out->found = true;
    return 0;
}
#endif /* IS_ENABLED(CONFIG_SETTINGS) */

/* ponytail: reads flash settings on every flush rather than caching +
 * invalidating on an rgb_ug keypress; fine at this module's event-driven
 * (not animated) flush rate, revisit if flushes start happening at
 * animation speed. */
static void paseo_leds_get_brightness_saturation(uint8_t *brt, uint8_t *sat) {
    *brt = 40;
    *sat = 100;

#if IS_ENABLED(CONFIG_SETTINGS)
    struct paseo_leds_rgb_ug_read ug_read = {0};

    settings_load_subtree_direct("rgb/underglow/state", paseo_leds_rgb_ug_state_cb, &ug_read);

    if (ug_read.found && ug_read.state.color.b <= 100 && ug_read.state.color.s <= 100) {
        *brt = ug_read.state.color.b;
        *sat = ug_read.state.color.s;
    }
#endif
}

/* Hardware power ceiling: CONFIG_ZMK_RGB_UNDERGLOW_BRT_MAX is capped at 40
 * in go60_lh_defconfig ("DO NOT CHANGE ... TO ABOVE 40 ... can draw more
 * than 500mA ... WARRANTY IS VOID"). We don't share code with rgb_ug's own
 * scaling, so re-apply the same 40%-of-255 ceiling here, unconditionally,
 * on every channel we ever write. */
#define PASEO_LEDS_CHANNEL_MAX ((255 * 40) / 100)

static struct led_rgb paseo_leds_scale_pixel(struct led_rgb px, uint8_t brt, uint8_t sat) {
    int r = ((int)px.r * brt) / 100;
    int g = ((int)px.g * brt) / 100;
    int b = ((int)px.b * brt) / 100;

    if (sat < 100) {
        int luma = (r * 299 + g * 587 + b * 114) / 1000;
        r = luma + ((r - luma) * sat) / 100;
        g = luma + ((g - luma) * sat) / 100;
        b = luma + ((b - luma) * sat) / 100;
    }

    return (struct led_rgb){
        .r = (uint8_t)CLAMP(r, 0, PASEO_LEDS_CHANNEL_MAX),
        .g = (uint8_t)CLAMP(g, 0, PASEO_LEDS_CHANNEL_MAX),
        .b = (uint8_t)CLAMP(b, 0, PASEO_LEDS_CHANNEL_MAX),
    };
}

/* Paseo-mode toggle flag (see paseo_leds_shared.h / paseo_leds_toggle.c).
 * Frame/behavior-invocation handlers always update strip_pixels; only this
 * flush function is gated on it. */
static bool paseo_leds_enabled = true;
#if !IS_ENABLED(CONFIG_ZMK_SPLIT_ROLE_CENTRAL)
static bool paseo_leds_mic_active;
BUILD_ASSERT(DT_PROP(PASEO_LEDS_CFG, right_mic_index) < STRIP_NUM_PIXELS,
             "right-mic-index is outside the LED strip");
#endif

static void paseo_leds_flush(void) {
    if (!paseo_leds_enabled) {
        return;
    }

    if (!device_is_ready(led_strip)) {
        LOG_WRN("led_strip device not ready, dropping frame");
        return;
    }

    uint8_t brt, sat;
    paseo_leds_get_brightness_saturation(&brt, &sat);

    struct led_rgb scaled[STRIP_NUM_PIXELS];
    for (int i = 0; i < STRIP_NUM_PIXELS; i++) {
        scaled[i] = paseo_leds_scale_pixel(strip_pixels[i], brt, sat);
    }

#if !IS_ENABLED(CONFIG_ZMK_SPLIT_ROLE_CENTRAL)
    if (paseo_leds_mic_active) {
        scaled[DT_PROP(PASEO_LEDS_CFG, right_mic_index)] =
            paseo_leds_scale_pixel((struct led_rgb){.r = 255}, brt, sat);
    }
#endif

    int err = led_strip_update_rgb(led_strip, scaled, STRIP_NUM_PIXELS);
    if (err < 0) {
        LOG_ERR("led_strip_update_rgb failed (%d)", err);
    }
}

void paseo_leds_mic_set(bool active) {
#if IS_ENABLED(CONFIG_ZMK_SPLIT_ROLE_CENTRAL)
    ARG_UNUSED(active);
#else
    paseo_leds_mic_active = active;
    paseo_leds_power_on();
    paseo_leds_flush();
#endif
}

/* Flips paseo_leds_enabled -- see paseo_leds_shared.h for the contract. */
void paseo_leds_toggle(void) {
    paseo_leds_enabled = !paseo_leds_enabled;

    if (!paseo_leds_enabled) {
        struct led_rgb off[STRIP_NUM_PIXELS];
        memset(off, 0, sizeof(off));
        if (device_is_ready(led_strip)) {
            int err = led_strip_update_rgb(led_strip, off, STRIP_NUM_PIXELS);
            if (err < 0) {
                LOG_ERR("led_strip_update_rgb (blank) failed (%d)", err);
            }
        }
    } else {
        paseo_leds_flush();
    }
}

#if IS_ENABLED(CONFIG_ZMK_SPLIT_ROLE_CENTRAL)

/*
 * ---- Central only: strip indices for the LEFT half. ----
 *
 * Derivation (see go60_lh.dts's `underglow_indicators` node comment, which
 * documents the physical-key -> strip-index map for the left half):
 *
 *   26 22 17 12  7  3
 *   27 23 18 13  8  4
 *   28 24 19 14  9  5
 *   29 25 20 15 10  6
 *         21 16 11   0 1 2
 *
 * Row 0 (top row) is the number row; go60.keymap's "windows"/"mac" layers
 * put EQUAL/N1/N2/N3/N4/N5 in columns 0-5 of that row. Column bases read:
 * col0=26 col1=22 col2=17 col3=12 col4=7 col5=3. So:
 *   N1(col1)=22 N2(col2)=17 N3(col3)=12 N4(col4)=7 N5(col5)=3
 * This is read directly off the documented comment, not a guess.
 *
 * F/D (left-extra-indices, logical 16/17): "mo A S D F G" is row 2, columns
 * 0-5 -> mo=col0 A=col1 S=col2 D=col3 F=col4 G=col5. Row 2 col4 = 9.
 */
static const uint8_t left_number_row_indices[] = DT_PROP(PASEO_LEDS_CFG, left_number_row_indices);
static const uint8_t left_extra_indices[] = DT_PROP(PASEO_LEDS_CFG, left_extra_indices);

BUILD_ASSERT(ARRAY_SIZE(left_number_row_indices) == 5, "expected 5 left number-row indices");
BUILD_ASSERT(ARRAY_SIZE(left_extra_indices) == 2, "expected 2 left extra indices (F D)");

struct paseo_leds_pixel {
    uint8_t index;
    uint8_t r, g, b;
};

struct paseo_leds_frame {
    uint8_t count;
    struct paseo_leds_pixel pixels[PASEO_LEDS_MAX_PIXELS];
};

/* ponytail: six queued writes cover one complete host repaint (five normal
 * chunks or four usage-column chunks). Add acknowledgements only if a faster
 * sender makes this queue overflow. */
K_MSGQ_DEFINE(paseo_leds_frames, sizeof(struct paseo_leds_frame), 6, 4);

static uint32_t paseo_leds_pack(const struct paseo_leds_pixel *px) {
    return ((uint32_t)px->index << 24) | ((uint32_t)px->r << 16) | ((uint32_t)px->g << 8) | px->b;
}

static void paseo_leds_apply_one_frame(const struct paseo_leds_frame *frame) {

    bool left_dirty = false;
    uint32_t right_packed[PASEO_LEDS_MAX_PIXELS];
    size_t right_count = 0;

    for (int i = 0; i < frame->count; i++) {
        const struct paseo_leds_pixel *px = &frame->pixels[i];

        if (px->index == PASEO_LEDS_FILL_ALL) {
            /* Fill+flush this half immediately (own 30 pixels), then
             * forward exactly one packed fill-all entry so the peripheral
             * does the same to its half. Later entries in this same frame
             * still layer on top via the usual left_dirty/right_packed
             * paths below. */
            paseo_leds_fill_local(px->r, px->g, px->b);
            paseo_leds_flush();
            right_packed[right_count++] = paseo_leds_pack(px);
        } else if (px->index >= PASEO_LEDS_USAGE_COLUMN_FIRST && px->index < PASEO_LEDS_USAGE_RIGHT_FIRST) {
            paseo_leds_set_local_column(px->index - PASEO_LEDS_USAGE_COLUMN_FIRST, px->r, px->g, px->b);
            left_dirty = true;
        } else if (px->index < 5) {
            paseo_leds_set_local_pixel(left_number_row_indices[px->index], px->r, px->g, px->b);
            left_dirty = true;
        } else if (px->index == 16 || px->index == 17) {
            paseo_leds_set_local_pixel(left_extra_indices[px->index - 16], px->r, px->g, px->b);
            left_dirty = true;
        } else if (px->index < PASEO_LEDS_NUM_LOGICAL) {
            /* Right-owned (5-9 number row, 10-15 extra): pack for the
             * peripheral, 2 pixels per behavior invocation. */
            right_packed[right_count++] = paseo_leds_pack(px);
        } else {
            LOG_WRN("Ignoring out-of-range logical index %d", px->index);
        }
    }

    if (left_dirty) {
        paseo_leds_flush();
    }

    for (size_t i = 0; i < right_count; i += 2) {
        struct zmk_behavior_binding binding = {
            .behavior_dev = DEVICE_DT_NAME(DT_NODELABEL(pled)),
            .param1 = right_packed[i],
            .param2 = (i + 1 < right_count) ? right_packed[i + 1] : PASEO_LEDS_NO_PIXEL,
        };
        struct zmk_behavior_binding_event event = {
            .position = 0,
            .timestamp = k_uptime_get(),
        };

        int err = zmk_behavior_invoke_binding(&binding, event, true);
        if (err) {
            LOG_WRN("Failed to push right-half pixels to peripheral (%d)", err);
        }
    }
}

static void paseo_leds_apply_frame(struct k_work *work) {
    ARG_UNUSED(work);

    struct paseo_leds_frame frame;
    paseo_leds_power_on();
    while (k_msgq_get(&paseo_leds_frames, &frame, K_NO_WAIT) == 0) {
        paseo_leds_apply_one_frame(&frame);
    }
}

static K_WORK_DEFINE(apply_frame_work, paseo_leds_apply_frame);

/*
 * Frame protocol (host -> keyboard GATT write):
 *   byte 0 = pixel count n (1-18)
 *   then n * 4 bytes: [logical_index, r, g, b]
 * Logical index 0-9 = number keys 1,2,3,4,5,6,7,8,9,0. Logical index
 * 10-15 = Y,U,J,K,L,SEMI. Logical indices 16-17 = F,D. Logical indices 18-29 set
 * whole keyboard columns (=, 1-5, 6-0, -). Index byte 0xFE is the
 * fill-all sentinel (see module comment), not a logical index -- it may
 * appear at most meaningfully once per frame but nothing stops a second
 * one; both would just be applied and flushed in order like any other
 * entry.
 */
static ssize_t paseo_leds_write(struct bt_conn *conn, const struct bt_gatt_attr *attr,
                                const void *buf, uint16_t len, uint16_t offset, uint8_t flags) {
    ARG_UNUSED(conn);
    ARG_UNUSED(attr);
    ARG_UNUSED(flags);

    if (offset != 0) {
        return BT_GATT_ERR(BT_ATT_ERR_INVALID_OFFSET);
    }

    if (len < 1) {
        return BT_GATT_ERR(BT_ATT_ERR_INVALID_ATTRIBUTE_LEN);
    }

    const uint8_t *data = buf;
    uint8_t n = data[0];

    if (n < 1 || n > PASEO_LEDS_MAX_PIXELS || len < (uint16_t)(1 + n * 4)) {
        return BT_GATT_ERR(BT_ATT_ERR_INVALID_ATTRIBUTE_LEN);
    }

    /* Copy only -- never do BLE/strip work from this callback. */
    struct paseo_leds_frame frame = {.count = n};
    for (int i = 0; i < n; i++) {
        const uint8_t *entry = &data[1 + i * 4];
        frame.pixels[i] = (struct paseo_leds_pixel){
            .index = entry[0],
            .r = entry[1],
            .g = entry[2],
            .b = entry[3],
        };
    }

    if (k_msgq_put(&paseo_leds_frames, &frame, K_NO_WAIT) != 0) {
        LOG_WRN("LED frame queue full, dropping frame");
        return BT_GATT_ERR(BT_ATT_ERR_INSUFFICIENT_RESOURCES);
    }
    k_work_submit(&apply_frame_work);

    return len;
}

BT_GATT_SERVICE_DEFINE(paseo_leds_svc,
                       BT_GATT_PRIMARY_SERVICE(BT_UUID_DECLARE_128(PASEO_LEDS_SVC_UUID)),
                       BT_GATT_CHARACTERISTIC(BT_UUID_DECLARE_128(PASEO_LEDS_CHRC_UUID),
                                              BT_GATT_CHRC_WRITE |
                                                  BT_GATT_CHRC_WRITE_WITHOUT_RESP,
                                              BT_GATT_PERM_WRITE_ENCRYPT, NULL, paseo_leds_write,
                                              NULL));

#else /* !CONFIG_ZMK_SPLIT_ROLE_CENTRAL -- peripheral (right half) */

/*
 * ---- Peripheral only: strip indices for the RIGHT half. ----
 *
 * go60_rh.dts ships no `underglow_indicators` node at all -- upstream only
 * documented/wired that for the left half -- so there is no equivalent
 * source to read this off. Best-supported guess: the two halves share PCB
 * routing conventions, so the right half's zig-zag is assumed to mirror
 * the left half's by column position (RH col1, nearest the center gap,
 * plays the same role as LH's innermost col5; RH col6, the outermost/pinky
 * column, plays the same role as LH's col0):
 *   N6(col1)->3 N7(col2)->7 N8(col3)->12 N9(col4)->17 N0(col5)->22
 * NOT verified on hardware -- kept as a devicetree property specifically
 * so it's a one-line fix in config/go60.keymap after on-hardware testing,
 * no firmware logic change needed.
 *
 * right-extra-indices (Y U J K L SEMI, logical 10-15) applies that same
 * "RH colN mirrors LH col(6-N)" rule one row up/down, against the LH grid
 * documented above paseo_leds_apply_frame():
 *   row1 (Y U I O P ESC, LH row1 = 27 23 18 13 8 4, cols0-5):
 *     Y=col1 -> LH col5 = 4     U=col2 -> LH col4 = 8
 *   row2 (H J K L ; ', LH row2 = 28 24 19 14 9 5, cols0-5):
 *     J=col2 -> LH col4 = 9     K=col3 -> LH col3 = 14
 *     L=col4 -> LH col2 = 19    SEMI=col5 -> LH col1 = 24
 * (H itself isn't in this table -- it's a keycode, not an LED position.)
 * Same not-verified-on-hardware caveat as right-number-row-indices.
 */
static const uint8_t right_number_row_indices[] =
    DT_PROP(PASEO_LEDS_CFG, right_number_row_indices);
static const uint8_t right_extra_indices[] = DT_PROP(PASEO_LEDS_CFG, right_extra_indices);

BUILD_ASSERT(ARRAY_SIZE(right_number_row_indices) == 5, "expected 5 right number-row indices");
BUILD_ASSERT(ARRAY_SIZE(right_extra_indices) == 6, "expected 6 right extra indices (Y U J K L SEMI)");

static void paseo_leds_apply_packed(uint32_t packed) {
    uint8_t index = (packed >> 24) & 0xFF;
    uint8_t r = (packed >> 16) & 0xFF;
    uint8_t g = (packed >> 8) & 0xFF;
    uint8_t b = packed & 0xFF;

    if (index == PASEO_LEDS_FILL_ALL) {
        paseo_leds_fill_local(r, g, b);
        paseo_leds_flush();
        return;
    }

    /* Right-owned is 5-9 (number row), 10-15 (Y U J K L SEMI), and usage
     * columns 24-29 (6-0, -). 16-17 (F/D) and columns 18-23 are left-owned. */
    bool right_owned = (index >= 5 && index < 16)
        || (index >= PASEO_LEDS_USAGE_RIGHT_FIRST && index < PASEO_LEDS_NUM_LOGICAL);
    if (!right_owned) {
        LOG_WRN("Ignoring out-of-range/left-owned logical index %d on peripheral", index);
        return;
    }

    if (index >= PASEO_LEDS_USAGE_RIGHT_FIRST) {
        /* The right half is mirrored: 6 is the leftmost right-side column,
         * so it uses the left layout's column 5; - uses column 0. */
        paseo_leds_set_local_column(
            PASEO_LEDS_USAGE_COLUMN_FIRST + PASEO_LEDS_USAGE_COLUMN_COUNT - 1 - index,
            r, g, b);
    } else if (index < 10) {
        uint8_t right_slot = index - 5;
        if (right_slot < ARRAY_SIZE(right_number_row_indices)) {
            paseo_leds_set_local_pixel(right_number_row_indices[right_slot], r, g, b);
        }
    } else {
        uint8_t extra_slot = index - 10;
        if (extra_slot < ARRAY_SIZE(right_extra_indices)) {
            paseo_leds_set_local_pixel(right_extra_indices[extra_slot], r, g, b);
        }
    }
}

#endif /* IS_ENABLED(CONFIG_ZMK_SPLIT_ROLE_CENTRAL) */

/* ---- Boot self-test: both halves, independently. ----
 *
 * Lights this half's five number-row LEDs dim green ~2s after boot, then
 * clears them 3s later. Serves two purposes: visible proof that THIS half
 * is running the paseo-leds firmware (no other build lights the number
 * row at boot), and a visual check of the strip-index tables -- if the
 * wrong keys light up, the indices arrays in go60.keymap need fixing. Goes
 * through the same paseo_leds_flush() as everything else, so it's also a
 * boot-time smoke test of the brightness/saturation scaling and the
 * Paseo-mode toggle gate. */
#if IS_ENABLED(CONFIG_ZMK_SPLIT_ROLE_CENTRAL)
#define PASEO_OWN_INDICES left_number_row_indices
#else
#define PASEO_OWN_INDICES right_number_row_indices
#endif

static void paseo_leds_boot_clear(struct k_work *work) {
    ARG_UNUSED(work);
    for (size_t i = 0; i < ARRAY_SIZE(PASEO_OWN_INDICES); i++) {
        paseo_leds_set_local_pixel(PASEO_OWN_INDICES[i], 0, 0, 0);
    }
    paseo_leds_flush();
}
static K_WORK_DELAYABLE_DEFINE(boot_clear_work, paseo_leds_boot_clear);

static void paseo_leds_boot_test(struct k_work *work) {
    ARG_UNUSED(work);
    paseo_leds_power_on();
    for (size_t i = 0; i < ARRAY_SIZE(PASEO_OWN_INDICES); i++) {
        paseo_leds_set_local_pixel(PASEO_OWN_INDICES[i], 0, 60, 0);
    }
    paseo_leds_flush();
    k_work_schedule(&boot_clear_work, K_SECONDS(3));
}
static K_WORK_DELAYABLE_DEFINE(boot_test_work, paseo_leds_boot_test);

static int paseo_leds_init(void) {
    k_work_schedule(&boot_test_work, K_SECONDS(2));
    return 0;
}
SYS_INIT(paseo_leds_init, APPLICATION, CONFIG_APPLICATION_INIT_PRIORITY);

/* ---- Behavior device: exists on BOTH halves (shared keymap node). ---- */
#if DT_HAS_COMPAT_STATUS_OKAY(DT_DRV_COMPAT)

static int paseo_leds_binding_pressed(struct zmk_behavior_binding *binding,
                                      struct zmk_behavior_binding_event event) {
    ARG_UNUSED(event);

#if IS_ENABLED(CONFIG_ZMK_SPLIT_ROLE_CENTRAL)
    /* Global-locality behaviors also run locally on the central. The
     * central applies its own (left-owned) pixels directly from the GATT
     * write handler and never invokes this behavior for itself, so
     * there's nothing to do here. */
    ARG_UNUSED(binding);
    return ZMK_BEHAVIOR_OPAQUE;
#else
    paseo_leds_power_on();
    paseo_leds_apply_packed(binding->param1);
    if (binding->param2 != PASEO_LEDS_NO_PIXEL) {
        paseo_leds_apply_packed(binding->param2);
    }
    paseo_leds_flush();
    return ZMK_BEHAVIOR_OPAQUE;
#endif
}

static int paseo_leds_binding_released(struct zmk_behavior_binding *binding,
                                       struct zmk_behavior_binding_event event) {
    ARG_UNUSED(binding);
    ARG_UNUSED(event);
    return ZMK_BEHAVIOR_OPAQUE;
}

static const struct behavior_driver_api paseo_leds_driver_api = {
    .binding_pressed = paseo_leds_binding_pressed,
    .binding_released = paseo_leds_binding_released,
    .locality = BEHAVIOR_LOCALITY_GLOBAL,
#if IS_ENABLED(CONFIG_ZMK_BEHAVIOR_METADATA)
    .get_parameter_metadata = zmk_behavior_get_empty_param_metadata,
#endif
};

BEHAVIOR_DT_INST_DEFINE(0, NULL, NULL, NULL, NULL, POST_KERNEL, CONFIG_KERNEL_INIT_PRIORITY_DEFAULT,
                        &paseo_leds_driver_api);

#endif /* DT_HAS_COMPAT_STATUS_OKAY(DT_DRV_COMPAT) */

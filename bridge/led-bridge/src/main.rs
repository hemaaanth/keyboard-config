//! Host-side BLE bridge for the Go60 keyboard LED project.
//!
//! Connects to a ZMK keyboard's custom GATT LED service and writes frames
//! that light up keys 1-9,0 plus Y U J K L ; F (logical indices 10-16). See
//! `bridge/led-bridge/README.md` for usage.
//!
//! `run` is the live-sync daemon: it connects to the Paseo daemon's
//! WebSocket API (see `../../docs/go60-paseo-layer.md` for the ZMK side)
//! and mirrors agent/workspace state onto the keyboard's LEDs, plus a
//! foreground-window action indicator, a permission glow, a usage-bar
//! hotkey mode, and a 90%-usage alarm. `scan`/`frame`/`demo`/`debug` are
//! the lower-level BLE spike commands `run` is built on top of.
//!
//! Windows-only at runtime: this talks to the WinRT Bluetooth LE APIs
//! directly (via the `windows` crate) against already-**paired** devices,
//! and to Win32 (global hotkey, foreground window) for the desk-side
//! integration. A device that is bonded and actively connected as a BLE
//! HID keyboard does not emit advertisements, so any API that discovers
//! peripherals via scanning (e.g. btleplug's Windows backend) can never
//! see it. Enumerating paired devices sidesteps that: no advertisement
//! needed.

use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const DEFAULT_NAME_FILTER: &str = "Go60";

// Fixed protocol UUIDs (canonical 36-char form), shared with the firmware.
const SERVICE_UUID_STR: &str = "70617365-6f4c-4544-b0a0-000000000001";
const CHAR_UUID_STR: &str = "70617365-6f4c-4544-b0a0-000000000002";

// Default Paseo daemon WebSocket endpoint, per Paseo's protocol source
// (packages/protocol/src/messages.ts): listen address defaults to
// 127.0.0.1:6767 (override with PASEO_LISTEN), path `/ws`. Overridable via
// `--ws-url` / `PASEO_WS_URL` in case a given deployment differs.
const DEFAULT_WS_URL: &str = "ws://127.0.0.1:6767/ws";

// Logical LED indices, per the firmware contract.
const IDX_Y: u8 = 10;
const IDX_U: u8 = 11;
const IDX_J: u8 = 12;
const IDX_K: u8 = 13;
const IDX_L: u8 = 14;
const IDX_SEMI: u8 = 15;
const IDX_F: u8 = 16;
const FRAME_LEN: usize = 17;
/// Fill-all op: a frame entry with this index byte sets all 30 LEDs on both
/// halves to that r,g,b. Later entries in the same frame layer on top.
const FILL_ALL_INDEX: u8 = 0xFE;

// Status colors (slot LEDs 0-9).
const COLOR_DONE: (u8, u8, u8) = (0x0A, 0x0A, 0x0A);
const COLOR_NEEDS_INPUT: (u8, u8, u8) = (0xFF, 0x5F, 0x00);
const COLOR_ATTENTION: (u8, u8, u8) = (0x00, 0xC8, 0x00);
const COLOR_FAILED: (u8, u8, u8) = (0xFF, 0x00, 0x00);
const COLOR_RUNNING: (u8, u8, u8) = (0x00, 0x33, 0xFF);
const COLOR_OFF: (u8, u8, u8) = (0x00, 0x00, 0x00);

// Permission glow (logical 10-11).
const COLOR_PERMISSION_Y: (u8, u8, u8) = (0xFF, 0x5F, 0x00);
const COLOR_PERMISSION_U: (u8, u8, u8) = (0xFF, 0x00, 0x00);

// Foreground-window action indicators (logical 12-16).
const COLOR_COMMIT: (u8, u8, u8) = (0x00, 0xFF, 0x60);
const COLOR_PUSH: (u8, u8, u8) = (0x00, 0x80, 0xFF);
const COLOR_PR: (u8, u8, u8) = (0xA0, 0x20, 0xF0);
const COLOR_MERGE: (u8, u8, u8) = (0xFF, 0x20, 0x00);
// F is inverted vs. J/K/L/;: a "press to focus" call-to-action while Paseo
// is NOT foreground, dim once it is (no action needed).
const COLOR_FOCUS_CTA: (u8, u8, u8) = (0x40, 0x40, 0x40);
const COLOR_FOCUS_DIM: (u8, u8, u8) = (0x0A, 0x0A, 0x0A);

fn main() -> Result<()> {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let (name_filter, args) = extract_name_flag(&raw_args)?;
    let (ws_url_flag, args) = extract_flag(&args, "--ws-url")?;

    match args.first().map(String::as_str) {
        Some("scan") => run_scan(),
        Some("frame") => {
            let spec = args
                .get(1)
                .ok_or_else(|| anyhow!("usage: frame <key=color,...>  (e.g. frame 1=blue,2=orange)"))?;
            run_frame(&name_filter, spec)
        }
        Some("demo") => run_demo(&name_filter),
        Some("debug") => run_debug(&name_filter),
        Some("run") => {
            let ws_url = ws_url_flag
                .or_else(|| std::env::var("PASEO_WS_URL").ok())
                .unwrap_or_else(|| DEFAULT_WS_URL.to_string());
            run_run(&name_filter, &ws_url)
        }
        Some(other) => {
            print_usage();
            bail!("unknown subcommand '{other}'");
        }
        None => {
            print_usage();
            bail!("no subcommand given");
        }
    }
}

fn print_usage() {
    eprintln!("paseo-led-bridge - Go60 LED BLE bridge");
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("  paseo-led-bridge scan [--name FILTER]");
    eprintln!("  paseo-led-bridge frame <key=color,...> [--name FILTER]");
    eprintln!("  paseo-led-bridge demo [--name FILTER]");
    eprintln!("  paseo-led-bridge run [--name FILTER] [--ws-url URL]");
    eprintln!();
    eprintln!("  --name FILTER   substring match on the paired BLE device name (default: {DEFAULT_NAME_FILTER})");
    eprintln!("  --ws-url URL    Paseo daemon WebSocket URL (default: {DEFAULT_WS_URL}, or $PASEO_WS_URL)");
    eprintln!();
    eprintln!("COLORS: blue orange red green off, or hex RRGGBB");
    eprintln!("KEYS:   1 2 3 4 5 6 7 8 9 0  (maps to logical index 0-9)");
    eprintln!();
    eprintln!("EXAMPLE: paseo-led-bridge frame 1=blue,2=orange,6=green,0=FF00FF");
    eprintln!();
    eprintln!("`run` is the live-sync daemon: mirrors Paseo agent/workspace state onto");
    eprintln!("the keyboard LEDs. See README.md for the full key/color map.");
}

/// Pulls `flag VALUE` out of the arg list, returning the value (if present)
/// and the remaining positional args, in order.
fn extract_flag(args: &[String], flag: &str) -> Result<(Option<String>, Vec<String>)> {
    let mut value = None;
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == flag {
            let val = args
                .get(i + 1)
                .ok_or_else(|| anyhow!("{flag} requires a value"))?;
            value = Some(val.clone());
            i += 2;
        } else {
            rest.push(args[i].clone());
            i += 1;
        }
    }
    Ok((value, rest))
}

/// Pulls `--name VALUE` out of the arg list, returning the filter (default
/// "Go60") and the remaining positional args.
fn extract_name_flag(args: &[String]) -> Result<(String, Vec<String>)> {
    let (value, rest) = extract_flag(args, "--name")?;
    Ok((value.unwrap_or_else(|| DEFAULT_NAME_FILTER.to_string()), rest))
}

// ---------------------------------------------------------------------
// Frame spec parsing (pure functions -- platform independent, always
// compiled and tested, including on the Linux dev host).
// ---------------------------------------------------------------------

/// Maps a number-row key ('1'..'9','0') to the device's logical LED index
/// (0-9), per the firmware protocol: 1,2,...,9,0 -> 0,1,...,8,9.
fn key_to_logical_index(key: &str) -> Result<u8> {
    match key {
        "1" => Ok(0),
        "2" => Ok(1),
        "3" => Ok(2),
        "4" => Ok(3),
        "5" => Ok(4),
        "6" => Ok(5),
        "7" => Ok(6),
        "8" => Ok(7),
        "9" => Ok(8),
        "0" => Ok(9),
        _ => bail!("invalid key '{key}': expected one of 1-9,0"),
    }
}

fn parse_color(s: &str) -> Result<(u8, u8, u8)> {
    let rgb: u32 = match s.to_ascii_lowercase().as_str() {
        "blue" => 0x0033FF,
        "orange" => 0xFF5F00,
        "red" => 0xFF0000,
        "green" => 0x00C800,
        "off" => 0x000000,
        _ => {
            let hex = s.strip_prefix('#').unwrap_or(s);
            if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                bail!("invalid color '{s}': expected blue|orange|red|green|off or hex RRGGBB");
            }
            u32::from_str_radix(hex, 16)
                .map_err(|_| anyhow!("invalid hex color '{s}'"))?
        }
    };
    Ok((((rgb >> 16) & 0xFF) as u8, ((rgb >> 8) & 0xFF) as u8, (rgb & 0xFF) as u8))
}

/// Parses `key=color,key=color,...` into (logical_index, r, g, b) pixels.
fn parse_spec(spec: &str) -> Result<Vec<(u8, u8, u8, u8)>> {
    let mut pixels = Vec::new();
    for entry in spec.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (key, color) = entry
            .split_once('=')
            .ok_or_else(|| anyhow!("invalid spec entry '{entry}': expected key=color"))?;
        let idx = key_to_logical_index(key.trim())?;
        let (r, g, b) = parse_color(color.trim())?;
        pixels.push((idx, r, g, b));
    }
    if pixels.is_empty() {
        bail!("empty frame spec");
    }
    if pixels.len() > 10 {
        bail!("too many pixels in spec: {} (max 10)", pixels.len());
    }
    Ok(pixels)
}

fn build_frame(pixels: &[(u8, u8, u8, u8)]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(1 + pixels.len() * 4);
    frame.push(pixels.len() as u8);
    for &(idx, r, g, b) in pixels {
        frame.extend_from_slice(&[idx, r, g, b]);
    }
    frame
}

/// Max pixels per BLE write: 1 (count byte) + 4*4 = 17 bytes, safe under
/// the ATT MTU-3 write-without-response payload cap even at the minimum
/// ATT MTU of 23 (20-byte payload) -- a 17-pixel frame (69 bytes) does not
/// fit and is silently rejected by Windows (HRESULT 0x80070057).
const MAX_PIXELS_PER_CHUNK: usize = 4;

/// Splits a pixel list into BLE-safe sub-frames (each a valid, self
/// contained frame: count byte + entries) of at most
/// `MAX_PIXELS_PER_CHUNK` pixels. Order is preserved within and across
/// chunks, so a caller that puts a fill-all entry first keeps it in the
/// first chunk, applied before any override pixels that follow it --  the
/// firmware applies entries additively, in arrival order.
fn chunk_pixels(pixels: &[(u8, u8, u8, u8)]) -> Vec<Vec<u8>> {
    pixels.chunks(MAX_PIXELS_PER_CHUNK).map(build_frame).collect()
}

/// A simple hue-wheel rainbow, one color per logical LED index (0-9).
fn rainbow_colors() -> [(u8, u8, u8); 10] {
    let mut colors = [(0u8, 0u8, 0u8); 10];
    for (i, slot) in colors.iter_mut().enumerate() {
        let hue = i as f32 / 10.0 * 360.0;
        *slot = hsv_to_rgb(hue, 1.0, 1.0);
    }
    colors
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r1, g1, b1) = match h as u32 {
        0..=59 => (c, x, 0.0),
        60..=119 => (x, c, 0.0),
        120..=179 => (0.0, c, x),
        180..=239 => (0.0, x, c),
        240..=299 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r1 + m) * 255.0).round() as u8,
        ((g1 + m) * 255.0).round() as u8,
        ((b1 + m) * 255.0).round() as u8,
    )
}

// ---------------------------------------------------------------------
// `run` daemon: pure logic. Platform independent, always compiled and
// tested -- everything the daemon *decides* lives here; everything it
// *does* (BLE writes, WS I/O, Win32 calls) lives in `mod winrt` below and
// is just a thin caller of these functions.
// ---------------------------------------------------------------------

/// Slot status, per the new status color semantics (slot LEDs 0-9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Status {
    Done,
    NeedsInput,
    Attention,
    Failed,
    Running,
}

impl Status {
    fn from_wire(s: &str) -> Option<Status> {
        match s {
            "done" => Some(Status::Done),
            "needs_input" => Some(Status::NeedsInput),
            "attention" => Some(Status::Attention),
            "failed" => Some(Status::Failed),
            "running" => Some(Status::Running),
            _ => None,
        }
    }

    fn color(self) -> (u8, u8, u8) {
        match self {
            Status::Done => COLOR_DONE,
            Status::NeedsInput => COLOR_NEEDS_INPUT,
            Status::Attention => COLOR_ATTENTION,
            Status::Failed => COLOR_FAILED,
            Status::Running => COLOR_RUNNING,
        }
    }
}

/// A Paseo workspace, as reported by `fetch_workspaces_response` /
/// `workspace_update`. Tolerant of unknown/missing fields (server-side
/// additions shouldn't break parsing) via `#[serde(default)]`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
struct WorkspaceDescriptor {
    id: String,
    #[allow(dead_code)] // parsed for completeness, not rendered
    name: String,
    #[serde(rename = "pinnedAt")]
    pinned_at: Option<String>,
    #[serde(rename = "archivingAt")]
    archiving_at: Option<String>,
    status: String,
}

/// Slots 1..10 = workspaces where `pinnedAt` is non-null AND `archivingAt`
/// is null, sorted by `pinnedAt` DESCENDING (lexicographic ISO-8601
/// compare is correct), capped at 10. Newest pin = slot 1 = logical LED 0.
fn derive_slots(workspaces: &HashMap<String, WorkspaceDescriptor>) -> Vec<String> {
    let mut pinned: Vec<&WorkspaceDescriptor> =
        workspaces.values().filter(|w| w.pinned_at.is_some() && w.archiving_at.is_none()).collect();
    pinned.sort_by(|a, b| b.pinned_at.cmp(&a.pinned_at));
    pinned.into_iter().take(10).map(|w| w.id.clone()).collect()
}

/// The live workspace store + blink bookkeeping: everything needed to
/// derive the slot frame, kept separate from the daemon's IO/threading
/// state (usage cache, alarm, hotkey mode, ...) so it's fully
/// unit-testable without any WS/BLE/Win32 plumbing.
#[derive(Debug, Default)]
struct WorkspaceStore {
    workspaces: HashMap<String, WorkspaceDescriptor>,
    /// Keyed by workspace id, not slot position: pins can reorder which
    /// workspace occupies which slot, and blink state belongs to the
    /// workspace, not the slot it happens to be sitting in.
    blink_started: HashMap<String, Instant>,
}

impl WorkspaceStore {
    fn new() -> Self {
        Self::default()
    }

    /// Full state rebuild from a `fetch_workspaces_response` snapshot
    /// (sent on first connect and again after every reconnect, per the
    /// protocol). Workspaces present in the snapshot start with no blink
    /// history, even if already "attention" -- see `just_entered_attention`.
    fn apply_snapshot(&mut self, entries: Vec<WorkspaceDescriptor>) {
        self.workspaces.clear();
        self.blink_started.clear();
        for w in entries {
            self.workspaces.insert(w.id.clone(), w);
        }
    }

    fn apply_upsert(&mut self, w: WorkspaceDescriptor, now: Instant) {
        let prev_status = self.workspaces.get(&w.id).and_then(|old| Status::from_wire(&old.status));
        let new_status = Status::from_wire(&w.status);
        if just_entered_attention(prev_status, new_status) {
            self.blink_started.insert(w.id.clone(), now);
        } else if new_status != Some(Status::Attention) {
            self.blink_started.remove(&w.id);
        }
        self.workspaces.insert(w.id.clone(), w);
    }

    fn apply_remove(&mut self, id: &str) {
        self.workspaces.remove(id);
        self.blink_started.remove(id);
    }

    fn slot_ids(&self) -> Vec<String> {
        derive_slots(&self.workspaces)
    }
}

/// The full 17-pixel logical LED state: 0-9 slots, 10-11 permission glow,
/// 12-16 foreground action indicators.
type Frame17 = [(u8, u8, u8); FRAME_LEN];

fn off_frame() -> Frame17 {
    [COLOR_OFF; FRAME_LEN]
}

// --- Blink-then-solid on just-finished (feature 2) --------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlinkPhase {
    On,
    Off,
    /// The blink schedule has run its course; render solid.
    Done,
}

/// A generic on/off square wave: `cycles` full on+off periods of `half`
/// each, starting at `started_at`. Shared by the slot blink (400ms/3) and
/// the usage alarm flash (500ms/3) below.
fn square_wave_phase(started_at: Instant, now: Instant, half: Duration, cycles: u32) -> BlinkPhase {
    let elapsed = now.saturating_duration_since(started_at);
    let total = half * 2 * cycles;
    if elapsed >= total {
        return BlinkPhase::Done;
    }
    let half_ms = half.as_millis().max(1);
    let half_idx = elapsed.as_millis() / half_ms;
    if half_idx % 2 == 0 {
        BlinkPhase::On
    } else {
        BlinkPhase::Off
    }
}

const BLINK_HALF: Duration = Duration::from_millis(400);
const BLINK_CYCLES: u32 = 3;

/// Blink schedule for a slot that just transitioned into "attention":
/// given the transition time and now, is it on, off, or done blinking
/// (render solid)?
fn blink_phase(transitioned_at: Instant, now: Instant) -> BlinkPhase {
    square_wave_phase(transitioned_at, now, BLINK_HALF, BLINK_CYCLES)
}

/// True iff `new` is a transition *into* attention from a different,
/// previously-known status. A workspace that simply *appears* already at
/// "attention" (no prior status on record -- e.g. in a fresh snapshot, or
/// newly upserted) is not a transition and must not blink.
fn just_entered_attention(prev: Option<Status>, new: Option<Status>) -> bool {
    matches!(prev, Some(p) if p != Status::Attention) && new == Some(Status::Attention)
}

const ALARM_HALF: Duration = Duration::from_millis(500);
const ALARM_CYCLES: u32 = 3;

fn alarm_phase(started_at: Instant, now: Instant) -> BlinkPhase {
    square_wave_phase(started_at, now, ALARM_HALF, ALARM_CYCLES)
}

// --- Frame composition --------------------------------------------------

/// Renders the 10 slot LEDs, applying the blink-then-solid override for any
/// slot currently mid-blink.
fn slot_colors(store: &WorkspaceStore, now: Instant) -> [(u8, u8, u8); 10] {
    let mut out = [COLOR_OFF; 10];
    for (i, id) in store.slot_ids().iter().enumerate() {
        let Some(w) = store.workspaces.get(id) else { continue };
        out[i] = match Status::from_wire(&w.status) {
            None => COLOR_OFF,
            Some(Status::Attention) => match store.blink_started.get(id) {
                Some(t) => match blink_phase(*t, now) {
                    BlinkPhase::On | BlinkPhase::Done => COLOR_ATTENTION,
                    BlinkPhase::Off => COLOR_OFF,
                },
                None => COLOR_ATTENTION,
            },
            Some(s) => s.color(),
        };
    }
    out
}

/// Permission glow (feature 4): Y and U both follow the same condition --
/// any *slotted* workspace (pinned, non-archiving, one of the current top
/// 10) currently needs input.
fn permission_glow(store: &WorkspaceStore) -> bool {
    store
        .slot_ids()
        .iter()
        .filter_map(|id| store.workspaces.get(id))
        .any(|w| Status::from_wire(&w.status) == Some(Status::NeedsInput))
}

/// Foreground-window action indicator colors (feature 3), J K L ; in that
/// order, all-or-nothing based on whether Paseo is foreground. F is
/// handled separately by `focus_indicator_color` -- its semantics are
/// inverted, not all-or-nothing.
fn action_indicator_colors(foreground_is_paseo: bool) -> [Option<(u8, u8, u8)>; 4] {
    if foreground_is_paseo {
        [Some(COLOR_COMMIT), Some(COLOR_PUSH), Some(COLOR_PR), Some(COLOR_MERGE)]
    } else {
        [None; 4]
    }
}

/// F (logical 16): inverted from J/K/L/; -- a bright "press to focus"
/// call-to-action while Paseo is NOT foreground, dim once it is (nothing
/// to do). Never off.
fn focus_indicator_color(foreground_is_paseo: bool) -> (u8, u8, u8) {
    if foreground_is_paseo { COLOR_FOCUS_DIM } else { COLOR_FOCUS_CTA }
}

/// Composes the full normal status frame (slots + permission glow +
/// action indicators).
fn compute_status_frame(store: &WorkspaceStore, foreground_is_paseo: bool, now: Instant) -> Frame17 {
    let mut f = off_frame();
    let colors = slot_colors(store, now);
    f[..10].copy_from_slice(&colors);
    let glow = if permission_glow(store) { COLOR_PERMISSION_Y } else { COLOR_OFF };
    let glow_u = if permission_glow(store) { COLOR_PERMISSION_U } else { COLOR_OFF };
    f[IDX_Y as usize] = glow;
    f[IDX_U as usize] = glow_u;
    let [j, k, l, semi] = action_indicator_colors(foreground_is_paseo);
    f[IDX_J as usize] = j.unwrap_or(COLOR_OFF);
    f[IDX_K as usize] = k.unwrap_or(COLOR_OFF);
    f[IDX_L as usize] = l.unwrap_or(COLOR_OFF);
    f[IDX_SEMI as usize] = semi.unwrap_or(COLOR_OFF);
    f[IDX_F as usize] = focus_indicator_color(foreground_is_paseo);
    f
}

// --- Usage-bar mode (feature 5) ------------------------------------------

#[derive(Debug, Clone, PartialEq)]
struct UsageWindow {
    id: String,
    label: String,
    used_pct: f64,
    #[allow(dead_code)] // parsed for completeness / future use, not rendered
    remaining_pct: f64,
    resets_at: String,
    #[allow(dead_code)]
    tone: String,
}

#[derive(Debug, Clone, PartialEq)]
struct UsageProvider {
    provider_id: String,
    display_name: String,
    #[allow(dead_code)]
    status: String,
    windows: Vec<UsageWindow>,
}

fn parse_usage_providers(payload: &serde_json::Value) -> Vec<UsageProvider> {
    let mut out = Vec::new();
    let Some(providers) = payload.get("providers").and_then(|v| v.as_array()) else {
        return out;
    };
    for p in providers {
        let mut windows = Vec::new();
        if let Some(ws) = p.get("windows").and_then(|v| v.as_array()) {
            for w in ws {
                windows.push(UsageWindow {
                    id: w.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    label: w.get("label").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    used_pct: w.get("usedPct").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    remaining_pct: w.get("remainingPct").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    resets_at: w.get("resetsAt").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    tone: w.get("tone").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                });
            }
        }
        out.push(UsageProvider {
            provider_id: p.get("providerId").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            display_name: p.get("displayName").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            status: p.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            windows,
        });
    }
    out
}

/// One tracked usage window, ready to render/log/alarm on.
#[derive(Debug, Clone, PartialEq)]
struct UsageEntry {
    /// Matched key from `USAGE_DISPLAY_ORDER`, e.g. "claude".
    provider_key: String,
    /// Matched window id from `USAGE_DISPLAY_ORDER`, e.g. "five_hour".
    window_key: String,
    display_name: String,
    used_pct: f64,
    resets_at: String,
}

/// Fixed display order: claude five_hour, claude weekly, codex session
/// (its primary window), codex weekly (only sometimes present). Absent
/// entries are skipped (not padded). Any claude window whose id starts
/// with "weekly_model" (extra per-model windows, e.g.
/// "weekly_model_fable") is appended after these, in provider order --
/// see `ordered_usage_entries`.
const USAGE_DISPLAY_ORDER: [(&str, &str); 4] =
    [("claude", "five_hour"), ("claude", "weekly"), ("codex", "session"), ("codex", "weekly")];

fn usage_entry(provider_key: &str, window_key: &str, p: &UsageProvider, w: &UsageWindow) -> UsageEntry {
    UsageEntry {
        provider_key: provider_key.to_string(),
        window_key: window_key.to_string(),
        display_name: if p.display_name.is_empty() { p.provider_id.clone() } else { p.display_name.clone() },
        used_pct: w.used_pct,
        resets_at: w.resets_at.clone(),
    }
}

fn ordered_usage_entries(providers: &[UsageProvider]) -> Vec<UsageEntry> {
    let mut out = Vec::new();
    for (prov_key, win_key) in USAGE_DISPLAY_ORDER {
        let Some(p) = providers.iter().find(|p| p.provider_id.to_ascii_lowercase().contains(prov_key)) else {
            continue;
        };
        let Some(w) = p.windows.iter().find(|w| w.id.eq_ignore_ascii_case(win_key)) else {
            continue;
        };
        out.push(usage_entry(prov_key, win_key, p, w));
    }
    if let Some(p) = providers.iter().find(|p| p.provider_id.to_ascii_lowercase().contains("claude")) {
        for w in &p.windows {
            if w.id.to_ascii_lowercase().starts_with("weekly_model") {
                out.push(usage_entry("claude", &w.id, p, w));
            }
        }
    }
    out
}

fn format_usage_log(e: &UsageEntry) -> String {
    format!("usage: {} {} {:.0}% (resets {})", e.provider_key, e.window_key, e.used_pct, e.resets_at)
}

/// Number of lit segments (0-10): key n lit iff usedPct >= n*10, minimum 1
/// segment if usedPct > 0.
fn segments_lit(used_pct: f64) -> u8 {
    let mut n = 0u8;
    for i in 1..=10u8 {
        if used_pct >= (i as f64) * 10.0 {
            n = i;
        }
    }
    if n == 0 && used_pct > 0.0 {
        n = 1;
    }
    n
}

/// Segment color: 1-5 green, 6-8 yellow, 9-10 red. `seg` is 1-indexed.
fn segment_color(seg: u8) -> (u8, u8, u8) {
    match seg {
        1..=5 => COLOR_ATTENTION, // green
        6..=8 => COLOR_NEEDS_INPUT, // yellow (0xFF5F00)
        _ => COLOR_FAILED, // red (9, 10)
    }
}

/// Renders the usage bar onto the number row (logical 0-9); action/
/// permission keys (10-16) are off.
fn usage_bar_frame(used_pct: f64) -> Frame17 {
    let mut f = off_frame();
    let lit = segments_lit(used_pct);
    for seg in 1..=10u8 {
        let idx = (seg - 1) as usize;
        f[idx] = if seg <= lit { segment_color(seg) } else { COLOR_OFF };
    }
    f
}

// --- 90% usage alarm (feature 6) -----------------------------------------

/// True iff usedPct crossed upward through 90% relative to `prev` (the
/// previous poll's value). `prev == None` means no baseline yet (first
/// poll): never a crossing, seed silently.
fn crossed_90_upward(prev: Option<f64>, current: f64) -> bool {
    matches!(prev, Some(p) if p < 90.0) && current >= 90.0
}

/// Returns the indices into `entries` that crossed 90% upward relative to
/// `baseline` (keyed by provider_key/window_key).
fn detect_crossings(baseline: &HashMap<(String, String), f64>, entries: &[UsageEntry]) -> Vec<usize> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            let key = (e.provider_key.clone(), e.window_key.clone());
            crossed_90_upward(baseline.get(&key).copied(), e.used_pct)
        })
        .map(|(i, _)| i)
        .collect()
}

// --- Frame diff / dedupe / 30s repush ------------------------------------

fn full_frame_pixels(current: &Frame17) -> Vec<(u8, u8, u8, u8)> {
    (0..FRAME_LEN as u8)
        .map(|i| {
            let (r, g, b) = current[i as usize];
            (i, r, g, b)
        })
        .collect()
}

fn frame_diff(prev: Option<&Frame17>, current: &Frame17) -> Vec<(u8, u8, u8, u8)> {
    match prev {
        None => full_frame_pixels(current),
        Some(p) => (0..FRAME_LEN as u8)
            .filter(|&i| current[i as usize] != p[i as usize])
            .map(|i| {
                let (r, g, b) = current[i as usize];
                (i, r, g, b)
            })
            .collect(),
    }
}

const DEBOUNCE: Duration = Duration::from_millis(60);
const REPUSH_INTERVAL: Duration = Duration::from_secs(30);

/// Debounces rapid state changes into a single write, dedupes unchanged
/// pixels out of every write, and forces a full resend every 30s
/// regardless of whether anything changed (repush, in case the firmware
/// missed a write or reset).
struct FrameSender {
    last_sent: Option<Frame17>,
    last_sent_at: Option<Instant>,
    pending: Option<(Frame17, Instant)>,
}

impl FrameSender {
    fn new() -> Self {
        Self { last_sent: None, last_sent_at: None, pending: None }
    }

    /// Forces the next `update()` to send a full frame, e.g. after an
    /// alarm flash or a mode switch where we don't trust what's on-device.
    fn reset(&mut self) {
        self.last_sent = None;
        self.last_sent_at = None;
        self.pending = None;
    }

    /// Call on every state change and on every timer tick. Returns pixels
    /// to write now, if it's time (debounce elapsed, or repush due).
    fn update(&mut self, desired: Frame17, now: Instant) -> Option<Vec<(u8, u8, u8, u8)>> {
        let changed_from_last_sent = self.last_sent != Some(desired);
        match self.pending {
            Some((pf, _)) if pf == desired => {}
            _ => self.pending = if changed_from_last_sent { Some((desired, now)) } else { None },
        }

        if let Some((pf, since)) = self.pending {
            if now.saturating_duration_since(since) >= DEBOUNCE {
                let pixels = frame_diff(self.last_sent.as_ref(), &pf);
                self.last_sent = Some(pf);
                self.last_sent_at = Some(now);
                self.pending = None;
                return if pixels.is_empty() { None } else { Some(pixels) };
            }
        }

        let repush_due = match self.last_sent_at {
            None => true,
            Some(t) => now.saturating_duration_since(t) >= REPUSH_INTERVAL,
        };
        if repush_due {
            if let Some(cur) = self.last_sent.or(self.pending.map(|(f, _)| f)) {
                self.last_sent = Some(cur);
                self.last_sent_at = Some(now);
                self.pending = None;
                return Some(full_frame_pixels(&cur));
            }
        }
        None
    }
}

fn fill_all_pixels(r: u8, g: u8, b: u8) -> Vec<(u8, u8, u8, u8)> {
    vec![(FILL_ALL_INDEX, r, g, b)]
}

// --- Foreground process match (feature 3) --------------------------------

fn is_paseo_foreground(image_path: &str) -> bool {
    image_path.to_ascii_lowercase().ends_with("paseo.exe")
}

// --- Paseo WS message parsing ---------------------------------------------
//
// Verified against Paseo's protocol source (packages/protocol/src/messages.ts).
//
// Connection sequence (see `winrt::ws_thread` for the I/O side): send
// `hello_message()` top-level, then every other outbound request (e.g.
// `fetch_workspaces_request_message()`, `usage_list_request_message()`)
// wrapped in the session envelope via `session_wrap` -- a bare,
// unwrapped request gets silently `rpc_error`'d by the server. Every
// SERVER message then arrives wrapped one level:
// `{"type":"session","message":{...inner...}}`. The inner message is one
// of `fetch_workspaces_response` (payload.entries: WorkspaceDescriptor[],
// full snapshot), `workspace_update` (payload tagged on "kind": upsert |
// remove, streamed forever once subscribed), `provider.usage.list.response`,
// or `rpc_error` (payload.requestType/error -- logged loudly by the
// caller, see `winrt::apply_ws_event`). Top-level (non-"session")
// messages are `pong` (liveness only, no state -- ignored here, tracked
// by the WS thread on any successful read) and are ignored by this parser.

#[derive(Debug, Clone, PartialEq)]
enum WsEvent {
    WorkspacesSnapshot { entries: Vec<WorkspaceDescriptor>, has_more: bool },
    WorkspaceUpsert(WorkspaceDescriptor),
    WorkspaceRemove(String),
    UsageResponse { request_id: String, providers: Vec<UsageProvider> },
    RpcError { request_type: String, error: String },
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct PageInfo {
    #[serde(rename = "hasMore")]
    has_more: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct FetchWorkspacesPayload {
    entries: Vec<WorkspaceDescriptor>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind")]
enum WorkspaceUpdate {
    #[serde(rename = "upsert")]
    Upsert { workspace: WorkspaceDescriptor },
    #[serde(rename = "remove")]
    Remove { id: String },
}

fn parse_inner_message(inner: &serde_json::Value) -> Vec<WsEvent> {
    let Some(t) = inner.get("type").and_then(|t| t.as_str()) else { return Vec::new() };
    let Some(payload) = inner.get("payload") else { return Vec::new() };
    match t {
        "fetch_workspaces_response" => match serde_json::from_value::<FetchWorkspacesPayload>(payload.clone()) {
            Ok(p) => vec![WsEvent::WorkspacesSnapshot { entries: p.entries, has_more: p.page_info.has_more }],
            Err(_) => Vec::new(),
        },
        "workspace_update" => match serde_json::from_value::<WorkspaceUpdate>(payload.clone()) {
            Ok(WorkspaceUpdate::Upsert { workspace }) => vec![WsEvent::WorkspaceUpsert(workspace)],
            Ok(WorkspaceUpdate::Remove { id }) => vec![WsEvent::WorkspaceRemove(id)],
            Err(_) => Vec::new(),
        },
        "provider.usage.list.response" => {
            let request_id = inner.get("requestId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            vec![WsEvent::UsageResponse { request_id, providers: parse_usage_providers(payload) }]
        }
        "rpc_error" => {
            let request_type = payload.get("requestType").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let error = payload.get("error").and_then(|v| v.as_str()).unwrap_or("").to_string();
            vec![WsEvent::RpcError { request_type, error }]
        }
        _ => Vec::new(),
    }
}

/// Every server message is session-wrapped: unwrap exactly one level and
/// parse the inner message. Non-"session" top-level messages (`pong`)
/// carry no state and are ignored here.
fn parse_ws_value(v: &serde_json::Value) -> Vec<WsEvent> {
    if v.get("type").and_then(|t| t.as_str()) != Some("session") {
        return Vec::new();
    }
    match v.get("message") {
        Some(inner) => parse_inner_message(inner),
        None => Vec::new(),
    }
}

fn parse_ws_text(raw: &str) -> Vec<WsEvent> {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(v) => parse_ws_value(&v),
        Err(_) => Vec::new(),
    }
}

/// The client->server handshake, sent immediately on connect (must be the
/// first message, within 15s, or the server closes with code 4001; wrong
/// `protocolVersion` closes with 4003). Top-level -- NOT session-wrapped
/// (only `hello` and `ping` are).
fn hello_message() -> serde_json::Value {
    serde_json::json!({
        "type": "hello",
        "clientId": "paseo-led-bridge",
        "clientType": "cli",
        "protocolVersion": 1,
        "appVersion": env!("CARGO_PKG_VERSION"),
    })
}

/// Every outbound client->server request except `hello` and `ping` must
/// be wrapped in this session envelope -- a bare, unwrapped request gets
/// silently `rpc_error`'d by the server (verified live).
fn session_wrap(message: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"type": "session", "message": message})
}

/// Sent right after `hello_message()`: fetches the current workspace list
/// and subscribes to `workspace_update` for the rest of the connection.
fn fetch_workspaces_request_message(request_id: &str) -> serde_json::Value {
    session_wrap(serde_json::json!({
        "type": "fetch_workspaces_request",
        "requestId": request_id,
        "sort": [{"key": "activity_at", "direction": "desc"}],
        "page": {"limit": 200},
        "subscribe": {},
    }))
}

/// Fetches usage for the usage-bar hotkey mode / 90% alarm poll.
fn usage_list_request_message(request_id: &str) -> serde_json::Value {
    session_wrap(serde_json::json!({
        "type": "provider.usage.list.request",
        "requestId": request_id,
    }))
}

static REQUEST_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn next_request_id(prefix: &str) -> String {
    let n = REQUEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{prefix}-{n}")
}

// ---------------------------------------------------------------------
// Subcommand entry points: real WinRT implementation on Windows, a clear
// stub everywhere else (this tool has no reason to run outside Windows).
// ---------------------------------------------------------------------

#[cfg(windows)]
fn run_scan() -> Result<()> {
    winrt::cmd_scan()
}

#[cfg(windows)]
fn run_frame(name_filter: &str, spec: &str) -> Result<()> {
    winrt::cmd_frame(name_filter, spec)
}

#[cfg(windows)]
fn run_demo(name_filter: &str) -> Result<()> {
    winrt::cmd_demo(name_filter)
}

#[cfg(windows)]
fn run_debug(name_filter: &str) -> Result<()> {
    winrt::cmd_debug(name_filter)
}

#[cfg(windows)]
fn run_run(name_filter: &str, ws_url: &str) -> Result<()> {
    winrt::cmd_run(name_filter, ws_url)
}

#[cfg(not(windows))]
const NOT_WINDOWS_MSG: &str = "paseo-led-bridge only works on Windows: it talks to the WinRT \
Bluetooth LE APIs to reach already-paired devices. Build a Windows exe with: \
cargo zigbuild --release --target x86_64-pc-windows-gnu";

#[cfg(not(windows))]
fn run_scan() -> Result<()> {
    bail!(NOT_WINDOWS_MSG)
}

#[cfg(not(windows))]
fn run_frame(_name_filter: &str, _spec: &str) -> Result<()> {
    bail!(NOT_WINDOWS_MSG)
}

#[cfg(not(windows))]
fn run_debug(_name_filter: &str) -> Result<()> {
    bail!(NOT_WINDOWS_MSG)
}

#[cfg(not(windows))]
fn run_demo(_name_filter: &str) -> Result<()> {
    bail!(NOT_WINDOWS_MSG)
}

#[cfg(not(windows))]
fn run_run(_name_filter: &str, _ws_url: &str) -> Result<()> {
    bail!(NOT_WINDOWS_MSG)
}

// ---------------------------------------------------------------------
// WinRT connection layer (Windows only). Uses blocking `.join()` on the
// WinRT async ops -- fine for a one-shot CLI, and it lets us drop tokio
// entirely.
// ---------------------------------------------------------------------

#[cfg(windows)]
mod winrt {
    use super::{
        chunk_pixels, compute_status_frame, detect_crossings, fetch_workspaces_request_message, fill_all_pixels,
        hello_message, is_paseo_foreground, next_request_id, ordered_usage_entries, parse_spec, parse_ws_text,
        rainbow_colors, usage_bar_frame, usage_list_request_message, format_usage_log, alarm_phase, BlinkPhase,
        FrameSender, UsageEntry, UsageProvider, WorkspaceStore, WsEvent, CHAR_UUID_STR, SERVICE_UUID_STR,
    };
    use anyhow::{anyhow, bail, Context, Result};
    use std::collections::HashMap;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    use windows::core::{GUID, HSTRING};
    use windows::Devices::Bluetooth::GenericAttributeProfile::{
        GattCharacteristic, GattCommunicationStatus, GattDeviceService, GattWriteOption,
    };
    use windows::Devices::Bluetooth::{BluetoothCacheMode, BluetoothLEDevice};
    use windows::Devices::Enumeration::DeviceInformation;
    use windows::Storage::Streams::DataWriter;

    fn service_guid() -> GUID {
        GUID::try_from(SERVICE_UUID_STR).expect("SERVICE_UUID_STR is a valid GUID literal")
    }

    fn char_guid() -> GUID {
        GUID::try_from(CHAR_UUID_STR).expect("CHAR_UUID_STR is a valid GUID literal")
    }

    /// Human-readable status name plus, for the non-obvious failure modes,
    /// a one-line hint about what usually causes it.
    fn describe_status(status: GattCommunicationStatus) -> String {
        match status {
            GattCommunicationStatus::Success => "Success".to_string(),
            GattCommunicationStatus::Unreachable => {
                "Unreachable (device is out of range or not connected)".to_string()
            }
            GattCommunicationStatus::ProtocolError => {
                "ProtocolError (the firmware rejected the request at the GATT protocol level)"
                    .to_string()
            }
            GattCommunicationStatus::AccessDenied => {
                "AccessDenied (the characteristic requires an encrypted/authenticated link; \
                 the device is bonded so Windows should elevate automatically -- if this \
                 persists, re-pair the Go60 in Windows Settings > Bluetooth & devices and \
                 retry)"
                    .to_string()
            }
            other => format!("unknown status ({})", other.0),
        }
    }

    struct PairedDevice {
        id: HSTRING,
        name: String,
    }

    /// Enumerates every BLE device paired/bonded in Windows -- no scanning,
    /// no advertisement required, which is exactly what a bonded-and-
    /// connected HID keyboard needs.
    fn list_paired_devices() -> Result<Vec<PairedDevice>> {
        let selector = BluetoothLEDevice::GetDeviceSelectorFromPairingState(true)
            .context("failed to build the paired-BLE-device selector")?;
        let collection = DeviceInformation::FindAllAsyncAqsFilter(&selector)
            .context("failed to start paired-device enumeration")?
            .join()
            .context("paired-device enumeration failed")?;

        let count = collection.Size().context("failed to read paired-device count")?;
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let info = collection.GetAt(i).context("failed to read a paired-device entry")?;
            let name = info.Name().map(|n| n.to_string()).unwrap_or_default();
            let id = info.Id().context("paired device is missing an Id")?;
            out.push(PairedDevice { id, name });
        }
        Ok(out)
    }

    fn find_paired_device(name_filter: &str) -> Result<PairedDevice> {
        let filter_lower = name_filter.to_ascii_lowercase();
        let devices = list_paired_devices()?;
        let checked = devices.len();
        devices
            .into_iter()
            .find(|d| d.name.to_ascii_lowercase().contains(&filter_lower))
            .ok_or_else(|| {
                anyhow!(
                    "no paired BLE device matching name filter '{name_filter}' \
                     ({checked} paired BLE device(s) checked) -- the Go60 must be paired in \
                     Windows Settings > Bluetooth & devices first; a merely-advertising, \
                     unpaired device will not show up here"
                )
            })
    }

    /// Connects to `device_id` and resolves the LED GATT service. Returns a
    /// precise, step-specific error: no such service (old firmware) vs. a
    /// GATT-level failure.
    fn locate_service(device: &BluetoothLEDevice, display_name: &str) -> Result<GattDeviceService> {
        let result = device
            .GetGattServicesForUuidWithCacheModeAsync(service_guid(), BluetoothCacheMode::Uncached)
            .context("failed to start GATT service discovery")?
            .join()
            .context("GATT service discovery failed")?;

        let status = result.Status().context("failed to read service-discovery status")?;
        if status != GattCommunicationStatus::Success {
            bail!(
                "{display_name}: GATT service discovery did not succeed: {}",
                describe_status(status)
            );
        }

        let services = result.Services().context("failed to read discovered services")?;
        let count = services.Size().context("failed to read service count")?;
        for i in 0..count {
            let svc = services.GetAt(i).context("failed to read a discovered service")?;
            if svc.Uuid().context("failed to read a service UUID")? == service_guid() {
                return Ok(svc);
            }
        }
        bail!(
            "{display_name} does not expose the LED GATT service ({SERVICE_UUID_STR}) -- \
             likely old firmware without the LED extension"
        );
    }

    /// Resolves the LED write characteristic within an already-located
    /// service.
    fn locate_characteristic(
        service: &GattDeviceService,
        display_name: &str,
    ) -> Result<GattCharacteristic> {
        let result = service
            .GetCharacteristicsForUuidAsync(char_guid())
            .context("failed to start characteristic discovery")?
            .join()
            .context("characteristic discovery failed")?;

        let status = result.Status().context("failed to read characteristic-discovery status")?;
        if status != GattCommunicationStatus::Success {
            bail!(
                "{display_name}: characteristic discovery did not succeed: {}",
                describe_status(status)
            );
        }

        let chars = result.Characteristics().context("failed to read discovered characteristics")?;
        let count = chars.Size().context("failed to read characteristic count")?;
        for i in 0..count {
            let c = chars.GetAt(i).context("failed to read a discovered characteristic")?;
            if c.Uuid().context("failed to read a characteristic UUID")? == char_guid() {
                return Ok(c);
            }
        }
        bail!(
            "LED service found on {display_name} but write characteristic ({CHAR_UUID_STR}) is \
             missing"
        );
    }

    /// Finds the paired keyboard, connects, and locates the LED write
    /// characteristic. Every step reports a distinct, precise error.
    ///
    /// Returns the `GattDeviceService` alongside the device and
    /// characteristic (not just the characteristic) so the caller can
    /// explicitly `.Close()` both on teardown/reconnect -- WinRT GATT
    /// sessions are a system resource that Windows keeps open until
    /// `Close()` is called; merely dropping (releasing) the COM reference
    /// leaves it open and makes the *next* discovery attempt on the same
    /// device fail with `AccessDenied` for the rest of the process's life.
    /// For the same reason, any partial session opened here (device
    /// connected but service/characteristic discovery failed) is closed
    /// before the error is returned.
    fn connect_and_locate(name_filter: &str) -> Result<(BluetoothLEDevice, GattDeviceService, GattCharacteristic)> {
        println!("looking for a paired device matching '{name_filter}'...");
        let paired = find_paired_device(name_filter)?;
        println!("connecting to {}...", paired.name);

        let device = BluetoothLEDevice::FromIdAsync(&paired.id)
            .context("failed to start device connection")?
            .join()
            .with_context(|| format!("failed to connect to {}", paired.name))?;

        println!("discovering LED service...");
        let service = match locate_service(&device, &paired.name) {
            Ok(s) => s,
            Err(e) => {
                let _ = device.Close();
                return Err(e);
            }
        };
        println!("discovering LED characteristic...");
        let characteristic = match locate_characteristic(&service, &paired.name) {
            Ok(c) => c,
            Err(e) => {
                let _ = service.Close();
                let _ = device.Close();
                return Err(e);
            }
        };

        Ok((device, service, characteristic))
    }

    /// Writes one frame, trying write-without-response first (the
    /// firmware's normal mode) and falling back to write-with-response if
    /// the device rejects it. Reports which mode is in use.
    struct Writer {
        characteristic: GattCharacteristic,
        mode: GattWriteOption,
        mode_reported: bool,
    }

    impl Writer {
        fn new(characteristic: GattCharacteristic) -> Self {
            Self { characteristic, mode: GattWriteOption::WriteWithoutResponse, mode_reported: false }
        }

        fn write_with(&self, data: &[u8], option: GattWriteOption) -> Result<GattCommunicationStatus> {
            let writer = DataWriter::new().context("failed to create a data writer")?;
            writer.WriteBytes(data).context("failed to buffer frame bytes")?;
            let buffer = writer.DetachBuffer().context("failed to build a GATT write buffer")?;
            self.characteristic
                .WriteValueWithOptionAsync(&buffer, option)
                .context("failed to start GATT write")?
                .join()
                .context("GATT write did not complete")
        }

        fn write(&mut self, data: &[u8]) -> Result<()> {
            let status = self.write_with(data, self.mode)?;
            if status == GattCommunicationStatus::Success {
                if !self.mode_reported {
                    println!(
                        "write mode: {}",
                        if self.mode == GattWriteOption::WriteWithoutResponse {
                            "write-without-response"
                        } else {
                            "write-with-response"
                        }
                    );
                    self.mode_reported = true;
                }
                return Ok(());
            }

            // Only worth falling back once, from without-response to
            // with-response -- that's the one substitution the firmware
            // characteristic is documented to support.
            if self.mode == GattWriteOption::WriteWithoutResponse {
                let fallback_status = self.write_with(data, GattWriteOption::WriteWithResponse)?;
                if fallback_status == GattCommunicationStatus::Success {
                    self.mode = GattWriteOption::WriteWithResponse;
                    println!(
                        "write-without-response failed ({}); falling back to write-with-response, \
                         which worked",
                        describe_status(status)
                    );
                    self.mode_reported = true;
                    return Ok(());
                }
                bail!(
                    "write failed: write-without-response gave {}, write-with-response gave {}",
                    describe_status(status),
                    describe_status(fallback_status)
                );
            }

            bail!("write failed: {}", describe_status(status));
        }

        /// Writes a full pixel list, chunked into BLE-safe sub-frames (see
        /// `chunk_pixels`) and sent sequentially with a 10ms gap between
        /// writes. A 17-pixel frame is 69 bytes, which exceeds the ATT
        /// MTU-3 write-without-response payload cap (hence chunking, not
        /// a write-with-response "long write" -- the firmware rejects
        /// nonzero-offset writes by design, so that path can't help here
        /// regardless of size).
        fn write_pixels(&mut self, pixels: &[(u8, u8, u8, u8)]) -> Result<()> {
            for chunk in chunk_pixels(pixels) {
                self.write(&chunk)?;
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        }
    }

    pub fn cmd_scan() -> Result<()> {
        let devices = list_paired_devices()?;
        if devices.is_empty() {
            println!("no paired BLE devices found (pair the Go60 in Windows Settings > Bluetooth & devices)");
            return Ok(());
        }

        println!("{:<32} LED SERVICE", "NAME");
        for d in devices {
            let has_service = match BluetoothLEDevice::FromIdAsync(&d.id).ok().and_then(|op| op.join().ok()) {
                Some(device) => locate_service(&device, &d.name).is_ok(),
                None => false,
            };
            println!(
                "{:<32} {}",
                if d.name.is_empty() { "(unnamed)" } else { &d.name },
                if has_service { "yes" } else { "no" }
            );
        }
        Ok(())
    }

    /// Ground-truth dump: connection status plus EVERY GATT service the
    /// device exposes, queried in both cache modes. Diagnoses "service
    /// missing" disputes between firmware and Windows.
    pub fn cmd_debug(name_filter: &str) -> Result<()> {
        let paired = find_paired_device(name_filter)?;
        println!("device: {} (id: {})", paired.name, paired.id);

        let device = BluetoothLEDevice::FromIdAsync(&paired.id)
            .context("failed to start device connection")?
            .join()
            .with_context(|| format!("failed to connect to {}", paired.name))?;

        if let Ok(status) = device.ConnectionStatus() {
            println!("connection status: {:?} ({})", status.0, if status.0 == 1 { "Connected" } else { "Disconnected" });
        }
        if let Ok(addr) = device.BluetoothAddress() {
            println!("address: {:012X}", addr);
        }

        for (label, mode) in [
            ("CACHED", BluetoothCacheMode::Cached),
            ("UNCACHED", BluetoothCacheMode::Uncached),
        ] {
            println!("\n--- all GATT services ({label} mode) ---");
            let result = device
                .GetGattServicesWithCacheModeAsync(mode)
                .context("failed to start full service enumeration")?
                .join()
                .context("full service enumeration failed")?;
            let status = result.Status().context("failed to read enumeration status")?;
            println!("status: {}", describe_status(status));
            if status == GattCommunicationStatus::Success {
                let services = result.Services().context("failed to read services")?;
                let count = services.Size().context("failed to read service count")?;
                println!("{count} service(s):");
                for i in 0..count {
                    let svc = services.GetAt(i)?;
                    let uuid = svc.Uuid()?;
                    let marker = if uuid == service_guid() { "  <-- LED SERVICE" } else { "" };
                    println!("  {uuid:?}{marker}");
                }
            }
        }
        Ok(())
    }

    pub fn cmd_frame(name_filter: &str, spec: &str) -> Result<()> {
        let pixels = parse_spec(spec)?;

        let (_device, _service, characteristic) = connect_and_locate(name_filter)?;
        let mut writer = Writer::new(characteristic);
        writer.write_pixels(&pixels)?;
        println!("frame written ({} pixel(s))", pixels.len());
        Ok(())
    }

    pub fn cmd_demo(name_filter: &str) -> Result<()> {
        let (_device, _service, characteristic) = connect_and_locate(name_filter)?;
        let mut writer = Writer::new(characteristic);

        let colors = rainbow_colors();
        const TICKS: usize = 30; // 15s at 2Hz
        println!("running rainbow demo for 15s (2 Hz)...");
        for tick in 0..TICKS {
            let pixels: Vec<(u8, u8, u8, u8)> = (0..10u8)
                .map(|idx| {
                    let (r, g, b) = colors[(idx as usize + tick) % 10];
                    (idx, r, g, b)
                })
                .collect();
            writer.write_pixels(&pixels)?;
            std::thread::sleep(std::time::Duration::from_millis(500));
        }

        let off: Vec<(u8, u8, u8, u8)> = (0..10u8).map(|idx| (idx, 0, 0, 0)).collect();
        writer.write_pixels(&off)?;
        println!("demo complete, LEDs off");
        Ok(())
    }

    // -------------------------------------------------------------
    // `run`: the live-sync daemon.
    // -------------------------------------------------------------

    /// Connects (with retry/backoff) and wraps the BLE device, GATT
    /// service, and write characteristic. Kept together so the device
    /// (whose WinRT connection the characteristic writes depend on) stays
    /// alive for the life of the daemon, not just one call -- and so the
    /// service is reachable for an explicit `.Close()` on teardown (see
    /// `BleConnection::close` and `connect_and_locate`).
    struct BleConnection {
        device: BluetoothLEDevice,
        service: GattDeviceService,
        writer: Writer,
    }

    impl BleConnection {
        /// WinRT GATT sessions must be explicitly closed -- releasing the
        /// COM reference alone leaves Windows holding the session open,
        /// which makes the next characteristic-discovery attempt on this
        /// device fail with `AccessDenied` until the process exits. Must
        /// be called (and its retryable `AccessDenied` given a chance to
        /// clear) *before* opening a replacement session.
        fn close(&self) {
            let _ = self.service.Close();
            let _ = self.device.Close();
        }
    }

    fn connect_ble_with_retry(name_filter: &str) -> BleConnection {
        loop {
            match connect_and_locate(name_filter) {
                Ok((device, service, characteristic)) => {
                    println!("led-bridge: BLE connected");
                    return BleConnection { device, service, writer: Writer::new(characteristic) };
                }
                Err(e) => {
                    eprintln!("led-bridge: BLE connect failed: {e:#}; retrying in 5s...");
                    std::thread::sleep(Duration::from_secs(5));
                }
            }
        }
    }

    /// Everything that can arrive on the main loop's single mpsc channel.
    /// The WS client, the hotkey thread, and the foreground poller are all
    /// additional senders into this one channel; the main loop is the only
    /// receiver.
    enum Event {
        Ws(String),
        WsDisconnected,
        ForegroundChanged(bool),
        HotkeyPressed,
    }

    /// Paseo daemon WebSocket client: reconnect-loops (full state rebuild
    /// each time -- hello, then fetch_workspaces_request with subscribe),
    /// forwards inbound messages as `Event::Ws`, sends whatever JSON text
    /// arrives on `cmd_rx`, and keeps the connection alive with a 10s
    /// top-level ping / 15s read-silence-is-dead check. Single-threaded,
    /// non-blocking-poll design (rather than a reader/writer thread pair)
    /// so both directions share one connection without needing to split or
    /// mutex-wrap the socket.
    fn ws_thread(url: String, event_tx: mpsc::Sender<Event>, cmd_rx: mpsc::Receiver<String>) {
        use tungstenite::stream::MaybeTlsStream;
        use tungstenite::Message;

        const PING_INTERVAL: Duration = Duration::from_secs(10);
        const READ_SILENCE_TIMEOUT: Duration = Duration::from_secs(15);
        const RECONNECT_DELAY: Duration = Duration::from_secs(3);

        loop {
            match tungstenite::connect(&url) {
                Ok((mut socket, _response)) => {
                    println!("led-bridge: ws connected to {url}");
                    if let MaybeTlsStream::Plain(tcp) = socket.get_ref() {
                        let _ = tcp.set_nonblocking(true);
                    }

                    // Step 1-2 of the connection sequence: hello must be
                    // the first message, then fetch_workspaces_request
                    // (with subscribe) to get a snapshot and start the
                    // workspace_update stream.
                    let sent_hello = socket.send(Message::text(hello_message().to_string())).is_ok();
                    let sent_fetch = sent_hello
                        && socket
                            .send(Message::text(fetch_workspaces_request_message(&next_request_id("fetch")).to_string()))
                            .is_ok();

                    if sent_fetch {
                        let mut last_read = Instant::now();
                        let mut last_ping = Instant::now();
                        'conn: loop {
                            match socket.read() {
                                Ok(Message::Text(text)) => {
                                    last_read = Instant::now();
                                    if event_tx.send(Event::Ws(text.to_string())).is_err() {
                                        return; // main loop is gone, shut down
                                    }
                                }
                                Ok(Message::Close(_)) => {
                                    println!("led-bridge: ws closed by server");
                                    break 'conn;
                                }
                                Ok(_) => {
                                    last_read = Instant::now(); // pong etc. still count as liveness
                                }
                                Err(tungstenite::Error::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                                Err(e) => {
                                    eprintln!("led-bridge: ws read error: {e}");
                                    break 'conn;
                                }
                            }
                            while let Ok(json) = cmd_rx.try_recv() {
                                if let Err(e) = socket.send(Message::text(json)) {
                                    eprintln!("led-bridge: ws send error: {e}");
                                }
                            }
                            let now = Instant::now();
                            if now.saturating_duration_since(last_ping) >= PING_INTERVAL {
                                last_ping = now;
                                // Ping is top-level, not session-wrapped.
                                let _ = socket.send(Message::text(r#"{"type":"ping"}"#));
                            }
                            if now.saturating_duration_since(last_read) >= READ_SILENCE_TIMEOUT {
                                eprintln!(
                                    "led-bridge: ws read silence >{}s, reconnecting",
                                    READ_SILENCE_TIMEOUT.as_secs()
                                );
                                break 'conn;
                            }
                            std::thread::sleep(Duration::from_millis(50));
                        }
                    } else {
                        eprintln!("led-bridge: ws handshake send failed; reconnecting");
                    }
                }
                Err(e) => {
                    eprintln!("led-bridge: ws connect failed: {e}; retrying in {}s...", RECONNECT_DELAY.as_secs());
                }
            }
            let _ = event_tx.send(Event::WsDisconnected);
            std::thread::sleep(RECONNECT_DELAY);
        }
    }

    /// Global hotkey thread: Shift+F17 toggles/cycles usage-bar mode. Owns
    /// its own thread because RegisterHotKey delivers WM_HOTKEY through a
    /// GetMessageW loop on the registering thread. No window is created, so
    /// there's no TranslateMessage/DispatchMessageW step -- we just inspect
    /// each posted thread message directly.
    fn hotkey_thread(event_tx: mpsc::Sender<Event>) {
        use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, MOD_SHIFT, VK_F17};
        use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};

        const HOTKEY_ID: i32 = 1;
        unsafe {
            if let Err(e) = RegisterHotKey(None, HOTKEY_ID, MOD_SHIFT, VK_F17.0 as u32) {
                eprintln!("led-bridge: RegisterHotKey(Shift+F17) failed: {e}; usage-bar hotkey disabled");
                return;
            }
        }
        println!("led-bridge: usage-bar hotkey armed (Shift+F17)");
        let mut msg = MSG::default();
        loop {
            let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
            if ret.0 == 0 || ret.0 == -1 {
                break; // WM_QUIT or error
            }
            if msg.message == WM_HOTKEY && msg.wParam.0 as i32 == HOTKEY_ID {
                if event_tx.send(Event::HotkeyPressed).is_err() {
                    return;
                }
            }
        }
    }

    /// Reads the foreground window's owning process image path, e.g.
    /// `C:\...\Paseo.exe`. `None` on any failure (permission denied,
    /// system process, etc.) -- treated as "not Paseo".
    fn foreground_image_path() -> Option<String> {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
        use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
        use windows::core::PWSTR;

        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_invalid() {
                return None;
            }
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
            if pid == 0 {
                return None;
            }
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            let result = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len as *mut u32);
            let _ = CloseHandle(handle);
            result.ok()?;
            Some(String::from_utf16_lossy(&buf[..len as usize]))
        }
    }

    /// Polls the foreground window every 500ms and sends an event only
    /// when the "is it Paseo" answer changes.
    fn foreground_poll_thread(event_tx: mpsc::Sender<Event>) {
        let mut last: Option<bool> = None;
        loop {
            let is_paseo = foreground_image_path().map(|p| is_paseo_foreground(&p)).unwrap_or(false);
            if last != Some(is_paseo) {
                last = Some(is_paseo);
                if event_tx.send(Event::ForegroundChanged(is_paseo)).is_err() {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum UsagePurpose {
        Interactive,
        Background,
    }

    struct UsageModeState {
        entries: Vec<UsageEntry>,
        cursor: usize,
        last_press_at: Instant,
    }

    struct AlarmState {
        started_at: Instant,
        last_phase: Option<BlinkPhase>,
    }

    const USAGE_CACHE_TTL: Duration = Duration::from_secs(60);
    const USAGE_MODE_AUTO_EXIT: Duration = Duration::from_secs(6);
    // ponytail: fixed 5s timeout for an interactive usage fetch: not
    // specified by the spec, chosen to be well above a local WS
    // round-trip; tune if the real daemon is slower than that.
    const USAGE_FETCH_TIMEOUT: Duration = Duration::from_secs(5);
    const BACKGROUND_POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);

    struct RunState {
        store: WorkspaceStore,
        foreground_is_paseo: bool,
        usage_cache: Option<(Instant, Vec<UsageProvider>)>,
        pending_usage: HashMap<String, UsagePurpose>,
        awaiting_interactive: Option<Instant>,
        usage_mode: Option<UsageModeState>,
        alarm_baseline: HashMap<(String, String), f64>,
        last_bg_poll: Option<Instant>,
        alarm: Option<AlarmState>,
    }

    impl RunState {
        fn new() -> Self {
            Self {
                store: WorkspaceStore::new(),
                foreground_is_paseo: false,
                usage_cache: None,
                pending_usage: HashMap::new(),
                awaiting_interactive: None,
                usage_mode: None,
                alarm_baseline: HashMap::new(),
                last_bg_poll: None,
                alarm: None,
            }
        }
    }

    /// Cached usage if fresh (<60s old); otherwise sends a
    /// `provider.usage.list.request` over `ws_cmd_tx` and returns `None`
    /// (caller awaits the matching `WsEvent::UsageResponse`).
    fn request_usage(
        state: &mut RunState,
        purpose: UsagePurpose,
        now: Instant,
        ws_cmd_tx: &mpsc::Sender<String>,
    ) -> Option<Vec<UsageProvider>> {
        if let Some((t, providers)) = &state.usage_cache {
            if now.saturating_duration_since(*t) < USAGE_CACHE_TTL {
                return Some(providers.clone());
            }
        }
        let request_id = next_request_id(match purpose {
            UsagePurpose::Interactive => "interactive",
            UsagePurpose::Background => "background",
        });
        state.pending_usage.insert(request_id.clone(), purpose);
        let _ = ws_cmd_tx.send(usage_list_request_message(&request_id).to_string());
        None
    }

    fn apply_ws_event(state: &mut RunState, ev: WsEvent, now: Instant) {
        match ev {
            WsEvent::WorkspacesSnapshot { entries, has_more } => {
                if has_more {
                    eprintln!("led-bridge: fetch_workspaces_response has more pages (not paginating)");
                }
                state.store.apply_snapshot(entries);
            }
            WsEvent::WorkspaceUpsert(w) => state.store.apply_upsert(w, now),
            WsEvent::WorkspaceRemove(id) => state.store.apply_remove(&id),
            WsEvent::UsageResponse { request_id, providers } => {
                state.usage_cache = Some((now, providers.clone()));
                let Some(purpose) = state.pending_usage.remove(&request_id) else { return };
                match purpose {
                    UsagePurpose::Interactive => {
                        state.awaiting_interactive = None;
                        enter_usage_mode_with(state, providers, now);
                    }
                    UsagePurpose::Background => {
                        check_alarm_crossings(state, &providers, now);
                    }
                }
            }
            WsEvent::RpcError { request_type, error } => {
                eprintln!("led-bridge: rpc error {request_type}: {error}");
            }
        }
    }

    fn enter_usage_mode_with(state: &mut RunState, providers: Vec<UsageProvider>, now: Instant) {
        let entries = ordered_usage_entries(&providers);
        if entries.is_empty() {
            println!("usage: no tracked windows found");
            state.usage_mode = None;
            return;
        }
        println!("{}", format_usage_log(&entries[0]));
        state.usage_mode = Some(UsageModeState { entries, cursor: 0, last_press_at: now });
    }

    fn check_alarm_crossings(state: &mut RunState, providers: &[UsageProvider], now: Instant) {
        let entries = ordered_usage_entries(providers);
        let crossed = detect_crossings(&state.alarm_baseline, &entries);
        for &i in &crossed {
            println!("usage alarm: {} crossed 90%", format_usage_log(&entries[i]));
        }
        for e in &entries {
            state.alarm_baseline.insert((e.provider_key.clone(), e.window_key.clone()), e.used_pct);
        }
        if !crossed.is_empty() && state.alarm.is_none() {
            state.alarm = Some(AlarmState { started_at: now, last_phase: None });
        }
    }

    fn handle_hotkey(state: &mut RunState, ws_cmd_tx: &mpsc::Sender<String>, now: Instant) {
        if let Some(mode) = &mut state.usage_mode {
            mode.cursor = (mode.cursor + 1) % mode.entries.len();
            mode.last_press_at = now;
            println!("{}", format_usage_log(&mode.entries[mode.cursor]));
            return;
        }
        match request_usage(state, UsagePurpose::Interactive, now, ws_cmd_tx) {
            Some(providers) => enter_usage_mode_with(state, providers, now),
            None => state.awaiting_interactive = Some(now),
        }
    }

    /// One tick of rendering: advances the alarm animation (if any) with
    /// direct fill-all writes, checks usage-mode timeout/auto-exit, fires
    /// the background usage poll on its 5-minute timer, and otherwise
    /// composes+sends the current normal/usage-bar frame through the
    /// debounce/dedupe/repush engine.
    fn render_tick(
        state: &mut RunState,
        ble: &mut BleConnection,
        name_filter: &str,
        frame_sender: &mut FrameSender,
        ws_cmd_tx: &mpsc::Sender<String>,
        now: Instant,
    ) {
        if let Some(alarm) = &mut state.alarm {
            let phase = alarm_phase(alarm.started_at, now);
            if phase == BlinkPhase::Done {
                state.alarm = None;
                frame_sender.reset();
            } else if alarm.last_phase != Some(phase) {
                alarm.last_phase = Some(phase);
                let pixels = match phase {
                    BlinkPhase::On => fill_all_pixels(0xFF, 0x00, 0x00),
                    _ => fill_all_pixels(0x00, 0x00, 0x00),
                };
                write_with_reconnect(ble, name_filter, &pixels);
            }
            return;
        }

        if let Some(started) = state.awaiting_interactive {
            if now.saturating_duration_since(started) >= USAGE_FETCH_TIMEOUT {
                eprintln!("led-bridge: usage fetch timed out");
                state.awaiting_interactive = None;
                let pixels: Vec<(u8, u8, u8, u8)> = (0..10u8).map(|i| (i, 0xFF, 0x00, 0x00)).collect();
                write_with_reconnect(ble, name_filter, &pixels);
                std::thread::sleep(Duration::from_secs(1));
                state.usage_mode = None;
                frame_sender.reset();
            }
        }

        if let Some(mode) = &state.usage_mode {
            if now.saturating_duration_since(mode.last_press_at) >= USAGE_MODE_AUTO_EXIT {
                state.usage_mode = None;
                frame_sender.reset();
            }
        }

        let bg_due = match state.last_bg_poll {
            None => true,
            Some(t) => now.saturating_duration_since(t) >= BACKGROUND_POLL_INTERVAL,
        };
        if bg_due {
            state.last_bg_poll = Some(now);
            match request_usage(state, UsagePurpose::Background, now, ws_cmd_tx) {
                Some(providers) => check_alarm_crossings(state, &providers, now),
                None => {}
            }
        }

        let desired = match &state.usage_mode {
            Some(mode) => usage_bar_frame(mode.entries[mode.cursor].used_pct),
            None => compute_status_frame(&state.store, state.foreground_is_paseo, now),
        };
        // Slot area is overridden by the usage bar; action/permission keys
        // are already off in `usage_bar_frame`, matching the spec.

        if let Some(pixels) = frame_sender.update(desired, now) {
            write_with_reconnect(ble, name_filter, &pixels);
        }
    }

    fn write_with_reconnect(ble: &mut BleConnection, name_filter: &str, pixels: &[(u8, u8, u8, u8)]) {
        if let Err(e) = ble.writer.write_pixels(pixels) {
            eprintln!("led-bridge: BLE write failed ({e:#}); reconnecting...");
            // Close the failed session *before* opening a new one -- WinRT
            // won't grant a fresh GATT session on this device while the
            // old one is still open (see `BleConnection::close`).
            ble.close();
            *ble = connect_ble_with_retry(name_filter);
        }
    }

    pub fn cmd_run(name_filter: &str, ws_url: &str) -> Result<()> {
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        let (ws_cmd_tx, ws_cmd_rx) = mpsc::channel::<String>();

        {
            let tx = event_tx.clone();
            let url = ws_url.to_string();
            std::thread::spawn(move || ws_thread(url, tx, ws_cmd_rx));
        }
        {
            let tx = event_tx.clone();
            std::thread::spawn(move || hotkey_thread(tx));
        }
        {
            let tx = event_tx.clone();
            std::thread::spawn(move || foreground_poll_thread(tx));
        }
        drop(event_tx); // main loop keeps event_rx; threads hold their own clones

        let mut ble = connect_ble_with_retry(name_filter);
        let mut state = RunState::new();
        let mut frame_sender = FrameSender::new();
        const TICK: Duration = Duration::from_millis(100);

        println!("led-bridge: run started (ws: {ws_url}, ble: {name_filter})");
        loop {
            match event_rx.recv_timeout(TICK) {
                Ok(Event::Ws(text)) => {
                    let now = Instant::now();
                    for ev in parse_ws_text(&text) {
                        apply_ws_event(&mut state, ev, now);
                    }
                }
                Ok(Event::WsDisconnected) => {
                    eprintln!("led-bridge: ws disconnected, reconnecting...");
                }
                Ok(Event::ForegroundChanged(is_paseo)) => {
                    state.foreground_is_paseo = is_paseo;
                }
                Ok(Event::HotkeyPressed) => {
                    handle_hotkey(&mut state, &ws_cmd_tx, Instant::now());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("led-bridge: all event senders dropped unexpectedly");
                }
            }
            render_tick(&mut state, &mut ble, name_filter, &mut frame_sender, &ws_cmd_tx, Instant::now());
        }
    }
}

// ---------------------------------------------------------------------
// Self-check: run with `cargo test` (no BLE hardware needed). These cover
// only the pure functions above, so they run the same on Linux and
// Windows -- the WinRT connection layer has no meaningful unit-testable
// surface without real hardware.
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn spec_parsing_and_frame_encoding() {
        let pixels = parse_spec("1=blue,2=orange,6=green,0=FF00FF").unwrap();
        assert_eq!(pixels, vec![
            (0, 0x00, 0x33, 0xFF),
            (1, 0xFF, 0x5F, 0x00),
            (5, 0x00, 0xC8, 0x00),
            (9, 0xFF, 0x00, 0xFF),
        ]);
        let frame = build_frame(&pixels);
        assert_eq!(frame[0], 4);
        assert_eq!(frame.len(), 1 + 4 * 4);
        assert_eq!(&frame[1..5], &[0, 0x00, 0x33, 0xFF]);

        assert!(parse_spec("").is_err());
        assert!(parse_spec("x=blue").is_err());
        assert!(parse_spec("1=notacolor").is_err());
        assert!(parse_spec("1=GHIJKL").is_err());
    }

    /// Decodes a sequence of chunk byte-buffers back into the flat pixel
    /// list they encode, in order -- used to check chunking round-trips.
    fn decode_chunks(chunks: &[Vec<u8>]) -> Vec<(u8, u8, u8, u8)> {
        let mut out = Vec::new();
        for c in chunks {
            let n = c[0] as usize;
            assert_eq!(c.len(), 1 + n * 4, "chunk {c:?} has a malformed length for its count byte");
            for i in 0..n {
                let base = 1 + i * 4;
                out.push((c[base], c[base + 1], c[base + 2], c[base + 3]));
            }
        }
        out
    }

    #[test]
    fn chunk_pixels_caps_each_chunk_at_4_pixels_and_preserves_order() {
        let pixels: Vec<(u8, u8, u8, u8)> = (0..17u8).map(|i| (i, i, i, i)).collect();
        let chunks = chunk_pixels(&pixels);
        assert_eq!(chunks.len(), 5); // 4+4+4+4+1
        for c in &chunks[..4] {
            assert_eq!(c[0], 4);
            assert_eq!(c.len(), 1 + 4 * 4); // <= 20 bytes, safe at minimum ATT MTU
        }
        assert_eq!(chunks[4][0], 1);
        assert_eq!(decode_chunks(&chunks), pixels);
    }

    #[test]
    fn chunk_pixels_single_pixel_is_one_chunk() {
        let pixels = vec![(3, 1, 2, 3)];
        let chunks = chunk_pixels(&pixels);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], vec![1, 3, 1, 2, 3]);
    }

    #[test]
    fn chunk_pixels_keeps_fill_all_in_first_chunk_with_overrides() {
        // fill-all + 2 overrides = 3 pixels: all fit in one chunk, fill-all first.
        let pixels = vec![(0xFE, 255, 0, 0), (3, 0, 0, 255), (5, 0, 255, 0)];
        let chunks = chunk_pixels(&pixels);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0][0], 3);
        assert_eq!(&chunks[0][1..5], &[0xFE, 255, 0, 0]);
        assert_eq!(decode_chunks(&chunks), pixels);
    }

    #[test]
    fn chunk_pixels_keeps_fill_all_in_first_chunk_even_when_overrides_spill_over() {
        // fill-all + 5 overrides = 6 pixels -> chunks of 4 then 2; the
        // fill-all must land in the first chunk, applied before the
        // overrides that follow it (same or later chunk).
        let mut pixels = vec![(0xFE, 1, 2, 3)];
        pixels.extend((0..5u8).map(|i| (i, i, i, i)));
        let chunks = chunk_pixels(&pixels);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0][0], 4);
        assert_eq!(&chunks[0][1..5], &[0xFE, 1, 2, 3]);
        assert_eq!(chunks[1][0], 2);
        assert_eq!(decode_chunks(&chunks), pixels);
    }

    #[test]
    fn name_flag_extraction() {
        let args: Vec<String> = vec!["--name".into(), "MyKb".into(), "scan".into()];
        let (name, rest) = extract_name_flag(&args).unwrap();
        assert_eq!(name, "MyKb");
        assert_eq!(rest, vec!["scan".to_string()]);

        let args: Vec<String> = vec!["frame".into(), "1=red".into()];
        let (name, rest) = extract_name_flag(&args).unwrap();
        assert_eq!(name, DEFAULT_NAME_FILTER);
        assert_eq!(rest, vec!["frame".to_string(), "1=red".to_string()]);
    }

    #[test]
    fn ws_url_flag_extraction() {
        let args: Vec<String> = vec!["run".into(), "--ws-url".into(), "ws://x:1/y".into()];
        let (val, rest) = extract_flag(&args, "--ws-url").unwrap();
        assert_eq!(val, Some("ws://x:1/y".to_string()));
        assert_eq!(rest, vec!["run".to_string()]);
        assert!(extract_flag(&["--ws-url".to_string()], "--ws-url").is_err());
    }

    // --- Status colors ---

    #[test]
    fn status_color_map() {
        assert_eq!(Status::Done.color(), (0x0A, 0x0A, 0x0A));
        assert_eq!(Status::NeedsInput.color(), (0xFF, 0x5F, 0x00));
        assert_eq!(Status::Attention.color(), (0x00, 0xC8, 0x00));
        assert_eq!(Status::Failed.color(), (0xFF, 0x00, 0x00));
        assert_eq!(Status::Running.color(), (0x00, 0x33, 0xFF));
        assert_eq!(Status::from_wire("done"), Some(Status::Done));
        assert_eq!(Status::from_wire("bogus"), None);
    }

    // --- Blink-then-solid ---

    #[test]
    fn blink_schedule() {
        let t0 = Instant::now();
        assert_eq!(blink_phase(t0, t0), BlinkPhase::On);
        assert_eq!(blink_phase(t0, t0 + Duration::from_millis(200)), BlinkPhase::On);
        assert_eq!(blink_phase(t0, t0 + Duration::from_millis(400)), BlinkPhase::Off);
        assert_eq!(blink_phase(t0, t0 + Duration::from_millis(799)), BlinkPhase::Off);
        assert_eq!(blink_phase(t0, t0 + Duration::from_millis(800)), BlinkPhase::On);
        assert_eq!(blink_phase(t0, t0 + Duration::from_millis(1600)), BlinkPhase::On);
        assert_eq!(blink_phase(t0, t0 + Duration::from_millis(2000)), BlinkPhase::Off);
        // 3 full cycles = 2400ms, then solid.
        assert_eq!(blink_phase(t0, t0 + Duration::from_millis(2399)), BlinkPhase::Off);
        assert_eq!(blink_phase(t0, t0 + Duration::from_millis(2400)), BlinkPhase::Done);
        assert_eq!(blink_phase(t0, t0 + Duration::from_secs(60)), BlinkPhase::Done);
    }

    #[test]
    fn attention_transition_detection() {
        // A workspace that simply *appears* already at attention (no prior
        // status on record) must NOT blink.
        assert!(!just_entered_attention(None, Some(Status::Attention)));
        assert!(just_entered_attention(Some(Status::Running), Some(Status::Attention)));
        assert!(!just_entered_attention(Some(Status::Attention), Some(Status::Attention)));
        assert!(!just_entered_attention(Some(Status::Attention), Some(Status::Done)));
        assert!(!just_entered_attention(None, None));
    }

    #[test]
    fn alarm_schedule_matches_spec_500ms_x3() {
        let t0 = Instant::now();
        assert_eq!(alarm_phase(t0, t0), BlinkPhase::On);
        assert_eq!(alarm_phase(t0, t0 + Duration::from_millis(500)), BlinkPhase::Off);
        assert_eq!(alarm_phase(t0, t0 + Duration::from_millis(2999)), BlinkPhase::Off);
        assert_eq!(alarm_phase(t0, t0 + Duration::from_millis(3000)), BlinkPhase::Done);
    }

    // --- Workspace store / slot derivation ---

    /// A pinned, non-archiving `WorkspaceDescriptor` with the given id,
    /// `pinnedAt`, and status -- the common case for slot tests.
    fn ws(id: &str, pinned_at: &str, status: &str) -> WorkspaceDescriptor {
        WorkspaceDescriptor {
            id: id.to_string(),
            name: String::new(),
            pinned_at: Some(pinned_at.to_string()),
            archiving_at: None,
            status: status.to_string(),
        }
    }

    #[test]
    fn derive_slots_filters_unpinned_and_archiving_sorts_desc_caps_at_10() {
        let mut workspaces = HashMap::new();
        let mut unpinned = ws("unpinned", "2026-08-09T10:00:00Z", "running");
        unpinned.pinned_at = None;
        workspaces.insert(unpinned.id.clone(), unpinned);
        let mut archiving = ws("archiving", "2026-08-09T10:00:00Z", "running");
        archiving.archiving_at = Some("2026-08-09T11:00:00Z".to_string());
        workspaces.insert(archiving.id.clone(), archiving);
        for i in 0..12 {
            let id = format!("ws{i}");
            // ws0 pinned earliest, ws11 pinned latest (newest).
            let w = ws(&id, &format!("2026-08-{:02}T00:00:00Z", i + 1), "running");
            workspaces.insert(id, w);
        }

        let slots = derive_slots(&workspaces);
        assert_eq!(slots.len(), 10); // capped at 10
        assert_eq!(slots[0], "ws11"); // newest pin = slot 1 = index 0
        assert_eq!(slots[9], "ws2");
        assert!(!slots.contains(&"unpinned".to_string()));
        assert!(!slots.contains(&"archiving".to_string()));
    }

    #[test]
    fn workspace_store_upsert_and_remove() {
        let mut store = WorkspaceStore::new();
        let now = Instant::now();
        store.apply_upsert(ws("a", "2026-08-09T10:00:00Z", "running"), now);
        assert_eq!(store.slot_ids(), vec!["a".to_string()]);
        store.apply_remove("a");
        assert!(store.slot_ids().is_empty());
    }

    // --- Frame composition ---

    #[test]
    fn compute_status_frame_renders_slots_glow_and_actions() {
        let mut store = WorkspaceStore::new();
        let now = Instant::now();
        // "a" pinned after "b" -> newest pin first: a = slot 0, b = slot 1.
        store.apply_upsert(ws("a", "2026-08-09T10:00:00Z", "running"), now);
        store.apply_upsert(ws("b", "2026-08-08T10:00:00Z", "needs_input"), now);

        let f = compute_status_frame(&store, true, now);
        assert_eq!(f[0], COLOR_RUNNING);
        assert_eq!(f[1], COLOR_NEEDS_INPUT);
        assert_eq!(f[2], COLOR_OFF);
        // permission glow on: some slotted workspace needs input
        assert_eq!(f[IDX_Y as usize], COLOR_PERMISSION_Y);
        assert_eq!(f[IDX_U as usize], COLOR_PERMISSION_U);
        // foreground is paseo: action indicators lit, F dim (nothing to do)
        assert_eq!(f[IDX_J as usize], COLOR_COMMIT);
        assert_eq!(f[IDX_K as usize], COLOR_PUSH);
        assert_eq!(f[IDX_L as usize], COLOR_PR);
        assert_eq!(f[IDX_SEMI as usize], COLOR_MERGE);
        assert_eq!(f[IDX_F as usize], COLOR_FOCUS_DIM);

        // not foreground: J/K/L/; off, F bright (call to action)
        let f2 = compute_status_frame(&store, false, now);
        assert_eq!(f2[IDX_J as usize], COLOR_OFF);
        assert_eq!(f2[IDX_F as usize], COLOR_FOCUS_CTA);
    }

    #[test]
    fn compute_status_frame_applies_blink_override() {
        let mut store = WorkspaceStore::new();
        let t0 = Instant::now();
        // Establish prior status "running" first (the `now` passed here is
        // irrelevant: no transition into attention happens on this call).
        store.apply_upsert(ws("a", "2026-08-09T10:00:00Z", "running"), t0);
        // Now transition into attention at t0: this is the one that blinks.
        store.apply_upsert(ws("a", "2026-08-09T10:00:00Z", "attention"), t0);

        let f_on = compute_status_frame(&store, false, t0);
        assert_eq!(f_on[0], COLOR_ATTENTION);
        let f_off = compute_status_frame(&store, false, t0 + Duration::from_millis(400));
        assert_eq!(f_off[0], COLOR_OFF);
        let f_done = compute_status_frame(&store, false, t0 + Duration::from_secs(5));
        assert_eq!(f_done[0], COLOR_ATTENTION);
    }

    #[test]
    fn workspace_appearing_already_attention_does_not_blink() {
        // A workspace newly upserted straight into "attention" (no prior
        // status on record) must render solid, not blink.
        let mut store = WorkspaceStore::new();
        let t0 = Instant::now();
        store.apply_upsert(ws("a", "2026-08-09T10:00:00Z", "attention"), t0);
        assert!(store.blink_started.get("a").is_none());
        assert_eq!(compute_status_frame(&store, false, t0)[0], COLOR_ATTENTION);
        assert_eq!(compute_status_frame(&store, false, t0 + Duration::from_millis(400))[0], COLOR_ATTENTION);
    }

    #[test]
    fn permission_glow_condition() {
        let mut store = WorkspaceStore::new();
        let now = Instant::now();
        assert!(!permission_glow(&store));
        store.apply_upsert(ws("a", "2026-08-09T10:00:00Z", "needs_input"), now);
        assert!(permission_glow(&store));

        // An unpinned (unslotted) workspace with needs_input must NOT glow.
        let mut store2 = WorkspaceStore::new();
        let mut unpinned = ws("b", "2026-08-09T10:00:00Z", "needs_input");
        unpinned.pinned_at = None;
        store2.apply_upsert(unpinned, now);
        assert!(!permission_glow(&store2));
    }

    // --- Usage bar ---

    #[test]
    fn segments_lit_thresholds() {
        assert_eq!(segments_lit(0.0), 0);
        assert_eq!(segments_lit(1.0), 1); // minimum 1 segment if > 0
        assert_eq!(segments_lit(9.9), 1);
        assert_eq!(segments_lit(10.0), 1);
        assert_eq!(segments_lit(10.1), 1);
        assert_eq!(segments_lit(62.0), 6);
        assert_eq!(segments_lit(100.0), 10);
    }

    #[test]
    fn segment_colors_by_band() {
        assert_eq!(segment_color(1), COLOR_ATTENTION);
        assert_eq!(segment_color(5), COLOR_ATTENTION);
        assert_eq!(segment_color(6), COLOR_NEEDS_INPUT);
        assert_eq!(segment_color(8), COLOR_NEEDS_INPUT);
        assert_eq!(segment_color(9), COLOR_FAILED);
        assert_eq!(segment_color(10), COLOR_FAILED);
    }

    #[test]
    fn usage_bar_frame_lights_number_row_and_leaves_rest_off() {
        let f = usage_bar_frame(62.0);
        // segments 1-5 (idx 0-4) green, segment 6 (idx 5) yellow, rest off.
        for i in 0..5 {
            assert_eq!(f[i], COLOR_ATTENTION);
        }
        assert_eq!(f[5], COLOR_NEEDS_INPUT);
        for i in 6..10 {
            assert_eq!(f[i], COLOR_OFF);
        }
        for i in 10..17 {
            assert_eq!(f[i], COLOR_OFF);
        }
    }

    // --- Usage parsing / ordering ---

    fn sample_usage_payload() -> serde_json::Value {
        json!({
            "providers": [
                {
                    "providerId": "claude",
                    "displayName": "Claude",
                    "status": "ok",
                    "windows": [
                        {"id": "five_hour", "label": "5h", "usedPct": 62.0, "remainingPct": 38.0, "resetsAt": "2026-08-10T18:00:00Z", "tone": "warn"},
                        {"id": "weekly", "label": "Weekly", "usedPct": 30.0, "remainingPct": 70.0, "resetsAt": "2026-08-15T00:00:00Z", "tone": "ok"}
                    ]
                },
                {
                    "providerId": "codex",
                    "displayName": "Codex",
                    "status": "ok",
                    "windows": [
                        {"id": "weekly", "label": "Weekly", "usedPct": 91.0, "remainingPct": 9.0, "resetsAt": "2026-08-16T00:00:00Z", "tone": "danger"}
                    ]
                }
            ]
        })
    }

    #[test]
    fn usage_ordering_and_skip_absent() {
        // sample_usage_payload's codex has no "session" window -- confirms
        // an absent window in the middle of the fixed order is skipped,
        // not padded, and doesn't disturb what comes after it.
        let providers = parse_usage_providers(&sample_usage_payload());
        let entries = ordered_usage_entries(&providers);
        assert_eq!(entries.len(), 3);
        assert_eq!((entries[0].provider_key.as_str(), entries[0].window_key.as_str()), ("claude", "five_hour"));
        assert_eq!(entries[0].used_pct, 62.0);
        assert_eq!((entries[1].provider_key.as_str(), entries[1].window_key.as_str()), ("claude", "weekly"));
        assert_eq!((entries[2].provider_key.as_str(), entries[2].window_key.as_str()), ("codex", "weekly"));

        // absent provider is skipped, not padded
        let partial = json!({"providers": [ {"providerId":"codex","displayName":"Codex","status":"ok","windows":[{"id":"weekly","label":"Weekly","usedPct":5.0,"remainingPct":95.0,"resetsAt":"x","tone":"ok"}]} ]});
        let providers2 = parse_usage_providers(&partial);
        let entries2 = ordered_usage_entries(&providers2);
        assert_eq!(entries2.len(), 1);
        assert_eq!(entries2[0].provider_key, "codex");
    }

    #[test]
    fn usage_ordering_prefers_codex_session_and_appends_weekly_model_windows() {
        let payload = json!({
            "providers": [
                {
                    "providerId": "claude",
                    "displayName": "Claude",
                    "status": "ok",
                    "windows": [
                        {"id": "five_hour", "label": "5h", "usedPct": 62.0, "remainingPct": 38.0, "resetsAt": "r1", "tone": "warn"},
                        {"id": "weekly", "label": "Weekly", "usedPct": 30.0, "remainingPct": 70.0, "resetsAt": "r2", "tone": "ok"},
                        {"id": "weekly_model_fable", "label": "Fable Weekly", "usedPct": 10.0, "remainingPct": 90.0, "resetsAt": "r3", "tone": "ok"}
                    ]
                },
                {
                    "providerId": "codex",
                    "displayName": "Codex",
                    "status": "ok",
                    "windows": [
                        {"id": "session", "label": "Session", "usedPct": 45.0, "remainingPct": 55.0, "resetsAt": "r4", "tone": "ok"},
                        {"id": "weekly", "label": "Weekly", "usedPct": 91.0, "remainingPct": 9.0, "resetsAt": "r5", "tone": "danger"}
                    ]
                }
            ]
        });
        let providers = parse_usage_providers(&payload);
        let entries = ordered_usage_entries(&providers);
        let keys: Vec<(&str, &str)> = entries.iter().map(|e| (e.provider_key.as_str(), e.window_key.as_str())).collect();
        assert_eq!(
            keys,
            vec![
                ("claude", "five_hour"),
                ("claude", "weekly"),
                ("codex", "session"),
                ("codex", "weekly"),
                ("claude", "weekly_model_fable"),
            ]
        );
    }

    #[test]
    fn usage_log_format() {
        let providers = parse_usage_providers(&sample_usage_payload());
        let entries = ordered_usage_entries(&providers);
        assert_eq!(format_usage_log(&entries[0]), "usage: claude five_hour 62% (resets 2026-08-10T18:00:00Z)");
    }

    // --- 90% crossing ---

    #[test]
    fn crossing_detection_upward_only() {
        assert!(!crossed_90_upward(None, 95.0)); // first poll: seed silently
        assert!(crossed_90_upward(Some(85.0), 92.0));
        assert!(!crossed_90_upward(Some(92.0), 95.0)); // already >=90, not a new crossing
        assert!(!crossed_90_upward(Some(85.0), 88.0)); // didn't cross
        assert!(!crossed_90_upward(Some(95.0), 80.0)); // downward, ignored
    }

    #[test]
    fn detect_crossings_multi_entry() {
        let mut baseline = HashMap::new();
        baseline.insert(("claude".to_string(), "five_hour".to_string()), 85.0);
        baseline.insert(("codex".to_string(), "weekly".to_string()), 50.0);
        let entries = vec![
            UsageEntry { provider_key: "claude".into(), window_key: "five_hour".into(), display_name: "Claude".into(), used_pct: 91.0, resets_at: "x".into() },
            UsageEntry { provider_key: "codex".into(), window_key: "weekly".into(), display_name: "Codex".into(), used_pct: 55.0, resets_at: "x".into() },
        ];
        assert_eq!(detect_crossings(&baseline, &entries), vec![0]);
    }

    // --- Frame diff / FrameSender ---

    #[test]
    fn frame_diff_full_vs_incremental() {
        let mut a = off_frame();
        a[0] = (1, 2, 3);
        assert_eq!(frame_diff(None, &a).len(), 17);
        let mut b = a;
        b[5] = (9, 9, 9);
        let d = frame_diff(Some(&a), &b);
        assert_eq!(d, vec![(5, 9, 9, 9)]);
        assert!(frame_diff(Some(&a), &a).is_empty());
    }

    #[test]
    fn frame_sender_debounces_and_dedupes() {
        let mut fs = FrameSender::new();
        let t0 = Instant::now();
        let mut f = off_frame();
        f[0] = (1, 1, 1);
        // First update: nothing sent before debounce elapses... but repush_due
        // is true on the very first call (last_sent_at is None), so it sends
        // immediately as a full frame.
        let sent = fs.update(f, t0).expect("first update sends full frame");
        assert_eq!(sent.len(), 17);

        // No further change, well within repush interval: nothing to send.
        assert!(fs.update(f, t0 + Duration::from_millis(100)).is_none());

        // Change the frame: should not send before debounce elapses.
        let mut f2 = f;
        f2[1] = (2, 2, 2);
        assert!(fs.update(f2, t0 + Duration::from_millis(110)).is_none());
        // After debounce, sends just the changed pixel.
        let sent2 = fs.update(f2, t0 + Duration::from_millis(180)).expect("debounced send");
        assert_eq!(sent2, vec![(1, 2, 2, 2)]);

        // 30s later with no change: forced repush of the full frame.
        let sent3 = fs.update(f2, t0 + Duration::from_secs(31)).expect("repush");
        assert_eq!(sent3.len(), 17);
    }

    #[test]
    fn frame_sender_reset_forces_full_resend() {
        let mut fs = FrameSender::new();
        let t0 = Instant::now();
        let f = off_frame();
        fs.update(f, t0);
        fs.reset();
        let sent = fs.update(f, t0 + Duration::from_millis(1)).expect("full resend after reset");
        assert_eq!(sent.len(), 17);
    }

    // --- Foreground process match ---

    #[test]
    fn foreground_match_is_case_insensitive_suffix() {
        assert!(is_paseo_foreground(r"C:\Users\me\AppData\Local\Programs\Paseo\Paseo.exe"));
        assert!(is_paseo_foreground(r"C:\paseo\PASEO.EXE"));
        assert!(!is_paseo_foreground(r"C:\Windows\explorer.exe"));
        assert!(!is_paseo_foreground(r"C:\NotPaseo.exe.bak"));
    }

    // --- WS message parsing: fixtures matching the real Paseo protocol ---

    /// Session-wrapped `fetch_workspaces_response` with 3 workspaces: one
    /// pinned (goes to a slot), one unpinned (excluded), one pinned-but-
    /// archiving (excluded).
    fn fetch_workspaces_response_fixture() -> serde_json::Value {
        json!({
            "type": "session",
            "message": {
                "type": "fetch_workspaces_response",
                "payload": {
                    "requestId": "fetch-1",
                    "entries": [
                        {"id": "ws-pinned", "name": "Pinned One", "pinnedAt": "2026-08-09T10:00:00Z", "archivingAt": null, "status": "running"},
                        {"id": "ws-unpinned", "name": "Unpinned", "pinnedAt": null, "archivingAt": null, "status": "done"},
                        {"id": "ws-archiving", "name": "Archiving", "pinnedAt": "2026-08-09T09:00:00Z", "archivingAt": "2026-08-09T12:00:00Z", "status": "failed"}
                    ],
                    "pageInfo": {"hasMore": false}
                }
            }
        })
    }

    fn workspace_update_upsert_fixture(id: &str, pinned_at: &str, status: &str) -> serde_json::Value {
        json!({
            "type": "session",
            "message": {
                "type": "workspace_update",
                "payload": {"kind": "upsert", "workspace": {"id": id, "name": "X", "pinnedAt": pinned_at, "archivingAt": null, "status": status}}
            }
        })
    }

    fn workspace_update_remove_fixture(id: &str) -> serde_json::Value {
        json!({
            "type": "session",
            "message": {"type": "workspace_update", "payload": {"kind": "remove", "id": id}}
        })
    }

    #[test]
    fn parse_and_apply_fetch_workspaces_response_snapshot() {
        let events = parse_ws_value(&fetch_workspaces_response_fixture());
        assert_eq!(events.len(), 1);
        let WsEvent::WorkspacesSnapshot { entries, has_more } = &events[0] else {
            panic!("expected WorkspacesSnapshot, got {:?}", events[0]);
        };
        assert!(!has_more);
        assert_eq!(entries.len(), 3);

        // Slot derivation: only the pinned, non-archiving workspace is slotted.
        let mut store = WorkspaceStore::new();
        store.apply_snapshot(entries.clone());
        assert_eq!(store.slot_ids(), vec!["ws-pinned".to_string()]);
    }

    #[test]
    fn parse_workspace_update_upsert_and_remove() {
        let up = parse_ws_value(&workspace_update_upsert_fixture("ws-1", "2026-08-09T10:00:00Z", "needs_input"));
        assert_eq!(up.len(), 1);
        match &up[0] {
            WsEvent::WorkspaceUpsert(w) => {
                assert_eq!(w.id, "ws-1");
                assert_eq!(w.status, "needs_input");
                assert_eq!(w.pinned_at.as_deref(), Some("2026-08-09T10:00:00Z"));
            }
            other => panic!("expected WorkspaceUpsert, got {other:?}"),
        }

        let rm = parse_ws_value(&workspace_update_remove_fixture("ws-1"));
        assert_eq!(rm, vec![WsEvent::WorkspaceRemove("ws-1".to_string())]);
    }

    #[test]
    fn parse_ws_usage_response_session_wrapped() {
        let msg = json!({
            "type": "session",
            "message": {
                "type": "provider.usage.list.response",
                "requestId": "req-1",
                "payload": sample_usage_payload()
            }
        });
        let events = parse_ws_value(&msg);
        assert_eq!(events.len(), 1);
        match &events[0] {
            WsEvent::UsageResponse { request_id, providers } => {
                assert_eq!(request_id, "req-1");
                assert_eq!(providers.len(), 2);
            }
            other => panic!("expected UsageResponse, got {other:?}"),
        }
    }

    #[test]
    fn top_level_pong_and_malformed_messages_carry_no_events() {
        assert!(parse_ws_text("not json").is_empty());
        assert!(parse_ws_value(&json!({"type": "pong"})).is_empty()); // top-level, not session-wrapped
        assert!(parse_ws_value(&json!({"type": "session"})).is_empty()); // no "message"
        assert!(parse_ws_value(&json!({"type": "session", "message": {"type": "some_unknown_inner"}})).is_empty());
    }

    #[test]
    fn parse_ws_rpc_error() {
        // The exact shape a bare (non-session-wrapped) outbound request
        // used to provoke, before session_wrap fixed it.
        let msg = json!({
            "type": "session",
            "message": {
                "type": "rpc_error",
                "payload": {
                    "requestId": "fetch-1",
                    "requestType": "fetch_workspaces_request",
                    "error": "Invalid message",
                    "code": "invalid_message"
                }
            }
        });
        let events = parse_ws_value(&msg);
        assert_eq!(
            events,
            vec![WsEvent::RpcError {
                request_type: "fetch_workspaces_request".to_string(),
                error: "Invalid message".to_string(),
            }]
        );
    }

    #[test]
    fn status_transition_drives_blink_end_to_end() {
        let mut store = WorkspaceStore::new();
        let t0 = Instant::now();

        // Snapshot arrives with the workspace already at "attention" --
        // must NOT blink (no prior status on record).
        let WsEvent::WorkspacesSnapshot { mut entries, .. } = parse_ws_value(&fetch_workspaces_response_fixture()).remove(0) else {
            panic!("expected snapshot");
        };
        entries[0].status = "attention".to_string(); // ws-pinned
        store.apply_snapshot(entries);
        assert!(store.blink_started.get("ws-pinned").is_none());
        assert_eq!(compute_status_frame(&store, false, t0)[0], COLOR_ATTENTION);

        // A workspace_update transitions it away, then back into attention:
        // that second transition is a real one and starts a blink.
        let WsEvent::WorkspaceUpsert(running) = parse_ws_value(&workspace_update_upsert_fixture(
            "ws-pinned", "2026-08-09T10:00:00Z", "running",
        )).remove(0) else { panic!("expected upsert") };
        store.apply_upsert(running, t0);
        assert!(store.blink_started.get("ws-pinned").is_none());

        let WsEvent::WorkspaceUpsert(attention) = parse_ws_value(&workspace_update_upsert_fixture(
            "ws-pinned", "2026-08-09T10:00:00Z", "attention",
        )).remove(0) else { panic!("expected upsert") };
        store.apply_upsert(attention, t0);
        assert_eq!(store.blink_started.get("ws-pinned"), Some(&t0));
        assert_eq!(compute_status_frame(&store, false, t0)[0], COLOR_ATTENTION);
        assert_eq!(compute_status_frame(&store, false, t0 + Duration::from_millis(400))[0], COLOR_OFF);
    }

    #[test]
    fn outgoing_handshake_messages_match_protocol() {
        // hello is top-level -- NOT session-wrapped.
        let hello = hello_message();
        assert_eq!(hello["type"], "hello");
        assert_eq!(hello["clientId"], "paseo-led-bridge");
        assert_eq!(hello["clientType"], "cli");
        assert_eq!(hello["protocolVersion"], 1);
        assert!(hello["appVersion"].is_string());

        // Every other outbound request is session-wrapped, or the server
        // silently rpc_error's it.
        let req = fetch_workspaces_request_message("abc");
        assert_eq!(req["type"], "session");
        let inner = &req["message"];
        assert_eq!(inner["type"], "fetch_workspaces_request");
        assert_eq!(inner["requestId"], "abc");
        assert_eq!(inner["sort"][0]["key"], "activity_at");
        assert_eq!(inner["sort"][0]["direction"], "desc");
        assert_eq!(inner["page"]["limit"], 200);
        assert!(inner["subscribe"].is_object());

        let usage_req = usage_list_request_message("xyz");
        assert_eq!(usage_req["type"], "session");
        assert_eq!(usage_req["message"]["type"], "provider.usage.list.request");
        assert_eq!(usage_req["message"]["requestId"], "xyz");
    }

    #[test]
    fn request_id_uniqueness() {
        let a = next_request_id("x");
        let b = next_request_id("x");
        assert_ne!(a, b);
        assert!(a.starts_with("x-"));
    }
}

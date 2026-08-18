# Napkin Runbook

## Curation Rules
- Re-prioritize on every read.
- Keep recurring, high-value notes only.
- Max 10 items per category.
- Each item includes date + "Do instead".

## Execution & Validation (Highest Priority)

## Shell & Command Reliability

## Domain Behavior Guardrails

1. **[2026-08-17] Web-app reinstalls change the Chromium window class**
   Do instead: derive bb's launch URL and `chrome-<host>__-Default` class from the installed `BB.desktop` entry; do not hardcode the loopback class.

2. **[2026-08-17] Status Sidebar ordering is persisted by the plugin**
   Do instead: query both `listLater` and `listThreadOrder`, then apply the plugin's `Pinned`, `Unread`, `Active`, `Needs input`, `Idle`, `Later`, `Archived` section model; do not infer row order from timestamps alone.

## User Directives

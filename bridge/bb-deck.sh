#!/usr/bin/env bash
# Go60 deck actions for bb on Omarchy/Hyprland.
set -euo pipefail

BB_URL="${BB_URL:-http://127.0.0.1:38886}"
STATE_FILE="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/bb-deck-active-thread"

find_bb() {
    if command -v bb >/dev/null 2>&1; then
        command -v bb
    elif [[ -x "$HOME/.local/opt/bb-app/node_modules/bb-app/host-daemon/dist/bb" ]]; then
        printf '%s\n' "$HOME/.local/opt/bb-app/node_modules/bb-app/host-daemon/dist/bb"
    else
        echo "bb CLI not found" >&2
        return 1
    fi
}

focus_bb() {
    # The PWA title changes to the selected thread, so match its stable
    # Chromium app class rather than the transient title "bb".
    omarchy-launch-or-focus "chrome-127.0.0.1__-Default" "omarchy-launch-webapp '$BB_URL/'"
}

wait_for_bb_focus() {
    local active_window title class
    for _ in {1..40}; do
        active_window=$(hyprctl activewindow -j 2>/dev/null || true)
        title=$(jq -r '.title // ""' <<<"$active_window" 2>/dev/null || true)
        class=$(jq -r '.class // ""' <<<"$active_window" 2>/dev/null || true)
        if [[ "${title,,}" == "bb" || "$class" == chrome-127.0.0.1__-* ]]; then
            return 0
        fi
        sleep 0.05
    done
    return 1
}

bb_is_focused() {
    local active_window class initial_class
    active_window=$(hyprctl activewindow -j 2>/dev/null) || return 1
    class=$(jq -r '.class // ""' <<<"$active_window" 2>/dev/null) || return 1
    initial_class=$(jq -r '.initialClass // ""' <<<"$active_window" 2>/dev/null) || return 1
    [[ "$class" == chrome-127.0.0.1__-* || "$initial_class" == chrome-127.0.0.1__-* ]]
}

status_sidebar_slots() {
    local navigation later_rpc
    navigation=$(curl -fsS "$BB_URL/api/v1/sidebar-bootstrap")
    if ! later_rpc=$(curl -fsS \
        -H 'content-type: application/json' \
        -d 'null' \
        "$BB_URL/api/v1/plugins/status-sidebar/rpc/listLater"); then
        later_rpc='{"result":{"rows":[]}}'
    fi

    jq -cn --argjson navigation "$navigation" --argjson laterRpc "$later_rpc" '
        def unread:
            (.lastReadAt // 0) < (.latestAttentionAt // 0);
        def unread_done:
            .parentThreadId == null
            and (.status == "idle" or .status == "error")
            and unread;
        def active:
            ([
                .activity.activeWorkflowCount,
                .activity.activeBackgroundAgentCount,
                .activity.activeBackgroundCommandCount,
                .activity.activePlanModeCount,
                .activity.activeGoalCount
            ] | map(. // 0) | add) > 0
            or (.runtime.displayStatus as $status
                | ["active", "host-reconnecting", "provisioning", "starting", "stopping"]
                | index($status) != null);
        def bucket($later):
            if .archivedAt != null then 4
            elif .hasPendingInteraction or (.status == "error" and unread_done) then 1
            elif active then 0
            elif $later[.id] != null then 3
            else 2
            end;
        def bucket_name($bucket):
            ["active", "needs-input", "idle", "later", "archived"][$bucket];
        def color:
            if .hasPendingInteraction then "question"
            elif .status == "error" and unread_done then "error"
            elif active then "working"
            elif unread_done then "unread"
            else "idle"
            end;
        def sorted_bucket($bucket; $later):
            map(select(bucket($later) == $bucket))
            | sort_by(
                (if .pinnedAt != null then 0 else 1 end),
                (if $bucket == 3 then -($later[.id] // 0)
                 elif $bucket == 1 then -(.latestAttentionAt // 0)
                 else -(.updatedAt // 0)
                 end),
                ._source
            );
        def environment_grouped:
            reduce .[] as $thread ({ keys: [], rows: {} };
                ($thread.environmentId // null) as $environment
                | (if $environment == null
                   then "thread:\($thread.id)"
                   else "\($thread.projectId):\($environment)"
                   end) as $key
                | if .rows[$key] == null
                  then .keys += [$key] | .rows[$key] = [$thread]
                  else .rows[$key] += [$thread]
                  end
            )
            | [.keys[] as $key | .rows[$key][]];

        (reduce ($laterRpc.result.rows // [])[] as $row ({};
            .[$row.threadId] = $row.placedAt
        )) as $later
        | ((($navigation.projects // []) + [$navigation.personalProject])
            | map(.threads // []) | add // []) as $source
        | [range(0; $source | length) as $index
            | $source[$index] + { _source: $index }] as $rows
        | [range(0; 5) as $bucket
            | ($rows | sorted_bucket($bucket; $later) | environment_grouped)[]
            | . + {
                bucket: bucket_name($bucket),
                color: color
              }]
        | .[:10]
        | to_entries
        | map(.value + { slot: (.key + 1) })
    '
}

thread_for_slot() {
    local slot="$1"
    status_sidebar_slots | jq -r --argjson slot "$slot" \
        '.[] | select(.slot == $slot) | .id'
}

active_thread() {
    if [[ -s "$STATE_FILE" ]]; then
        head -n1 "$STATE_FILE"
        return
    fi
    status_sidebar_slots | jq -r \
        '(map(select(.bucket == "needs-input"))[0] // .[0]).id // empty'
}

notify() {
    notify-send -u low "BB Deck" "$1" >/dev/null 2>&1 || true
}

signal_open_thread() {
    local thread_id="$1" response delivered

    response=$(curl -fsS \
        -H 'content-type: application/json' \
        -d '{"file":null}' \
        "$BB_URL/api/v1/threads/$thread_id/open") || response=''
    delivered=$(jq -r '.delivered // 0' <<<"$response" 2>/dev/null || printf '0')
    (( delivered > 0 ))
}

open_thread() {
    local thread_id="$1"

    # When bb is already active, navigate in place. Otherwise focus its window
    # first, then navigate; this avoids two competing route/focus transitions.
    if ! bb_is_focused; then
        focus_bb
        wait_for_bb_focus || true
    fi

    for _ in {1..20}; do
        if signal_open_thread "$thread_id"; then
            return 0
        fi
        sleep 0.1
    done
    notify "BB opened, but the thread jump was not delivered"
    return 1
}

open_slot() {
    local slot="$1" thread_id
    [[ "$slot" =~ ^([1-9]|10)$ ]] || { echo "invalid slot: $slot" >&2; exit 2; }
    thread_id=$(thread_for_slot "$slot")
    if [[ -z "$thread_id" ]]; then
        focus_bb
        notify "No status-sidebar thread in slot $slot"
        exit 1
    fi
    open_thread "$thread_id"
    printf '%s\n' "$thread_id" >"$STATE_FILE"
}

send_action() {
    local verb="$1" prompt thread_id bb_cli
    case "$verb" in
        commit) prompt="Commit the current changes with a clear, conventional message." ;;
        push) prompt="Push the current branch to its remote." ;;
        pr) prompt="Open a pull request for the current branch." ;;
        merge) prompt="Merge the open pull request once its checks pass." ;;
        *) echo "unknown action: $verb" >&2; exit 2 ;;
    esac
    thread_id=$(active_thread)
    [[ -n "$thread_id" ]] || { notify "Press a pinned-thread slot first"; exit 1; }
    bb_cli=$(find_bb)
    "$bb_cli" thread tell "$thread_id" "$prompt" --mode queue >/dev/null
    notify "Queued $verb"
}

focus_composer() {
    focus_bb
    wait_for_bb_focus || { notify "Could not focus bb"; exit 1; }
    wtype -M ctrl -M shift -k c -m shift -m ctrl
}

case "${1:-}" in
    slot) open_slot "${2:?slot required}" ;;
    slots) status_sidebar_slots | jq -r '.[] | [.slot, .color, .bucket, .title] | @tsv' ;;
    focus) focus_bb ;;
    composer) focus_composer ;;
    action) send_action "${2:?action required}" ;;
    *) echo "usage: bb-deck.sh {slot 1..10|slots|focus|composer|action commit|push|pr|merge}" >&2; exit 2 ;;
esac

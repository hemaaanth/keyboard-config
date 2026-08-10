#!/usr/bin/env bash
# Paseo Deck helper — bridges Go60 F13-F24 hotkeys to the paseo CLI.
# Subcommands: resolve <slot> | approve [slot] | deny [slot] | action <slot> <verb>
set -euo pipefail

LOCK=/tmp/paseo-deck.lock
exec 200>"$LOCK"
flock -n 200 || { echo "busy"; exit 1; }

PASEO=/home/system/.local/share/mise/shims/paseo
JQ=/home/system/.local/share/mise/shims/jq
[ -x "$JQ" ] || JQ=$(command -v jq || echo /home/system/.local/share/mise/installs/jq/1.8.2/jq)

# resolve_agent <slot> -> prints "agentId<TAB>workspaceName" on success (rc 0),
# or a one-line error message (rc 1). Agents come back from `agent ls -g --json`
# already sorted running > idle > others, most recent first, so the first cwd
# match is already the right tie-break winner.
resolve_agent() {
    local slot="$1"
    [[ "$slot" =~ ^([1-9]|10)$ ]] || { echo "invalid slot: $slot"; return 1; }
    local ws wcwd wname agent_id
    ws=$("$PASEO" workspace ls --json | "$JQ" -c --argjson i "$((slot - 1))" '.[$i] // empty')
    [ -n "$ws" ] || { echo "no workspace at slot $slot"; return 1; }
    wcwd=$(echo "$ws" | "$JQ" -r '.cwd')
    wname=$(echo "$ws" | "$JQ" -r '.name')
    agent_id=$("$PASEO" agent ls -g --json | "$JQ" -r --arg home "$HOME" --arg cwd "$wcwd" '
        map(select((.cwd | if startswith("~") then $home + .[1:] else . end) == $cwd))
        | .[0].id // empty')
    [ -n "$agent_id" ] || { echo "no agent found for workspace '$wname'"; return 1; }
    printf '%s\t%s\n' "$agent_id" "$wname"
}

cmd_resolve() {
    local slot="${1:?slot required}" out
    out=$(resolve_agent "$slot") || { echo "$out"; exit 1; }
    printf '%s %s\n' "$(cut -f1 <<<"$out")" "$(cut -f2 <<<"$out")"
}

# approve|deny [slot] — target selection per spec, then paseo permit allow|deny.
cmd_permit() {
    local action="$1" slot="${2:-}" psub verb
    if [ "$action" = "approve" ]; then psub=allow; verb=approved; else psub=deny; verb=denied; fi
    local permits count
    permits=$("$PASEO" permit ls --json)
    count=$(echo "$permits" | "$JQ" 'length')
    [ "$count" -gt 0 ] || { echo "no pending permissions"; exit 1; }
    local target_agent="" target_req=""
    if [ -n "$slot" ]; then
        local out sid match
        if out=$(resolve_agent "$slot"); then
            sid=$(cut -f1 <<<"$out")
            match=$(echo "$permits" | "$JQ" -c --arg a "$sid" '[.[] | select(.agentId==$a)][0] // empty')
            if [ -n "$match" ]; then
                target_agent="$sid"
                target_req=$(echo "$match" | "$JQ" -r '.id')
            fi
        fi
    fi

    if [ -z "$target_agent" ] && [ "$count" -eq 1 ]; then
        target_agent=$(echo "$permits" | "$JQ" -r '.[0].agentId')
        target_req=$(echo "$permits" | "$JQ" -r '.[0].id')
    fi
    if [ -z "$target_agent" ] && [ "$count" -gt 1 ]; then
        # Multiple pending: pick the most recent running agent (agent ls is
        # pre-sorted running > idle > others, most recent first).
        local ids winner
        ids=$(echo "$permits" | "$JQ" -c '[.[].agentId] | unique')
        winner=$("$PASEO" agent ls -g --json | "$JQ" -r --argjson ids "$ids" '
            map(select(.id as $i | $ids | index($i))) | .[0].id // empty')
        if [ -n "$winner" ]; then
            target_agent="$winner"
            target_req=$(echo "$permits" | "$JQ" -r --arg a "$winner" '[.[] | select(.agentId==$a)][0].id')
        fi
    fi

    [ -n "$target_agent" ] && [ -n "$target_req" ] || {
        echo "multiple pending permissions — press an agent key first"; exit 1
    }
    local info name label
    info=$(echo "$permits" | "$JQ" -r --arg r "$target_req" '[.[] | select(.id==$r)][0] | "\(.name)\t\(.agentShortId)"')
    name=$(cut -f1 <<<"$info")
    label=$(cut -f2 <<<"$info")
    "$PASEO" permit "$psub" "$target_agent" "$target_req" >/dev/null
    echo "${verb}: $name ($label)"
}

cmd_action() {
    local slot="${1:?slot required}" verb="${2:?verb required}" prompt
    case "$verb" in
        commit) prompt="Commit the current changes with a clear, conventional message." ;;
        push) prompt="Push the current branch to the remote." ;;
        pr) prompt="Open a pull request for the current branch." ;;
        merge) prompt="Merge the open pull request once checks pass." ;;
        *) echo "unknown action: $verb"; exit 1 ;;
    esac
    local out agent_id
    out=$(resolve_agent "$slot") || { echo "$out"; exit 1; }
    agent_id=$(cut -f1 <<<"$out")
    "$PASEO" agent send "$agent_id" --no-wait --prompt "$prompt" >/dev/null
    echo "sent $verb to $agent_id"
}

main() {
    local sub="${1:-}"
    [ $# -gt 0 ] && shift
    case "$sub" in
        resolve) cmd_resolve "$@" ;;
        approve) cmd_permit approve "$@" ;;
        deny) cmd_permit deny "$@" ;;
        action) cmd_action "$@" ;;
        *) echo "usage: paseo-deck.sh {resolve <slot>|approve [slot]|deny [slot]|action <slot> <verb>}"; exit 1 ;;
    esac
}

main "$@"

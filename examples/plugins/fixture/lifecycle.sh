#!/bin/sh
set -eu
export CLAUDE_CONFIG_DIR=/home/theseus/plugin-config
export DISABLE_AUTOUPDATER=1
export CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1
export CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1
export LC_ALL=C
client=/inputs/target/client/claude
action=$1
scope=$2
case "$scope" in
    user) settings=$CLAUDE_CONFIG_DIR/settings.json ;;
    project) settings=/workspace/plugin-project/.claude/settings.json ;;
    local) settings=/workspace/plugin-project/.claude/settings.local.json ;;
    *) exit 64 ;;
esac
if test "$action" = install_base; then
    mkdir -p "$HOME" "$CLAUDE_CONFIG_DIR" /workspace/plugin-project/.claude /workspace/local-market
fi
cd /workspace/plugin-project
case "$action" in
    install_base)
        seed='{"env":{"THESEUS_UNRELATED":"keep-me","THESEUS_SECOND":"still-here"},"enabledPlugins":{}}'
        printf '%s\n' "$seed" > "$CLAUDE_CONFIG_DIR/settings.json"
        printf '%s\n' "$seed" > "$settings"
        cp -R /inputs/target/market-v1/. /workspace/local-market/
        chmod -R u+w /workspace/local-market
        "$client" plugin marketplace add /workspace/local-market --json
        "$client" plugin install theseus-companion@theseus-probe-market --scope user --json
        "$client" plugin install theseus-probe@theseus-probe-market --scope "$scope" --json
        ;;
    upgrade)
        cp -R /inputs/target/market-v2/. /workspace/local-market/
        "$client" plugin marketplace update theseus-probe-market --json
        "$client" plugin update theseus-probe@theseus-probe-market --scope "$scope" --json
        ;;
    disable_plugin) "$client" plugin disable theseus-probe@theseus-probe-market --scope "$scope" --json ;;
    enable_plugin) "$client" plugin enable theseus-probe@theseus-probe-market --scope "$scope" --json ;;
    uninstall) "$client" plugin uninstall theseus-probe@theseus-probe-market --scope "$scope" --json ;;
    inventory) "$client" plugin list --json ;;
    user-settings) cat "$CLAUDE_CONFIG_DIR/settings.json" ;;
    scope-settings) cat "$settings" ;;
    installed-records) cat "$CLAUDE_CONFIG_DIR/plugins/installed_plugins.json" ;;
    *) exit 64 ;;
esac

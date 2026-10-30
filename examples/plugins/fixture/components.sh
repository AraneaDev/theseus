#!/bin/sh
set -eu
export CLAUDE_CONFIG_DIR=/home/theseus/plugin-config
export DISABLE_AUTOUPDATER=1
export CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1
export CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1
export LC_ALL=C
cmp /inputs/target/client/claude /home/theseus/.local/bin/theseus-client
cd /workspace/plugin-project
version=$1
case "$version" in 1.0.0) generation=v1 ;; 2.0.0) generation=v2 ;; *) exit 64 ;; esac
for name in theseus-probe theseus-companion; do
    actual=$version
    if test "$name" = theseus-companion; then actual=1.0.0; fi
    source=/workspace/local-market/plugins/$name
    cache=$CLAUDE_CONFIG_DIR/plugins/cache/theseus-probe-market/$name/$actual
    reference=/inputs/target/market-$generation/plugins/$name
    for file in .claude-plugin/plugin.json skills/check/SKILL.md commands/hello.md; do
        test -f "$source/$file" && test -f "$cache/$file"
        cmp "$reference/$file" "$source/$file"
        cmp "$reference/$file" "$cache/$file"
    done
    grep -F '"version": "'"$actual"'"' "$cache/.claude-plugin/plugin.json" > /dev/null
    grep -F 'name: check' "$cache/skills/check/SKILL.md" > /dev/null
    grep -F 'description: Inert legacy hello declaration' "$cache/commands/hello.md" > /dev/null
    if test "$name" = theseus-probe; then
        for file in hooks/hooks.json hooks/echo.sh; do
            cmp "$reference/$file" "$source/$file"
            cmp "$reference/$file" "$cache/$file"
        done
        grep -F 'SessionStart' "$cache/hooks/hooks.json" > /dev/null
        grep -F '${CLAUDE_PLUGIN_ROOT}/hooks/echo.sh' "$cache/hooks/hooks.json" > /dev/null
        marker=$(CLAUDE_PLUGIN_ROOT="$cache" sh "$cache/hooks/echo.sh")
        test "$marker" = "theseus-hook-$version"
        printf 'Manual cache hook output: %s\n' "$marker"
    fi
done
# Retain native details as text only. Modern clients count one skill plus one
# legacy command as Skills: 2; this fixture does not assert model dispatch.
printf 'Details observation (Skills includes legacy hello; expected count 2):\n'
/home/theseus/.local/bin/theseus-client plugin details theseus-probe@theseus-probe-market
printf 'source/cache manifests skills legacy-command manual-hook verified %s\n' "$version"

#!/bin/sh
# Native commands only. Raw JSON documents are embedded whole, never normalized.
set -eu
export LC_ALL=C
export DISABLE_AUTOUPDATER=1 CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1
export CLAUDE_CONFIG_DIR=/home/theseus/plugin-config CODEX_HOME=/home/theseus/codex
client=/home/theseus/.local/bin/theseus-client
action=$1 adapter=$2 delivery=$3 scope=$4 generation=$5
alias=probe
source="/bin/sh /inputs/$generation/server.sh"
if test "$delivery" = plugin; then alias=plugin:theseus-probe:probe; source='/bin/sh /workspace/local-market/plugins/theseus-probe/server.sh'; fi
mkdir -p "$CLAUDE_CONFIG_DIR" "$CODEX_HOME" /home/theseus/.local/bin
case "$action" in
 install_target|install_base)
  cp /inputs/target/client/claude "$client"
  chmod 0755 "$client" ;;
esac
cmp /inputs/target/client/claude "$client"
cd /workspace
seed() {
 printf '%s\n' '{"hasCompletedOnboarding":true,"mcpServers":{"companion":{"command":"/bin/true","args":[]}},"projects":{"/workspace":{"mcpServers":{}}}}' > "$CLAUDE_CONFIG_DIR/.claude.json"
 printf '%s\n' '{"mcpServers":{}}' > /workspace/.mcp.json
 # Named approval lives only in this disposable guest's private user settings.
 # Project files cannot approve themselves in an untrusted workspace.
 if test "$delivery" = standalone && test "$scope" = project; then
  printf '%s\n' '{"enabledMcpjsonServers":["probe"]}' > "$CLAUDE_CONFIG_DIR/settings.json"
 fi
}
raw() {
 printf '{"client":'; cat "$CLAUDE_CONFIG_DIR/.claude.json"
 printf ',"project":'; cat /workspace/.mcp.json
 if test "$delivery" = plugin; then printf ',"plugins":'; "$client" plugin list --json; fi
 printf '}\n'
}
case "$action" in
 install_target|install_base)
  if test "$delivery" = plugin; then
   seed
   sh /inputs/target/lifecycle.sh install_base user
  elif test "$adapter" = codex-cli-0.166.0; then
   printf 'hide_agent_reasoning = true\n[tui]\ndisable_paste_burst = true\n' > "$CODEX_HOME/config.toml"
   "$client" mcp add companion -- /bin/true
  else seed; fi ;;
 configure|upgrade)
  if test "$delivery" = plugin; then sh /inputs/target/lifecycle.sh upgrade user
  elif test "$adapter" = codex-cli-0.166.0; then "$client" mcp add probe -- /bin/sh "/inputs/$generation/server.sh"
  else
   if test "$action" = upgrade; then "$client" mcp remove probe --scope "$scope"; fi
   "$client" mcp add --scope "$scope" probe -- /bin/sh "/inputs/$generation/server.sh"
  fi ;;
 disable_plugin|enable_plugin|uninstall)
  if test "$delivery" = plugin; then sh /inputs/target/lifecycle.sh "$action" user
  elif test "$adapter" = codex-cli-0.166.0; then "$client" mcp remove probe
  else "$client" mcp remove probe --scope "$scope"; fi ;;
 exercise) : ;;
 version)
  capture=/home/theseus/version.capture
  (ulimit -f 8; "$client" --version > "$capture")
  cmp /inputs/target/expected-version.txt "$capture"
  printf '{"version":"'; tr -d '\n' < "$capture"; printf '"}\n' ;;
 registration|removed-registration)
  if test "$adapter" = codex-cli-0.166.0; then
   printf '{"list":'; "$client" mcp list --json
   if test "$action" = registration; then printf ',"get":'; "$client" mcp get probe --json; fi
   printf '}\n'
  else raw; fi ;;
 preservation)
  if test "$adapter" = codex-cli-0.166.0; then
   # Fail closed on unfamiliar formatting; no TOML parser/normalizer in guest.
   head -n 3 "$CODEX_HOME/config.toml" > "$CODEX_HOME/settings-prefix.capture"
   cmp /inputs/target/codex-settings-prefix.txt "$CODEX_HOME/settings-prefix.capture"
   printf '{"settings_preserved":true,"companion":'; "$client" mcp get companion --json; printf '}\n'
  else raw; fi ;;
 health|inactive-health)
  capture=/home/theseus/native-health.capture
  status=0
  # Bound intermediate capture independently of final command/check output.
  (ulimit -f 128; timeout 12 "$client" mcp list > "$capture") || status=$?
  test "$status" -eq 0 || exit "$status"
  if test "$action" = health; then
   grep -F -x -- "$alias: $source - ✔ Connected" "$capture" > /dev/null
   printf '{"status":0,"alias":"%s","source":"%s","connected":true}\n' "$alias" "$source"
  else
   if grep -F -- "$alias:" "$capture" > /dev/null; then exit 65; fi
   printf '{"status":0,"alias":"%s","source":"","connected":false}\n' "$alias"
  fi ;;
 *) exit 64 ;;
esac

#!/bin/sh
# Small unauthenticated fixture. Supports bounded simple native string/integer IDs;
# Theseus independently verifies the actual received identity and tool inventory.
set -eu
version=@@VERSION@@
tool=@@TOOL@@
protocol=@@PROTOCOL@@
# The selected typed probe remains explicit. Native health may independently use
# the other supported era; this fixture implements both without client fallback.
count=0
while IFS= read -r line; do
 count=$((count + 1)); test "$count" -le 32 || exit 91
 id=$(printf '%s\n' "$line" | sed -n 's/.*"id"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\)[,}].*/\1/p')
 if test -z "$id"; then
  value=$(printf '%s\n' "$line" | sed -n 's/.*"id"[[:space:]]*:[[:space:]]*"\([A-Za-z0-9_.-][A-Za-z0-9_.-]*\)"[,}].*/\1/p')
  test -n "$value" || continue
  id="\"$value\""
 fi
 case "$line" in
  *'server/discover'*)
   protocol=2026-07-28
   result='{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"public","_meta":{"io.modelcontextprotocol/serverInfo":{"name":"theseus-fixture","version":"'"$version"'"}}}' ;;
  *'initialize'*)
   protocol=2025-11-25
   result='{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"theseus-fixture","version":"'"$version"'"}}' ;;
  *'tools/list'*)
   result='{"tools":[{"name":"'"$tool"'","inputSchema":{"type":"object"}}]}'
   if test "$protocol" = 2026-07-28; then result='{"resultType":"complete","ttlMs":0,"cacheScope":"public","tools":[{"name":"'"$tool"'","inputSchema":{"type":"object"}}]}'; fi ;;
  *) exit 92 ;;
 esac
 printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$result"
done

#!/bin/sh
# Synthetic wire fixture, intentionally using Theseus's documented static IDs.
# It validates request order; arbitrary ID encoding is covered by pure decoder tests.
set -eu
IFS= read -r request
case "${ERA:-modern}:$request" in
 modern:*'"method":"server/discover"'*)
  printf '%s\n' '{"jsonrpc":"2.0","id":"theseus-discover-1","result":{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"public","_meta":{"io.modelcontextprotocol/serverInfo":{"name":"fixture","version":"2"}}}}'
  ;;
 legacy:*'"method":"initialize"'*)
  printf '%s\n' '{"jsonrpc":"2.0","id":"theseus-initialize-1","result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"2"}}}'
  IFS= read -r request
  case "$request" in *'"method":"notifications/initialized"'*) :;; *) exit 91;; esac
  ;;
 *) exit 90;;
esac
IFS= read -r request
case "$request" in *'"id":1'*'"method":"tools/list"'*|*'"method":"tools/list"'*'"id":1'*) :;; *) exit 92;; esac
if test "${MODE:-clean}" != silent; then
 tool=hello_v2
 case "${MODE:-clean}" in wrong*) tool=wrong;; esac
 if test "${ERA:-modern}" = modern; then
  printf '{"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","tools":[{"name":"%s","inputSchema":{"type":"object"}}],"ttlMs":0,"cacheScope":"public"}}\n' "$tool"
 else
  printf '{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"%s","inputSchema":{"type":"object"}}]}}\n' "$tool"
 fi
fi
# Wait for real stdin EOF before normal completion/receipt.
while IFS= read -r request; do :; done
case "${MODE:-clean}" in *ignore*|silent) while :; do sleep 1; done;; esac
printf 'observed stdin EOF\n' >&2
exit "${APP_EXIT:-0}"

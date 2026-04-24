#!/usr/bin/env bash
#
# Runs vue-tsc and checks for the TS4023 error referencing GoogleCastOptions.
# Emits BUG CONFIRMED / BUG NOT REPRODUCED verdict in the final lines.

set -u
cd "$(dirname "$0")"

echo "=== Step 1: ensure dependencies are installed ==="
if [ ! -d node_modules ]; then
  echo "node_modules missing — running: npm install"
  npm install
else
  echo "node_modules present — skipping install"
fi

echo ""
echo "=== Step 2: run vue-tsc --noEmit ==="
set +e
OUTPUT="$(npx --no-install vue-tsc --noEmit 2>&1)"
EXIT_CODE=$?
set -e

echo "$OUTPUT"
echo ""
echo "vue-tsc exit code: $EXIT_CODE"

echo ""
echo "=== Step 3: check for TS4023 / GoogleCastOptions ==="
if echo "$OUTPUT" | grep -qE "TS4023.*GoogleCastOptions|GoogleCastOptions.*cannot be named"; then
  echo "Matched TS4023 error referencing GoogleCastOptions."
  echo ""
  echo "BUG CONFIRMED: vue-tsc emits TS4023 because GoogleCastOptions is referenced by MediaPlayerProps.googleCast but not re-exported from the vidstack package."
  exit 0
fi

if [ "$EXIT_CODE" -eq 0 ]; then
  echo ""
  echo "BUG NOT REPRODUCED: vue-tsc completed without any errors."
  exit 1
fi

echo ""
echo "BUG NOT REPRODUCED: vue-tsc failed, but not with the expected TS4023/GoogleCastOptions error."
exit 1

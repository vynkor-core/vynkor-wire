#!/usr/bin/env bash
# Sync proto/vynkor_protocol.proto to all SDK vendored copies.
# Run after any proto change to keep byte-identical copies in sync.
#
# Usage: ./scripts/sync-proto.sh
# Or from the vynkor kernel repo: ../vynkor-wire/scripts/sync-proto.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WIRE_PROTO="$SCRIPT_DIR/../proto/vynkor_protocol.proto"

if [[ ! -f "$WIRE_PROTO" ]]; then
    echo "Error: proto not found at $WIRE_PROTO" >&2
    exit 1
fi

# SDK repos relative to vynkor-wire (sibling repos)
SDK_REPOS=(
    "../vynkor-sdk-cpp"
    "../vynkor-sdk-python"
)

echo "Syncing proto from vynkor-wire to SDK repos..."

for sdk in "${SDK_REPOS[@]}"; do
    TARGET="$sdk/proto/vynkor_protocol.proto"
    if [[ -f "$TARGET" ]]; then
        if cmp -s "$WIRE_PROTO" "$TARGET"; then
            echo "  ✓ $sdk: already in sync"
        else
            cp "$WIRE_PROTO" "$TARGET"
            echo "  ✓ $sdk: updated"
        fi
    else
        echo "  ? $sdk: no proto found (skipped)"
    fi
done

echo ""
echo "Done. Remember to:"
echo "  1. Regenerate Python bindings: cd ../vynkor-sdk-python && python scripts/gen_proto_python.py"
echo "  2. Commit changes in each SDK repo"
echo "  3. Run CI drift check: cargo test --test unit test_proto_sync"

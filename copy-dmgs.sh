#!/bin/bash
set -e
cd "$(dirname "$0")"

DATE=$(date +%Y%m%d)
DEST="$HOME/Downloads"

next_letter() {
    local prefix="$1"
    local letter="a"
    while [ -f "${prefix}${letter}.dmg" ]; do
        letter=$(echo "$letter" | tr 'a-y' 'b-z')
        if [ "$letter" = "z" ] && [ -f "${prefix}z.dmg" ]; then
            echo "Error: ran out of letter indices for today" >&2
            exit 1
        fi
    done
    echo "$letter"
}

if [ -f "target/debug/bundle/osx/Galaxy.dmg" ]; then
    LETTER=$(next_letter "$DEST/Galaxy-debug-${DATE}")
    cp "target/debug/bundle/osx/Galaxy.dmg" "$DEST/Galaxy-debug-${DATE}${LETTER}.dmg"
    echo "Copied: $DEST/Galaxy-debug-${DATE}${LETTER}.dmg"
else
    echo "Skipped: no debug DMG found"
fi

if [ -f "target/release/bundle/osx/Galaxy.dmg" ]; then
    LETTER=$(next_letter "$DEST/Galaxy-release-${DATE}")
    cp "target/release/bundle/osx/Galaxy.dmg" "$DEST/Galaxy-release-${DATE}${LETTER}.dmg"
    echo "Copied: $DEST/Galaxy-release-${DATE}${LETTER}.dmg"
else
    echo "Skipped: no release DMG found"
fi

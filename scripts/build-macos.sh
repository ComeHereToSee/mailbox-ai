#!/usr/bin/env bash
# Builds MailboxAI.app (ad-hoc signed) and dist/MailboxAI.dmg.
# Requirements: Xcode 16+, Rust (rustup), XcodeGen (brew install xcodegen).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONFIG="${CONFIG:-Release}"

"$ROOT/scripts/build-core-macos.sh"

cd "$ROOT/macos"
xcodegen generate --quiet
DERIVED="$ROOT/macos/build/DerivedData"
LOG="$ROOT/macos/build/xcodebuild.log"
mkdir -p "$ROOT/macos/build"
echo "== xcodebuild ($CONFIG)"
set +e
xcodebuild -project Mailbox.xcodeproj -scheme Mailbox -configuration "$CONFIG" \
  -derivedDataPath "$DERIVED" \
  CODE_SIGN_IDENTITY="-" CODE_SIGN_STYLE=Manual DEVELOPMENT_TEAM="" \
  ONLY_ACTIVE_ARCH=NO \
  build 2>&1 | tee "$LOG" | grep --line-buffered -E "^(CompileSwift|SwiftCompile|SwiftDriver|Ld |CodeSign|ProcessInfoPlistFile|CompileAssetCatalog|ExtractAppIntentsMetadata|PhaseScriptExecution)|error: |\*\* BUILD" | awk '{ print substr($0, 1, 200); fflush() }'
status=${PIPESTATUS[0]}
set -e
echo "== errors"
grep -E "error: " "$LOG" | sort -u | head -100 || true
if [ "$status" -ne 0 ]; then
  echo "xcodebuild failed ($status)" >&2
  exit "$status"
fi

APP="$DERIVED/Build/Products/$CONFIG/MailboxAI.app"
test -d "$APP/Contents/Library/LoginItems/MailboxAIHelper.app"
echo "== codesign verify"
codesign --verify --deep --strict "$APP" && echo "signature ok" || echo "signature verification failed (ad-hoc build)" >&2

mkdir -p "$ROOT/dist"
STAGE="$(mktemp -d)"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
rm -f "$ROOT/dist/MailboxAI.dmg"
echo "== hdiutil"
for attempt in 1 2 3; do
  if hdiutil create -volname "Mailbox AI" -srcfolder "$STAGE" -ov -format UDZO "$ROOT/dist/MailboxAI.dmg" >/dev/null; then break; fi
  echo "hdiutil failed, retrying" >&2
  sleep 5
done
rm -rf "$STAGE"
du -h "$ROOT/dist/MailboxAI.dmg"
echo "Built $APP"

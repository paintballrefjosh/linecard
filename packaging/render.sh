#!/usr/bin/env bash
# Fill in the winget manifests and the Homebrew cask for a published release.
#   packaging/render.sh 0.4.10
# Reads the installers' SHA-256 from the release (GitHub records it for every
# asset), so nothing is downloaded. Output goes to packaging/out/.
set -euo pipefail
version=${1:?usage: render.sh VERSION (e.g. 0.4.10)}
cd "$(dirname "$0")"
repo=paintballrefjosh/linecard
api="repos/$repo/releases/tags/v$version"
digest() {
  gh api "$api" --jq ".assets[] | select(.name == \"$1\") | .digest | sub(\"^sha256:\"; \"\")"
}
exe_sha=$(digest "Linecard_${version}_x64-setup.exe" | tr a-f A-F)
dmg_sha=$(digest "Linecard_${version}_universal.dmg")
date=$(gh api "$api" --jq ".published_at[:10]")
[ -n "$exe_sha" ] && [ -n "$dmg_sha" ] || { echo "release v$version is missing its installer or dmg" >&2; exit 1; }

out=out
rm -rf "$out"; mkdir -p "$out/winget/$version" "$out/homebrew"
fill() {
  sed -e "s|@VERSION@|$version|g" -e "s|@DATE@|$date|g" -e "s|@EXE_SHA@|$exe_sha|g" -e "s|@DMG_SHA@|$dmg_sha|g" "$1" > "$2"
}
for f in winget/*.tmpl; do
  fill "$f" "$out/winget/$version/$(basename "${f%.tmpl}")"
done
fill homebrew/linecard.rb.tmpl "$out/homebrew/linecard.rb"
echo "wrote packaging/$out for v$version"

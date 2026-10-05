#!/bin/bash
# Builds the .deb on this machine and publishes it as a GitHub release.
#
#   scripts/release.sh 0.2.0
#
# Checks first: a clean tree, the same version in tauri.conf.json, package.json
# and Cargo.toml, a "## <version>" section in CHANGELOG.md (it becomes the
# release notes) and no linux-v<version> tag yet. Needs `gh` signed in with
# the right to push to this repository.
set -euo pipefail
cd "$(dirname "$0")/.."

v="${1:-}"
if [ -z "$v" ]; then echo "usage: scripts/release.sh <version>"; exit 1; fi
tag="linux-v$v"

if [ -n "$(git status --porcelain)" ]; then echo "The working tree has uncommitted changes."; exit 1; fi
conf=$(node -p "require('./src-tauri/tauri.conf.json').version")
pkg=$(node -p "require('./package.json').version")
cargo=$(sed -nE 's/^version\s*=\s*"(.+)"/\1/p' Cargo.toml | head -1)
if [ "$conf $pkg $cargo" != "$v $v $v" ]; then
  echo "Version mismatch: tauri.conf.json $conf, package.json $pkg, Cargo.toml $cargo (wanted $v)."; exit 1
fi
notes=$(awk -v h="## $v" 'index($0, h) == 1 { on = 1; next } on && /^## / { exit } on' CHANGELOG.md)
if [ -z "$(echo "$notes" | tr -d '[:space:]')" ]; then echo "No \"## $v\" section in CHANGELOG.md."; exit 1; fi
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then echo "Tag $tag already exists."; exit 1; fi

# linux-dev.sh drops the snap environment a VS Code terminal hands down.
scripts/linux-dev.sh npm run tauri build -- --bundles deb

deb=$(ls -t target/release/bundle/deb/*_"$v"_*.deb | head -1)
mkdir -p release
out="release/Coucou-Linux-$v-amd64.deb"
cp "$deb" "$out"
(cd release && sha256sum "$(basename "$out")" > SHA256SUMS)

body="$notes

## Install

1. Download \`Coucou-Linux-$v-amd64.deb\` below and open it (or \`sudo apt install ./Coucou-Linux-$v-amd64.deb\`).
2. Start **Coucou** from the app grid.
3. Settings → GNOME → **Turn on extension**, then log out and back in.
4. Settings → Claude Code → **Install hooks…**"

git tag "$tag"
git push origin "$tag"
gh release create "$tag" "$out" release/SHA256SUMS --title "Coucou for Linux $v" --notes "$body"

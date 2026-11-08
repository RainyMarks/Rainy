#!/usr/bin/env bash
# Build, check and publish a Rainy release in stages, each of which stops at the first failure:
#
#   scripts/release.sh <version> bump        version in every file, changelog entries, Cargo.lock; one commit
#   scripts/release.sh <version> build       offline inputs renamed and restored, then scripts/package.ps1
#   scripts/release.sh <version> resources   the v<version>-resources prerelease with every module piece, checked
#   scripts/release.sh <version> qa          the installer QA (tmp/qa/installer/run-qa.ps1 -Build), all checks passing
#   scripts/release.sh <version> publish     validation record, the release with installer and signed latest.yml, main
#
# Several stages run in order: `scripts/release.sh 4.10.0 bump build resources qa publish`.
#
# What a release says is written beforehand, in tmp/release-notes/:
#   changelog-<version>.zh.md, changelog-<version>.en.md   the CHANGELOG entries, each starting with "## <version> · <date>"
#   v<version>-resources.md, v<version>.md                 the notes of the two GitHub releases
#   validation-<version>.md                                the docs/validation.md entry
#
# Run from Git Bash at the repository's top. Needs gh (signed in), node, the GNU Rust toolchain, cargo-zigbuild and
# Zig, MSYS2 UCRT64, pnpm (rainy-pnpm), WSL with the build distribution, and build/release-signing-key.pem.
set -euo pipefail

version="${1:-}"
shift || true
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || [ $# -eq 0 ]; then
  sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
fi

repo=RainyMarks/Rainy
top="$(git rev-parse --show-toplevel)"
cd "$top"
branch="$(git branch --show-current)"
notes="tmp/release-notes"
# Tools kept outside the repository: pnpm, and the release helpers (upload-release-asset.mjs).
tools="${RAINY_RELEASE_TOOLS:-$HOME/.rainy-release/tools}"
export PATH="$(cygpath "$LOCALAPPDATA")/rainy-pnpm:$HOME/.cargo/bin:/c/msys64/ucrt64/bin:$PATH"

say() { printf '\n== %s\n' "$*"; }
fail() { printf 'release: %s\n' "$*" >&2; exit 1; }
need() { for file in "$@"; do [ -s "$file" ] || fail "missing $file"; done; }

previous_version() {
  node -e 'process.stdout.write(require("./package.json").version)'
}

# Insert the file $2 before the first line of $1 that starts with $3.
insert_before() {
  node - "$1" "$2" "$3" <<'EOF'
const fs = require('fs')
const [file, entryFile, marker] = process.argv.slice(2)
const text = fs.readFileSync(file, 'utf8')
const entry = fs.readFileSync(entryFile, 'utf8').replace(/\r\n/g, '\n').trimEnd() + '\n\n'
const at = text.search(new RegExp('^' + marker.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'm'))
if (at < 0) throw new Error(`${file}: no line starting with ${marker}`)
if (text.includes(entry.split('\n')[0] + '\n')) throw new Error(`${file} already has ${entry.split('\n')[0]}`)
fs.writeFileSync(file, text.slice(0, at) + entry + text.slice(at))
EOF
}

# No commit on this branch, since main, by an author other than RainyMarks (cloud sessions push Claude-authored ones).
check_authors() {
  git fetch -q origin main
  local others
  others="$(git log --format='%an' origin/main..HEAD | grep -v '^RainyMarks$' | sort -u || true)"
  [ -z "$others" ] || fail "commits by $others since main: rewrite their authors first"
}

stage_bump() {
  local previous
  previous="$(previous_version)"
  [ "$previous" != "$version" ] || fail "package.json already says $version"
  need "$notes/changelog-$version.zh.md" "$notes/changelog-$version.en.md"
  [ -z "$(git status --porcelain --untracked-files=no)" ] || fail "commit or put aside the changes first"
  say "version $previous -> $version"
  # package.json: the version field only (a blind replace once changed a dependency pin).
  node -e '
    const fs = require("fs"); const text = fs.readFileSync("package.json", "utf8")
    const next = text.replace(/("version":\s*")[^"]+(")/, `$1${process.argv[1]}$2`)
    if (next === text) throw new Error("package.json: no version field"); fs.writeFileSync("package.json", next)' "$version"
  local pattern="${previous//./\\.}"
  for file in README.md README.zh.md docs/desktop-setup.zh-CN.md native/Cargo.toml native/installer/offline-readme.txt toolpacks/build-inputs.v1.json; do
    grep -q "$pattern" "$file" || fail "$file does not mention $previous"
    sed -i "s/$pattern/$version/g" "$file"
  done
  insert_before CHANGELOG.zh.md "$notes/changelog-$version.zh.md" "## "
  insert_before CHANGELOG.md "$notes/changelog-$version.en.md" "## "
  (cd native && cargo metadata --format-version 1 > /dev/null)
  git add README.md README.zh.md docs/desktop-setup.zh-CN.md native/Cargo.toml native/Cargo.lock native/installer/offline-readme.txt \
    package.json toolpacks/build-inputs.v1.json CHANGELOG.md CHANGELOG.zh.md
  git commit -q -m "$version: version and changelog"
  git log --oneline -1
}

stage_build() {
  local offline
  offline="$(ls -d release/offline-* 2>/dev/null | sort -V | tail -1)"
  [ -n "$offline" ] || fail "no release/offline-<version> to start from"
  if [ "$offline" != "release/offline-$version" ]; then
    say "offline inputs $offline -> release/offline-$version"
    mv "$offline" "release/offline-$version"
  fi
  # Every build rewrites these, and restoring the inputs refuses ones that differ: they are put aside first.
  mkdir -p "tmp/media-aside/$version"
  [ ! -f "release/offline-$version/environment/media-verification.json" ] ||
    mv "release/offline-$version/environment/media-verification.json" "tmp/media-aside/$version/offline-media-verification.json"
  [ ! -f runtime/environment/media-verification.json ] ||
    mv runtime/environment/media-verification.json "tmp/media-aside/$version/runtime-media-verification.json"
  say "restoring the build inputs"
  node scripts/bootstrap-release-inputs.mjs --manifest toolpacks/build-inputs.v1.json 2>&1 | tail -2
  say "packaging (about 25 minutes; log in tmp/package-$version.log)"
  powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/package.ps1 -Distribution Ubuntu \
    -ComponentSource "release/offline-$version/environment-components" > "tmp/package-$version.log" 2>&1 ||
    { tail -20 "tmp/package-$version.log"; fail "packaging failed"; }
  need "release/Rainy-$version-windows-x64-setup.exe" release/latest.yml release/latest.yml.sig
  grep -q "^version: $version$" release/latest.yml || fail "release/latest.yml is not for $version"
  sha256sum "release/Rainy-$version-windows-x64-setup.exe"
}

stage_resources() {
  need "$notes/v$version-resources.md"
  ls "release/resources-$version"/*/*.001 > /dev/null 2>&1 || fail "no pieces in release/resources-$version"
  check_authors
  git push -q origin "$branch"
  local commit
  commit="$(git rev-parse HEAD)"
  if ! gh release view "v$version-resources" --repo "$repo" > /dev/null 2>&1; then
    say "creating v$version-resources at $commit"
    gh release create "v$version-resources" --repo "$repo" --target "$commit" --prerelease \
      --title "Rainy $version resources" --notes-file "$notes/v$version-resources.md"
  fi
  # Uploads can die silently: each piece is tried three times, and the digests are compared at the end.
  local file try
  for file in "release/resources-$version"/*/*.001; do
    for try in 1 2 3; do
      node "$tools/upload-release-asset.mjs" "$repo" "v$version-resources" "$file" > /dev/null 2>&1 && break
      echo "retrying $(basename "$file")"
    done
  done
  gh release view "v$version-resources" --repo "$repo" --json assets --jq '.assets[] | "\(.digest[7:71]) \(.name)"' | sort > tmp/resources-remote.txt
  for file in "release/resources-$version"/*/*.001; do echo "$(sha256sum "$file" | cut -c1-64) $(basename "$file")"; done | sort > tmp/resources-local.txt
  diff tmp/resources-local.txt tmp/resources-remote.txt || fail "the uploaded pieces differ from the built ones"
  say "$(wc -l < tmp/resources-local.txt) pieces uploaded and matching"
}

stage_qa() {
  say "installer QA (log in tmp/qa-installer-$version.log)"
  [ -f tmp/qa/installer/run-qa.ps1 ] || fail "tmp/qa/installer/run-qa.ps1 is missing"
  local started
  started="$(date +%s)"
  powershell.exe -NoProfile -ExecutionPolicy Bypass -File tmp/qa/installer/run-qa.ps1 -Build > "tmp/qa-installer-$version.log" 2>&1 || true
  local results=tmp/qa/installer/qa-results.txt
  [ -f "$results" ] && [ "$(stat -c %Y "$results")" -ge "$started" ] || fail "the QA wrote no results"
  grep '^FAIL' "$results" || true
  grep -q '^failures: 0$' "$results" || fail "the installer QA failed"
  say "$(grep -c '^PASS' "$results") checks passed"
}

stage_publish() {
  need "$notes/v$version.md" "$notes/validation-$version.md" "release/Rainy-$version-windows-x64-setup.exe" release/latest.yml release/latest.yml.sig
  grep -q "^version: $version$" release/latest.yml || fail "release/latest.yml is not for $version"
  gh release view "v$version-resources" --repo "$repo" > /dev/null 2>&1 || fail "publish the resources first"
  if ! grep -q "^## $version" docs/validation.md; then
    insert_before docs/validation.md "$notes/validation-$version.md" "## "
    git add docs/validation.md
    git commit -q -m "Record the $version validation"
  fi
  check_authors
  git push -q origin "$branch"
  local commit
  commit="$(git rev-parse HEAD)"
  say "publishing v$version at $commit"
  gh release create "v$version" --repo "$repo" --target "$commit" --title "Rainy $version" --notes-file "$notes/v$version.md" --latest \
    "release/Rainy-$version-windows-x64-setup.exe" release/latest.yml release/latest.yml.sig ||
    gh release view "v$version" --repo "$repo" > /dev/null || fail "the release was not created"
  gh release view "v$version" --repo "$repo" --json assets --jq '.assets[] | "\(.name) \(.size)"'
  local published
  published="$(curl -sL --max-time 60 "https://github.com/$repo/releases/latest/download/latest.yml" | head -1)"
  [ "$published" = "version: $version" ] || fail "the latest release's latest.yml says: $published"
  git push -q origin "$branch:main"
  # The working copy that follows main.
  if [ -d ../RainyAgent/.git ]; then
    (cd ../RainyAgent && git fetch -q origin && git merge --ff-only -q origin/main && git log --oneline -1)
  fi
  say "Rainy $version is published"
}

for stage in "$@"; do
  case "$stage" in
    bump | build | resources | qa | publish) "stage_$stage" ;;
    *) fail "unknown stage $stage" ;;
  esac
done

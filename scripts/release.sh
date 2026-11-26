#!/usr/bin/env bash
# Build, check and publish a Rainy release in stages, each of which stops at the first failure:
#
#   scripts/release.sh <version> bump        version in every file, changelog entries, Cargo.lock; one commit
#   scripts/release.sh <version> build       offline inputs renamed and restored, then scripts/package.ps1
#   scripts/release.sh <version> resources   the v<version>-resources prerelease with every module piece, checked
#   scripts/release.sh <version> qa          the installer QA (tmp/qa/installer/run-qa.ps1 -Build), all checks passing
#   scripts/release.sh <version> publish     validation record, the release with installer and signed latest.yml, main;
#                                            then the clean stage
#   scripts/release.sh <version> clean       what the build and the QA left that the next release makes again
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

# Run a command up to three times, pausing between the tries: calls to GitHub sometimes end on a dropped connection
# (EOF, a TLS handshake timeout) that the next try gets through.
retry() {
  local try
  for try in 1 2 3; do
    "$@" && return 0
    if [ "$try" -lt 3 ]; then
      printf 'retrying: %s %s\n' "$1" "${2:-}" >&2
      sleep $((try * 15))
    fi
  done
  return 1
}

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
  retry git fetch -q origin main || fail "could not fetch main"
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
  local -a pieces=()
  local saved_nullglob
  saved_nullglob="$(shopt -p nullglob || true)"
  shopt -s nullglob
  pieces=("release/resources-$version"/*/*.[0-9][0-9][0-9])
  eval "$saved_nullglob"
  [ "${#pieces[@]}" -gt 0 ] || fail "no pieces in release/resources-$version"
  check_authors
  retry git push -q origin "$branch" || fail "could not push $branch"
  local commit
  commit="$(git rev-parse HEAD)"
  if ! gh release view "v$version-resources" --repo "$repo" > /dev/null 2>&1; then
    say "creating v$version-resources at $commit"
    gh release create "v$version-resources" --repo "$repo" --target "$commit" --prerelease \
      --title "Rainy $version resources" --notes-file "$notes/v$version-resources.md" ||
      retry gh release view "v$version-resources" --repo "$repo" > /dev/null || fail "v$version-resources was not created"
  fi
  # A run that stopped part way left some pieces attached: those are not sent again (the digests are compared at the
  # end). Uploads can die silently: each missing piece is tried three times, with a pause between the tries.
  local attached file name try uploaded
  attached="$(retry gh release view "v$version-resources" --repo "$repo" --json assets --jq '.assets[].name')" ||
    fail "could not list the assets of v$version-resources"
  for file in "${pieces[@]}"; do
    name="$(basename "$file")"
    if grep -qxF "$name" <<< "$attached"; then
      echo "already attached: $name"
      continue
    fi
    uploaded=0
    for try in 1 2 3; do
      if node "$tools/upload-release-asset.mjs" "$repo" "v$version-resources" "$file" > /dev/null 2>&1; then
        uploaded=1
        break
      fi
      if [ "$try" -lt 3 ]; then
        echo "retrying $name"
        sleep $((try * 15))
      fi
    done
    [ "$uploaded" -eq 1 ] || fail "failed to upload $name after 3 attempts (run the resources stage again to go on)"
  done
  local remote
  remote="$(retry gh release view "v$version-resources" --repo "$repo" --json assets --jq '.assets[] | "\(.digest[7:71]) \(.name)"')" ||
    fail "could not list the assets of v$version-resources"
  sort <<< "$remote" > tmp/resources-remote.txt
  for file in "${pieces[@]}"; do echo "$(sha256sum "$file" | cut -c1-64) $(basename "$file")"; done | sort > tmp/resources-local.txt
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
  retry gh release view "v$version-resources" --repo "$repo" > /dev/null 2>&1 || fail "publish the resources first"
  if ! grep -q "^## $version" docs/validation.md; then
    insert_before docs/validation.md "$notes/validation-$version.md" "## "
    git add docs/validation.md
    git commit -q -m "Record the $version validation"
  fi
  check_authors
  retry git push -q origin "$branch" || fail "could not push $branch"
  local commit
  commit="$(git rev-parse HEAD)"
  say "publishing v$version at $commit"
  gh release create "v$version" --repo "$repo" --target "$commit" --title "Rainy $version" --notes-file "$notes/v$version.md" --latest \
    "release/Rainy-$version-windows-x64-setup.exe" release/latest.yml release/latest.yml.sig ||
    retry gh release view "v$version" --repo "$repo" > /dev/null || fail "the release was not created"
  # A create that broke off part way can leave the release without some of its files: check each one is there.
  local assets file
  assets="$(retry gh release view "v$version" --repo "$repo" --json assets --jq '.assets[] | "\(.name) \(.size)"')" ||
    fail "could not read v$version back"
  printf '%s\n' "$assets"
  for file in "release/Rainy-$version-windows-x64-setup.exe" release/latest.yml release/latest.yml.sig; do
    grep -qxF "$(basename "$file") $(stat -c %s "$file")" <<< "$assets" ||
      fail "v$version lacks $(basename "$file") (gh release upload v$version $file --repo $repo --clobber, then publish again)"
  done
  local published=""
  retry curl -fsSL --max-time 60 -o tmp/latest-published.yml "https://github.com/$repo/releases/latest/download/latest.yml" &&
    published="$(head -1 tmp/latest-published.yml)"
  [ "$published" = "version: $version" ] || fail "the latest release's latest.yml says: ${published:-nothing (the download failed)}"
  retry git push -q origin "$branch:main" || fail "could not push main"
  # The working copy that follows main.
  if [ -d ../RainyAgent/.git ]; then
    (cd ../RainyAgent && retry git fetch -q origin && git merge --ff-only -q origin/main && git log --oneline -1)
  fi
  say "Rainy $version is published"
  stage_clean
}

# Megabytes under the given paths.
megabytes() {
  { du -sm "$@" 2>/dev/null || true; } | awk '{ total += $1 } END { print total + 0 }'
}

# What a release leaves behind that the next one builds again, often tens of gigabytes: the build outputs
# (native/target, the Windows and the Linux Hosts included), the staged payload and the NSIS work folder, earlier
# installers and resource pieces (each on its GitHub release), the installer QA's installations and the unpacked
# overlay copies. Kept: this version's installer, latest.yml and its signature, release/offline-<version>,
# release/resources-<version>, tmp/qa/installer/run-qa.ps1 and the QA results, and the pinned NSIS archives in
# ~/.rainy-release/toolchain. The next build compiles from scratch (about 40 minutes instead of a few).
stage_clean() {
  say "clearing what the build and the QA left"
  local before after path
  before="$(megabytes native/target release tmp/qa/installer tmp/overlay-*)"
  rm -rf native/target release/win-unpacked release/nsis
  for path in release/Rainy-*-windows-x64-setup.exe; do
    [ -e "$path" ] && [ "$path" != "release/Rainy-$version-windows-x64-setup.exe" ] && rm -f "$path"
  done
  for path in release/resources-*; do
    [ -d "$path" ] && [ "$path" != "release/resources-$version" ] && rm -rf "$path"
  done
  for path in tmp/qa/installer/*; do
    case "$(basename "$path")" in
      run-qa.ps1 | qa-results.txt) ;;
      *) rm -rf "$path" ;;
    esac
  done
  rm -rf tmp/overlay-*/app
  after="$(megabytes native/target release tmp/qa/installer tmp/overlay-*)"
  say "freed $((before - after)) MB (left: release/offline-$version, release/resources-$version, this installer)"
}

for stage in "$@"; do
  case "$stage" in
    bump | build | resources | qa | publish | clean) "stage_$stage" ;;
    *) fail "unknown stage $stage" ;;
  esac
done

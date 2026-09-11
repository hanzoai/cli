#!/bin/sh
# Install a Hanzo native binary.
#
#   curl -fsSL https://raw.githubusercontent.com/hanzoai/cli/main/install.sh | sh
#
# Downloads the prebuilt release asset for THIS machine, verifies its sha256, and
# puts it on PATH. Nothing is built, and no package manager is involved. It
# refuses loudly on a platform we do not publish, because a script that
# half-works is worse than one that says why it cannot.
#
# This is the ONE implementation of "fetch a Hanzo binary". It defaults to the
# `hanzo` CLI, and hanzo.sh drives it once per tool by overriding three
# variables rather than carrying a second copy of platform detection, asset
# naming and checksum verification:
#
#   HANZO_INSTALL_REPO   owning repo            (default hanzoai/cli)
#   HANZO_INSTALL_BIN    binary + asset prefix  (default hanzo)
#   HANZO_INSTALL_ALIAS  second name, same build (default hanzo-node; "" = none)
#   HANZO_INSTALL_PREFIX install dir            (default ~/.local/bin)
#   HANZO_VERSION        pin a tag              (default: highest stable semver)
#
# The convention every published Hanzo binary follows, and the only thing this
# needs to know: the asset is <BIN>-<os>-<arch>.tar.gz, it is accompanied by
# <asset>.sha256, and it unpacks to a single file named <BIN>.
set -eu

REPO="${HANZO_INSTALL_REPO:-hanzoai/cli}"
BIN="${HANZO_INSTALL_BIN:-hanzo}"
# The second name this same build installs under: what cloud's control binary
# delegates to. One build, two names — never two versions.
DELEGATE="${HANZO_INSTALL_ALIAS-hanzo-node}"
PREFIX="${HANZO_INSTALL_PREFIX:-$HOME/.local/bin}"

die() { printf '\n%s: %s\n' "$BIN" "$1" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "need $1 on PATH"; }

need curl
need tar
need awk

# A private repo answers 404 to an anonymous asset fetch, so carry a token when
# one is available. Public installs need none.
TOKEN="${HANZO_INSTALL_TOKEN:-${GH_TOKEN:-${GITHUB_TOKEN:-}}}"
if [ -z "$TOKEN" ] && command -v gh >/dev/null 2>&1; then
  TOKEN="$(gh auth token 2>/dev/null || true)"
fi
# Never interpolate the token into an argument list: unquoted substitution
# word-splits it into its own argv entry, and curl echoes argv on failure — which
# prints the token. Branch, and keep the header a single quoted argument.
# Whether to draw curl's own progress bar, decided once.
#
# A binary is tens of megabytes and `-s` draws nothing while it comes down, so
# a slow connection is indistinguishable from a hang — which is how somebody
# ends up pressing ctrl-c halfway through an install that was working.
#
# The bar goes to stderr. `curl hanzo.sh | sh` consumes stdout and leaves
# stderr on the terminal, so it is visible in exactly the case this is for.
# Where there is no terminal — CI, a log file, a pipe to tee — a bar is a few
# thousand carriage returns in the transcript, so it stays silent there and
# `-S` keeps errors reported either way.
if [ -t 2 ]; then DL='-#'; else DL='-sS'; fi

get() { # get <url> <dest>
  if [ -n "$TOKEN" ]; then
    curl -fL $DL -H "Authorization: Bearer $TOKEN" "$1" -o "$2"
  else
    curl -fL $DL "$1" -o "$2"
  fi
}
get_stdout() { # get_stdout <url>
  if [ -n "$TOKEN" ]; then
    curl -fsSL -H "Authorization: Bearer $TOKEN" "$1"
  else
    curl -fsSL "$1"
  fi
}

# Published targets: linux-{amd64,arm64}, darwin-{amd64,arm64}, windows-amd64.
os="$(uname -s)"
arch="$(uname -m)"
ext=""
case "$os" in
  Linux)  os=linux ;;
  Darwin) os=darwin ;;
  MINGW*|MSYS*|CYGWIN*|Windows_NT) os=windows; ext=".exe" ;;  # git-bash / msys2
  *) die "unsupported OS '$os'." ;;
esac
case "$arch" in
  x86_64|amd64)  arch=amd64 ;;
  aarch64|arm64) arch=arm64 ;;
  *) die "unsupported architecture '$arch'." ;;
esac
target="${os}-${arch}"

TAG="${HANZO_VERSION:-}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# GitHub's /latest follows release chronology, which can select an older
# maintenance release over a higher version. Parse release objects (including
# compact JSON and escaped body text), then compare stable semantic versions.
# Only releases containing this native product participate: MCP also publishes
# independent JavaScript versions in the same repository.
release_tags() {
  awk -v bin="$BIN" '
    function scalar(value) {
      if (depth == 2) {
        if (key[depth] == "tag_name") tag = value
        if (key[depth] == "draft") draft = value
        if (key[depth] == "prerelease") prerelease = value
      }
      if (depth == 4 && context[3] == "assets" && key[depth] == "name" &&
          index(value, bin "-") == 1 && value ~ /\.tar\.gz$/) native = 1
    }
    { document = document $0 "\n" }
    END {
      for (i = 1; i <= length(document); i++) {
        c = substr(document, i, 1)
        if (c == "\"") {
          value = ""; closed = 0
          while (++i <= length(document)) {
            c = substr(document, i, 1)
            if (c == "\\") { value = value c substr(document, ++i, 1); continue }
            if (c == "\"") { closed = 1; break }
            value = value c
          }
          if (!closed) exit 3
          nextchar = i + 1
          while (substr(document, nextchar, 1) ~ /[ \t\r\n]/) nextchar++
          if (substr(document, nextchar, 1) == ":") key[depth] = value
          else scalar(value)
        } else if (c == "{" || c == "[") {
          context[depth + 1] = key[depth]; depth++
          if (depth == 2 && c == "{") {
            tag = ""; draft = ""; prerelease = ""; native = 0
          }
        } else if (c == "}" || c == "]") {
          if (depth == 2 && c == "}") {
            count++
            version = tag; sub(/^(rust-)?v/, "", version)
            if (native && draft == "false" && prerelease == "false" &&
                version ~ /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(\+[0-9A-Za-z.-]+)?$/) {
              sub(/\+.*/, "", version)
              print version "\t" tag
            }
          }
          delete key[depth]; delete context[depth]; depth--
          if (depth < 0) exit 3
        } else if (c ~ /[a-z0-9-]/) {
          value = c
          while (substr(document, i + 1, 1) ~ /[a-z0-9.+-]/)
            value = value substr(document, ++i, 1)
          scalar(value)
        }
      }
      if (depth != 0) exit 3
      print "COUNT\t" count + 0
    }
  ' "$1"
}

if [ -z "$TAG" ]; then
  page=1
  : > "$tmp/tags"
  while :; do
    get "https://api.github.com/repos/$REPO/releases?per_page=100&page=$page" "$tmp/releases.json" \
      || die "could not list releases of $REPO"
    release_tags "$tmp/releases.json" > "$tmp/page-tags" \
      || die "invalid release metadata from $REPO"
    sed '/^COUNT/d' "$tmp/page-tags" >> "$tmp/tags"
    count="$(awk '$1 == "COUNT" { print $2 }' "$tmp/page-tags")"
    [ "$count" -ge 100 ] || break
    page=$((page + 1))
  done
  TAG="$(sort -t . -k1,1n -k2,2n -k3,3n "$tmp/tags" | tail -1 | awk '{print $2}')"
  [ -n "$TAG" ] || die "could not resolve a stable native release of $REPO.
  If $REPO is private, set GH_TOKEN (or run \`gh auth login\`); or pin HANZO_VERSION=vX.Y.Z."
fi

asset="${BIN}-${target}.tar.gz"
base="https://github.com/$REPO/releases/download/$TAG"

# A private release's browser download URL is not fetchable with a token; assets
# must come from the API by id, with an octet-stream Accept.
fetch() { # fetch <asset-name> <dest>
  if [ -z "$TOKEN" ]; then
    get "$base/$1" "$2"
    return
  fi
  # A private release's browser URL is not token-fetchable; assets come from the
  # API by id, with an octet-stream Accept.
  # Pull the asset id out of the release JSON. GitHub pretty-prints, so collapse
  # the newlines FIRST — otherwise each asset object spans many lines and the
  # "id" never lands on the same record as the "name" we matched.
  id="$(get_stdout "https://api.github.com/repos/$REPO/releases/tags/$TAG" \
      | tr -d '\n' | tr '{' '\n' \
      | grep -F "\"$1\"" \
      | sed -n 's/.*"id": *\([0-9][0-9]*\).*/\1/p' | head -1)"
  [ -n "$id" ] || return 1
  curl -fL $DL -H "Authorization: Bearer $TOKEN" -H "Accept: application/octet-stream" \
    "https://api.github.com/repos/$REPO/releases/assets/$id" -o "$2"
}

printf '%s: %s %s\n' "$BIN" "$TAG" "$target"
fetch "$asset" "$tmp/$asset" \
  || die "no published build for $target at $TAG."
fetch "$asset.sha256" "$tmp/$asset.sha256" \
  || die "release $TAG has no checksum for $target — refusing to install unverified"

# Verify BEFORE unpacking: an unverified binary is not installed, ever.
( cd "$tmp" && \
  if command -v sha256sum >/dev/null 2>&1; then sha256sum -c "$asset.sha256";
  elif command -v shasum   >/dev/null 2>&1; then shasum -a 256 -c "$asset.sha256";
  else die "need sha256sum or shasum to verify the download"; fi ) >/dev/null \
  || die "checksum MISMATCH for $asset — refusing to install"

tar -xzf "$tmp/$asset" -C "$tmp"
[ -f "$tmp/$BIN$ext" ] || die "archive did not contain '$BIN$ext'"

mkdir -p "$PREFIX"
mv "$tmp/$BIN$ext" "$PREFIX/$BIN$ext"
chmod 755 "$PREFIX/$BIN$ext"

printf '%s: installed %s\n' "$BIN" "$PREFIX/$BIN$ext"

# The SAME build under a second name. cloud's control binary hands every verb it
# does not own to `hanzo-node` (it resolves that name before `hanzo`), so the two
# names must never be two versions: install one and not the other, or upgrade one
# and not the other, and a user types `hanzo`, gets delegated, and runs an old
# build with no version anywhere on screen. That is not theoretical — a stale
# twin served ~150 commands that no longer existed, silently.
#
# A symlink makes the skew impossible rather than merely unlikely; where links
# are not available (Windows) a copy still matches by version. A caller that
# wants no second name passes HANZO_INSTALL_ALIAS=''.
if [ -n "$DELEGATE" ]; then
  if ln -sf "$BIN$ext" "$PREFIX/$DELEGATE$ext" 2>/dev/null; then
    :
  else
    cp -f "$PREFIX/$BIN$ext" "$PREFIX/$DELEGATE$ext" && chmod 755 "$PREFIX/$DELEGATE$ext"
  fi
  [ -e "$PREFIX/$DELEGATE$ext" ] \
    || die "could not install the second name $PREFIX/$DELEGATE$ext — a half-install is not an install"
  printf '%s: installed %s (the same build, second name)\n' "$BIN" "$PREFIX/$DELEGATE$ext"
fi

# Whatever PATH says, name the copy that would ACTUALLY run. An earlier entry
# wins, and that precedence is exactly how a stale install hides: the user
# upgrades here and keeps running something else, and re-running the installer
# never fixes it because it keeps writing to a directory that never wins.
found="$(command -v "$BIN" 2>/dev/null || true)"
if [ -n "$found" ] && [ "$found" != "$PREFIX/$BIN$ext" ]; then
  printf '%s: WARNING %s comes first on PATH and will run instead of the\n' "$BIN" "$found"
  printf '       build just installed at %s. Remove it, or put %s first.\n' "$PREFIX/$BIN$ext" "$PREFIX"
fi
case ":$PATH:" in
  *":$PREFIX:"*) ;;
  *) printf '%s: %s is not on PATH — add it:\n  export PATH="%s:$PATH"\n' "$BIN" "$PREFIX" "$PREFIX" ;;
esac
# `hanzo login` is not a command. An unrecognised first word is read as a task
# for the coding agent, so telling someone to run it starts a session about the
# word "login" instead of signing them in — and this was the LAST line the
# installer printed.
if [ "$BIN" = hanzo ]; then printf 'hanzo: next → hanzo auth login\n'; fi

#!/bin/sh
# Install the `hanzo` CLI.
#
#   curl -fsSL https://raw.githubusercontent.com/hanzoai/cli/main/install.sh | sh
#
# Resolves the release asset for THIS machine, verifies its sha256, and installs
# it. It refuses loudly on a platform we do not publish, because a script that
# half-works is worse than one that says why it cannot.
#
# WHILE hanzoai/cli IS PRIVATE this needs a token (GH_TOKEN, or gh's own):
#   GH_TOKEN=... sh install.sh
# Public `curl | sh` self-service starts working the moment the repo is public;
# nothing else here has to change.
set -eu

REPO="hanzoai/cli"
BIN="hanzo"
# The second name this same build installs under: what cloud's control binary
# delegates to. One build, two names — never two versions.
DELEGATE="hanzo-node"
PREFIX="${HANZO_INSTALL_PREFIX:-$HOME/.local/bin}"

die() { printf '\nhanzo: %s\n' "$1" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "need $1 on PATH"; }

need curl
need tar

# A private repo answers 404 to an anonymous asset fetch, so carry a token when
# one is available. Public installs need none.
TOKEN="${HANZO_INSTALL_TOKEN:-${GH_TOKEN:-${GITHUB_TOKEN:-}}}"
if [ -z "$TOKEN" ] && command -v gh >/dev/null 2>&1; then
  TOKEN="$(gh auth token 2>/dev/null || true)"
fi
# Never interpolate the token into an argument list: unquoted substitution
# word-splits it into its own argv entry, and curl echoes argv on failure — which
# prints the token. Branch, and keep the header a single quoted argument.
get() { # get <url> <dest>
  if [ -n "$TOKEN" ]; then
    curl -fsSL -H "Authorization: Bearer $TOKEN" "$1" -o "$2"
  else
    curl -fsSL "$1" -o "$2"
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
if [ -z "$TAG" ]; then
  TAG="$(get_stdout "https://api.github.com/repos/$REPO/releases/latest" \
        | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)"
  [ -n "$TAG" ] || die "could not resolve the latest release of $REPO.
  $REPO is private: set GH_TOKEN (or run \`gh auth login\`), or pin HANZO_VERSION=vX.Y.Z."
fi

asset="hanzo-${target}.tar.gz"
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
  curl -fsSL -H "Authorization: Bearer $TOKEN" -H "Accept: application/octet-stream" \
    "https://api.github.com/repos/$REPO/releases/assets/$id" -o "$2"
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

printf 'hanzo: %s %s\n' "$TAG" "$target"
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

# The SAME build under the second name. cloud's control binary hands every verb
# it does not own to `hanzo-node` (it resolves that name before `hanzo`), so the
# two names must never be two versions: install one and not the other, or
# upgrade one and not the other, and a user types `hanzo`, gets delegated, and
# runs an old build with no version anywhere on screen. That is not theoretical
# — a stale twin served ~150 commands that no longer existed, silently.
#
# A symlink makes the skew impossible rather than merely unlikely; where links
# are not available (Windows) a copy still matches by version.
if ln -sf "$BIN$ext" "$PREFIX/$DELEGATE$ext" 2>/dev/null; then
  :
else
  cp -f "$PREFIX/$BIN$ext" "$PREFIX/$DELEGATE$ext" && chmod 755 "$PREFIX/$DELEGATE$ext"
fi

printf 'hanzo: installed %s\n' "$PREFIX/$BIN$ext"
printf 'hanzo: installed %s (the delegate name, same build)\n' "$PREFIX/$DELEGATE$ext"

case ":$PATH:" in
  *":$PREFIX:"*)
    # On PATH, but is it the FIRST one? An earlier entry wins, and that
    # precedence is exactly how a stale install hides: the user upgrades here
    # and keeps running something else. Name the one that would actually run.
    found="$(command -v "$BIN" 2>/dev/null || true)"
    if [ -n "$found" ] && [ "$found" != "$PREFIX/$BIN$ext" ]; then
      printf 'hanzo: WARNING %s comes first on PATH and will run instead of the\n' "$found"
      printf '       build just installed at %s. Remove it, or put %s first.\n' "$PREFIX/$BIN$ext" "$PREFIX"
    fi
    ;;
  *) printf 'hanzo: %s is not on PATH — add it:\n  export PATH="%s:$PATH"\n' "$PREFIX" "$PREFIX" ;;
esac
printf 'hanzo: next → hanzo login\n'

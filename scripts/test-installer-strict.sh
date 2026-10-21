#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT HUP INT TERM
mkdir -p "$fixture/tools" "$fixture/archive" "$fixture/installed"
printf '#!/bin/sh\necho hanzo 1.2.3\n' > "$fixture/archive/hanzo"
tar -czf "$fixture/hanzo-linux-amd64.tar.gz" -C "$fixture/archive" hanzo
(cd "$fixture" && sha256sum hanzo-linux-amd64.tar.gz > hanzo-linux-amd64.tar.gz.sha256)
cat > "$fixture/tools/uname" <<'SH'
#!/bin/sh
case "$1" in -s) echo Linux;; *) echo x86_64;; esac
SH
cat > "$fixture/tools/gh" <<'SH'
#!/bin/sh
case "$*" in
    'auth status') exit 0 ;;
    'auth token') exit 1 ;;
    '--version') echo 'gh version 2.45.0'; exit 0 ;;
    'attestation verify --help')
        [ "$VERIFY_MODE" = old ] || echo '--source-ref'
        exit 0 ;;
    *) [ "$VERIFY_MODE" = pass ] ;;
esac
SH
cat > "$fixture/tools/curl" <<'SH'
#!/bin/sh
url=''; dest=''
while [ $# -gt 0 ]; do
    case "$1" in
        https://*) url=$1; shift ;;
        -o) dest=$2; shift 2 ;;
        *) shift ;;
    esac
done
cp "$INSTALL_FIXTURE/${url##*/}" "$dest"
SH
chmod +x "$fixture/tools/"*
export INSTALL_FIXTURE="$fixture"
export HANZO_INSTALL_TOKEN='' GH_TOKEN='' GITHUB_TOKEN=''
export HANZO_VERSION=v1.2.3 HANZO_INSTALL_REPO=hanzoai/cli HANZO_INSTALL_BIN=hanzo
export HANZO_INSTALL_ALIAS=hanzo-cli HANZO_INSTALL_PREFIX="$fixture/installed"
export PATH="$fixture/tools:$PATH"
for shell in sh bash; do
    for mode in old reject; do
        printf 'previous installation\n' > "$fixture/installed/hanzo"
        if VERIFY_MODE="$mode" HANZO_INSTALL_STRICT=1 "$shell" "$root/install.sh" > "$fixture/log" 2>&1; then
            echo "strict $mode unexpectedly succeeded" >&2; exit 1
        fi
        grep -q 'previous installation' "$fixture/installed/hanzo"
    done
    ln -sf hanzo "$fixture/installed/hanzo-node"
    VERIFY_MODE=pass HANZO_INSTALL_STRICT=1 "$shell" "$root/install.sh" > "$fixture/log" 2>&1
    "$fixture/installed/hanzo-cli" --version | grep -q 'hanzo 1.2.3'
    [ ! -L "$fixture/installed/hanzo-node" ]
    printf 'network node\n' > "$fixture/installed/hanzo-node"
    VERIFY_MODE=old HANZO_INSTALL_STRICT=0 HANZO_INSTALL_SIGNER=.github/workflows/release.yml \
        "$shell" "$root/install.sh" > "$fixture/log" 2>&1
    grep -q 'provenance not checked' "$fixture/log"
    grep -q 'upgrade GitHub CLI' "$fixture/log"
    grep -q 'network node' "$fixture/installed/hanzo-node"
    rm "$fixture/installed/hanzo-node"
    echo "$shell: strict verification and preservation of existing installs passed"
done

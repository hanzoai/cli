#!/bin/sh
# generate.sh --check against a document that matches the lock, one that moved, and a
# committed spec that is stale: cargo is a stub that records what it was asked.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM
mkdir -p "$work/bin" "$work/tree/scripts"
cp "$root/scripts/generate.sh" "$work/tree/scripts/"
printf 'ref=r\nsha256=%s\nrepo=x\npath=y\n' "$(printf locked | sha256sum | cut -d' ' -f1)" > "$work/tree/.spec-lock"
cat > "$work/bin/cargo" <<'SH'
#!/bin/sh
case "$*" in *genproduct*) bin=genproduct ;; *) bin=genspec ;; esac
echo "$bin" >> "$CALLS"
[ "$bin" != "${FAIL:-}" ]
SH
chmod +x "$work/bin/cargo"
PATH="$work/bin:$PATH"
CALLS="$work/calls"
export CALLS PATH

check() { : > "$CALLS"; SPEC="$1" sh "$work/tree/scripts/generate.sh" --check; }

printf locked > "$work/same"
check "$work/same" > /dev/null
[ "$(tr '\n' ' ' < "$CALLS")" = "genproduct genspec " ]

printf moved > "$work/moved"
check "$work/moved" > "$work/out"
[ "$(tr '\n' ' ' < "$CALLS")" = "genproduct " ]
grep -q 're-pin is due' "$work/out"

if FAIL=genproduct check "$work/moved" > /dev/null 2>&1; then exit 1; fi
if FAIL=genspec check "$work/same" > /dev/null 2>&1; then exit 1; fi
echo "generate --check: current, moved and stale all ruled"

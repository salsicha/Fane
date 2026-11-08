#!/usr/bin/env bash
# Bundle CodeGem (SuperQode, Apache-2.0) with a private, relocatable Python so Fane's packages
# can ship it. Fane runs it when `superqode` isn't on PATH (see `bundled` in
# src/connectors/delegate.rs).
#
#   packaging/codegem/bundle.sh <out dir> [platform]
#
# platform: linux-x86_64 | linux-aarch64 | macos-aarch64 | macos-x86_64 | windows-x86_64
# (default: this machine). Wheels are chosen for the target platform, so an Intel Mac bundle
# can be built on Apple silicon. Needs uv (https://docs.astral.sh/uv/).
set -euo pipefail

out=$1
platform=${2:-}
superqode_version=${SUPERQODE_VERSION:-2.31.4}
py=3.13
uv=${UV:-uv}

if [ -z "$platform" ]; then
    case "$(uname -s)-$(uname -m)" in
        Linux-x86_64) platform=linux-x86_64 ;;
        Linux-aarch64 | Linux-arm64) platform=linux-aarch64 ;;
        Darwin-arm64) platform=macos-aarch64 ;;
        Darwin-x86_64) platform=macos-x86_64 ;;
        MINGW* | MSYS* | CYGWIN*) platform=windows-x86_64 ;;
        *) echo "unknown platform; pass one explicitly" >&2; exit 2 ;;
    esac
fi
case "$platform" in
    linux-x86_64) request=cpython-$py-linux-x86_64-gnu; target=x86_64-manylinux_2_28 ;;
    linux-aarch64) request=cpython-$py-linux-aarch64-gnu; target=aarch64-manylinux_2_28 ;;
    macos-aarch64) request=cpython-$py-macos-aarch64-none; target=aarch64-apple-darwin ;;
    macos-x86_64) request=cpython-$py-macos-x86_64-none; target=x86_64-apple-darwin ;;
    windows-x86_64) request=cpython-$py-windows-x86_64-none; target=x86_64-pc-windows-msvc ;;
    *) echo "unknown platform $platform" >&2; exit 2 ;;
esac

rm -rf "$out"
mkdir -p "$out"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

"$uv" python install "$request" --install-dir "$work" --no-bin
mv "$(find "$work" -maxdepth 1 -type d -name "cpython-*" | head -1)" "$out/python"
if [ "${platform%%-*}" = windows ]; then
    site="$out/python/Lib/site-packages"
else
    site="$out/python/lib/python$py/site-packages"
fi

# Prebuilt wheels only: the bundle is assembled for another platform, so nothing can be compiled.
constraints=()
if [ "$platform" = macos-x86_64 ]; then
    # cryptography 49+ ships no Intel-Mac wheels.
    printf 'cryptography<49\n' > "$work/constraints.txt"
    constraints=(--constraint "$work/constraints.txt")
fi
"$uv" pip install --no-cache --only-binary :all: --target "$site" --python-platform "$target" \
    --python-version "$py" ${constraints[@]+"${constraints[@]}"} "superqode==$superqode_version"

# Headers, docs, tests and caches aren't needed to run.
rm -rf "$out/python/include" "$out/python/share" "$out/python/lib/python$py/test" \
    "$out/python/Lib/test" "$site/bin" "$site/../bin" 2>/dev/null || true
find "$out" -name __pycache__ -type d -prune -exec rm -rf {} +
# Helper scripts with absolute shebangs (pip, idle, ...) would break once the bundle moves.
find "$out/python/bin" \( -name 'idle*' -o -name 'pip*' -o -name 'pydoc*' -o -name '*-config' \) -delete 2>/dev/null || true
rm -f "$out/python/Scripts/"*.exe 2>/dev/null || true

cat > "$out/README" <<EOF
CodeGem (SuperQode $superqode_version, Apache-2.0, https://pypi.org/project/superqode/)
bundled with Fane, with CPython $py from python-build-standalone ($platform).
Fane starts it as: python -I -c "from superqode.main import cli_main; cli_main()" ...
EOF
du -sh "$out"

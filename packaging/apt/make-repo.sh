#!/usr/bin/env bash
# Build a signed apt repository from .deb files.
#
#   packaging/apt/make-repo.sh <dir with .deb files> <output dir>
#
# Signing uses the GPG key named by $GPG_KEY_ID (already imported), with $GPG_PASSPHRASE if
# the key has one. Without GPG_KEY_ID the repository is left unsigned (local testing only).
# Users install with:
#   curl -fsSL https://salsicha.github.io/Fane/fane-archive-keyring.gpg | sudo tee /usr/share/keyrings/fane-archive-keyring.gpg >/dev/null
#   echo "deb [signed-by=/usr/share/keyrings/fane-archive-keyring.gpg] https://salsicha.github.io/Fane stable main" | sudo tee /etc/apt/sources.list.d/fane.list
#   sudo apt update && sudo apt install fane
set -euo pipefail

debs=$(cd "$1" && pwd)
out=$2
suite=stable
component=main
archs="amd64 arm64"

rm -rf "$out"
mkdir -p "$out/pool/$component/f/fane"
cp "$debs"/*.deb "$out/pool/$component/f/fane/"
cd "$out"

for arch in $archs; do
    dir=dists/$suite/$component/binary-$arch
    mkdir -p "$dir"
    apt-ftparchive --arch "$arch" packages pool > "$dir/Packages"
    gzip -9kf "$dir/Packages"
done

apt-ftparchive \
    -o APT::FTPArchive::Release::Origin=Fane \
    -o APT::FTPArchive::Release::Label=Fane \
    -o APT::FTPArchive::Release::Suite=$suite \
    -o APT::FTPArchive::Release::Codename=$suite \
    -o "APT::FTPArchive::Release::Architectures=$archs" \
    -o APT::FTPArchive::Release::Components=$component \
    -o "APT::FTPArchive::Release::Description=Fane desktop assistant" \
    release "dists/$suite" > "dists/$suite/Release"

if [ -n "${GPG_KEY_ID:-}" ]; then
    gpg_args=(--batch --yes --default-key "$GPG_KEY_ID")
    if [ -n "${GPG_PASSPHRASE:-}" ]; then
        gpg_args+=(--pinentry-mode loopback --passphrase "$GPG_PASSPHRASE")
    fi
    gpg "${gpg_args[@]}" --clearsign -o "dists/$suite/InRelease" "dists/$suite/Release"
    gpg "${gpg_args[@]}" --armor --detach-sign -o "dists/$suite/Release.gpg" "dists/$suite/Release"
    gpg --batch --export "$GPG_KEY_ID" > fane-archive-keyring.gpg
else
    echo "warning: GPG_KEY_ID not set; the repository is unsigned" >&2
fi

base=${REPO_URL:-https://salsicha.github.io/Fane}
cat > index.html <<EOF
<!doctype html>
<meta charset="utf-8">
<title>Fane apt repository</title>
<style>body{font:16px/1.5 system-ui,sans-serif;max-width:46rem;margin:3rem auto;padding:0 1rem}pre{background:#f3f3f3;padding:1rem;overflow-x:auto}</style>
<h1>Fane apt repository</h1>
<p>Ubuntu 24.04 or newer (amd64, arm64).</p>
<pre>curl -fsSL $base/fane-archive-keyring.gpg | sudo tee /usr/share/keyrings/fane-archive-keyring.gpg &gt;/dev/null
echo "deb [signed-by=/usr/share/keyrings/fane-archive-keyring.gpg] $base stable main" | sudo tee /etc/apt/sources.list.d/fane.list
sudo apt update &amp;&amp; sudo apt install fane</pre>
<p>Source and other platforms: <a href="https://github.com/salsicha/Fane">github.com/salsicha/Fane</a></p>
EOF
echo "apt repository written to $(pwd)"

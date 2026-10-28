#!/bin/sh
# installs the latest mux build for linux into ~/.local, with an app launcher entry and icons
# usage: curl -fsSL https://raw.githubusercontent.com/chokhu-hq/mux/main/scripts/install.sh | sh
# pass --uninstall to remove it again, which leaves sessions and settings in place
set -eu

repo="chokhu-hq/mux"
# a release tag like v0.2.0, or the rolling experimental build
tag="${MUX_VERSION:-experimental}"
bin_dir="${XDG_BIN_HOME:-$HOME/.local/bin}"
data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
sizes="16 32 48 64 128 256 512"

say() {
  printf '%s\n' "$*"
}

fail() {
  printf 'mux: %s\n' "$*" >&2
  exit 1
}

refresh_launcher() {
  update-desktop-database "$data_dir/applications" 2>/dev/null || true
  gtk-update-icon-cache -q "$data_dir/icons/hicolor" 2>/dev/null || true
}

if [ "${1:-}" = "--uninstall" ]; then
  rm -f "$bin_dir/mux" "$data_dir/applications/mux.desktop"
  for size in $sizes; do
    rm -f "$data_dir/icons/hicolor/${size}x${size}/apps/mux.png"
  done
  refresh_launcher
  say "Removed Mux. Your sessions and settings are still in $data_dir/mux"
  exit 0
fi

[ "$(uname -s)" = "Linux" ] || fail "this installer is for Linux, see https://github.com/$repo#download for other systems"

case "$(uname -m)" in
  x86_64 | amd64) arch="x86_64" ;;
  aarch64 | arm64) arch="aarch64" ;;
  *) fail "there's no build for $(uname -m) yet" ;;
esac

name="mux-linux-$arch"
url="https://github.com/$repo/releases/download/$tag/$name.tar.gz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

say "Downloading $url"
if command -v curl >/dev/null 2>&1; then
  curl -fL --progress-bar "$url" -o "$tmp/mux.tar.gz" || fail "couldn't download $url"
elif command -v wget >/dev/null 2>&1; then
  wget -q --show-progress "$url" -O "$tmp/mux.tar.gz" || fail "couldn't download $url"
else
  fail "curl or wget is needed to download Mux"
fi
tar -xzf "$tmp/mux.tar.gz" -C "$tmp"

mkdir -p "$bin_dir" "$data_dir/applications"
# written beside the old binary and moved over it, so a running copy isn't cut off mid-write
cp "$tmp/$name/mux-gui" "$bin_dir/.mux.part"
chmod 755 "$bin_dir/.mux.part"
mv -f "$bin_dir/.mux.part" "$bin_dir/mux"

for size in $sizes; do
  icon_dir="$data_dir/icons/hicolor/${size}x${size}/apps"
  mkdir -p "$icon_dir"
  cp "$tmp/$name/icons/mux-$size.png" "$icon_dir/mux.png"
done
# the entry's name matches the window's app id, which is how desktops find the icon
sed "s|^Exec=.*|Exec=$bin_dir/mux|" "$tmp/$name/mux.desktop" > "$data_dir/applications/mux.desktop"
refresh_launcher

say "Installed Mux to $bin_dir/mux"
case ":$PATH:" in
  *":$bin_dir:"*) say "Open it from your app launcher, or run: mux" ;;
  *) say "Open it from your app launcher, or add $bin_dir to your PATH to run: mux" ;;
esac

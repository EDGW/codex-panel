#!/usr/bin/env bash
# Build a native Debian package from an existing release executable.
set -euo pipefail

if [ "$#" -ne 4 ]; then
  echo "Usage: $0 EXECUTABLE VERSION ARCHITECTURE OUTPUT_DIRECTORY" >&2
  exit 2
fi
binary=$(realpath "$1")
version=$2
architecture=$3
case "$architecture" in
  amd64|arm64) ;;
  *) echo "Unsupported architecture: $architecture" >&2; exit 2 ;;
esac
test "$(dpkg --print-architecture)" = "$architecture"
dpkg --validate-version "$version"
source_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
mkdir -p "$4"
output=$(realpath "$4")
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
chmod 755 "$stage"

mkdir -p "$stage/DEBIAN" "$stage/debian" "$stage/usr/bin" \
  "$stage/usr/share/codex-panel" "$stage/usr/share/doc/codex-panel"
install -m 755 "$binary" "$stage/usr/bin/codex-panel"
install -m 644 "$source_root/destinations.toml" "$stage/usr/share/codex-panel/destinations.toml"
install -m 644 "$source_root/README.md" "$source_root/README.zh_CN.md" \
  "$stage/usr/share/doc/codex-panel/"
install -m 644 "$source_root/LICENSE" "$stage/usr/share/doc/codex-panel/copyright"

# dpkg-shlibdeps uses the build host's package metadata to infer ABI requirements.
cat > "$stage/debian/control" <<EOF
Source: codex-panel
Section: utils
Priority: optional
Maintainer: EDGW <EDGW@users.noreply.github.com>

Package: codex-panel
Architecture: $architecture
Description: Terminal billing panel for Codex CLI
EOF
dependencies=$(cd "$stage" && dpkg-shlibdeps -O -eusr/bin/codex-panel)
dependencies=${dependencies#shlibs:Depends=}
test -n "$dependencies"
installed_size=$(du -sk "$stage/usr" | cut -f1)
cat > "$stage/DEBIAN/control" <<EOF
Package: codex-panel
Version: $version
Architecture: $architecture
Section: utils
Priority: optional
Maintainer: EDGW <EDGW@users.noreply.github.com>
Homepage: https://github.com/EDGW/codex-panel
Installed-Size: $installed_size
Depends: $dependencies, tmux, lsof, ca-certificates
Description: Terminal billing panel for Codex CLI
 Displays session costs alongside Codex CLI using tmux.
 Codex CLI must be installed separately and available in PATH.
EOF
rm -r "$stage/debian"
package="codex-panel_${version}_${architecture}.deb"
dpkg-deb --build --root-owner-group "$stage" "$output/$package"
(cd "$output" && sha256sum "$package" > "$package.sha256")

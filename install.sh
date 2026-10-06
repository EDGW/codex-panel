#!/usr/bin/env bash
# Install the Linux release for the current user; no root access is required.
set -euo pipefail

die() { printf 'codex-panel: %s\n' "$*" >&2; exit 1; }

main() (
  local version=latest
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --version)
        [ "$#" -ge 2 ] || die '--version requires a release tag (for example v0.1.3)'
        version=$2
        [[ "$version" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([.-][a-zA-Z0-9.-]+)?$ ]] || die "Invalid release tag: $version"
        shift 2
        ;;
      -h|--help)
        printf 'Usage: bash install.sh [--version vVERSION]\nInstalls the latest Linux release (including pre-releases) by default, with codex-panel-remove.\n'
        return
        ;;
      *) die "Unknown argument: $1" ;;
    esac
  done

  [ "$(uname -s)" = Linux ] || die 'This installer supports Linux only.'
  local target
  case "$(uname -m)" in
    x86_64|amd64) target=x86_64-unknown-linux-gnu ;;
    aarch64|arm64) target=aarch64-unknown-linux-gnu ;;
    *) die "Unsupported architecture: $(uname -m)" ;;
  esac
  local tool
  for tool in curl tar sha256sum mktemp install readlink awk; do
    command -v "$tool" >/dev/null 2>&1 || die "Required command not found: $tool"
  done
  local libc libc_version libc_major libc_minor
  libc=$(getconf GNU_LIBC_VERSION 2>/dev/null) || die 'Linux releases require glibc 2.39 or newer.'
  libc_version=${libc#glibc }
  [[ "$libc_version" =~ ^([0-9]+)\.([0-9]+) ]] || die "Cannot determine glibc version: $libc"
  libc_major=${BASH_REMATCH[1]}
  libc_minor=${BASH_REMATCH[2]}
  (( 10#$libc_major > 2 || (10#$libc_major == 2 && 10#$libc_minor >= 39) )) || die "Linux releases require glibc 2.39 or newer (found $libc_version)."

  [ -n "${HOME:-}" ] && [[ "$HOME" = /* ]] || die 'HOME must be an absolute path.'
  local bin_dir="$HOME/.local/bin"
  local data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
  [[ "$data_home" = /* ]] || die 'XDG_DATA_HOME must be an absolute path.'
  local install_dir="$data_home/codex-panel/installation"
  local name
  for name in codex-panel codex-panel-remove; do
    if [ -e "$bin_dir/$name" ] || [ -L "$bin_dir/$name" ]; then
      [ -L "$bin_dir/$name" ] && [ "$(readlink "$bin_dir/$name")" = "$install_dir/$name" ] || die "Refusing to overwrite $bin_dir/$name; remove the previous installation first."
    fi
  done

  local asset="codex-panel-$target.tar.gz"
  local work_dir
  work_dir=$(mktemp -d)
  trap 'rm -rf -- "$work_dir"' EXIT
  if [ "$version" = latest ]; then
    command -v python3 >/dev/null 2>&1 || die 'Required command not found: python3 (needed to select the latest release)'
    # GitHub's /latest/download endpoint excludes pre-releases.
    curl --fail --show-error --silent --location --retry 3 \
      --output "$work_dir/releases.json" \
      'https://api.github.com/repos/EDGW/codex-panel/releases?per_page=1'
    version=$(python3 -c '
import json, sys
with open(sys.argv[1]) as source:
    releases = json.load(source)
if not releases:
    sys.exit("No published releases found.")
print(releases[0]["tag_name"])
' "$work_dir/releases.json") || die 'Cannot determine the latest release; try --version vVERSION.'
    [[ "$version" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([.-][a-zA-Z0-9.-]+)?$ ]] || die "Invalid release tag: $version"
  fi
  local base_url="https://github.com/EDGW/codex-panel/releases/download/$version"
  printf 'Downloading %s (%s)…\n' "$asset" "$version"
  curl --fail --show-error --location --retry 3 --output "$work_dir/$asset" "$base_url/$asset"
  curl --fail --show-error --location --retry 3 --output "$work_dir/$asset.sha256" "$base_url/$asset.sha256"
  local digest checksum_name extra
  read -r digest checksum_name extra < "$work_dir/$asset.sha256"
  [[ "$digest" =~ ^[a-fA-F0-9]{64}$ ]] && [ "${checksum_name#\*}" = "$asset" ] && [ -z "$extra" ] || die 'Invalid release checksum file.'
  (cd "$work_dir" && printf '%s  %s\n' "$digest" "$asset" | sha256sum --check --status) || die 'Release checksum verification failed.'
  local package="codex-panel-$target"
  # Extract only the two required regular files from the verified release.
  tar -xzf "$work_dir/$asset" -C "$work_dir" --no-same-owner --no-same-permissions \
    "$package/codex-panel" "$package/destinations.toml"
  for name in codex-panel destinations.toml; do
    [ -f "$work_dir/$package/$name" ] && [ ! -L "$work_dir/$package/$name" ] || die "Release is missing a regular file: $name"
  done
  local installed_version
  installed_version=$(env -u CC_PANEL_PROCESS_KIND "$work_dir/$package/codex-panel" --panel-version) || die 'The release executable cannot run on this system.'
  [[ "$installed_version" = 'codex-panel v'* ]] || die 'Unexpected release executable version.'
  [ "$installed_version" = "codex-panel $version" ] || die 'The release executable does not match the requested version.'

  local rc_file zdotdir="${ZDOTDIR:-$HOME}"
  [[ "$zdotdir" = /* ]] || die 'ZDOTDIR must be an absolute path.'
  local path_block
  # POSIX shell quoting also works in Bash and Zsh startup files.
  local quoted_bin="'${bin_dir//\'/\'\\\'\'}'"
  path_block=$(cat <<EOF
# >>> codex-panel PATH >>>
case ":\$PATH:" in
  *:$quoted_bin:*) ;;
  *) export PATH=$quoted_bin:"\$PATH" ;;
esac
# <<< codex-panel PATH <<<
EOF
  )
  local -a rc_files=("$HOME/.profile" "$HOME/.bashrc" "$zdotdir/.zshrc")
  if [ -f "$HOME/.bash_profile" ]; then rc_files+=("$HOME/.bash_profile"); fi
  mkdir -p "$bin_dir" "$install_dir"
  # Stage a new executable before replacing it, so upgrading a running copy works.
  install -m 755 "$work_dir/$package/codex-panel" "$install_dir/.codex-panel.new"
  install -m 644 "$work_dir/$package/destinations.toml" "$install_dir/.destinations.toml.new"

  {
    printf '#!/usr/bin/env bash\nset -euo pipefail\n'
    printf 'install_dir=%q\nbin_dir=%q\n' "$install_dir" "$bin_dir"
    printf 'path_block=%q\n' "$path_block"
    printf 'rc_files=('
    printf ' %q' "${rc_files[@]}"
    printf ' )\n'
    cat <<'REMOVE'
if [ "$#" -gt 0 ]; then
  case "$1" in
    -h|--help) printf 'Usage: codex-panel-remove\nRemoves this script installation and its PATH entries; preserves personal configuration.\n'; exit 0 ;;
    *) printf 'Unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
fi
for name in codex-panel codex-panel-remove; do
  link="$bin_dir/$name"
  if [ -L "$link" ] && [ "$(readlink "$link")" = "$install_dir/$name" ]; then
    rm -- "$link"
  fi
done
# Remove only the exact PATH block installed by this script.
for rc_file in "${rc_files[@]}"; do
  [ -f "$rc_file" ] || continue
  edited=$(mktemp)
  if ! CODEX_PANEL_PATH_BLOCK="$path_block" awk '
    BEGIN { count = split(ENVIRON["CODEX_PANEL_PATH_BLOCK"], lines, "\n") }
    { content[++total] = $0 }
    END {
      for (i = 1; i <= total; i++) {
        matched = (content[i] == lines[1])
        for (j = 2; matched && j <= count; j++)
          matched = (content[i + j - 1] == lines[j])
        if (matched) i += count - 1
        else print content[i]
      }
    }
  ' "$rc_file" > "$edited"; then
    rm -- "$edited"
    exit 1
  fi
  if ! cmp -s "$rc_file" "$edited"; then cat "$edited" > "$rc_file"; fi
  rm -- "$edited"
done
rm -f -- "$install_dir/codex-panel" "$install_dir/destinations.toml" "$install_dir/codex-panel-remove"
rmdir -- "$install_dir" 2>/dev/null || true
rmdir -- "$(dirname "$install_dir")" 2>/dev/null || true
printf 'codex-panel removed. Personal configuration has been preserved.\n'
REMOVE
  } > "$install_dir/.codex-panel-remove.new"
  chmod 755 "$install_dir/.codex-panel-remove.new"
  mv -f -- "$install_dir/.codex-panel.new" "$install_dir/codex-panel"
  mv -f -- "$install_dir/.destinations.toml.new" "$install_dir/destinations.toml"
  mv -f -- "$install_dir/.codex-panel-remove.new" "$install_dir/codex-panel-remove"
  ln -sfn -- "$install_dir/codex-panel" "$bin_dir/codex-panel"
  ln -sfn -- "$install_dir/codex-panel-remove" "$bin_dir/codex-panel-remove"

  # Retain existing blocks on reinstall so uninstall can still clean them up.
  case ":$PATH:" in
    *:"$bin_dir":*) ;;
    *)
      for rc_file in "${rc_files[@]}"; do
        if ! [ -f "$rc_file" ] || ! grep -Fqx '# >>> codex-panel PATH >>>' "$rc_file"; then
          mkdir -p "$(dirname "$rc_file")"
          printf '\n%s\n' "$path_block" >> "$rc_file"
        fi
      done
      printf 'PATH configured for new terminals. For this terminal, run:\n  export PATH=%s:"$PATH"\n' "$quoted_bin"
      ;;
  esac
  printf 'Installed %s\nRun: codex-panel\nUninstall: codex-panel-remove\n' "$installed_version"
  for tool in tmux lsof codex; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      printf 'Install %s separately and make it available in PATH before running codex-panel.\n' "$tool"
    fi
  done
)

# Read the whole script before starting, so piping to Bash never consumes input
# intended for a command launched by the installer.
main "$@"

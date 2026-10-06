# codex-panel

English | [简体中文](./README.zh_CN.md)

## Introduction

Adds a cost panel to Codex CLI: the native interface stays at the top, while the panel below shows the current session's total cost, the additional cost incurred during monitoring, and the request count.

Supports Linux x86_64 and ARM64, and macOS Apple Silicon. Currently supports billing platforms based on [Claude Code Hub](https://github.com/ding113/claude-code-hub). Billing amounts can be converted to a payment currency using a fixed value, a JSON source, or an XML source.

## Download and Usage

### Linux One-line Install

```sh
curl -fsSL https://raw.githubusercontent.com/EDGW/codex-panel/main/install.sh | bash
```

The installer detects x86_64 or ARM64, downloads the latest GitHub release (including pre-releases), verifies its SHA-256 checksum, and installs for the current user without sudo. Selecting the latest release requires Python 3; `--version` skips this requirement. Linux releases require glibc 2.39 or newer. Install tmux, lsof, and Codex CLI separately.

The `codex-panel` and `codex-panel-remove` commands are placed in `~/.local/bin`; the executable and defaults are stored in `${XDG_DATA_HOME:-$HOME/.local/share}/codex-panel/installation/`. If needed, the installer adds `~/.local/bin` to PATH in Bash, Zsh, and POSIX shell startup files. Open a new terminal or run the printed `export PATH=…` command to use them in your current terminal.

Run the same installation command again to upgrade. To select a specific release:

```sh
curl -fsSL https://raw.githubusercontent.com/EDGW/codex-panel/main/install.sh | bash -s -- --version v0.1.2
```

Uninstall with:

```sh
codex-panel-remove
```

This removes the script installation and its PATH entries, while preserving `~/.codex-panel/` and Codex configuration. Choose one installation method; the script refuses to overwrite commands installed by another method.

### Homebrew

Linux and macOS also support Homebrew:

```sh
brew install EDGW/tap/codex-panel
```

Upgrade with:

```sh
brew update
brew upgrade codex-panel
```

Install Codex CLI separately. It must support `--remote unix://` and the daemon; version 0.159.3 has been verified.

### Release Archives

Download the archive for your platform from [GitHub Releases](https://github.com/EDGW/codex-panel/releases):

| Platform | Archive |
| --- | --- |
| Linux x86_64 | `codex-panel-x86_64-unknown-linux-gnu.tar.gz` |
| Linux ARM64 | `codex-panel-aarch64-unknown-linux-gnu.tar.gz` |
| macOS Apple Silicon | `codex-panel-aarch64-apple-darwin.tar.gz` |

Install tmux and Codex CLI, then extract and run. For example, on Linux x86_64:

```sh
sha256sum --check codex-panel-x86_64-unknown-linux-gnu.tar.gz.sha256
tar -xzf codex-panel-x86_64-unknown-linux-gnu.tar.gz
cd codex-panel-x86_64-unknown-linux-gnu
./codex-panel
```

Use the corresponding filename for ARM64 or macOS; on macOS, verify the checksum with `shasum -a 256 -c FILE.sha256`. Archives contain the executable, default `destinations.toml`, both READMEs, and license. Keep the executable and defaults together. Rust is only required for source builds.

Linux packages are built on Ubuntu 24.04 and require compatible system libraries (glibc 2.39 or newer). For a manual Linux installation, place the executable in `~/.local/bin/codex-panel` and defaults in `${XDG_DATA_HOME:-$HOME/.local/share}/codex-panel/destinations.toml`. Upgrade by replacing both files; personal overrides remain separate.

### Debian / Ubuntu Packages

Download `codex-panel_<version>_amd64.deb` or `codex-panel_<version>_arm64.deb` and its `.sha256` file from the same release. For example:

```sh
sha256sum --check codex-panel_0.1.2_amd64.deb.sha256
sudo apt install ./codex-panel_0.1.2_amd64.deb
codex-panel --panel-version
```

Use `arm64` for ARM64. apt installs tmux, lsof, CA certificates and required system libraries; install Codex CLI separately. The executable is installed at `/usr/bin/codex-panel`, defaults at `/usr/share/codex-panel/destinations.toml`. Inspect system dependencies with `dpkg-deb -I PACKAGE.deb`.

Upgrade by downloading the new deb and running `sudo apt install ./NEW_PACKAGE.deb`. There is no APT repository, so `apt upgrade` does not discover new releases. Uninstall with `sudo apt remove codex-panel`; personal configuration is preserved. Choose one installation method to avoid another copy taking precedence in PATH.

Publishing a GitHub Release builds and uploads the archives, Linux deb packages and SHA-256 files. Tags must use `vVERSION` matching `Cargo.toml`. Draft releases do not trigger builds.

## Build from Source

Requires the Rust toolchain, tmux, and Codex CLI. Codex must support `--remote unix://` and the daemon; version 0.159.3 has been verified.

```sh
cargo build --release
cp destinations.toml target/release/destinations.toml
./target/release/codex-panel
```

When moving the program, keep `destinations.toml` beside the executable. When launched through a symlink, configuration is read beside the resolved executable. Run `codex-panel --panel-version` to print the panel version; other startup arguments are passed directly to Codex.

Codex opens in the working directory where you run `codex-panel`. Run the executable from your project directory, or use `-C /path/to/project` / `--cd /path/to/project` to select another directory.

The program reads authentication configuration from `CODEX_HOME` (default: `~/.codex`). API keys are resolved in this order: `PREVX_API_KEY` → the environment variable specified by the current provider's `env_key` → `experimental_bearer_token` → `OPENAI_API_KEY` in `auth.json`. If you only use ChatGPT login, provide a Hub API key through `PREVX_API_KEY`.

In the panel, `Session total` is the current session's total cost, and `Since monitoring` is the additional cost accumulated after the first successful query, which resets on restart. `Requests` is the request count returned by the billing platform, and `Estimated` indicates a cost estimate based on token pricing.

Click **Open Settings** in the lower panel to view configuration and conversion status; press Esc to return. Settings are read-only, so restart the program after changing the configuration. Exiting Codex closes the interface; press `Ctrl-b`, then `d` to detach while keeping the session running.

## Configuration Format

Configuration uses TOML. Every file must include `version = 1`.

| File | Purpose |
| --- | --- |
| `destinations.toml` | Default configuration; release builds use the discovery order below, while development builds read it from the project directory |
| `~/.codex-panel/destinations.toml` | Optional user configuration, created manually; overrides defaults by instance `id` |

Use the user configuration for your changes. To override an existing instance, specify its `id` and only the fields you want to change. Tables are merged by field, while arrays are replaced entirely. Changing an instance's `type` replaces its `config`; changing a conversion source's `type` replaces the entire `source`.

You can also set `CC_PANEL_CONFIG` to specify the user configuration path, or `CC_PANEL_DEFAULTS_CONFIG` to specify the default configuration path. Explicitly specified files must exist.

### Linux default configuration discovery

`CC_PANEL_DEFAULTS_CONFIG` takes precedence. An empty value, missing file, or read failure is an error. When unset, release builds search in this order:

1. `destinations.toml` beside the resolved executable.
2. `${XDG_DATA_HOME:-$HOME/.local/share}/codex-panel/destinations.toml`.
3. `/usr/local/share/codex-panel/destinations.toml`.
4. `/usr/share/codex-panel/destinations.toml`.

The first existing file is selected. Unreadable files, broken symlinks, and invalid contents are errors. If none exist, the error lists the searched paths. User overrides still come from `CC_PANEL_CONFIG` or `~/.codex-panel/destinations.toml`; upgrades do not overwrite them. Development builds continue to use the project defaults.

### Billing Instances

```toml
version = 1

[[destinations]]
id = "my-hub"
type = "claude-code-hub"
name = "My Hub"
enabled = true
api_urls = ["https://api.example.com/v1"]

[destinations.config]
hub_url = "https://billing.example.com"
```

| Field | Description |
| --- | --- |
| `id` | Unique instance identifier, also used to match user overrides |
| `type` | Billing type; currently supports `claude-code-hub` |
| `name` | Display name |
| `enabled` | Whether the instance is enabled; defaults to `true` |
| `api_urls` | Matches the Codex provider's `base_url`; each URL can belong to only one enabled instance |
| `config.hub_url` | Billing site URL, containing only the scheme, host, and optional port |

Set the provider's `base_url` in Codex's `config.toml`. Temporary overrides supplied through CLI `-c` are currently not reflected in the panel.

### Payment Currency Conversion

Add the following configuration under an instance:

```toml
[destinations.conversion]
enabled = true
currency = "CNY"
multiplier = 1.0

[destinations.conversion.source]
type = "value"
value = 0.14
```

Payment amount = billing amount × source value × `multiplier`. `currency` is the payment currency, and `multiplier` defaults to 1. Source values and conversion rates must be finite positive numbers. If conversion is not configured or `conversion.enabled` is set to `false`, only the original billing currency is shown. Conversion failures do not affect billing display.

`source` supports the following formats:

| `type` | Required fields | Description |
| --- | --- | --- |
| `value` | `value` | Fixed number, such as `value = 0.14` |
| `json` | `url`, `pointer` | HTTP URL and JSON Pointer, such as `pointer = "/data/price"` |
| `xml` | `url`, `xpath` | HTTP URL and XPath, such as `xpath = "/pricing/rate/text()"` |

JSON/XML extraction must return a number or a numeric string. XPath node queries must match exactly one node. Both sources support `cache_seconds` (default: 300) and `timeout_seconds` (default: 10). XML namespace prefixes can be configured with `namespaces = { p = "namespace URI" }`.

JSON/XML sources can include a success condition in `source`, such as `expect = { pointer = "/ok", equals = true }`; for XML, use `xpath` instead of `pointer`. Overrides that keep the same source type inherit the existing condition. Set `expect = false` to clear it.

# codex-panel

English | [简体中文](./README.zh_CN.md)

## Introduction

Adds a cost panel to Codex CLI: the native interface stays at the top, while the panel below shows the current session's total cost, the additional cost incurred during monitoring, and the request count.

Currently supports billing platforms based on [Claude Code Hub](https://github.com/ding113/claude-code-hub). Billing amounts can be converted to a payment currency using a fixed value, a JSON source, or an XML source.

## Download and Usage

On Apple Silicon Macs, download `codex-panel-aarch64-apple-darwin.tar.gz` from [GitHub Releases](https://github.com/EDGW/codex-panel/releases). Install tmux and Codex CLI, then extract and run:

```sh
tar -xzf codex-panel-aarch64-apple-darwin.tar.gz
cd codex-panel-aarch64-apple-darwin
./codex-panel
```

The archive includes the executable and its default `destinations.toml`; keep them together. Rust is only required when building from source. Each release also includes a `.tar.gz.sha256` checksum file.

Publishing a GitHub Release automatically runs the release workflow against its tag, builds the Apple Silicon executable, and uploads the archive and checksum. Draft releases do not trigger the build.

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
| `destinations.toml` | Default configuration; release builds read the file beside the executable, while development builds read it from the project directory |
| `~/.codex-panel/destinations.toml` | Optional user configuration, created manually; overrides defaults by instance `id` |

Use the user configuration for your changes. To override an existing instance, specify its `id` and only the fields you want to change. Tables are merged by field, while arrays are replaced entirely. Changing an instance's `type` replaces its `config`; changing a conversion source's `type` replaces the entire `source`.

You can also set `CC_PANEL_CONFIG` to specify the user configuration path, or `CC_PANEL_DEFAULTS_CONFIG` to specify the default configuration path. Explicitly specified files must exist.

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

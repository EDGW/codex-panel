# codex-panel

English | [简体中文](./README.zh_CN.md)

## Introduction

Adds a cost panel to Codex CLI: the native interface stays at the top, while the panel below shows the current session's total cost, the additional cost incurred during monitoring, and the request count.

Supports billing platforms based on [Claude Code Hub](https://github.com/ding113/claude-code-hub) and token-based cost estimates using models.dev. Billing amounts can be converted to a payment currency using a fixed value, a JSON source, or an XML source.

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

In billed mode, `Session total` shows the platform's current session bill and request count; `Since monitoring` accumulates increases after the first successful query. In `Estimated` mode, Session shows `Not available`. Monitoring adds each observed response's own token usage at that response's model price, including the first response after monitoring starts. `Requests` counts successfully priced responses; duplicate usage notifications and turn completion events do not add requests. Monitoring resets on restart; historical session usage is not estimated.

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
| `type` | Billing type; supports `claude-code-hub` and `models_dev` |
| `name` | Display name |
| `enabled` | Whether the instance is enabled; defaults to `true` |
| `api_urls` | Matches the Codex provider's `base_url`; each URL can belong to only one enabled instance |
| `config.hub_url` | Billing site URL, containing only the scheme, host, and optional port |

Set the provider's `base_url` in Codex's `config.toml`. Temporary overrides supplied through CLI `-c` are currently not reflected in the panel.

### models.dev

`models_dev` estimates token costs using the [models.dev catalog](https://models.dev/api.json?type=all). The defaults were rebuilt from 226 catalog providers: 196 enabled providers with 198 exact API URL mappings, plus the two Hub instances. The remaining 30 providers are disabled with comments explaining missing, local, account-specific or shared endpoints. Names, provider IDs and published URLs come from the catalog; matching existing concrete API URLs are retained as explicit aliases. Runway and SambaNova are absent from the current catalog and have no default price destination. `apikey-names.json` records the crawl source, timestamp, enabled mappings and disabled providers.

```toml
[[destinations]]
id = "my-provider"
type = "models_dev"
name = "My Provider"
api_urls = ["https://api.example.com/v1"]

[destinations.config]
provider_id = "provider-id-from-models-dev"
source_url = "https://models.dev/api.json?type=all"
cache_seconds = 300
timeout_seconds = 10
note = "Optional pricing information shown when this destination is recognized."

# Optional explicit mapping from API identifiers to catalog model IDs.
[destinations.config.model_aliases]
"api-model-id" = "catalog-model-id"
```

`provider_id` is required. `source_url` defaults to `https://models.dev/api.json`, cache to 300 seconds and timeout to 10 seconds. `note` is optional and appears once in the selected destination's recognition detail. The built-in DeepSeek instance notes that its published prices cover only off-peak billing and estimates may understate actual charges. There is no global time-of-day pricing notice.

Lookup uses `catalog[provider_id].models[model_id].cost`, matching exact API model IDs, including IDs containing `/`. Unknown IDs require an explicit `model_aliases` entry. Missing ordinary input/output prices, malformed responses and network failures report errors. Optional cache read/write and reasoning rates fall back to ordinary input/output rates. Zero is accepted only when explicitly published by the source. The adapter uses the published base `cost` rates; it does not reconstruct context tiers, subscription charges or non-token billing.

models.dev prices are USD per million tokens. Billing stays in USD unless a separate payment conversion is configured; the old `provider_slug` and `display_currency` fields are replaced by `provider_id` and the source's fixed USD currency. Existing user overrides using `type = "llmrates"` must migrate to `models_dev` and its config fields.

Estimated accounting uses immutable per-response model and usage snapshots, so later model switches cannot reprice earlier responses. It does not read or write session history in `~/.codex-panel/history`. Before a model is available, Monitoring shows zero cost and zero requests while prices wait. Once the configured or session model is available, prices are fetched immediately without requiring a session or token usage. Price failures preserve accumulated amounts and keep responses queued for retry at their original models; failures never count as zero cost. Responses without usable per-request telemetry are reported as unavailable rather than charging historical totals.

The display destination area shows only fetched prices. The configured note appears once in recognition details; query errors stay in the host status. Settings show provider ID, source, USD billing currency, cache, timeout and model aliases. Data attribution: [models.dev](https://models.dev), maintained in [anomalyco/models.dev](https://github.com/anomalyco/models.dev).

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

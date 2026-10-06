# codex-panel

[English](./README.md) | 简体中文

## 介绍

为 Codex CLI 增加费用面板：上方保留原生界面，下方显示当前会话的总费用、本次监视期间新增的费用和请求数。

目前支持基于 [Claude Code Hub](https://github.com/ding113/claude-code-hub) 的账单平台，可通过固定值、JSON 或 XML 来源将账单金额换算成付款币种。

## 下载与使用

Apple Silicon Mac 用户可从 [GitHub Releases](https://github.com/EDGW/codex-panel/releases) 下载 `codex-panel-aarch64-apple-darwin.tar.gz`。安装 tmux 和 Codex CLI 后，解压并运行：

```sh
tar -xzf codex-panel-aarch64-apple-darwin.tar.gz
cd codex-panel-aarch64-apple-darwin
./codex-panel
```

压缩包包含可执行文件和默认的 `destinations.toml`，移动时请保持两者在同一目录。只有从源码构建时才需要 Rust。每个发布版本还提供 `.tar.gz.sha256` 校验文件。

发布 GitHub Release 后，工作流会自动检出对应标签，编译 Apple Silicon 版本，并上传压缩包和校验文件。保存为草稿不会触发构建。

## 从源码构建

需要 Rust 工具链、tmux 和 Codex CLI。Codex 需支持 `--remote unix://` 和 daemon，已验证版本为 0.159.3。

```sh
cargo build --release
cp destinations.toml target/release/destinations.toml
./target/release/codex-panel
```

移动程序时，将 `destinations.toml` 一起放在可执行文件旁。启动参数直接传给 Codex。

Codex 默认在运行 `codex-panel` 时的当前工作目录中打开。可在项目目录中运行可执行文件，或通过 `-C /path/to/project` / `--cd /path/to/project` 指定其他目录。

程序读取 `CODEX_HOME`（默认 `~/.codex`）中的认证配置。API key 优先级为：`PREVX_API_KEY` → 当前 provider 的 `env_key` 环境变量 → `experimental_bearer_token` → `auth.json` 中的 `OPENAI_API_KEY`。仅使用 ChatGPT 登录时，需通过 `PREVX_API_KEY` 额外提供 Hub API key。

面板中，`Session total` 是当前会话的总费用，`Since monitoring` 是首次成功查询后累计的新增费用，重启后重置；`Requests` 是账单平台返回的请求数，`Estimated` 表示按 token 单价估算。

点击下方面板的 **Open Settings** 可查看配置与换算状态，按 Esc 返回。设置页仅供查看，修改配置后需重启。退出 Codex 会关闭界面；按 `Ctrl-b` 再按 `d` 可脱离并保留会话。

## 配置文件格式

配置使用 TOML，所有文件都需包含 `version = 1`。

| 文件 | 用途 |
| --- | --- |
| `destinations.toml` | 默认配置；发布构建读取可执行文件旁的文件，开发构建读取项目目录中的文件 |
| `~/.codex-panel/destinations.toml` | 可选用户配置，需自行创建，按实例 `id` 覆盖默认值 |

建议修改用户配置。覆盖已有实例时只需填写 `id` 和要修改的字段；表按字段合并，数组整体替换。改变实例 `type` 会替换其 `config`，改变转换来源 `type` 会替换整个 `source`。

也可通过 `CC_PANEL_CONFIG` 指定用户配置路径，通过 `CC_PANEL_DEFAULTS_CONFIG` 指定默认配置路径；指定的文件必须存在。

### 账单实例

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

| 字段 | 说明 |
| --- | --- |
| `id` | 实例唯一标识，也用于匹配用户覆盖配置 |
| `type` | 账单类型，目前支持 `claude-code-hub` |
| `name` | 显示名称 |
| `enabled` | 是否启用，默认 `true` |
| `api_urls` | 匹配 Codex provider 的 `base_url`，每个地址只能属于一个启用的实例 |
| `config.hub_url` | 账单站点地址，只能包含协议、主机和可选端口 |

将 provider 的 `base_url` 写入 Codex `config.toml`；通过 CLI `-c` 临时覆盖的地址暂不同步到面板。

### 付款转换

在实例下添加以下配置：

```toml
[destinations.conversion]
enabled = true
currency = "CNY"
multiplier = 1.0

[destinations.conversion.source]
type = "value"
value = 0.14
```

付款金额 = 账单金额 × 来源数值 × `multiplier`。`currency` 是付款币种，`multiplier` 默认是 1，来源数值和换算率须为有限正数。未配置转换或将 `conversion.enabled` 设为 `false` 时，只显示原始账单币种；转换失败不影响账单显示。

`source` 支持以下格式：

| `type` | 必填字段 | 说明 |
| --- | --- | --- |
| `value` | `value` | 固定数值，如 `value = 0.14` |
| `json` | `url`、`pointer` | HTTP 地址及 JSON Pointer，如 `pointer = "/data/price"` |
| `xml` | `url`、`xpath` | HTTP 地址及 XPath，如 `xpath = "/pricing/rate/text()"` |

JSON/XML 提取结果须为数字或数字字符串，XPath 节点查询须匹配一个节点。两者可设置 `cache_seconds`（默认 300）和 `timeout_seconds`（默认 10）；XML 可通过 `namespaces = { p = "命名空间 URI" }` 配置前缀。

JSON/XML 可在 `source` 中添加成功条件，如 `expect = { pointer = "/ok", equals = true }`；XML 使用 `xpath` 替代 `pointer`。同类型覆盖会继承原条件，设置 `expect = false` 可清除。

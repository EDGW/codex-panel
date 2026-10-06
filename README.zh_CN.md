# codex-panel

[English](./README.md) | 简体中文

## 介绍

为 Codex CLI 增加费用面板：上方保留原生界面，下方显示当前会话的总费用、本次监视期间新增的费用和请求数。

支持基于 [Claude Code Hub](https://github.com/ding113/claude-code-hub) 的账单平台，以及使用 models.dev 聚合价格估算 token 费用的 provider。可通过固定值、JSON 或 XML 来源将账单金额换算成付款币种。

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

移动程序时，将 `destinations.toml` 一起放在可执行文件旁。通过 symlink 启动时，配置从解析后的实际可执行文件所在目录读取。运行 `codex-panel --panel-version` 可查看面板版本，其余启动参数直接传给 Codex。

Codex 默认在运行 `codex-panel` 时的当前工作目录中打开。可在项目目录中运行可执行文件，或通过 `-C /path/to/project` / `--cd /path/to/project` 指定其他目录。

程序读取 `CODEX_HOME`（默认 `~/.codex`）中的认证配置。API key 优先级为：`PREVX_API_KEY` → 当前 provider 的 `env_key` 环境变量 → `experimental_bearer_token` → `auth.json` 中的 `OPENAI_API_KEY`。仅使用 ChatGPT 登录时，需通过 `PREVX_API_KEY` 额外提供 Hub API key。

账单模式下，`Session total` 显示平台返回的当前会话账单和请求数，`Since monitoring` 累计首次成功查询之后的新增费用。`Estimated` 模式下 Session 显示 `Not available`，Monitoring 按每个观察到的响应自身用量和所用模型的价格逐笔累加，包括开始监控后的第一个响应。`Requests` 统计成功计价的响应数，重复用量通知和回合完成事件不会增加请求数。监控费用重启后重置，不估算会话的历史用量。

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
| `type` | 账单类型，支持 `claude-code-hub` 和 `models_dev` |
| `name` | 显示名称 |
| `enabled` | 是否启用，默认 `true` |
| `api_urls` | 匹配 Codex provider 的 `base_url`，每个地址只能属于一个启用的实例 |
| `config.hub_url` | 账单站点地址，只能包含协议、主机和可选端口 |

将 provider 的 `base_url` 写入 Codex `config.toml`；通过 CLI `-c` 临时覆盖的地址暂不同步到面板。

### models.dev

`models_dev` 使用 [models.dev 数据集](https://models.dev/api.json?type=all) 估算 token 费用。默认配置根据当前 226 个供应商重写：启用 196 个供应商，覆盖 198 个精确 API 地址，并保留两个 Hub 实例。另外 30 个条目因缺少地址、本地地址、账户占位符或共用地址而默认禁用，原因写在注释中。名称、供应商 ID 和已发布地址来自数据集；对应供应商已有的明确 API 地址保留为配置别名。当前数据集没有 Runway 和 SambaNova，因此不为它们配置默认价格源。`apikey-names.json` 保存抓取来源、时间、启用的地址映射和禁用原因。

```toml
[[destinations]]
id = "my-provider"
type = "models_dev"
name = "My Provider"
api_urls = ["https://api.example.com/v1"]

[destinations.config]
provider_id = "models-dev-provider-id"
source_url = "https://models.dev/api.json?type=all"
cache_seconds = 300
timeout_seconds = 10
note = "识别到这个实例后显示的可选价格说明。"

# 可选：API 模型 ID 到数据集模型 ID 的明确映射。
[destinations.config.model_aliases]
"api-model-id" = "catalog-model-id"
```

`provider_id` 必填。`source_url` 默认 `https://models.dev/api.json`，缓存默认 300 秒，超时默认 10 秒。`note` 可选，只在选中实例的识别说明中显示一次。DeepSeek 默认单独配置英文提示：`Only off-peak prices are shown; estimated costs may be lower than actual charges.`，说明只展示低峰期价格，估算费用可能偏低。原来的通用峰谷提示已移除。

按 `catalog[provider_id].models[model_id].cost` 读取价格，只匹配准确的 API 模型 ID，支持包含 `/` 的 ID。不同的 API 别名需要配置 `model_aliases`。未知模型、缺失普通输入或输出价格、无效响应和网络失败均报错。缺失缓存读取、写入或推理单价时，使用普通输入或输出单价；只有源数据明确发布零价时才视为免费。使用 `cost` 发布的基础价格，不复现上下文阶梯、订阅费用或非 token 计费。

models.dev 价格单位为美元 / 百万 tokens。账单以 USD 展示；支付币种转换仍需单独配置。原来的 `provider_slug`、`display_currency` 改为 `provider_id` 和源数据固定的 USD 币种。用户覆盖文件若仍使用 `type = "llmrates"`，需要迁移到 `models_dev` 及其配置字段。

Estimated 使用每个响应的模型和用量快照，切换模型不会重新计算之前的响应；不读取或写入 `~/.codex-panel/history`。尚无模型时，Monitoring 显示零费用和零请求，价格显示等待。获取到配置或会话模型后立即查价，无需等待会话或用量。价格查询失败保留累计金额，响应留在队列中按原模型重试，不把失败当成零费用；缺少可用的单次响应用量时报告错误，不使用历史总用量代替。

display 的数据源区域只显示获取到的价格；`note` 只在识别说明中显示一次，查询错误由面板的状态区域展示。设置展示供应商 ID、来源、USD 币种、缓存、超时和模型别名。数据来源：[models.dev](https://models.dev)，仓库为 [anomalyco/models.dev](https://github.com/anomalyco/models.dev)。

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

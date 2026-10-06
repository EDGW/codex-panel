# codex-panel

[English](./README.md) | 简体中文

## 介绍

为 Codex CLI 增加费用面板：上方保留原生界面，下方显示当前会话的总费用、本次监视期间新增的费用和请求数。

支持 Linux x86_64、ARM64 和 macOS Apple Silicon。目前支持基于 [Claude Code Hub](https://github.com/ding113/claude-code-hub) 的账单平台，可通过固定值、JSON 或 XML 来源将账单金额换算成付款币种。

## 下载与使用

推荐在 Linux 和 macOS 上通过 Homebrew 安装：

```sh
brew install EDGW/tap/codex-panel
```

升级时运行：

```sh
brew update
brew upgrade codex-panel
```

Codex CLI 需单独安装，需支持 `--remote unix://` 和 daemon，已验证版本为 0.159.3。

### 压缩包

从 [GitHub Releases](https://github.com/EDGW/codex-panel/releases) 下载对应平台的压缩包：

| 平台 | 压缩包 |
| --- | --- |
| Linux x86_64 | `codex-panel-x86_64-unknown-linux-gnu.tar.gz` |
| Linux ARM64 | `codex-panel-aarch64-unknown-linux-gnu.tar.gz` |
| macOS Apple Silicon | `codex-panel-aarch64-apple-darwin.tar.gz` |

安装 tmux 和 Codex CLI 后，解压并运行。以 Linux x86_64 为例：

```sh
sha256sum --check codex-panel-x86_64-unknown-linux-gnu.tar.gz.sha256
tar -xzf codex-panel-x86_64-unknown-linux-gnu.tar.gz
cd codex-panel-x86_64-unknown-linux-gnu
./codex-panel
```

ARM64 和 macOS 使用对应文件名；macOS 使用 `shasum -a 256 -c 文件.sha256` 校验。压缩包包含可执行文件、默认的 `destinations.toml`、中英文说明和许可证，移动时请保持程序和默认配置在同一目录。只有从源码构建时才需要 Rust。

Linux 包在 Ubuntu 24.04 上构建，需要兼容的系统库（glibc 2.39 或更新版本）。手动安装时，可将程序放在 `~/.local/bin/codex-panel`，默认配置放在 `${XDG_DATA_HOME:-$HOME/.local/share}/codex-panel/destinations.toml`。升级时替换这两个文件，个人配置独立保留。

### Debian / Ubuntu 安装包

从同一 Release 下载 `codex-panel_<版本>_amd64.deb` 或 `codex-panel_<版本>_arm64.deb` 及对应的 `.sha256` 文件。例如：

```sh
sha256sum --check codex-panel_0.1.2_amd64.deb.sha256
sudo apt install ./codex-panel_0.1.2_amd64.deb
codex-panel --panel-version
```

ARM64 使用 `arm64` 文件。apt 会安装 tmux、lsof、CA 证书及所需系统库，Codex CLI 需单独安装。程序安装到 `/usr/bin/codex-panel`，默认配置安装到 `/usr/share/codex-panel/destinations.toml`。可通过 `dpkg-deb -I 文件.deb` 查看系统依赖。

下载新版 deb 后再次执行 `sudo apt install ./新版文件.deb` 即可替换旧版本。暂不提供 APT 仓库，`apt upgrade` 不会自动发现本项目的新版本。通过 `sudo apt remove codex-panel` 卸载，个人配置保留。建议选择一种安装方式，避免 PATH 命中其他副本。

发布 GitHub Release 后，工作流会构建并上传上述压缩包、Linux deb 包及 SHA-256 校验文件。标签使用 `v版本号`，需与 `Cargo.toml` 一致。保存为草稿不会触发构建。

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

面板中，`Session total` 是当前会话的总费用，`Since monitoring` 是首次成功查询后累计的新增费用，重启后重置；`Requests` 是账单平台返回的请求数，`Estimated` 表示按 token 单价估算。

点击下方面板的 **Open Settings** 可查看配置与换算状态，按 Esc 返回。设置页仅供查看，修改配置后需重启。退出 Codex 会关闭界面；按 `Ctrl-b` 再按 `d` 可脱离并保留会话。

## 配置文件格式

配置使用 TOML，所有文件都需包含 `version = 1`。

| 文件 | 用途 |
| --- | --- |
| `destinations.toml` | 默认配置；发布构建按下方顺序查找，开发构建读取项目目录中的文件 |
| `~/.codex-panel/destinations.toml` | 可选用户配置，需自行创建，按实例 `id` 覆盖默认值 |

建议修改用户配置。覆盖已有实例时只需填写 `id` 和要修改的字段；表按字段合并，数组整体替换。改变实例 `type` 会替换其 `config`，改变转换来源 `type` 会替换整个 `source`。

也可通过 `CC_PANEL_CONFIG` 指定用户配置路径，通过 `CC_PANEL_DEFAULTS_CONFIG` 指定默认配置路径；指定的文件必须存在。

### Linux 默认配置查找

`CC_PANEL_DEFAULTS_CONFIG` 优先级最高；已设置但为空、文件缺失或读取失败时直接报错。未设置时，发布构建按顺序查找：

1. 实际可执行文件旁的 `destinations.toml`（解析软链接）。
2. `${XDG_DATA_HOME:-$HOME/.local/share}/codex-panel/destinations.toml`。
3. `/usr/local/share/codex-panel/destinations.toml`。
4. `/usr/share/codex-panel/destinations.toml`。

使用第一个存在的文件；文件不可读、软链接损坏或内容错误时直接报错。全部缺失时列出查找路径。个人配置仍从 `CC_PANEL_CONFIG` 或 `~/.codex-panel/destinations.toml` 读取并合并，升级不会改写个人配置。

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

# LLMRates 重构计划与完成记录

目标：以 LLMRates 聚合价格替换 DeepSeek destination，按 provider slug 获取模型价格，优先使用 display currency，缺失时回退至源币种；普通面板提供 destination 自有展示区；覆盖 apikey-names.json 的全部 mappings。

1. 已完成：核实在线 `/api/dataset` 的真实响应、每百万 token 单价、多币种、标准价格行和模型标识；使用完整数据集实现币种选择。
2. 已完成：审计接口和测试边界。具体 provider 留在适配器与注册组装处；配置发现、HTTP 获取、JSON/XML 提取、数值校验、缓存、费用累计与展示各自负责。共享层测试使用契约替身，删除 provider 专属及静态重复断言。
3. 已完成：新增可选 destination 展示能力。普通面板只分配区域并调用 height/render；Hub 不提供普通展示，LLMRates 自己绘制模型价格、币种回退、当前错误和错误历史。模型切换时等待新报价。
4. 已完成：token 报价携带真实币种，费用按币种分别累计；固定账单币种使用显式可选类型。报价失败不作为零费用，也不使用已过期报价继续核算。
5. 已完成：LLMRates 配置校验、独立 HTTP 获取、精确模型匹配与显式别名、标准最高价选择、缓存及按会话/凭据 profile 隔离的状态。行为验证覆盖币种隔离、失败恢复、状态显示和混合币种累计。
6. 已完成：生成全部 27 个 provider 的 destinations.toml，保留 2 个 Hub 实例，共 29 个；同步中英文文档与数据署名。
7. 已完成验证：`cargo test --locked --lib --bins`（47 passed）、`cargo clippy --locked --lib --bins -- -D warnings`、格式及 diff 检查。使用一次性验证程序解析真实数据集、核对全部 mappings 与实例数、核对无效覆盖配置的文件/实例/字段错误上下文，直接联网验证 OpenAI USD、DeepSeek CNY、Qwen USD→CNY 回退、缓存重复查询与面板渲染。一次性程序已删除。

限制：聚合源的 API 名称可能需要 model_aliases；无法从 token telemetry 核算 Runway 等视频/图像价格；不复现区域、token 阶梯或峰谷时间表，使用所选币种中各 token 类别的最高标准单价。刷新在缓存过期后的下一次费用查询发生。

# gkl — git 历史考古工具

只读分析 git 仓库历史的命令行工具。Rust + [gitoxide](https://github.com/GitoxideLabs/gitoxide) + [redb](https://github.com/cberner/redb) 索引引擎 + AI 问答,支持 `gkl://` 深链接。

## 设计原则

- **只读**:不写仓库、不跑 hook、不执行仓库内任何代码
- **先索引后查询**:redb 持久化 commit 元数据,增量扫描
- **可分享的结论**:每个考古结果都能生成 `gkl://` 深链接
- **无网络依赖也可用**:AI 问答无 API key 时退化为离线启发式

## 安装与构建

```bash
cargo build --release
```

## 快速开始

```bash
gkl scan                 # 建立索引(增量;--force 全量重建)
gkl log --limit 20       # 提交日志(支持 --author/--grep 过滤)
gkl authors              # 作者统计
gkl activity             # 月度活动柱状图
gkl show <rev>           # 提交详情 + 深链接
gkl blame <path> <line>  # 这行代码最后被谁改的(轻量,第一父链回溯)
gkl ask "热点在哪"        # AI 问答
```

### 深链接

```bash
gkl link commit HEAD              # gkl://commit/<40hex>
gkl link file src/main.rs --line 42
gkl link blame src/main.rs --line 42
gkl link search "fix panic"
gkl resolve gkl://blame/src/main.rs#L42   # 解析并执行
```

链接格式:`gkl://commit/<id>`、`gkl://file/<path>#L<n>`、`gkl://blame/<path>#L<n>`、`gkl://search/<query>`,路径百分号编码,往返一致。

### AI 问答

```bash
# LLM 模式(OpenAI 兼容 API)
export GKL_OPENAI_API_KEY=sk-...
export GKL_OPENAI_BASE_URL=https://api.openai.com/v1   # 可选,兼容网关
export GKL_MODEL=gpt-4o-mini                            # 可选

gkl ask "这个仓库的主要贡献者是谁?"
```

无 key 时离线启发式支持:`谁写的 <path>`、`热点`、`作者排行`。

### 全局选项

```bash
--repo <path>   指定仓库(默认当前目录,支持子目录向上发现)
--db <path>     索引文件(默认 <repo>/.git/gkl/index.redb)
--json          JSON 输出(供脚本与外部 agent 消费)
```

## 架构

```
crates/
├── gkl-git    gitoxide 只读封装:discover/rev_parse/walk/blame-lite/blob
├── gkl-db     redb 索引:commit 元数据表 + 扫描状态;增量 + 历史改写检测
├── gkl-core   查询引擎:log 过滤/作者统计/月度活动/gkl:// 深链接编解码
├── gkl-agent  AI:OpenAI 兼容 LLM + 离线启发式;回答附深链接证据
└── gkl-cli    clap CLI,全部能力入口
```

增量策略:上次 tip 仍在新历史第一父链上 → 只索引新区间;否则(rebase/amend/force push)自动全量重建。索引 schema 版本化,不匹配自动重建。

## 已知限制(路线图)

- blame 是第一父链回溯启发式,不做跨 rename/合并归因;计划接 `gix::trace::blame` 或自实现 Myers diff
- 索引只覆盖 first-parent 主线,侧链提交待扩展
- 无文件级 churn 统计(需要 diff 遍历)
- 无 .mailmap 身份合并
- partial clone 下 blob 缺失时直接报错,未做优雅降级

## 许可

MIT OR Apache-2.0

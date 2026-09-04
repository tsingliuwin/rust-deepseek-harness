# 上游更新分析法（模式 A 详细步骤）

上游仓库：`E:\aiproject\deepseek-harness`（git）。rustdsh 是其 **web 前端（packages/client/*）+ 存储行为（storage/session）+ llm-deepseek 适配层** 的 Rust/GPUI 1:1 复刻。

## 1. 一键差异分析

```bash
.agents/skills/rustdsh-sync-regression/scripts/check-upstream.sh [from] [to]
```

`from` 取上次同步点（查项目记忆 `rustdsh-sync-*-scope` 或 git log 里的「同步 web x.x.x」提交）。脚本输出五段：发布树差异 / 提交清单 / 同步面 stat / 词汇表 diff / 存储行为 diff。

**首个关键判定——发布是否有实质变更**：
`git diff tagA tagB --name-only | grep -v package.json` 为空 = 两发布点之间零功能变更（只有版本号批量 bump）。此时还要看 **master HEAD 相对发布标签的增量**（功能可能在打标签后才合入 master，用户的工作区拉的是 HEAD）。

## 2. 同步面判定表

| 上游改动 | 判定 | 理由 |
|----------|------|------|
| packages/client/ui-*（chat/conversation/settings*/sidebar/primitives/theme/tool）| **面内** | UI 1:1 镜像 |
| packages/client/*/locale(s).ts 词汇 | **面内** | 词汇表逐项核对 |
| packages/storage、session-projection-cache 的**行为**变更 | **面内** | projcache/存储格式互通 |
| packages/llm/llm-deepseek（translate/types/error）| **面内** | dsh-llm-deepseek 镜像 |
| ui-settings-models 的 fetch/discovery | **面内**（已补齐，见 fetch-models 功能） | 获取可用模型 |
| packages/llm/llm-pi-ai discovery/catalog | **半面内** | discovery 语义移植；富目录（catalogModels）不移植 |
| session-persistence 内部重构（coordinator/handle/storage-contract）| 面外，**除非**动磁盘格式 | host 内部 seam；jsonl 格式稳定性用回归 A2/A3 兜底 |
| read_image 图片卡 / 新工具渲染 | 面外（除非 rustdsh 新增对应工具） | rustdsh 工具集无 read_image |
| proxy/http-proxy/app-boot/python 打包 | 面外 | Node 网络层，web 壳层无涉 |
| issue-management（.github）、CI、docs、notes | 面外 | 仓库自动化 |
| experimental/*（agent-team、cordis）| 面外 | rustdsh 无镜像 |
| message-edit 这类 feat→revert 对 | 净抵消，净零 | 与基准 diff 即可确认 |

## 3. 实施顺序

1. 词汇表先行：新增 key 落到 rustdsh 对应界面文案；删除的 key 检查 rustdsh 是否残留。
2. UI 数值变更：照抄上游 `ui-theme` 包 / `*.module.css` 的数值（px/色阶/圆角/字号行高）。
3. 存储行为变更：先读上游 spec/测试（`*/tests/*.spec.ts`）确认语义，再改 dsh-persist，配等价 Rust 单测。
4. llm-deepseek 变更：对照 translate.ts 逐语义核对 adapter.rs（注意上游可能先引入又撤销，**以 HEAD 净态为准**）。
5. 每完成一块即 `cargo test --workspace`；全部完成后跑完整回归（模式 B）。
6. 提交信息格式参照项目历史：`同步 web <版本>：<一句话主旨>（<存储/UI 细目>）；<偏差与原因>；回归：<测数> + 实机验证点`。

## 4. 已知固化的偏差（勿重复推导）

- 无 pi-ai 富目录短路（PROVIDER_CATALOG 只有单默认模型）→ 目录型提供方改走端点探测。
- 获取可用模型：编辑卡 askable 恒真；空 base 报上游完整 `pi-ai ships no catalog...` 文案；UA `dsh-rust/*`；60s 总超时；禁用态无 tooltip。
- read_image 图片卡无渲染面（rustdsh 工具集无 read_image）。
- serde_json 开 `preserve_order`（端点顺序 = JS Object.entries）。

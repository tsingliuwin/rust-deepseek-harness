# rustdsh 功能点清单（回归遍历用）

每项标注验证方式：`[单测]` cargo test 覆盖、`[语料]` examples 真实 ~/.dsh 数据、`[UI]` 实机点击+截图。
UI 项给出入口路径与通过判据。回归时按层推进：A 全绿才进 B-H（UI 层失败先回查 A 层）。

## A. 核心链路（无 UI，regression-core.sh 一键跑）

| # | 功能点 | 验证 | 通过判据 |
|---|--------|------|----------|
| A1 | 全 workspace 单测（adapter 流式身份/4xx 分类、projcache 双布局/戳保留/版本门、session 投影等 34 组） | 单测 | 0 failed |
| A2 | 真实 ~/.dsh/sessions 全量加载（zstd 多帧 read_across_frames） | 语料 | load_web 列出全部会话无错误 |
| A3 | projcache 双布局标题互通（per-record 优先、旧整档回退、版本门） | 语料 | share_check 标题数 >0 |

## B. 聊天流（chat.rs / widgets.rs）

| # | 功能点 | 入口 | 判据 |
|---|--------|------|------|
| B1 | 会话加载渲染：点侧栏会话行 → 历史消息气泡（角色/对齐） | 点任一历史会话 | 消息完整、无缺块 |
| B2 | markdown：标题/列表/表格/引用/行内代码/代码卡（语言标注+复制钮） | 发送带 markdown 的消息 | 各元素样式与 web 一致 |
| B3 | 工具卡分派：shell→terminal、fs read→read（行号 gutter、8 行折叠、复制）、fs write→diff（footer +A -R）、web_search→search（分组折叠）、web_fetch→网页获取卡、grep/glob→结果卡、未知→io 兜底 | 触发对应工具的会话 | 卡类型正确、错误行走 io 兜底 |
| B4 | 折叠/展开点击（折叠后列表高度重测，无残影） | 点 read/搜索卡折叠钮 | 展开完整、下方内容随动 |
| B5 | 流式输出：发送 → delta 渐进上屏 → 结束停止；发送钮变停止钮 | 发一条真实消息 | 逐字出现、结束后恢复 |
| B6 | 操作常显规则：最新轮尾部与最新用户行操作常显，更早轮整行悬停显现 | 悬停历史消息 | 与 web 一致 |
| B7 | 消息流顶部 16px 内边距（首条气泡不贴 header 分隔线） | 打开任一会话 | 目测间距 |
| B8 | 用户气泡字号 14/22 | 目测 | 与 web .bubble 一致 |
| B9 | 错误行渲染（RATE_LIMIT 等）：红色 + 出错位置 | 触发限流的会话 | 文案完整可复制 |

## C. Composer

| # | 功能点 | 入口 | 判据 |
|---|--------|------|------|
| C1 | inert 态：空会话且未绑定工作区 → 编辑器不挂载、hero 引导选工作区 | 新建纯草稿 | 占位引导文案，无输入框 |
| C2 | 多行输入：Enter 发送（按 E3 设置）、Shift+Enter 换行 | 输入 | 行为符合设置 |
| C3 | 模型选择菜单：打开/切换模型/点外关闭（adopt 下拉同款窗口级 mousedown） | 点 composer 右侧模型名 | 菜单开合正常、点外即关 |
| C4 | 权限 chip 显示（Workspace Write） | 目测 | 文案与 web 一致 |
| C5 | @文件引用注入 | 输入 @ | 文件候选、选中后注入 |
| C6 | 繁忙时 Enter：按 E3 设置排队或打断 | 流式中按 Enter | 行为符合设置 |
| C7 | 模型切换公告（0.1.3-alpha.2）：会话中经 C3 切换模型后，下一轮消息批尾追加 user/plugin `model-selection` notice（summary「旧 → 新」），随日志派生进入模型历史；请求头 reason=change | C3 切换模型后发消息 | 日志新增 user/message 行（source 带 form=notice/summary）+ request/header reason=change；聊天流出现公告文本（显示形态为既有用户消息渲染面，上游折叠行不可复刻见偏差表） |
| C8 | v3 落盘序与 system 面节点（0.1.5-alpha.1）：step/start → system/message（head append，变化 replace）→ user 批；request/header 无 system 字段 | 跑一轮会话后查日志 | 日志行序正确、session.v3.jsonl.zstd、request/header data.header 无 system、system/message 行 source=plugin @dsh-system-prompt |
| C9 | 统计双 pill + 对话框（0.1.5-alpha.1）：composer 下仪表 pill（轮步+TPS）与数据 pill（总 token+缓存命中），点击开互斥对话框（会话统计 / Token 用量），再点关闭 | 跑一轮后看 composer 下方 | pill 数值正确、对话框行与数据一致（模型用时/TTFT/TPS；输入/缓存/输出）、互斥开合 |

## D. 侧栏（sidebar.rs）

| # | 功能点 | 入口 | 判据 |
|---|--------|------|------|
| D1 | 工作区分组列表（cwd 分组、目录名、折叠箭头） | 打开侧栏 | 分组正确 |
| D2 | 会话行：标题（projcache 优先、web 标题回退、新会话兜底）、时间标签（刚刚/N分钟/N天/N个月）、blank 态 | 目测 | 与真实语料一致 |
| D3 | 新建会话：+ → 当前工作区已存 blank 会话复用/新建 | 点 + | 列表/标题正确 |
| D4 | 会话搜索过滤 | 输入搜索词 | 列表过滤 |
| D5 | 工作区/会话重命名（行菜单 → 输入框） | 右键/… 菜单 | 改名生效并持久化 |
| D6 | 会话归档（菜单 → 归档后从列表消失、不入启动选择） | 行菜单 | 列表更新 |
| D7 | 菜单点外关闭（窗口级 mousedown 外关 + 触发器只开不关防回弹） | 开菜单后点空白 | 菜单关闭 |
| D8 | 零工作区 hero 引导（不落盘、不进列表） | 无 ~/.dsh 时启动 | 引导态 |
| D9 | 侧栏折叠（« ）与列宽拖拽 | 拖中栏分隔 | 宽度记忆 |

## E. 设置-通用

| # | 功能点 | 入口 | 判据 |
|---|--------|------|------|
| E1 | 外观：浅色/深色/跟随系统（即时切换，分段控件） | 设置→通用→外观 | 主题即时生效 |
| E2 | 界面语言：中文/English | 同上 | 文案切换 |
| E3 | 繁忙时 Enter 行为：排队发送/打断 | 同上 | 写入 settings.yaml |
| E4 | 对话视图：常规/紧凑 | 同上 | 气泡密度切换 |
| E5 | 打开配置文件（settings.yaml 用系统关联程序） | 设置头右上 | 文件打开 |

## F. 设置-模型（settings.rs models_page）

| # | 功能点 | 入口 | 判据 |
|---|--------|------|------|
| F1 | DeepSeek 卡：env 锁定只读态（DEEPSEEK_API_KEY 由启动环境提供）；编辑卡 key 替换 | 设置→模型 | 锁定文案/替换生效 |
| F2 | 自定义行卡：凭据绿点（有 key）、编辑/删除按钮、自定义 tag（目录外 route 才打） | 目测 | 与 yaml 一致 |
| F3 | 删除确认弹层：取消/确认（删除后 settings.yaml providers 移除、凭据保留） | 删除 | 二次确认、持久化 |
| F4 | 编辑卡：API 密钥（已配置 placeholder）、自定义设置折叠（API 地址/模型目录只读行/其余字段 hint）、保存（换 key 重注册 adapter）| 编辑 | 保存后路由生效 |
| F5 | adopt 卡：目录下拉（候选=目录未 adopt 项）、点外关闭防回弹、高级自定义折叠（API 地址）、保存 | 添加提供方 | 注册+持久化 |
| F6 | declare 卡：Provider ID 校验（格式错误/重复→error 行，空→hint）、显示名称、API 地址必填、协议恒 openai、API 密钥、模型行增删（查重）、至少一模型、创建 | 添加自定义提供方 | 错误行逐条正确、创建后注册 |
| F7 | 获取可用模型（declare）：空地址禁用置灰、填地址可点、busy「正在询问提供方…」、失败行（401/403 带 check the API key、无模型文案、UNSUPPORTED）、成功→候选弹层 | 填地址+key 后点 | 各分支正确 |
| F8 | 候选弹层：标题「选择要添加的模型」/描述/搜索过滤（id+name 子串）/全选-取消全选（rc.1 语义：过滤态取消全选清空整个集合）/单行勾选/取消（清候选与搜索词）/添加所选（已配置行胜出、容量 K/M 保留、去重） | 弹层内操作 | 与 web ModelListEditor 一致 |
| F9 | 获取可用模型（编辑卡）：恒可点（web askable 恒真）、key/地址回退链（表单键入→已存→目录 base_url）、空 base → 完整诊断 `pi-ai ships no catalog for provider "X", ...`、采纳写 display_name/容量进已存提供方 | 编辑卡折叠区内 | 各分支正确 |

## G. 轨迹视图

| # | 功能点 | 入口 | 判据 |
|---|--------|------|------|
| G1 | 对话/轨迹 tab 切换 | 顶部 tab | 视图切换、状态保持 |
| G2 | 轨迹事件列表渲染 | 切到轨迹 | 事件序列完整 |

## H. 框架/窗口

| # | 功能点 | 入口 | 判据 |
|---|--------|------|------|
| H1 | 自定义标题栏：最小化/最大化/关闭 | 窗口钮 | 正常 |
| H2 | 中栏列宽拖拽与记忆 | 拖分隔条 | 重启保持 |
| H3 | 标准模式 chip（composer 左侧） | 目测 | 与 web 一致 |

## 排障日志源

- `cargo test --workspace` 输出（断言失败即回归点）
- `cargo run --example load_web/share_check -p dsh-persist`（持久化互通）
- 会话原始日志：`~/.dsh/sessions/<dir>/<id>/session.jsonl.zstd`（zstd 多帧，用 `dump_lines`/`inspect_kinds` examples 解）
- 配置：`~/.dsh/settings.yaml`（llm-pi-ai.providers 段）、`~/.dsh/settings.json`、`~/.dsh/.credentials.yaml`、`~/.dsh/storages/`（projcache 双布局）
- UI 问题：CUA 截图 vs 上游 web 源码数值（packages/client/ui-*/src 与 *.module.css）

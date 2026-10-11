# C96 — 资讯聚合 → RSS-like 订阅 → 完整索引 → 本地 AI 知识库

> 给一个关键词，C96 就持续把网上最新内容搜回来、去重入库，并立刻变成一份**自己的 RSS / OPML 订阅源**（阅读器、手机直接添加）；这份本地库同时也是可检索的知识底座。
> Rust 核心（`src/`）+ Tauri v2 / WebView2 GUI（`gui/`）；HTML 抽取、BM25 索引、JSONL 存储、指纹去重都是自写，核心依赖只有 5 个 crate（serde / serde_json / toml / ureq / xml-rs）。

## 功能

### 关键词订阅（核心）
- **没搜过** → 立刻 Bing / Baidu 搜索 → 相关性排序 → 抓前 N 页正文（HTML 抽取 / RSS 条目 / 纯文本，带 ETag 条件 GET）→ 写库，**同时建立常驻任务**
- **搜过** → 直接读本地库返回（1–11 ms，不联网、`runs` 不增加）
- **数据旧了** → 先返回旧数据，再后台补搜一次；程序只要在运行，就按 `interval_secs`（默认 30 min）到期自动刷新
- 同一关键词的后台刷新只跑一个（inflight 去重）；任务可刷新 / 暂停 / 恢复 / 删除

### 本地 RSS 服务器（启动即用）
- GUI 一启动就监听 `127.0.0.1:8787`；`C96_RSS_BIND=0.0.0.0:8787` 可开放局域网
- **默认不需要 token**，地址直接给阅读器 / 手机；要限制访问才 `--token X` 或 `C96_RSS_TOKEN`
- 新入库文档 = 新 `<item>`（`guid` = 内容 hash，`pubDate` = 入库时间）→ 阅读器轮询即见推送
- 一键脚本 `start-rss-lan.ps1`（或双击 `start-rss-lan.bat`）：识别局域网 IP → 起 GUI → 等端口就绪 → 打印全部订阅地址；`-Headless` 只跑服务、`-Stop` 停服务、`-OpenIndex` 打开发现页

### RAG 知识库
- 单一 JSONL（默认 `%APPDATA%\com.c96.gui\kb.jsonl`），**GUI 与 CLI 共用同一个文件**
- 入库三道指纹去重：URL hash → 标题 shingle → 正文 simhash（≥400 字才判近重复；re-fetch 不产生重复记录）
- **30 天 TTL**：超期记录在打开 / purge 时自动丢弃并原子重写
- 手动增删查：`store add|list|search|purge|state`；GUI 面板可逐条删、按站点清空

### 检索（BM25）
- `C96 index query --query Q`：词项 = 字符码点整数（中文按字），标题 ×3 / 正文 ×1 + 整串短语加成
- 惰性建索引 + 进程内缓存；10k 文档 **0.42 ms/查询**，100k **2.1 ms/查询**

### 浏览器自控 + 反爬
- Tauri v2 + WebView2 浏览器窗口（默认停在 Bing），顶部叠加 C96 工具栏：关键词 / 网址框、链接修正（`target=_blank` / `window.open` 都留在当前窗口）
- 页面加载后自动把正文送进 RAG；工具栏搜索框的输入会成为后续入库条目的 `query` 标注
- 点击 / 跳转 / 搜索词都会作为浏览器事件记录下来（`browser_event`）

### RSS 内容管理器（GUI 顶部 RSS 图标）
- **查**：订阅列表（间隔 / runs / 入库数）+ 展开最近条目 + 来源分组
- **增**：填关键词 + 间隔 → 立刻搜一轮并建任务
- **改**：立即刷新 / 暂停恢复 / 行内改间隔
- **删**：订阅删除、单条内容删除、**按站点清空**，全部二次确认
- **全部内容**：列出库里所有条目（含随便浏览自动收录的那批），可按 标题 / 网址 / 主机 / 来源 过滤；「只看随手收录」只留不属于任何现存订阅的
- **忽略**：把某个站点加入忽略名单，之后不再自动收录（存在 `state.toml` 的 `[meta] ignored_hosts`）

## 快速开始

### 1. 构建
```powershell
cargo build --release                 # 核心 CLI → target/release/C96.exe
cargo test --offline                  # 103/103（lib 85 + bin 18）
cd gui ; cargo build --release        # GUI → gui/target/release/c96-gui.exe
```

### 2. GUI（双击即用）
把 `C96.exe` 与 `c96-gui.exe` 放同一目录，或保持 `gui/target/{release,debug}/` 布局；双击 `C96.exe`（无参数）就会拉起 GUI。也可以 `C96 gui`；找不到 GUI 时打印用法提示，`C96_GUI` 环境变量可显式指定路径。

### 3. 局域网 RSS
```powershell
.\start-rss-lan.ps1                    # 默认 8787、无 token，打印本机 + 局域网订阅地址
.\start-rss-lan.ps1 -Port 8899 -OpenIndex
.\start-rss-lan.ps1 -Token mysecret    # 需要限制访问时
.\start-rss-lan.ps1 -Headless -Kb D:\kb.jsonl -Tail
.\start-rss-lan.ps1 -Stop              # 停掉占用该端口的进程
```

### 4. CLI 常用
```powershell
C96 research --query "卡普空 最新游戏"           # 没搜过就立刻搜 + 入库 + 建任务
C96 research --query "rust 编译器" --cache-only  # 只读 RAG，不联网
C96 research --query "rust 编译器" --watch       # 常驻，按 --interval 到期刷新
C96 topics list|show|run|pause|resume|rm|due     # 任务清单（持久化）
C96 rss serve --bind 0.0.0.0:8787                # 无 GUI 的常驻 RSS 服务
C96 search --query "RAG 本地知识库" --store       # SERP 候选 + 顺手入库
C96 index query --query "卡普空" --json
C96 fetch --url https://example.com --store       # 单次抓取（ETag 缓存）+ 入库
```

## 订阅端点

| 地址 | 内容 |
|---|---|
| `/` | HTML 发现页（列出全部订阅地址） |
| `/rss/all.xml` | 全部入库内容 |
| `/rss/t/<slug>.xml` | 单个关键词订阅 |
| `/rss/topic.xml?q=关键词` | 同上，按词查询 |
| `/rss/source/<name>.xml` | 单个来源（`category` = source） |
| `/opml.xml` | OPML，阅读器一键导入全部订阅 |
| `/health` | 健康检查（返回 `ok`） |

开 token 时地址自动带 `?t=…`（也接受 `?token=…`）。端口以 `C96_RSS_BIND` / `--bind` 为准（默认 `127.0.0.1:8787`）。

## 数据与配置

| 东西 | 位置 / 开关 |
|---|---|
| 知识库 | `%APPDATA%\com.c96.gui\kb.jsonl`（`--kb PATH` 覆盖） |
| 抓取状态 / 订阅任务 / 忽略名单 | 同目录 `kb.state.toml`（`[[fetch]]` / `[[topic]]` / `[meta] ignored_hosts`） |
| HTTP 缓存（ETag / Last-Modified） | `cache/http_cache.toml` |
| 用户偏好 + `[[feeds]]` | `config/feeds.toml`（当前目录没有就找 exe 同级） |
| RSS 监听地址 | `C96_RSS_BIND`（默认 `127.0.0.1:8787`） |
| RSS token（可选） | `C96_RSS_TOKEN` / `rss serve --token X` / 启动脚本 `-Token` |
| GUI 可执行文件路径 | `C96_GUI` |

TTL 默认 30 天，`--ttl-days N` 覆盖。数据都在 `%APPDATA%` 下，换目录安装不影响已有知识库。

## 阶段进度

- [x] **A** 骨架：目录布局 + 编译验证
- [x] **B** 配置层：`config/feeds.toml`、`feeds list|add`、TOML 1.1 解析 + 校验报错
- [x] **C1** HTTP 抓取：UA / 超时 / 重试分类 / ETag 条件 GET + 磁盘缓存
- [x] **C2** RSS 2.0 + Atom 解析
- [x] **C3** HTML 正文抽取（text / link density 评分）
- [x] **D** 搜索引擎驱动：Bing（News RSS + SERP）、Baidu SERP、查询计划、URL 归一化
- [x] **E** 浏览器自控：WebView2 骨架 → 点击跳转事件 → 自动入 RAG → CLI→GUI 启动桥 → `target=_blank` 链接修复
- [x] **F** 存储层：JSONL + 原子重写、三道指纹去重、30 天 TTL、`state.toml`
- [x] **G** 索引层：自写倒排 + BM25（标题 ×3 / 正文 ×1 + 短语加成）
- [x] **I1 / I2 / I3** 关键词订阅任务 + 定时刷新 + 本地 RSS 服务器（GUI 与 CLI 共用一套存储）
- [ ] **H** AI 层：embedding、向量检索、`ask` 问答（当前检索止步于 BM25）
- [ ] **I1b** `[[feeds]]` 订阅并入同一调度器
- [ ] **I4** HTTP `/search` `/ask` 端点
- [ ] **J** 集成测试 / 基准 / 文档收尾（`cargo fmt` 待跑）

## 已知限制
- Baidu 对非浏览器请求常回「百度安全验证」，走浏览器自控窗口可绕过，但拿到的正文是 AJAX 终态（存整页 innerText，质量取决于 C3）
- 中文分词只做字符级近似，没有词典 / 词干；没有时间衰减权重，所以新内容不会排更前
- 无 embedding 与向量检索，`ask` 未实现；`[[feeds]]` 目前只支持解析 / 列表 / 新增（`feeds list|add`），还没接进抓取与调度

## 测试
```powershell
cargo test --offline                          # 103/103（lib 85 + bin 18）
cd gui
.\target\debug\c96-gui.exe --selftest-rag     # 自动入库 + 指纹去重 + TTL
.\target\debug\c96-gui.exe --selftest-topics  # 关键词订阅（首搜 / 命中 / 后台刷新）
.\target\debug\c96-gui.exe --selftest-links   # target=_blank / window.open 点击
.\target\debug\c96-gui.exe --selftest-rss-ui  # RSS 内容管理器（增删改查 + 忽略站点）
```

## License

MIT — 见 [LICENSE](LICENSE)。

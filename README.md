# C96 — 资讯聚合 → RSS-like 订阅 → 完整索引 → 本地 AI 知识库

> 给一个关键词，C96 就持续把网上最新内容搜回来、去重入库，并立刻变成一份**自己的 RSS / OPML 订阅源**（阅读器、手机直接添加）；这份本地库同时也是可检索的知识底座。
> Rust 核心（`src/`）+ Tauri v2 / WebView2 GUI（`gui/`）；HTML 抽取、BM25 索引、JSONL 存储、指纹去重都是自写，核心依赖只有 5 个 crate（serde / serde_json / toml / ureq / xml-rs）。

## 它是怎么工作的

```
关键词订阅（Bing / Baidu 找候选 → 抓正文）──┐
                                            ├─► 指纹去重 + 30 天 TTL ─► kb.jsonl（唯一知识库）
浏览器窗口（正文文本稳定后自动抽正文送库）───┘                                  │
                        ┌─────────────────────────────────────────┬───────────┴──────────────┐
                        ▼                                         ▼                          ▼
              C96 index query（BM25 检索）            本地 RSS 服务器               GUI「RSS 内容管理器」
                                                      /rss/all.xml                查 / 增 / 改 / 删 / 忽略
                                                      /rss/t/<关键词>.xml
                                                      /opml.xml ─► 阅读器 / 手机
```

- **两条采集路径写同一个库**：GUI 浏览器自动入库，CLI `fetch --store` / `search --store` / `store add` 手动入库，共享同一个 `kb.jsonl`，所以谁写的东西都能互相检索、互相出现在 feed 里。
- **RSS 是「任务的输出」**：阅读器只认 RSS/OPML，所以关键词任务被映射成 feed —— 新入库一条内容 = feed 多一个 `<item>`（`guid` 用内容 hash，天然不重复）。
- **先本地、后联网**：关键词第一次出现才真的去搜；之后每次都先读本地库（毫秒级），只有数据超过新鲜期才在后台补搜，界面/命令先用旧数据顶着。

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
- 页面加载后自动把**正文**送进 RAG：不是整页 innerText，而是先等文本稳定（最快约 1s，最多 6 次 ≈ 7s），再用注入的抽取器（文字密度高 × 链接密度低 × 语义容器加权，跳过导航 / 菜单 / 侧栏 / 推荐 / 广告 / 评论 / 页脚）挑出正文块；工具栏搜索框的输入会成为后续入库条目的 `query` 标注
- **导航页不入库**：抽完链接占比仍 ≥ 70% 且正文 < 3000 字（列表页 / 搜索页 / 导航堆）记为 `store-skip` 事件，不写库
- **重访自动修旧账**：同一个 URL 再访问时，如果这次抽取更干净（抽取模式升级，或正文明显更长），就用新正文**替换旧记录**——历史上存下来的「整页含菜单」条目会被一次重访修好（列表里可看到 `模式 raw -> article`）
- 点击 / 跳转 / 页面加载会记成浏览器事件（`browser_event`：进程内事件表，自检靠它断言，不落盘）

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

功能已经打通；下面是「做了什么、有什么用」的一句话版，没打勾的表示还没做：

- [x] **A 骨架** — 目录布局 + 能编译跑通，先把工程立起来
- [x] **B 配置层** — `config/feeds.toml` 存用户偏好和订阅清单，`C96 feeds list|add` 读写；TOML 写错给的是人话报错
- [x] **C1 抓取层** — 一个能用的 HTTP 客户端：伪装 UA、超时、按错误类型决定要不要重试、ETag/Last-Modified 条件 GET（304 复用本地 body，不白下载）
- [x] **C2 解析层** — RSS 2.0 与 Atom 统一成 `Article{标题, 链接, 正文, 时间, 站点, 语言}`
- [x] **C3 正文抽取** — 从整页 HTML 里挑出正文块（文字密度高、链接密度低者胜），丢掉导航 / 广告 / 评论 / 页脚
- [x] **D 搜索驱动** — Bing（News RSS + SERP）与 Baidu SERP 两条取数路径 + 结果归一化去重，产出候选列表
- [x] **E 浏览器自控** — Tauri v2 + WebView2 窗口：顶部注入 C96 工具栏、记录点击跳转、页面自动入库、双击 exe 就启动；顺手修掉 `target=_blank` 点了没反应
- [x] **F 存储层** — 本地知识库：JSONL 追加 + 原子重写；同一内容靠 URL / 标题 / 正文三道指纹只存一份；超 30 天自动清
- [x] **G 索引层** — 自写倒排 + BM25 打分（标题权重是正文的 3 倍），中文按汉字建词项，检索毫秒级
- [x] **I 订阅服务** — 关键词变成常驻任务（没搜过先搜、搜过读本地、旧了后台补），再把任务输出成 RSS/OPML；GUI 一启动就是 RSS 服务器
- [ ] **H AI 问答层** — embedding + 向量检索 + `ask` 问答；目前检索止步于 BM25，没有语义召回
- [ ] **I1b** — 把 `[[feeds]]` 里配置的订阅也交给调度器定时刷新（现在只支持列表 / 新增，不参与抓取）
- [ ] **I4** — 给本地服务器加 `/search`、`/ask` 两个 HTTP 接口，方便别的程序直接查这个库
- [ ] **J 收尾** — 集成测试、性能基准、文档（`cargo fmt` 也没跑）

## 已知限制
- Baidu 对非浏览器请求常回「百度安全验证」，走浏览器自控窗口可绕过，但拿到的正文是 AJAX 终态（由注入的正文抽取器尽量只摘正文块，站点自定义结构仍可能漏/带尾巴）
- 中文分词只做字符级近似，没有词典 / 词干；没有时间衰减权重，所以新内容不会排更前
- 无 embedding 与向量检索，`ask` 未实现；`[[feeds]]` 目前只支持解析 / 列表 / 新增（`feeds list|add`），还没接进抓取与调度

## 测试
```powershell
cargo test --offline                          # 104/104（lib 86 + bin 18）
cd gui
.\target\debug\c96-gui.exe --selftest-rag     # 自动入库 + 指纹去重 + TTL
.\target\debug\c96-gui.exe --selftest-extract # 正文抽取：菜单/侧栏/推荐不进门 + 链接堆页被拦住
#   C96_EXTRACT_LIVE='<url1>,<url2>' 可顺手验证真站重访（旧的整页记录会被替换成正文）
.\target\debug\c96-gui.exe --selftest-topics  # 关键词订阅（首搜 / 命中 / 后台刷新）
.\target\debug\c96-gui.exe --selftest-links   # target=_blank / window.open 点击
.\target\debug\c96-gui.exe --selftest-rss-ui  # RSS 内容管理器（增删改查 + 忽略站点）
```

## License

MIT — 见 [LICENSE](LICENSE)。

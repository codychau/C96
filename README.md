# C96 — 资讯聚合 → RSS-like 订阅 → 完整索引 → 本地 AI 知识库

## 阶段 C1 — 轻量抓取层：HTTP client ✅ 2026-10-07

**模块**：`src/fetch/`（`http.rs` + `cache.rs`）· **依赖**：`ureq 2.12.1`（rustls + ring 纯 Rust TLS；Cargo.lock 81 pkgs，一次联网后离线编译）

### 功能
- **UA**：默认 `C96/0.1.0 rss-aggregator` + `Accept: application/rss+xml, atom, xml;q=0.9, html;q=0.7, */*;q=0.5` + `Accept-Language: zh-CN,zh;q=0.9,en;q=0.8`（可 override）
- **timeout**：overall 20s（`--timeout`）· connect 6s（`--connect`）· read 20s
- **retry**：默认 2 次额外尝试；exponential backoff 400ms→800→1600…cap 8s；`Retry-After`（整数秒，cap 30s）优先；**只 retry 传输类错误 + 429/503**；确定性错误（InvalidUrl / UnknownScheme / TooManyRedirects / Proxy*）fail fast（1 attempt）；`or_any_status()` 让 4xx/5xx 落 Reply（报告、不 retry）
- **ETag/Last-Modified 缓存**：conditional GET（`If-None-Match` / `If-Modified-Since`）→ 304 = 复用 cached body（0 bytes re-fetched）；2xx 全替换 validators（stale ETag 绝不存活于 200）；304 回显的 validators 采纳、body 永远取 cached；fetch 失败保留旧 entry
- **缓存文件**：`cache/http_cache.toml`（TOML `[[entry]]`：url / etag / last_modified / content_type / status / body；atomic `path.tmp`→rename；cap 100 newest；broken/missing = empty cache，never fatal）
- **body cap**：1 MiB（`--max-body`），UTF-8 lossy 解码（GBK SERP 属阶段 D）
- **重定向**：redirects=5，`final_url` 报告（`redirected →`）

### CLI
```
cargo run -- fetch --url URL [--timeout N] [--connect N] [--retries N] [--max-body N]
                     [--cache PATH] [--no-cache] [--show-body]
```
输出：status / attempts / final_url / content-type / etag / last-modified / bytes / cache 状态

### 验证（2026-10-07，本机实测）
- `cargo build` **0 warning**（clean rebuild 11.65s）· `cargo test` **35/35 green**（C1 新增 19 tests）
- live 200→304：`example.com`（Last-Modified only）· `raw.githubusercontent.com/rust-lang/rust/master/README.md`（weak ETag `W/"…"`）· `www.rust-lang.org`（ETag+LM，redirect→rust-lang.org）
- live 无验证器：Bing News RSS（209 KB，redirect→cn.bing.com）→ 每次 plain GET 200（缓存仍存 body，供 C2 解析）
- retry 分类：`c96-nonexistent-domain.invalid` → Dns，3 attempts 后 exit 1 · `not a url` → InvalidUrl，1 attempt
- 截断：`--max-body 100` → `body: 100 bytes` + `truncated at --max-body cap 100`
- 网络抖动实录：raw 条件 GET 一次 `Io`（read timeout）→ retry 分类正确，复跑 304

### 有意推迟
- robots.txt / per-domain 限速 → 阶段 D/E（反爬层，与浏览器自控配套）
- HTTP-date 形式 `Retry-After` → 阶段 F（日历层）
- GBK/GB2312 SERP 解码 → 阶段 D

### 下一步
阶段 E3 已完成（默认双击 = 浏览器主窗口 + 搜索 + 自动入 RAG；CLI → GUI 启动桥）→ 下一步 **阶段 F 存储层** / embedding 检索

## 阶段 C2 — RSS 2.0 + Atom 解析 ✅ 2026-10-07

**模块**：`src/fetch/feed.rs` · **依赖**：`xml-rs 0.8.29`（离线已缓存）

- `parse_feed` 由 root 自动分派：`<rss>` → `parse_rss`，`<feed>` → `parse_atom`
- 统一 `Article{title,url,body,published,site,lang}`：`content:encoded`(CDATA) 优先 `description`、实体解码、Atom `<link rel=alternate>` 排序、`<published>` 优先 `<updated>`、`site` 取 host、`lang` 取 feed 语言
- CLI：`feed parse --file PATH [--limit N]`（UTF-8 BOM 剥离）
- 验证：`tests/fixtures/rss.xml`(6)/`atom.xml`(5) 全部结构化正确

## 阶段 C3 — HTML 正文抽取（link/text density）✅ 2026-10-07

**模块**：`src/fetch/html.rs` + `src/cli/html.rs` · 零新依赖

- **DOM-lite 容错 parser**：arena 节点（0=`#root`），void 元素、`<script>/<style>/<noscript>/<template>/<svg>/<iframe>` 内容整段丢弃，注释/doctype/PI 跳过；数字 + named entities 解码；畸形 HTML 不 panic
- **子树统计**：每个元素累计 text 字数 / 元素数 / `<a>` 链接字数（child 索引 > parent → 反向一次自底向上）
- **评分选块**：容器级标签候选，`score = text_density(text/(tags+1)) × (1 − link_density)² × classWeight`；nav/footer/sidebar/comment/ads 祖先直接排除；article/content/post… 加权
- **渲染**：块级标签插换行 → 逐行 collapse/trim → 去空行；boilerplate 子树跳过
- `extract()` → title/text_len/link_density/selector；`article_from_html()` → C2 `Article`（含 `<html lang>`）
- CLI：`html extract --file PATH [--url URL] [--max-chars N] [--meta]`
- 验证：`tests/fixtures/article.html` → 选中 `<article class="post-content">`，368 chars，link-density 0.000，script+style+nav+sidebar+comments+footer 全剔除

## 阶段 D — 搜索引擎驱动层（Bing / Baidu SERP）✅ 2026-10-07

**模块**：`src/search/{query,bing,baidu,normalize}.rs` + `src/cli/search.rs` · 零新依赖

- **D1 `query.rs`**：`build_plan(keywords, site, engine)` / `plan_from_feed(&Feed)` → query 文本（含 `site:HOST`）；`bing_rss_url` / `bing_serp_url` / `baidu_serp_url`；手写 percent + base64/base64url
- **D2 `bing.rs`**：News RSS（复用 C2）→ 候选；SERP `<li class="b_algo">`（h2>a / b_caption / 嵌套去重）；`/ck/a?u=a1<base64url>` 解码
- **D3 `baidu.rs`**：SERP `<div class="result|c-container">`；真实 URL：`mu` → `data-url` → `data-tools` JSON → `link?url=` 的 `ClickUrl/url/target/from`（percent/base64url）；解不开标 `resolved=false`；`cookie()` 生成 BAIDUID
- **D4 `normalize.rs`**：URL normalize（fragment/跟踪参数/host 小写/默认端口/尾 `/`）、FNV-1a 64 hash、glob 白黑名单、hash 去重（resolved 优先）
- CLI：`search [--query Q | --feed NAME] [--engine bing|baidu|auto] [--site HOST] [--max N] [--from FILE] [--cookie] [--timeout] [--retries]`
- 验证：live `search --query "rust 编译器"` → Bing 10 + Baidu 27 → 37 unique；`--feed bing-serp-zh` → 8 unique（blacklist 过滤 2）；`--from tests/fixtures/baidu_serp.html` → 4/4

### live 现状（2026-10-07）
- Bing News `format=rss` 端点已退化为返回首页 HTML → RSS driver 保留（fixture/单元验证），live 落到 Bing SERP
- Baidu 对非浏览器请求返回「百度安全验证」页 → Baidu 真实抓取需阶段 E（webview2-rs 快照 / BAIDUID 会话）

### 尚未做
- robots.txt / per-domain 限速（阶段 D/E 反爬层）
- GBK/GB2312 SERP 解码（`ie=utf-8` 已规避）
## 阶段 E1 — Tauri v2 GUI 骨架 + WebView2 自控窗口 ✅ 2026-10-07

**模块**：独立 crate `gui/`（**不进入根 C96 的零依赖核心构建**）· 依赖：`tauri 2.12` / `tauri-build 2.7` + Windows `webview2-com 0.39` / `windows 0.62`

- **外壳**：`gui/tauri.conf.json`（`withGlobalTauri` + 静态 `ui/` 前端 → 不需要 npm/bundler）；`gui/capabilities/default.json`；`gui/tools/make_icon.mjs` 零依赖生成 `icons/icon.ico`（满足 tauri-build 的 Windows 资源要求）
- **自控窗口** `gui/src/browser.rs`：label=`browser` 的第二个 WebView2 窗口；`initialization_script` 注入快照钩子 `window.__C96_SNAPSHOT__.capture()`（E3 入口）；`on_navigation` 记录用户点击/跳转
- **webview2-rs 裸控制**：`Webview::with_webview` → `PlatformWebview::controller()` → `ICoreWebView2Controller` / `ICoreWebView2`，读 `Source()` / `DocumentTitle()` / `Settings()` / `Bounds()`，执行 `SetZoomFactor` / `Navigate`
- **命令**：`open_browser` / `snapshot_probe` / `webview2_info` / `webview2_zoom` / `webview2_navigate`；`--selftest` 无人值守验收
- **验证**：gui `cargo build` **0 warning** · `c96-gui.exe --selftest` 原生 **exit 0**（`DocumentTitle()=C96_HOOK_OK` 证明注入成功；`Source()` example.com → rust-lang.org；zoom 1.5 ok）· 截图 [e1-window.png](gui/docs/e1-window.png) · 根 CLI 回归 **62/62 green**

### 构建 / 运行
- `cd gui; cargo run`（或直接双击 `c96-gui.exe`，**无参数**）— 打开**浏览器主窗口**：Bing 起始页 + 顶部 C96 搜索/网址条；页面加载后自动存入 RAG 知识库
- `cd gui; cargo run -- --selftest` — 无人值守 E1+E2 自检（裸 WebView2 + 事件，约 10s，exit 0）
- `cd gui; cargo run -- --selftest-rag` — 无人值守 E3 自检（自动入库 + 去重 + 搜索链路，约 30s，exit 0）
- RAG 知识库落盘：`%APPDATA%\com.c96.gui\kb.jsonl`（JSONL + URL 去重；命令 `kb_stats` / `kb_search`）
- `cd gui; cargo build --release` → 双击 **`gui/target/release/c96-gui.exe`**（Windows GUI 子系统，**无控制台闪窗**）
- ⚠️ 别混淆：`C96/target/debug/C96.exe` 是**核心 CLI**（无参数会打印 usage 后立即退出 → 双击「一闪而过」）；GUI 可执行文件叫 `c96-gui.exe`
- 🚩 **两个 crate 的产物不同**：根 `cargo build [--release]` 只生成核心 CLI `C96/target/release/C96.exe`；GUI 是独立 crate，**必须 `cd gui` 再构建**才会得到 `gui/target/release/c96-gui.exe`
- **双击入口**：项目根 [C96 GUI.lnk](RUST/C96/C96%20GUI.lnk)（指向 `gui/target/release/c96-gui.exe`，可拖到桌面）——双击即开浏览器主窗口，无控制台黑窗
- GUI 与核心分离：根 `cargo build` / `cargo test` 不会拉入 Tauri 依赖树
## 阶段 E2 — 用户点击/跳转事件监听 ✅ 2026-10-07

**模块**：`gui/src/browser.rs`（事件层）+ `gui/ui/app.js`（实时事件流）· 依赖新增 `windows-core 0.62`（`#[implement]` 生成代码需要）

- **注入钩子** `EVENTS_INIT`（`initialization_script`，逐文档生效）：capture 阶段 `click`（最近 `<a>` 的 tag|href|text）、`hashchange` / `popstate`、`location.href` 轮询（SPA）
- **回传通道**：`window.chrome.webview.postMessage`（WebView2 原生，不依赖 Tauri IPC 的远端 origin 策略）
- **接收**：裸 `ICoreWebView2::add_WebMessageReceived` + `#[implement] ICoreWebView2WebMessageReceivedEventHandler`；`TryGetWebMessageAsString` → `WebMessageAsJson` 兜底，`CoTaskMemFree` 释放
- **记录 / 广播**：`BrowserEvent{seq,kind,url,detail,at_ms}` 环形日志（cap 500）+ `emit("browser://event")`
- **导航**：`on_navigation`（含重定向）与 `on_page_load`（`load-start` / `load-finish`）
- **命令**：`browser_events` / `browser_events_clear` / `browser_event`（IPC 兜底）
- **验证**：`--selftest` exit 0 —— 合成点击 `click=1`；裸 `Navigate` 后 `nav=3 load=4 ready=1`

## 阶段 E3 — 默认双击 = 浏览器主窗口 + 搜索 + 自动入 RAG ✅ 2026-10-08

**E4/E5 已按用户要求删除**（idle 自动快照、CDP/Edge 兜底不再作为独立任务）。

**模块**：`gui/src/browser.rs`（`OVERLAY_INIT` + `open_main`）、`gui/src/store.rs`（RAG 知识库）、`gui/src/main.rs`（默认启动 + `boot_main`）

- **默认启动**：`tauri.conf.json` 的 `windows` 置空；`setup()` 用 `WebviewWindowBuilder` 建 `main` 窗口（`about:blank` → 装好 `add_WebMessageReceived` → `Navigate` 到 `https://www.bing.com/`）。**主窗口本身就是浏览器**，双击无需任何参数
- **页面内搜索条**：`OVERLAY_INIT` 在每页顶部叠加 C96 搜索/网址输入框；回车 → `{type:"search"}`（记为当前 query）→ `http(s)://` 直开、否则 Bing SERP；同时保留 click / SPA / `ready` 事件
- **自动入库**：`DOMContentLoaded` 后 1.8s 读 `document.body.innerText`（临时隐藏叠加条）→ `{type:"capture",url,title,text}` → `store.rs` 追加写
- **RAG 知识库** `gui/src/store.rs`：`%APPDATA%\com.c96.gui\kb.jsonl`，每行 `KbDoc{hash,url,title,text,query,host,stored_at_ms,text_len}`；`FNV-1a64(normalized URL)` 去重 + `text_len<200` 跳过；`kb_stats` / `kb_search`（子串匹配计分 + 片段）。embedding/向量索引留待后续
- **验证**：gui `cargo build` **0 warning** · 真实无参数双击 15s → kb `2→3`（`cn.bing.com` 搜索页 875 字）· `--selftest-rag` exit 0：`+1` 文档、`search("Rust")` `score=27`、重复 URL `skip: duplicate url`、模拟输入「c96 端到端检索」→ SERP 入库 `query=c96 端到端检索`（3245 字）· 截图 [e3-main-window.png](gui/docs/e3-main-window.png)

## 阶段 E3+ — CLI → GUI 启动桥 ✅ 2026-10-08

**模块**：`src/cli/gui.rs`（零新依赖：std + 一个 `ShellExecuteW` FFI 声明）

- **无参数（双击）** 运行 `C96.exe` → 查找并拉起 GUI 进程（不再打印 usage）；`C96 gui` 显式启动；GUI 未构建则回退打印 usage 并给出构建提示
- **候选路径**：同目录 `c96-gui.exe` → `<root>/gui/target/release/c96-gui.exe` → `<root>/gui/target/debug/...` → 当前工作目录下同理；环境变量 `C96_GUI` 可显式指定
- **不继承句柄**：Windows 用 `ShellExecuteW`（shell32）交由 shell 启动。用 `std::process::Command` 时 Windows 创建进程 `inherit_handles=TRUE`，GUI 子进程会持有父进程 stdout 管道，导致 `C96 gui` 带管道调用一直挂住（已实测并修掉）
- **验证**：core build（debug+release）**0 warning** · `cargo test --offline` **65/65 green**（+3 个 gui 路径单测）· release `C96 --help` 打印 usage 且**不**启动 GUI · 无参数 **46ms** 返回并拉起 GUI（标题 `C96 — 浏览器 / RAG 采集`）· `help` / `-h` / `--version` 同样不再误启 GUI

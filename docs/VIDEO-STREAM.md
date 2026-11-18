# 边下边放（Watch While Downloading）：调研与实现

> 最后更新：2026-11-18
> 相关代码：`src-tauri/src/services/video_stream/`、`video_server.rs`、`video_player.rs`、`commands/video_stream.rs`

## 一、同类产品怎么做

| 产品 | 模式 | 关键点 |
|---|---|---|
| **Wallpaper Engine（官方）** | 先下载后播放 | 工坊页只有作者上传的**预览视频**（Steam 视频 CDN），完整壁纸必须下载到本地后才能播放；没有"边下边放" |
| **Steam 客户端工坊页** | 在线预览 | `<video>` 播放作者上传的预览片（`steamusercontent.com`），不是 depot 里的原始视频文件 |
| **qBittorrent**（PR #12542） | **HTTP byte-serving** | 给播放器一个本地 HTTP URL，播放器按 `Range` 请求，qBittorrent 只下载被请求的 piece；支持播放中 seek，文件边下边看 |
| **TorrServer** | TorrServer 的 HTTP 流 | 把 torrent piece 映射为 HTTP 字节范围，播放器按需拉取；缓存已读 piece |
| **Plex / Jellyfin** | Direct Play 走 HTTP Range | 原文件直接按 `Range` 响应；需要转码时改用 HLS/DASH 分片 |
| **Lively Wallpaper** | 本地文件 + libmpv/VLC | 只播放本地文件，不涉及工坊下载 |
| **video-proxy-server**（Rust） | 本地代理 + 字节范围缓存 | 监听 127.0.0.1，按 `Range` 从上游拉取，并把**已完成的字节范围持久化到磁盘** |

**共同模式**：把"下载源"包装成一个**本地 HTTP 文件服务器**（支持 `Range`/`206`/`Content-Range`），播放器（mpv/VLC/浏览器）只认 URL，不关心数据来自 torrent、depot 还是磁盘。播放时按播放器请求的窗口按需拉取，配合预取/缓存避免卡顿。

## 二、WEaveR 现状

已经具备的核心（2026-11-18 前）：

```
Steam CM (refresh token 登录)
  -> PublishedFile.GetDetails (manifest id)
  -> depot key + CDN directory + per-host tokens
  -> manifest (chunk spans: offset/len/chunk_id/checksum)
  -> ResolvedVideo
       chunk_at(): cache -> inflight 合并 -> fetch_chunk(全 endpoint 竞速, 解密+解压)
       prefetch(): 12 并发 + 48 chunk runway
  -> VideoServer (axum, 127.0.0.1:0, GET/HEAD /video, 206 + Content-Range)
  -> mpv / VLC
```

即：**"按需拉 chunk + 本地 HTTP Range"已经实现**，与 qBittorrent/TorrServer 的思路一致。

## 三、差距（"尚未开发完成"的具体点）

| # | 差距 | 影响 |
|---|---|---|
| 1 | `video_stream_start` 不等待正在进行的 prewarm，会**重复 resolve**（再次登录 + manifest） | 点按钮可能卡 40-60s，网络翻倍 |
| 2 | prewarm 只有 `pending` 去重，**没有状态/错误记录** | 前端不知道"准备中/失败"，点击后只能干等 |
| 3 | `video_player::launch` 丢弃 `Child`，**播放器退出后 range server 不停止** | 后台继续 prefetch、占用端口/带宽，直到再次点击或退出应用 |
| 4 | mpv/VLC 没有缓存参数 | CDN 抖动时播放器容易缓冲/卡顿 |
| 5 | **不落盘**：播放过的 chunk 只在内存（128MB LRU） | 重复播放/seek 回看要重新下载 |
| 6 | 与「装壁纸」下载任务完全独立 | "下载中"与"播放"是两条链路，不共享数据 |

## 四、方案对比

| 方案 | 内容 | 收益 | 成本/风险 | 结论 |
|---|---|---|---|---|
| **A 最小闭环** | 状态机（pending/ready/failed）+ start 等待 prewarm + 播放器退出清理 + mpv/VLC 缓存参数 | 解决"点了没反应/重复解析/后台跑" | 低（约 200 行） | **必做** |
| **B A + 磁盘 chunk 缓存** | 已下载 chunk 原子写盘（`video-cache/<item>/<manifest>/`），读取优先命中磁盘 | 重复播放秒开；边下边存；seek 回看不再重新下载 | 中（约 200 行 + 磁盘清理） | **推荐** |
| C A + 下载任务联动 | 下载任务与流共享数据（DepotDownloader 写盘范围不可控） | 真正"下载即看" | 高（需改 DepotDownloader 或自研下载器） | 后续 |
| D 内置播放器 | libmpv/WebView 内嵌 | 不依赖外部 mpv/VLC | 高（体积大、跨平台差） | 不做 |

**选型：A + B**。理由：
- A 是功能完整性的底线，先保证"点了能稳定播放"；
- B 让"边下边放"闭环（边播边存、重复秒开），且纯 Rust、跨平台、与现有 `layout_cache` 风格一致；
- C/D 超出本轮范围，不引入不可控的 DepotDownloader 改造或大体积依赖。

## 五、实现设计

### 5.1 状态机（A）

`StreamRegistry` 增加：

```rust
enum PrewarmState {
    Pending { started: Instant },
    Ready,
    Failed { message: String, at: Instant },
}
state: Mutex<HashMap<u64, PrewarmState>>,
```

- `video_stream_prewarm`：首次调用写 `Pending`；完成写 `Ready` / `Failed`。
- `video_stream_status(item_id)`：返回 `{ state: "idle"|"pending"|"ready"|"failed", error? }`，供前端轮询。
- `video_stream_start`：
  - `ready` 有 → 直接用；
  - `pending` → 轮询等待（最多 120s），**不重复 resolve**；
  - `failed`/idle → inline resolve 一次，成功后进入 ready。

### 5.2 播放器生命周期（A）

- `video_player::launch` 改为返回 `std::process::Child`。
- `video_stream_start` 启动后 spawn 一个任务：`tokio::task::spawn_blocking(|| child.wait())` → 按 `port` 匹配移除并 `stop()` 对应的 `VideoServer`（防止重复点击时误删新 server）。

### 5.3 播放器缓存参数（A）

mpv：`--cache=yes --demuxer-max-bytes=256MiB --demuxer-max-back-bytes=64MiB --cache-pause=yes --cache-pause-wait=2 --network-timeout=30`
VLC：`--network-caching=3000`

### 5.4 磁盘 chunk 缓存（B）

新增 `services/video_stream/disk_cache.rs`：

```rust
pub fn chunk_path(item_id: u64, manifest_id: u64, chunk_id: &str) -> Option<PathBuf>;
pub fn load(item_id, manifest_id, chunk_id) -> Option<Vec<u8>>;
pub fn store(item_id, manifest_id, chunk_id, bytes: &[u8]) -> io::Result<()>; // tmp + rename 原子写
pub fn prune(max_bytes: u64); // 按 mtime 淘汰，启动/阈值触发
```

- 目录：`%LOCALAPPDATA%\WEave\video-cache\<item_id>\<manifest_id>\<chunk_id>.bin`
- `ResolvedVideo::chunk_at`：先查磁盘 → 命中返回；否则网络 → 写盘 → 内存缓存。
- 上限：默认 4 GiB（常量），超过按最旧 mtime 淘汰；`video_stream_stop` 不删除缓存。
- 校验：`process_chunk` 已做 checksum；磁盘命中可信任（文件由本进程原子写）。

### 5.5 前端（A）

- 后端已提供 `video_stream_status` 命令（idle/pending/ready/failed + error），前端当前沿用既有的按钮 spinner（`pendingId`）；后续可接轮询显示"准备中/已就绪"。
- 失败时显示后端 `Failed.message`（不再只显示 "STEAM_GUARD_REQUIRED" 等原始串）。

## 六、验收

1. 打开详情页 → prewarm 开始；点击「边下边看」→ 不重复 resolve（日志无第二次 `depot_key+cdn_servers`）。
2. 播放器退出 → 日志 `[video-stream] player exited for <id>` + `stopped range server`，端口释放、prefetch 停止。
3. 同一 item 第二次播放：已看过的 chunk 从磁盘命中（日志 `disk hit`），首帧明显更快。
4. mpv 缓存参数生效（`mpv --version` 参数不回显，但播放中 seek 更稳）。
5. item 3838500850 实测可播放；Rust 136 + 前端 154 测试通过。

## 七、实现状态（2026-11-18）

| 项 | 状态 | 证据 |
|---|---|---|
| prewarm 状态机（pending/ready/failed） | ✅ | `video_stream_status` 命令；`PrewarmState` |
| `video_stream_start` 等待 prewarm、不重复 resolve | ✅ | `PREWARM_WAIT_MAX=120s` 轮询 |
| 播放器退出后停止 range server | ✅ | `spawn_blocking(child.wait)` → 按 port `stop()` |
| mpv/VLC 缓存参数 | ✅ | mpv 256 MiB/64 MiB；VLC 3s |
| 磁盘 chunk 缓存 | ✅ | 实测 `%LOCALAPPDATA%\WEave\video-cache\3838500850\...` 7×1 MiB |
| 测试 | ✅ | Rust 136；clippy `-D warnings` 0 |

## 八、P0 / P1 实施（2026-11-18）

### 调研结论

| 项目 | 结论 |
|---|---|
| `psyattack/WEave`（原版 Python） | 纯下载管理器，没有「边下边看」；无可复用实现 |
| `SteamAutoCracks/WallpaperEngineWorkshopDownloader` | 全量下载，无流式 |
| `mpv-ipc` / `mpvipc` crate | 可控制已运行的 mpv（播放状态/IPC），本轮不需要 |
| `get-air/tauri-video-plugin` | 内置 GStreamer/libmpv 的 Tauri 播放器；属「内置播放器」方向，体积/平台成本高，不采用 |
| qBittorrent / TorrServer / Jellyfin | HTTP byte-serving 模式，WEaveR 已采用 |

**结论：没有可直接复用的成品「WEaveR 边下边放」实现；本轮不引入新依赖。**

### P0（正确性 / 省流）

| 项 | 实现 |
|---|---|
| 只对视频壁纸 prewarm | `isVideoItem()` 判断 tags（`Video`/`视频`/`動画`/`影片`/`movie`，兼容 string 与 {tag,category}）。**注意：browse 列表项不带 tags**，所以同时检查列表项与详情页 `meta.tags`；`DetailsSidebar` 仅在视频且未安装时调用 `prewarmVideoStream` |
| 只对视频壁纸显示「边下边看」 | `DetailsActionButtons` 加 `isVideoItem(item)` 条件 |
| 已安装的壁纸不走流 | `installedHandle` 存在时隐藏「边下边看」；`DetailsSidebar` 已安装不 prewarm |

### P1（体验）

| 项 | 实现 |
|---|---|
| 前端接 `video_stream_status` | `getVideoStreamStatus()`；`useWatchWhileDownloading` 点击时先查 status，`failed` 直接显示原因，`pending` 显示「正在准备视频…」 |
| 播放器设置 | `SettingsService` 的 `video_stream.player_path`；`video_player::detect_with(preferred)`；设置页「视频」tab 可浏览/输入路径并显示检测结果 |
| 缓存管理 | `disk_cache::stats()/clear()`（只删 `.bin`，保留目录）+ `video_stream_cache_stats`/`video_stream_cache_clear` 命令；设置页显示文件数/占用并支持清理 |
| 播放后引导「装为壁纸」 | `WatchState.startedId` 按 item 匹配，按钮下方提示「正在外部播放器播放；喜欢可点『装壁纸』保存到本地」 |

### 验证

- 前端：**160 passed**（新增 `isVideoItem` 3 个 + `DetailsActionButtons` 3 个），typecheck / lint 0。
- Rust：**136 passed**，clippy `-D warnings` 0。
## 八、端点健康池（2026-11-18）

### 问题（实测）

- 单流播放时 CDN ESTABLISHED 连接 44–81 个；
- 多个 CDN IP 的 TCP 连接卡在 SYN_SENT（对端不响应）；
- 网络长时间 0–10 KB/s，偶发 8–9 MB/s 爆发；一次卡顿持续 2 分 40 秒；
- 旧实现：每个 chunk 同时向所有 endpoint 发完整请求（select_all），probe 每个 prewarm 都并发探测所有 host；fetch_chunk 每次尝试 60s、chunk_at 重试 3 次 → 最坏 180s。

### 设计

新增 `services/video_stream/endpoint_pool.rs`：

| 项 | 值 |
|---|---|
| 每 chunk 最多尝试 host 数 | 2（MAX_ATTEMPTS） |
| 单次请求超时 | 10s（CHUNK_REQUEST_TIMEOUT） |
| 失败冷却 | 30s（FAILURE_COOLDOWN） |
| 排序 | 非冷却 > 失败次数少 > 平均延迟低 |
| chunk_at 重试 | 2 次（原 3 次） |

- fetch_chunk 改为顺序尝试 2 个候选 host，不再对全部 endpoint 竞速；
- 删除 per-prewarm 的全量 endpoint probe（保留全部 endpoint + token 优先排序）；
- HTTP 连接池 pool_max_idle_per_host(2)，避免 idle 连接堆积；
- 移除 ResolvedVideo.preferred，由 EndpointPool 统一记录成功/失败/延迟。

### 验证

- Rust：141 passed（新增 5 个 endpoint_pool 测试），clippy -D warnings 0；
- 播放实测：待重新构建后复测（目标：单流连接数从 44–81 降到约 12–20，卡顿空窗消失）。

### 后续实测（2026-11-18 16:25-16:28）

- 25s 超时 + token 过滤后，单流连接数约 21-47（旧版 44-81），但播放仍卡在首帧；
- 日志：所有 CDN host 的 chunk 请求 `error sending request` / `timed out after 25s` / `HTTP 403`；
- curl 实测：`st.dl.eccdnx.com` 的 chunk URL 返回 302，跟随 302 后 30s 无响应；`xz.pphimalayanrt.com` 返回 302（无 Location）；`dl.steam.clngaa.com` 返回 403；IPv6 全部失败；
- 结论：**当前网络到 Steam CDN 节点不可用/极不稳定**，不是线程数或本地代码能解决；
- 代码侧追加：chunk HTTP client 禁用重定向跟随（302 快速失败换 host），CDN server 列表从 5 个增加到 16 个。

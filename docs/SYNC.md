# 手机同步（WE-Sync 集成）

## 架构

- 依赖 `D:/Repos/we-sync` 的 core（path 依赖，`default-features = false`，不编译其 Win32 GUI）；
- `services/sync/mod.rs`：`SyncService` 持有 we-sync `EngineHandle`，事件线程把 `Event` 转成 `SyncEventDto` 并通过 `sync://event` 发给前端；
- `services/sync/config.rs`：WEaveR `settings.json` 的 `sync` 对象 ↔ we-sync `AppConfig`；
- `services/sync/event.rs`：唯一的 we-sync Event 适配层；
- `services/sync/state.rs`：设备/进度/统计/队列/日志快照，供 `sync_status` 立即返回；
- `commands/sync.rs`：`sync_start/stop/status/now/pause/transfer_pause/config_get/config_set`；
- `core/shutdown.rs`：关闭时先 `SyncService::stop()`（发送 `Cmd::Quit`，worker 保存 state 后退出）。

## we-sync 侧改动（独立仓库）

- `[features] default=["ui"]; ui=[]`；`[[bin]] required-features=["ui"]`，`lib.rs` 的 `pub mod ui` 由 feature gate 控制；
- `util::set_app_dir(PathBuf)`：WEaveR 启动时把 config/state/log 指到 `%LOCALAPPDATA%/WEave/sync`；
- `EngineHandle::quit()`：发送 `Cmd::Quit`；
- license 改为 `MIT OR GPL-3.0-only`（作者授权，WEaveR 以 MIT 使用）。

## 首版范围

- MTP / ADB（自动选择）；不包含 Multipath（依赖 Android agent）。
- Windows-only（MTP 走 WPD）；其他平台不显示入口。

## 验证

- we-sync：`cargo check --lib --no-default-features`、`cargo check --lib`、`cargo check` 全部通过；
- WEaveR：`cargo check` / `cargo test` / clippy / `tsc --noEmit` / vitest / `npm run build:fast`；
- 手测：无手机提示；MTP/ADB 同步；暂停/继续；关闭 WEaveR 后无残留线程。

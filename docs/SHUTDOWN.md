# 关闭与进程清理（Shutdown）

## 问题

- 自定义标题栏 X 走 `app_quit`：销毁所有窗口 + 150ms 硬退出（正常）；
- **Alt+F4 / 任务栏关闭**只销毁主窗口；隐藏的 `steam-webview`（cookie 同步用，`visible(false)`、`skip_taskbar(true)`）仍存活 → **WEaveR 进程残留**；
- `video_stream` 启动的 mpv/VLC 是独立进程，`app_quit` 没有句柄，**关闭应用后播放器仍在**；
- 残留实例还会让下一次启动被单实例插件吞掉（窗口不出现）。

## 设计

新增 `src-tauri/src/core/shutdown.rs`：

- `request(app, reason)`：进程级 `Once` 幂等；依次停视频流 → 杀播放器 → 保存窗口几何 → 销毁全部 webview → 800ms 后 `std::process::exit(0)`；
- `install_watchdog(app)`：每 2s 检查主窗口是否存在，消失则触发 `request`（兜底任务栏/异常路径）；
- `commands/app.rs::app_quit` 只转调 `request`；
- `lib.rs::on_window_event`：主窗口 `CloseRequested` → `prevent_close()` + `request`；
- `StreamRegistry` 新增 `players`（保存 `Child` 句柄），`shutdown_all()` 停 server + `Child::kill`。

## 验收

1. 标题栏 X / Alt+F4 / 任务栏关闭 → `Get-Process WEaveR,mpv` 1s 内为空；
2. 播放视频时关闭 → mpv 一并退出；
3. 关闭后立即启动 → 主窗口正常出现（无单实例残留）；
4. 日志出现 `[shutdown] requested: <reason>`。

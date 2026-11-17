# Steam 登录持久化：设计与现状

> 最后更新：2026-11-17
> 相关代码：`src-tauri/src/services/steam_session/`、`src-tauri/src/commands/steam/`、`src-tauri/src/services/workshop/webview/`

## 一、目标

登录一次后，应用重启、长时间运行均保持 Steam 登录；网络异常不误判为登出；任何失败都可诊断、可恢复。

## 二、旧实现的问题（有实测证据）

| # | 问题 | 证据 |
|---|---|---|
| 1 | `refresh_token` 只存不用，access token 约 24h 过期后无法续期 | 全仓搜索无 `GenerateAccessTokenForApp` 调用 |
| 2 | `steamLoginSecure` 是 session cookie（未设置 expiry），每次启动必须重建 | `services/workshop/webview.rs` 的 `install_cookies_for_window` |
| 3 | `auto_login` 只检查 cookie 是否存在，stale cookie 会导致假成功 | `webview/login.rs` 旧版 early return |
| 4 | `steam_restore_session` 把网络失败当作"Steam 拒绝会话" | 旧版用 `current_account()`（None 混淆失败与登出） |
| 5 | 表单登录路径不保存 OAuth session | `auto_login` 成功后只 `sync_cookies` |
| 6 | 账号文件 80 字节，仅用户名/密码，无 session | 用项目同款 PBKDF2+AES-GCM 解密实测 |
| 7 | 日志 `[startup] steam session restored: false` 113 次 / true 16 次（true 仅代表 cookie 注入成功） | `%LOCALAPPDATA%\WEave\.log` |

## 三、设计

### 模块

```
services/steam_session/
├── mod.rs          SteamSessionService：ensure_access_token / save_login / verify
├── token.rs        JWT claims（sub/exp/aud）解析与可用性判断
├── refresh.rs      refresh token -> access token（CM protobuf 优先，HTTP form 回退）
├── local_source.rs Steam 客户端 local.vdf 的 refresh token（best effort）
└── verify.rs       GET steamcommunity.com/my/，按 302 Location 判定
```

### 恢复流程

```
steam_restore_session(account)
  -> ensure_access_token(account)
       -> 存储的 access token 仍有效? 用它
       -> 否则用 refresh token（或本地 Steam token）刷新
       -> 成功后写回 user_accounts.enc（AES-256-GCM）
  -> install_session_cookies(steam_id, access_token)
  -> verify(): GET /my/（不跟随重定向）
       -> 302 /id/* 或 /profiles/{steamid} -> signed_in
       -> 302 /login/* -> signed_out
       -> 网络失败 -> unknown（不当作登出）
```

### 三态语义

| 状态 | 含义 | 前端行为 |
|---|---|---|
| `signed_in` | Steam 实际接受了重建的会话 | 显示已登录 |
| `signed_out` | 无可用 token，或 Steam 明确拒绝 | 显示未登录，提示登录 |
| `unknown` | 验证请求失败（网络） | 保持上次状态，不清 cookie、不提示"登录失败" |

## 四、P0 实测记录（2026-11-17）

| 尝试 | 结果 | 说明 |
|---|---|---|
| HTTP form POST `GenerateAccessTokenForApp`（本地 Steam token） | `200 {"response":{}}`，`x-eresult: 15`（AccessDenied） | 端点可达，但该本地 token 已被轮换/撤销 |
| 匿名 CM `call_service_method_non_authed` | `AccessDenied` | 该 API 需要已认证会话 |
| CM `with_refresh_token` 登录（本地 token） | `AccessDenied` | 本地 token 已失效 |
| refresh token 直接作 `steamLoginSecure` cookie | 302 到 `/login/home/` | Steam 只接受 access token，不接受 refresh token |

**结论**：本地 Steam token 只能作为 best-effort 回退；主路径必须是 OAuth/QR 登录保存的 refresh token。真实刷新链路的端到端验证需要一次新的登录（产生新鲜 token）。

## 五、故障排查

| 现象 | 检查 |
|---|---|
| 启动后 `signed_out` | 看日志 `[steam-session]` 行：`no usable session material` / `refresh failed` / `restore verify` |
| 一直 `unknown` | 检查 `steamcommunity_302` 是否运行（hosts `#S302` 指向 127.0.0.1:443） |
| 登录后仍匿名 | 看 `[steam-session] OAuth login verify`；若 `signed_out`，token 不被社区接受 |
| 刷新失败 `AccessDenied` | refresh token 已失效；重新登录一次 |

## 六、启动耗时优化（2026-11-17）

同一台机器、同一账号实测：

| 场景 | 优化前 | 优化后 |
|---|---:|---:|
| 已登录启动（首次，无持久 cookie） | 12.7s | **2.8s** |
| 已登录启动（持久 cookie 命中） | — | **1.6s** |

优化点：

1. `ensure_profile_loaded` 以前盲等 `steamLoginSecure` 最多 6 秒（40×150ms）；session cookie 在重启后本来就不存在，必然等满。改为 8×50ms（0.4s 上限）。
2. `install_cookies_for_window` 给 `steamLoginSecure` / `sessionid` 设置 30 天 `max_age`，WebView2 重启后窗口创建时即可见（日志 `webview profile ready: true`）。
3. `init_cookies` 用 `tokio::join!` 并行「窗口准备」与「token 准备」。
4. `SESSION_RESTORE_SETTLE`（1.2s 固定 sleep）→ `wait_for_session_cookie(10, 100)` 条件等待，通常 <100ms。
5. `steam_probe_account` 改用 `steam_session.verify`（直接构造 cookie + `/my/` 302 判定），不再依赖 reqwest cookie jar 的时序；消除了启动时一次假的 `signed_out` 与重复 `steam_restore_session`。
6. 刷新失败增加 5 分钟冷却；`force_refresh` 绕过冷却。

## 七、已知限制

- `steamcommunity_302` 未运行时，所有 Steam 请求失败 → `unknown`（这是预期行为，不代表会话失效）。
- 本地 Steam token 需要 Steam 客户端运行且 token 未过期；不能保证可用。
- `machine_seed` 目前绑定 `COMPUTERNAME + ARCH`，改机器名会导致账号文件无法解密（后续可改用 `MachineGuid` + 旧 seed 回退）。
- `GenerateAccessTokenForApp` 的真实可用性依赖 Steam 服务端策略；若 CM 与 HTTP 都被拒绝，用户需重新登录。
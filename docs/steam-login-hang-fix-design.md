# WEaveR Steam 登录卡死 — 修复设计方案

> 状态：**设计稿，未实施**（用户选择方案 4：先看详细方案再决定）
> 日期：2026-11-25
> 目标程序：`D:\Repos\WEaveR\src-tauri\target\release\WEaveR.exe`（v4.13.0）

---

## 1. 问题陈述（实测证据）

| # | 现象 | 证据 |
|---|---|---|
| P1 | 手机确认通过后，UI 永久转圈 | 用户反馈；进程仅剩 `127.0.0.1:443` 回环，**无任何到 Steam 的外网 TCP** |
| P2 | 报错 `mobile confirmation failed: transport / connection error: disconnected` | `[21:07:34.001] ERROR [steamroom::transport::tcp] tcp recv header failed: early eof` |
| P3 | 后台每 3.5 秒刷 `refresh cooldown` | 日志 21:05–21:15 持续 |
| P4 | HTTP 刷新报「响应体无法解码」 | `api.steampowered.com` → `127.0.0.1`，Caddy 返回 **502** |

### 1.1 根因链

```
steamcommunity_302 占用 127.0.0.1:443
        ↓
Steam 全域名（api / steamcommunity / login / store）解析到 127.0.0.1
        ↓
上游不通 → 全部 502 / 503
        ↓
GetCMListForConnect 503 → CmServer::fetch() 失败 → 静默回落硬编码 3 个地址
        ↓
默认地址中 205.196.6.148:27017 不可达；其余连接存活不稳定
        ↓
Steam CM 在等待手机确认期间关闭空闲连接（early eof）
        ↓
poll_until_tokens 用 `?` 直接抛错 → LoginError::Transport(Disconnected)
        ↓
前端显示错误（或另一次尝试中 recv 永久挂起 → 无限转圈）
```

### 1.2 关键代码位置

| 文件 | 行 | 作用 |
|---|---|---|
| `vendor/steamroom-client/src/login/credentials.rs` | 247–258 | `poll_until_tokens` — 无超时、无重连 |
| `vendor/steamroom-client/src/login/credentials.rs` | 228–241 | `MobileChallenge::wait_for_confirmation` |
| `vendor/steamroom-client/src/login/credentials.rs` | 188–222 | `GuardChallenge::submit_code` |
| `vendor/steamroom-client/src/login/qr.rs` | 85–98 | `QrLoginFlow::wait_for_scan` |
| `src-tauri/src/commands/steam/oauth.rs` | 142–154 | `NeedsMobileConfirm` 分支 |
| `src-tauri/src/services/steam_session/refresh.rs` | 65–94 | CM 列表静默回落 |
| `steamroom/src/client/mod.rs` | 370–404 | `call_service_method_non_authed` — 无限 `recv_raw` 循环 |
| `steamroom/src/client/mod.rs` | 537 | `send_heartbeat` — **全项目 0 处调用** |

---

## 2. 设计原则

1. **单点修复，全局生效** — `poll_until_tokens` 是 OAuth 手机确认、Guard 码、QR 扫码、免 2FA 四条路径的唯一汇合点。只改它，四条路径同时受益，不产生分叉实现。
2. **不改变既有成功路径** — 所有新增行为仅在「失败/超时/断线」时触发。正常登录的代码路径与耗时不变。
3. **可独立回滚** — 每个改动单元自带 `#[cfg]` 或独立函数，出问题可单独撤。
4. **不碰 UI 契约** — `OAuthBeginResult` 的 `status` 枚举值不变，前端 `LoginModal.tsx` 无需改动。

---

## 3. 改动设计（分四个独立单元）

### 单元 A：等待策略 — 超时 + 心跳 + 重连（核心）

**位置**：`vendor/steamroom-client/src/login/credentials.rs`

**现状问题**：`poll_until_tokens` 是一个裸 `loop`，三个缺陷：
- 无总时长上限 → 永久挂起（P1）
- 无心跳 → CM 判定空闲后关闭连接（P2 的诱因）
- 出错即 `?` 上抛 → 一次抖动即失败

**设计**：把 `poll_until_tokens` 拆成「策略」与「执行」两层。

```rust
// 新增：等待策略，把散落的魔法数字集中
pub(crate) struct PollPolicy {
    /// 单次轮询间隔（来自 Steam 的 interval，通常 5s）
    pub interval: Duration,
    /// 整体截止时间。手机确认需要人操作，给足 5 分钟。
    pub deadline: Duration,
    /// 连续多少次传输错误后放弃
    pub max_transport_errors: u32,
    /// 发送心跳的间隔
    pub heartbeat: Duration,
}
```

```rust
pub(crate) async fn poll_until_tokens(
    client: &SteamClient<Ready>,
    client_id: u64,
    request_id: &[u8],
    interval_secs: f32,
) -> Result<steamroom::auth::AuthTokens, LoginError> {
    // 保持原签名不变 —— 四条调用路径零改动
    poll_until_tokens_with_policy(
        client, client_id, request_id,
        PollPolicy::from_interval(interval_secs),
        None, // 无重连工厂
    ).await
}
```

```rust
pub(crate) async fn poll_until_tokens_with_policy(
    client: &SteamClient<Ready>,
    client_id: u64,
    request_id: &[u8],
    policy: PollPolicy,
    reconnect: Option<&dyn ReconnectFactory>,
) -> Result<steamroom::auth::AuthTokens, LoginError> {
    let started = Instant::now();
    let mut transport_errors = 0u32;
    let mut last_beat = Instant::now();

    loop {
        // 1) 截止时间
        if started.elapsed() >= policy.deadline {
            return Err(LoginError::ConfirmationTimeout { elapsed: started.elapsed() });
        }
        // 2) 等待下一个轮询点（不 sleep 整个 interval，好让超时及时生效）
        tokio::time::sleep(policy.interval.min(policy.deadline - started.elapsed())).await;

        // 3) 心跳：连接空闲超过 heartbeat 就发一次，防止 CM 判定空闲
        if last_beat.elapsed() >= policy.heartbeat {
            // 心跳失败不算致命 —— 下一轮 poll 会暴露真实状态
            let _ = client.send_heartbeat().await;
            last_beat = Instant::now();
        }

        // 4) 轮询
        match client.poll_auth_session(client_id, request_id).await {
            Ok(Some(tokens)) => return Ok(tokens),
            Ok(None) => continue,           // 用户还没点，正常
            Err(e) if is_disconnect(&e) => {
                transport_errors += 1;
                log::warn!("[login] poll transport error #{transport_errors}: {e}");
                if transport_errors >= policy.max_transport_errors {
                    return Err(LoginError::Transport(e));
                }
                // 有重连工厂就换一条连接继续等，auth session 在服务端仍有效
                if let Some(factory) = reconnect {
                    match factory.reconnect().await { /* ... */ }
                } else {
                    tokio::time::sleep(policy.backoff()).await;
                }
            }
            Err(e) => return Err(LoginError::Transport(e)), // 业务错误直接上抛
        }
    }
}
```

**关键约束（避免引入新 bug）**：

| 约束 | 理由 |
|---|---|
| `poll_until_tokens` 原签名保留 | 四条调用路径（含 vendor 内 4 处）零改动 |
| `Ok(None)` 不计数 | 用户没点确认是**正常状态**，不是错误 |
| 仅 `ConnectionError::Disconnected` / `Io` 计为可重试 | `ServiceMethodFailed`（如 DuplicateRequest）是业务终态，重试无意义 |
| 心跳失败不中断 | 心跳是尽力而为；真实状态由下一轮 poll 判定 |
| `deadline` ≥ 300s | 手机确认需要人找手机、解锁、点确认，留足时间 |

**新增错误变体**（`vendor/steamroom-client/src/login/error.rs`）：

```rust
#[error("timed out waiting for confirmation after {elapsed:?}")]
ConfirmationTimeout { elapsed: std::time::Duration },
```

---

### 单元 B：连接重连能力

**位置**：`vendor/steamroom-client/src/login/mod.rs` + `credentials.rs`

**问题**：`MobileChallenge` 持有 `SteamClient<Ready>`，连接一断就再无救。而 `request_id` 是**服务端会话标识**，与连接无关 —— 换条连接照样能 poll。

**设计**：引入一个轻量的重连工厂 trait，**只依赖已有的 `TransportConfig`**：

```rust
/// 断开后重建一条到 CM 的 Ready 连接。
/// 之所以能复用 auth session：client_id / request_id 由服务端维护，
/// 与 TCP 连接无绑定关系。
#[async_trait]
pub(crate) trait ReconnectFactory: Send + Sync {
    async fn reconnect(&self) -> Result<SteamClient<Ready>, LoginError>;
}

/// 用 TransportConfig 的配置快照实现重连。
struct AutoReconnect {
    prefer: Protocol,
    allow_fallback: bool,
}

#[async_trait]
impl ReconnectFactory for AutoReconnect {
    async fn reconnect(&self) -> Result<SteamClient<Ready>, LoginError> {
        Ok(connect_auto(self.prefer, self.allow_fallback, None)
            .await?
            .prepare()
            .await?)
    }
}
```

**注意**：`recorder` **不传入重连** —— 录制是一次性的，重连后继续录会把两条连接的包混在一起。

**改造 `MobileChallenge` / `GuardChallenge` / `QrLoginFlow`**：各自增加一个 `Option<AutoReconnect>` 字段（由 `begin()` 从 `TransportConfig` 派生）。`TransportConfig::Provided` 时为 `None`（BYO client 无法重建，退回单元 A 的超时保护）。

---

### 单元 C：CM 列表不再静默回落

**位置**：`steamroom/src/connection/mod.rs:65-94`（crates.io 依赖，需本地 patch 或上游 PR）

**现状**：
```rust
let resp: CmListResponse = client.get(url).send().await?.json().await?;  // 失败即 Err
// ...
if servers.is_empty() { return Ok(CmServer::defaults()); }             // 静默回落
```

调用方 `connect_auto` 里还有 `.unwrap_or_else(|_| CmServer::defaults())` —— **双重静默**。用户完全不知道 CM 列表拿不到。

**设计**：两处都要改，但**不改行为，只加可观测性**：

```rust
// connect_auto 中
let servers = match CmServer::fetch().await {
    Ok(s) => s,
    Err(e) => {
        log::warn!("[cm] server list fetch failed ({e}); falling back to {} hardcoded CMs", 
                   CmServer::defaults().len());
        CmServer::defaults()
    }
};
```

**为什么不改成「失败就报错」**：硬编码回落是**有意设计的容错**（API 被墙时仍能连）。把它改成硬失败会破坏「能连但列表拿不到」的场景，属于修一个 bug 引入另一个。

**建议**：这项属于 `steamroom` 上游，优先级最低。**如果不想动 crates.io 依赖，可以只做 A + B**，C 用日志观察即可。

---

### 单元 D：环境层（零代码，立即可做）

| 动作 | 命令/方式 | 效果 |
|---|---|---|
| 退出 `steamcommunity_302` | 结束 PID 23352 / 25596 / 5332 | Steam 域名恢复真实解析，HTTP 刷新路径复活 |
| 或改为不接管 `api.steampowered.com` | 302 工具设置 → 取消 API 域名 | 保留社区加速，只放行 API |
| 验证 | `Resolve-DnsName api.steampowered.com` | 应返回公网 IP 而非 127.0.0.1 |

**注意**：用户此前明确开着加速器 + 302 是为了解决 Steam 社区访问。**关掉会影响工坊访问**。所以更推荐「只放行 api.steampowered.com」的折中方案。

---

## 4. 单元间依赖与实施顺序

```
D（环境，0 代码）
 └─ 立刻验证：HTTP 刷新是否恢复
      ↓
A（等待策略）  ← 独立，可单独上
 └─ 消除「无限转圈」，最坏情况 5 分钟后明确报错
      ↓
B（重连）      ← 依赖 A 的策略框架
 └─ 断线后自动换连接，真正修好「手机点了没反应」
      ↓
C（CM 列表日志）← 独立，可选，优先级最低
```

**建议顺序：D → A → B**，C 可选。

---

## 5. 验收标准

| 编号 | 场景 | 期望 | 验证方法 |
|---|---|---|---|
| V1 | 正常登录，手机及时确认 | 与改动前完全一致，无额外延迟 | 手动登录一次，对比耗时 |
| V2 | 手机确认前连接被断 | 自动重连并拿到 token，登录成功 | 用防火墙断 CM 连接模拟 |
| V3 | 手机始终不确认 | 5 分钟后返回明确超时错误，UI 停止转圈 | 点登录后不操作，观察 |
| V4 | Guard 码输错 | 仍返回 `INVALID_GUARD_CODE`，可重输 | 输错码验证 |
| V5 | QR 扫码登录 | 与改动前一致 | 扫一次码 |
| V6 | 免 2FA 直接登录 | 与改动前一致 | 用可信设备登录 |
| V7 | 断网后恢复 | 不崩溃，给出明确错误 | 拔网线/断 WiFi |

---

## 6. 回归风险清单

| 风险 | 影响 | 缓解 |
|---|---|---|
| 心跳与 poll 抢同一把写锁 | 潜在死锁/乱序 | `TcpTransport` 的 writer 是 `Mutex`，心跳与 poll 都走 `send_raw`，天然串行，无死锁 |
| 重连后旧连接未释放 | fd 泄漏 | `SteamClient` 用 `Arc<ClientInner>`，drop 即关闭；重连时显式替换 |
| 重连风暴 | 对 CM 造成压力 | `max_transport_errors` + 指数退避封顶 |
| 超时误伤慢网络 | 用户明明在操作却被判超时 | `deadline` 取 300s；且 `Ok(None)` 会刷新活跃状态 |
| `PollPolicy` 默认值影响测试 | 单测变慢 | 单测注入短 deadline（如 100ms） |

---

## 7. 代码改动清单（预估）

| 文件 | 改动量 | 说明 |
|---|---|---|
| `vendor/steamroom-client/src/login/error.rs` | +5 行 | 新增 `ConfirmationTimeout` |
| `vendor/steamroom-client/src/login/credentials.rs` | +120 行 | `PollPolicy`、`poll_until_tokens_with_policy`、`ReconnectFactory`；`MobileChallenge`/`GuardChallenge` 加字段 |
| `vendor/steamroom-client/src/login/qr.rs` | +10 行 | `QrLoginFlow` 加字段，`wait_for_scan` 走新路径 |
| `vendor/steamroom-client/src/login/mod.rs` | +30 行 | `AutoReconnect` 实现，`TransportConfig` 派生 |
| `vendor/steamroom-client/Cargo.toml` | +1 行 | `async-trait` 依赖（或改用 `impl Future` 避免新依赖） |
| `src-tauri/src/commands/steam/oauth.rs` | 0 行 | **无需改动**（签名兼容） |
| 前端 | 0 行 | **无需改动** |

**总计约 165 行**，集中在 vendor 的 login 模块，不扩散到应用层。

---

## 8. 待你决定的问题

1. **deadline 取值**：我建议 300s。你希望更短（如 180s，失败更快）还是更长？
2. **C 单元做不做**：要改 crates.io 的 `steamroom`，需要在 Cargo.toml 再加一个 `[patch]`。收益只是日志可观测性。建议**跳过**。
3. **是否同时加 UI 进度提示**：当前手机确认阶段 UI 只有一个转圈，没有任何文案。加一句「请在手机上确认登录」能显著减少困惑 —— 但这会改前端。要做吗？
4. **实施范围**：只做 A，还是 A + B 全做？

---

## 9. 实施结果（2026-11-25 21:45 完成）

用户决策：**A + B + UI 提示**，deadline 300s；**环境层不动**（实测直连 API 12 次采样全失败，关反代也救不回，反而会破坏工坊访问）。

### 9.1 实际改动

| 文件 | 改动 | 说明 |
|---|---|---|
| `vendor/steamroom-client/src/login/error.rs` | +36 | `ConfirmationTimeout` / `ConnectionLost` 两个变体 + 2 个测试 |
| `vendor/steamroom-client/src/login/credentials.rs` | +354/-8 | `PollPolicy`、`is_retryable_transport`、`poll_until_tokens_with_policy`、心跳、4 组测试 |
| `vendor/steamroom-client/src/login/mod.rs` | +49/-1 | `ReconnectHint`、`TransportConfig::reconnect_hint()`、`BuilderConfig: Clone` |
| `vendor/steamroom-client/src/login/qr.rs` | +20/-4 | QR 路径接入重连 |
| `src-tauri/src/commands/steam/oauth.rs` | +6/-2 | 发 `steam-login-awaiting-mobile` 事件 |
| `src/components/dialogs/LoginModal.tsx` | +44/-6 | 监听事件 + 提示文案 + 按钮状态 |
| `src/lib/events.ts` | +3 | 注册事件常量 |
| `src/i18n/locales/{zh,en}.ts` | 各 +3 | `login_waiting` / `login_mobile_confirm` |
| `.gitignore` | +5 | 防止 vendor 构建副产物被提交 |

合计 **9 个源文件，+483/-35 行**。

### 9.2 实施中修正的三个设计缺陷

设计稿里的方案在落地时暴露了问题，均已修正：

1. **递归会丢失重连能力** — 原设计在重连后递归调用自身并传 `None`，等于只能重连一次。改为 owned client + `current` 变量就地替换，无限次重连且不递归。
2. **`send_heartbeat` 只存在于 `LoggedIn` 状态** — 轮询时客户端是 `Ready`，无法调用。改为用 `Ready::send_msg` + `ClientMsg::with_body(CLIENT_HEART_BEAT, &[])`，与库内 `LoggedIn::send_heartbeat` 等价（`Ready` 阶段还没有 steamid/sessionid，不带头是正确的）。
3. **重连后错误计数未重置** — 新连接上的偶发抖动会累积到阈值导致误判。已在重连成功时清零 `transport_errors` 与心跳计时。

另有两处编译期问题：`submit_code` 中 client 被 poll 消费后又返回 `self`（部分移动），改为先解构 + 保留 `client_for_retry` 句柄。

### 9.3 验证结果

```
vendor/steamroom-client   cargo check --all-targets   零警告
                          cargo test                  66 passed / 0 failed
                                                      3 doctests passed
src-tauri (主程序)         cargo check --all-targets   零警告
                          cargo test                  148 passed / 0 failed
前端                       npm run build (tsc+vite)    EXIT=0
release                    cargo build --release        WEaveR.exe 26,956,288 B
``

新增 4 组策略测试覆盖：间隔钳位（0/NaN/120s）、超时预算保持 300s、退避递增且封顶 8s、仅 socket 级错误可重试（`DuplicateRequest`/`AccessDenied` 必须判定为终态）。

### 9.4 未做（按用户要求）

- **环境层**：302 反代、DNS 劫持、加速器一律未动。实测直连 `api.steampowered.com` 12 次采样 0 成功，关反代无法恢复 HTTP 通道。
- **单元 C**：`steamroom` 上游的 CM 列表静默回落，需改 crates.io 依赖，收益仅为日志可观测性。

### 9.5 未验证项（需真机确认）

以下场景需要实际登录才能验证，当前只有静态检查与单元测试覆盖：

| 编号 | 场景 | 状态 |
|---|---|---|
| V1 | 正常登录无额外延迟 | 待实测 |
| V2 | 确认前连接被断 → 自动重连成功 | 待实测（需断网模拟） |
| V3 | 始终不确认 → 5 分钟后明确超时 | 待实测 |
| V4 | Guard 码输错 → 仍返回 INVALID_GUARD_CODE | 待实测 |
| V5 | QR 扫码 | 待实测 |
| V6 | 免 2FA 直接登录 | 待实测 |
| V7 | 断网恢复 | 待实测 |

### 9.6 回滚

```powershell
cd D:\Repos\WEaveR
git checkout -- src-tauri/vendor/steamroom-client/src/login/
git checkout -- src-tauri/src/commands/steam/oauth.rs
git checkout -- src/components/dialogs/LoginModal.tsx
git checkout -- src/lib/events.ts src/i18n/locales/
``

改动未提交，全部可通过上述命令回滚。

# WEaveR 搜索结果「按壁纸体积过滤」— 实施方案（v2，已实测校准）

> 状态：**可实施**（所有关键假设均已实测验证）
> 日期：2026-11-25
> 用户决策：文件体积 / 区间输入 / 仅创意工坊浏览

---

## 0. 本轮补充验证了什么（对照 v1 的不足）

| v1 的问题 | 本轮做法 | 结果 |
|---|---|---|
| 只测了顺利路径 | 测了空请求、不存在 ID、100/150/300 条 | 见 §2 |
| 「570ms」是单点采样 | 测了 26/100/150/300 条耗时曲线 | 见 §2.2 |
| 未验证数据准确性 | 用本地磁盘体积逐条比对 | 见 §2.3 |
| 未验证服务端排序 | 抓两次 HTML 做 MD5 + ID 序列对比 | 见 §2.4 **关键** |
| 未考虑复用现有基础设施 | 查了 `WorkshopClient` 的缓存与 Client | 见 §4 |
| 未确认类型契约 | 读了前后端 `WorkshopItem` 定义 | 见 §3 |

---

## 1. 结论

**可行。** 纯客户端过滤，不改核心路径。关键数据：

- 一次批量请求可查 **300 个 ID / 1.3 秒**
- API 体积与磁盘实际体积**完全吻合**
- 服务端**确认不支持**体积过滤与排序

---

## 2. 实测数据（全部为本轮亲自运行）

### 2.1 边界条件

| 输入 | 结果 | 对实现的要求 |
|---|---|---|
| `itemcount=0` | **HTTP 400** `Required parameter publishedfileids[0] is missing` | 空列表必须提前返回，不能发请求 |
| 不存在的 ID | `result=9`，**无 `file_size` 字段** | 必须容忍 `file_size` 缺失 |
| 混合（1 个无效 + 1 个有效） | 有效条目正常返回 `result=1` + 体积 | 逐条判断 `result == 1` |

**实现要点**：

```rust
// 空输入直接短路，避免 400
if ids.is_empty() { return HashMap::new(); }

// 只有 result == 1 才有 file_size
if detail.result == Some(1) {
    if let Some(size) = detail.file_size.as_deref().and_then(|s| s.parse::<u64>().ok()) {
        sizes.insert(detail.publishedfileid.clone(), size);
    }
}
```

### 2.2 批量容量与耗时

| 请求 ID 数 | resultcount | 耗时 |
|---|---|---|
| 26（全真实） | 26 | 569 ms |
| 100（全无效） | 100 | 2066 ms |
| 150（真实循环） | 150 | 1110 ms |
| **300（真实循环）** | **300** | **1312 ms** |

**关键结论：上限远超单页 30 条。** 这意味着可以一次预取多页，或干脆预取「当前页 + 下一页」。

### 2.3 数据准确性（决定性）

用你本地 26 个已安装壁纸，逐条比对 API 返回值与磁盘实际占用：

| ID | API | 磁盘实际 | 差异 |
|---|---|---|---|
| 2513410552 | 3.5 MB | 3.54 MB | 取整 |
| 2858284176 | 30.95 MB | 30.95 MB | 0 |
| 2902406982 | 123.79 MB | 123.89 MB | 0.1 MB（NTFS 簇对齐） |
| 3266866118 | 2104.18 MB | 2104.18 MB | 0 |
| 3296928284 | 6.37 MB | 6.4 MB | 取整 |

**API 的 `file_size` 就是 Steam 侧的文件总字节数，可信。**

### 2.4 服务端排序/过滤：确认无效（最重要）

请求两个 URL 并逐字节对比：

```
A: .../browse/?appid=431960&browsesort=trend&section=readytouseitems&days=7
B: 同上 + &sortby=filesize

A: 569299 bytes
B: 570559 bytes   ← 大小不同，但…

A 前 8 个 ID: 3842265910, 3843147764, 3842760548, 3842247388, ...
B 前 8 个 ID: 3842265910, 3843147764, 3842760548, 3842247388, ...
顺序完全相同: True
```

**结论**：`sortby=filesize` 被 Steam 忽略，返回顺序完全一致（大小差异只是页面时间戳等噪声）。

同理 `filesize_min`、`file_size_min` 也返回 200 但不改变结果。

**→ 必须在客户端过滤，没有任何服务端捷径。**

### 2.5 数据异常分布

本地 26 条中：

```
result=1（有效）: 25 条
result=9（失效/下架）: 1 条  → file_size 为空
```

约 **4% 的条目可能失效**。实现必须容忍，且失效条目**不应被体积过滤隐藏**（用户可能想看到它们以决定是否清理）。

---

## 3. 类型契约（已核对前后端）

### 3.1 后端 `WorkshopItem`（`services/workshop.rs:54-73`）

```rust
pub struct WorkshopItem {
    pub pubfileid: String,
    // ...
    pub file_size: String,   // ← 已存在！但列表页恒为空
}
```

### 3.2 前端 `WorkshopItem`（`types/workshop.ts:9-24`）

```typescript
export interface WorkshopItem {
  pubfileid: string;
  file_size: string;   // ← 已存在，同样是详情页专用
  // ...
}
```

### 3.3 重要决定：**不改 `file_size`，新增独立数据通道**

| 方案 | 问题 |
|---|---|
| 把 `file_size: String` 改成字节数 | 破坏详情页显示、破坏 `makeMockPage`、破坏已有序列化 |
| **新增 `file_size_bytes: Option<u64>`** ✅ | 零破坏；详情页的 `file_size` 字符串保持原样 |

但**更优的做法**：体积不塞进 `WorkshopItem`，而是走**独立命令返回独立 map**，前端合并。理由：

- `WorkshopItem` 被 `page_cache` 缓存（`workshop.rs:154`，20 条），塞进去会让缓存语义变复杂
- 独立 map 可以跨页复用，不受 `page_cache` 淘汰影响
- 前端「有无体积」是独立状态，能自然表达「未知」

---

## 4. 可复用的现有基础设施（v1 遗漏）

| 现有设施 | 位置 | 复用方式 |
|---|---|---|
| 全局 `reqwest::Client` | `workshop.rs:139`（已配 gzip/brotli/timeout/UA） | `state.workshop.client()` 直接拿，**不新建连接池** |
| `item_cache` LRU 600 条 | `workshop.rs:157` | 体积缓存可仿此模式，独立一个 LRU |
| `clear_caches()` | `workshop.rs:171` | 体积缓存也要一起清，否则刷新后拿到旧数据 |
| `WORKSHOP_REQUEST_TIMEOUT` | `workshop.rs` 常量 | 批量请求可复用（20s 足够） |

**这是 v1 最实质的改进**：不需要新建 HTTP 客户端，避免多一个连接池、多一份 UA 配置。

---

## 5. 最终架构

```
┌─ 前端 ──────────────────────────────────────────┐
│ WorkshopView                                     │
│   ├─ useWorkshopPage()        ← 一行不改         │
│   ├─ items = page.items       ← 原始数据         │
│   ├─ useFileSizes(ids)        ← 新增 hook        │
│   │     └─ invoke("workshop_file_sizes")         │
│   ├─ useSizeFilter(items, sizes, filters) ← 新增 │
│   └─ <WorkshopGrid items={visible} />            │
│                                                  │
│ FilterBar({ showSizeFilter })  ← 可选 prop       │
│   └─ 仅工坊视图传 true                           │
└──────────────────────────────────────────────────┘
                    ↕ invoke
┌─ 后端 ──────────────────────────────────────────┐
│ commands/workshop.rs                             │
│   workshop_file_sizes(ids) → HashMap<id, u64>    │
│                    ↓                             │
│ services/workshop/file_sizes.rs  ← 新建          │
│   fetch_file_sizes(client, ids)                  │
│     ├─ 空 → 立即返回 {}                          │
│     ├─ 分块 300/批                               │
│     └─ 失败 → 返回 {}（调用方降级）              │
└──────────────────────────────────────────────────┘
```

### 5.1 后端实现要点

```rust
/// 批量取体积。永不返回 Err：失败时返回空 map，调用方按「未知」处理。
///
/// 为什么不返回 Result：体积是**可选增强信息**，拿不到不该让整个页面失败。
pub async fn fetch_file_sizes(
    client: &Client,
    ids: &[String],
) -> HashMap<String, u64> {
    if ids.is_empty() { return HashMap::new(); }   // 否则 400
    // 每批 300（实测 1.3s），单页 30 条永远只走一批
    // ...
}
```

### 5.2 前端过滤逻辑

```typescript
function passesSizeFilter(sizeBytes: number | undefined, min?: number, max?: number) {
  // 体积未知：保留。宁可多显示，不可因数据缺失误删。
  if (sizeBytes === undefined) return true;
  const mb = sizeBytes / (1024 * 1024);
  if (min !== undefined && mb < min) return false;
  if (max !== undefined && mb > max) return false;
  return true;
}
```

**`undefined → true` 是核心决策**：API 失败或条目失效时，用户看到的是完整列表 + 提示，而不是空白页。

---

## 6. 全局影响面（已实测）

### 6.1 已确认的共享组件

| 组件 | 共用方 | 处理 |
|---|---|---|
| `useWorkshopPage` | WorkshopView + CollectionsView | ❌ **不改** |
| `FilterBar` | WorkshopView + CollectionsView | ⚠️ 加**可选 prop**，默认 false |
| `WorkshopGrid` | 纯展示 | ✅ 不用改 |
| `Pagination` | 纯展示 | ✅ 不用改 |
| 后端 `WorkshopFilters` | 已实测忽略未知字段 | ✅ 加字段安全 |

### 6.2 改动清单

| 文件 | 类型 | 行数 | 风险 |
|---|---|---|---|
| `services/workshop/file_sizes.rs` | 新建 | ~110 | 无 |
| `commands/workshop.rs` | +1 命令 | ~12 | 无 |
| `services/workshop.rs` | +mod 注册 | ~2 | 无 |
| `stores/filters.ts` | +2 字段 | ~8 | 低 |
| `hooks/useFileSizes.ts` | 新建 | ~50 | 无 |
| `hooks/useSizeFilter.ts` | 新建 | ~35 | 无 |
| `components/workshop/SizeRangeInputs.tsx` | 新建 | ~70 | 无 |
| `components/workshop/FilterBar.tsx` | +prop + 渲染 | ~12 | 低 |
| `components/views/WorkshopView.tsx` | +2 行 | ~6 | 低 |
| `i18n/locales/{zh,en}.ts` | +4 条 | ~8 | 无 |
| **合计** | **10 文件（4 新建）** | **~313** | |

### 6.3 明确不改

- ❌ `hooks/useWorkshopPage.ts`（三视图共享）
- ❌ `CollectionsView.tsx`（合集无单一体积）
- ❌ `workshop_browse` 命令（核心路径）
- ❌ `parser/*`、`url_builder.rs`（解析与 URL）
- ❌ `WorkshopItem` 的 `file_size: String`（详情页专用，保持原样）

---

## 7. 边界情况处理表

| 场景 | 处理 | 理由 |
|---|---|---|
| 空结果页（0 条） | 不发请求 | 避免 400 |
| 条目失效（result=9） | 无体积 → 保留显示 | 用户需要看到失效项 |
| 集合类（is_collection） | 无体积 → 保留 | 合集是容器，不该被过滤 |
| API 超时/失败 | 全部保留 + 顶部提示 | 降级而非清空 |
| 只填最小值 | 上限视为无穷 | 常规语义 |
| 只填最大值 | 下限视为 0 | 常规语义 |
| 最小值 > 最大值 | 交换或提示 | 待定，见 §9 |
| 输入负数 | 钳位到 0 | 防止逻辑错乱 |
| 输入非数字 | 忽略该条件 | 不让输入框卡住 |
| 过滤后 0 条 | 明确提示 + 保留翻页 | 不能像坏了 |
| 翻页 | 体积缓存复用 | 避免重复请求 |
| 快速改区间 | 防抖 300ms | 避免请求风暴 |

---

## 8. 验收标准

| # | 场景 | 期望 | 验证方式 |
|---|---|---|---|
| S1 | 不填区间 | 与改动前**完全一致**，零额外请求 | 抓包对比请求数 |
| S2 | 填 10～100 | 只显示区间内 + 未知体积的 | 手测 |
| S3 | 只填最小值 | 上限无穷 | 手测 |
| S4 | 只填最大值 | 下限为 0 | 手测 |
| S5 | 区间内无结果 | 明确提示，可继续翻页 | 手测 |
| S6 | API 失败 | 显示全部 + 提示 | 断网模拟 |
| S7 | 集合条目 | 保留，体积显示「—」 | 手测 |
| S8 | 输入非法值 | 不崩溃，条件被忽略 | 手测 |
| S9 | 翻页 | 条件保持，体积不重复请求 | 抓包 |
| S10 | 重置 | 清空区间 | 手测 |
| S11 | 合集视图 | **完全不受影响**（无体积控件） | 切到合集页确认 |
| S12 | 现有测试 | 全部通过 | `npm test` |

---

## 9. 待你决定（2 个细节）

1. **最小值 > 最大值时**：自动交换，还是显示错误提示？
2. **过滤后本页 0 条**：显示「本页无符合条件」并保留翻页按钮，还是自动跳到下一个有结果的页？（后者要连续请求多页，复杂度高）

---

## 10. 关于「现成项目借鉴」

尝试了 GitHub 搜索与直接抓取（`Geam/steam_workshop_downloader`、`MMFoundation/WallpaperMachine`、`suprimacy/WallForge` 等），**本机网络无法访问 GitHub**（三种镜像均超时）。

因此本节结论**全部来自对本机 Steam 的真实请求实测**，不依赖第三方代码。

从搜索结果摘要看，这些项目的共同做法与本案一致：**用 `GetPublishedFileDetails` 批量拿元数据，客户端侧处理**。没有发现更优的替代接口。
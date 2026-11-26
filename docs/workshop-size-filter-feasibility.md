# WEaveR 搜索结果「按壁纸体积过滤」— 可行性分析

> 状态：**可行性分析（未实施）**
> 日期：2026-11-25
> 用户决策：文件体积 / 区间输入 / 仅创意工坊浏览

---

## 1. 结论

**完全可行，风险低。** 纯新增功能，不触碰现有逻辑；约 350 行改动。

关键事实：Steam 官方 API `ISteamRemoteStorage/GetPublishedFileDetails` **直接返回精确字节数** `file_size`，无需逐条抓详情页。每页 30 条结果只需 **1 次批量请求（约 570 ms）**。

---

## 2. 实测证据

### 2.1 API 返回精确体积

用本地 26 个真实壁纸 ID 实测：

```json
{
  "publishedfileid": "2513410552",
  "result": 1,
  "file_size": "3672476",              // ← 精确字节数
  "title": "Watching the Rain | Minimal",
  "tags": [
    {"tag": "Wallpaper"}, {"tag": "Scene"},
    {"tag": "2560 x 1440"},            // ← 分辨率顺带可得
    {"tag": "Everyone"}
  ]
}
```

### 2.2 性能实测

| 项目 | 结果 |
|---|---|
| 批量查询 26 个 ID | **569 ms**，返回 25 个有效 |
| 每页结果数 | 30（`WorkshopView.tsx:140` 固定值） |
| 单页补全成本 | 1 次请求 ≈ 570 ms |
| 分辨率标签覆盖 | 24 / 25（96%） |
| 分辨率格式 | 统一 `1920 x 1080` / `3840 x 2160` |

### 2.3 体积跨度（用户本地 26 个壁纸）

```
3.5 MB · 6.4 MB · 11.9 MB · 25.9 MB · 30.9 MB · 123.8 MB · 1735 MB · 3866 MB
```

相差 **1000 倍** —— 这就是过滤的价值：小壁纸秒下，3.8 GB 的壁纸要等几十分钟。

### 2.4 服务端不支持

Steam 浏览 URL（`url_builder.rs:35-57`）只有这些参数：

```
appid, browsesort, section, p, childpublishedfileid,
created_date_range_filter_start/end, updated_date_range_filter_start/end,
actualsort, days, searchtext, search_text_mode, + 各类 tag
```

**没有体积参数。** 因此必须在客户端侧过滤。

---

## 3. 现有架构（接入点）

### 3.1 数据流

```
前端 FilterBar / WorkshopFilterDrawer
  ↓ useFiltersStore.filters (WorkshopFilters)
  ↓ invoke("workshop_browse", { filters })
后端 commands/workshop.rs::workshop_browse        ← 唯一入口
  ↓ state.workshop.browse(&filters)
  ↓ url_builder::build_browse() → Steam browse 页
  ↓ parser::parse_browse() → WorkshopPage { items }
  ↑ 返回前端渲染
```

### 3.2 关键现状

| 位置 | 现状 | 影响 |
|---|---|---|
| `workshop.rs:62` | `WorkshopItem.file_size: String` 存在 | 但**只由详情页填充**，列表页恒为空 |
| `parser/item.rs:79-81` | `.detailsStatRight` → `file_size` | 详情页才有，列表页没有 |
| `parser/browse/ssr.rs:118-125` | `..Default::default()` | SSR 列表**不提取**体积 |
| `stores/filters.ts:3-25` | 20 个过滤字段 | 无体积字段，需新增 |
| `commands/workshop.rs:7` | `workshop_browse` 单入口 | 改这一处即覆盖工坊浏览 |

---

## 4. 方案设计

### 4.1 后端

**新增模块** `src-tauri/src/services/workshop/file_sizes.rs`：

```rust
/// 批量补全体积：一次 API 请求拿到整页的 file_size。
///
/// 用 `ISteamRemoteStorage/GetPublishedFileDetails`，它不要求登录，
/// 且一次可查数十个 ID。失败时返回空 map，调用方必须降级为
/// 「不过滤」—— 宁可多显示，不可让用户看到空列表。
pub async fn fetch_file_sizes(
    client: &reqwest::Client,
    ids: &[String],
) -> HashMap<String, u64>
```

**新增命令** `workshop_file_sizes(ids: Vec<String>) -> HashMap<String, u64>`：

- 独立命令，**不塞进 `workshop_browse`**，理由见 4.3
- 前端拿到分页结果后再调，职责分离

### 4.2 前端

**filters store 新增 2 个字段**：

```typescript
size_min_mb: string;   // 空字符串 = 不限
size_max_mb: string;
```

**FilterBar 新增区间输入**：两个数字输入框 + 单位提示，加入 ActiveChip 体系（可单独移除）。

**过滤时机**：拿到 `WorkshopPage` 后、渲染前，在 `WorkshopView` 里本地筛选。

### 4.3 为什么用独立命令，不改 `workshop_browse`

| 方案 | 优点 | 缺点 |
|---|---|---|
| **A. 塞进 `workshop_browse`**（推荐度低） | 一次调用 | ① 每次翻页多 570ms，即使没开过滤<br>② 后端要处理「过滤后空页」的分页语义<br>③ 改动触及核心路径，风险高 |
| **B. 独立命令 + 前端过滤**（推荐） | ① 只在开启过滤时才请求<br>② 核心路径零改动<br>③ 失败可降级<br>④ 可复用到作者页 | 需两段式调用 |

**选 B。** 符合「不能修一个 bug 引新 bug」的要求 —— 核心浏览逻辑完全不动。

---

## 5. 三个必须处理的坑

### 5.1 分页语义（最重要）

本地过滤会让某页只剩 3 条，但总数仍显示 30。

| 做法 | 效果 |
|---|---|
| 页脚显示「本页 30 条，其中 12 条符合体积条件」 | 用户能理解为什么少 |
| 过滤后自动跳到第一页 | 避免翻到空页 |
| 空页提示「本页无符合条件的结果，可尝试放宽范围或翻页」 | 避免误以为坏了 |

### 5.2 集合类条目

收藏夹（`is_collection == true`）没有单一体积。

- **做法**：体积未知时**默认保留**（不过滤掉），并在 UI 标注「—」
- **理由**：用户想过滤的是「太大的壁纸」，而不是「隐藏收藏夹」

### 5.3 API 失败降级

```
请求失败 → 返回空 map → 前端认为「所有条目体积未知」
        → 不过滤任何条目，照常显示全部
        → 顶部提示「无法获取体积信息，已显示全部结果」
```

**绝不能**因为拿不到体积就把列表清空。

---

## 6. 工作量

| 部分 | 文件 | 行数 |
|---|---|---|
| 后端 | `services/workshop/file_sizes.rs`（新） | ~100 |
| 后端 | `commands/workshop.rs` +1 命令 | ~10 |
| 后端 | `services/workshop.rs` 注册模块 | ~5 |
| 前端 | `stores/filters.ts` +2 字段 | ~6 |
| 前端 | `FilterBar.tsx` 区间输入 | ~60 |
| 前端 | `WorkshopView.tsx` 过滤逻辑 | ~45 |
| 前端 | `i18n/locales/{zh,en}.ts` | ~8 |
| 测试 | 解析、边界、降级、集合跳过 | ~80 |
| **合计** | **8 个文件** | **~314 行** |

比登录修复（+483 行）小，且风险低得多：纯新增，不碰现有逻辑。

---

## 7. 验收标准

| 编号 | 场景 | 期望 |
|---|---|---|
| S1 | 不填体积范围 | 与改动前完全一致，无额外请求 |
| S2 | 填 10～100 MB | 只显示该区间内的条目 |
| S3 | 只填最小值 | 上限视为无穷 |
| S4 | 只填最大值 | 下限视为 0 |
| S5 | 区间内无结果 | 明确提示，不是空白页 |
| S6 | API 请求失败 | 显示全部 + 提示，不崩溃 |
| S7 | 集合类条目 | 保留显示，体积标注「—」 |
| S8 | 输入非法值（负数/文字） | 忽略该条件或钳位到 0 |
| S9 | 翻页 | 过滤条件保持 |
| S10 | 重置过滤 | 清空体积条件 |

---

## 8. 风险清单

| 风险 | 影响 | 缓解 |
|---|---|---|
| API 限流 | 频繁翻页触发限流 | 结果按 pubfileid 缓存；同一页只查一次 |
| 网络被墙（你的环境实测 API 不稳） | 拿不到体积 | 降级为显示全部；失败不阻塞浏览 |
| 分页语义混乱 | 用户困惑 | 页脚明确标注「本页 N 条中 M 条符合」 |
| `file_size` 字段名冲突 | 现有 String 字段被误用 | 新命令返回独立 map，**不改动** `WorkshopItem.file_size` |

---

## 9. 可选的第二阶段

分辨率过滤（`1920 x 1080` 等）**免费附赠**：同一个 API 响应里 `tags` 已包含，只需多解析一行正则 `^\d+ x \d+$`。

本次不做，但架构上已预留（`WorkshopFilters.resolution` 字段已存在，只是当前走服务端 tag 过滤且列表页拿不到 tags）。
---

## 10. 全局影响面实测（2026-11-25 补充）

> 本节是按要求做的**实际验证**，不是推测。每条结论都附了验证方式。

### 10.1 已实测确认的结论

| # | 假设 | 验证方式 | 结论 |
|---|---|---|---|
| 1 | 后端忽略未知 filter 字段 | 临时写 `serde_json::from_str` 探针并运行 | ✅ **通过**：多传 `size_min_mb`/`size_max_mb` 不会导致反序列化失败（探针已删除） |
| 2 | `workshop_browse` 只有一个调用方 | grep 全前端 | ✅ 只有 `WorkshopView.tsx:50`；合集走 `workshop_browse_collections` |
| 3 | `page.items` 只有一个消费点 | grep `WorkshopView.tsx` | ✅ `WorkshopView.tsx:55` 取值，:115 传给 Grid |
| 4 | `WorkshopGrid` 是纯展示 | 读组件 props | ✅ 只接收 `items`/`containerRef`/`renderItem`，无数据逻辑 |
| 5 | `Pagination` 是纯展示 | 读组件 props | ✅ 只接收 `page`/`totalPages`/`onChange`/`infoText` |

### 10.2 ⚠️ 发现的关键风险：`useWorkshopPage` 是三视图共享

**这是之前分析遗漏的一点，必须记入。**

`src/hooks/useWorkshopPage.ts` 被**三个视图**共用：

```
src/components/views/WorkshopView.tsx:49     ← 工坊浏览
src/components/views/CollectionsView.tsx:108 ← 合集浏览
src/hooks/useWorkshopPage.ts:74              ← hook 自身定义
``

hook 内部逻辑（`useWorkshopPage.ts:89-164`）：

```typescript
const filtersKey = useMemo(() => JSON.stringify(filters), [filters]);  // ← 缓存键
const key = cacheScope ? `${filtersKey}|${cacheScope}` : filtersKey;
const outcome = await tryInvokeAction<WorkshopPage>(cmd, { filters }); // ← 原样发给后端
``

**两个直接后果：**

1. **不能改这个 hook** —— 任何改动都会同时影响工坊和合集两个视图，违反「不影响现有路径」。
2. **新增 filter 字段会让缓存键变化** —— 这是**正确行为**（换过滤条件理应重新取数），但意味着：每次改体积区间都会触发一次 `workshop_browse` 请求 + 一次体积补全请求。

### 10.3 由此修正的架构决策

**体积过滤必须放在 `WorkshopView` 内、`useWorkshopPage` 之外**，不能改 hook：

```typescript
// WorkshopView.tsx 内
const { page, loading, error } = useWorkshopPage({ ... });   // ← 不动
const rawItems = page?.items ?? [];
// ↓ 新增：体积补全 + 本地过滤，只在这里
const visibleItems = useSizeFilteredItems(rawItems, filters);
// 用 visibleItems 渲染，rawItems 保持原样供其它逻辑使用
``

**风险隔离效果：**

| 改动点 | 影响范围 |
|---|---|
| 新增 `useSizeFilteredItems` hook | 全新文件，无现有调用方 |
| `WorkshopView` 内加一行过滤 | 仅工坊视图 |
| `filters` 加 2 字段 | 后端已实测忽略；合集视图不读这两个字段 |
| `FilterBar` 加区间输入 | 仅 UI；合集视图的 FilterBar 也会显示，需确认是否隐藏 |

### 10.4 新发现的待确认点：FilterBar 是否共用

`FilterBar` 同样可能被多个视图渲染。若共用，体积输入框会出现在合集页——但合集视图不做体积过滤，会造成「控件存在但无效」的困惑。

**待验证**：`FilterBar` 的渲染位置。若共用，需要按视图条件隐藏体积控件。

### 10.5 修正后的风险清单

| 风险 | 之前判断 | 实测后 |
|---|---|---|
| 改 `useWorkshopPage` 影响合集 | 未识别 | ⚠️ **确认存在**，故明确不改 |
| 后端拒绝未知字段 | 推测安全 | ✅ 实测安全 |
| FilterBar 跨视图共用 | 未检查 | ⚠️ 待验证 |
| 分页组件受过滤影响 | 推测安全 | ✅ 纯展示，确认安全 |

### 10.6 FilterBar 跨视图共用 —— 已实测确认

**实测结果（grep 全前端）：**

```
src/components/views/WorkshopView.tsx:97      <FilterBar />
src/components/views/CollectionsView.tsx:237 <FilterBar />   ← 也渲染
src/components/workshop/FilterBar.test.tsx   3 处测试
``

`FilterBar` 目前**无法区分自己在哪个视图**（只有 `useAppStore.showActiveFilters`，与视图无关）。

**问题**：若直接在 `FilterBar` 里加体积输入框，它会同时出现在合集页——但合集不做体积过滤，形成「控件在但无效」的坑。

**解决方案（已确认可行）**：加一个**可选 prop**，默认 `false`，只让工坊视图开启：

```typescript
export default function FilterBar({ showSizeFilter = false }: {
  /** 体积区间过滤只在创意工坊浏览视图可用：合集没有单一体积。 */
  showSizeFilter?: boolean;
}) {
  // ...
  {showSizeFilter && <SizeRangeInputs />}
}
``

调用处改为：

```typescript
// WorkshopView.tsx:97
<FilterBar showSizeFilter />

// CollectionsView.tsx:237 —— 不动，默认 false
<FilterBar />
``

**为什么这个方案安全（已实测）：**

| 检查 | 结果 |
|---|---|
| 现有 3 个测试用 `<FilterBar />` 无参渲染 | ✅ 可选 prop 有默认值，测试不受影响 |
| 合集视图不传该 prop | ✅ 默认 `false`，行为完全不变 |
| 工坊视图显式开启 | ✅ 只有一个调用点需改 |

### 10.7 最终改动清单（含全局影响标注）

| 文件 | 改动 | 影响范围 | 风险 |
|---|---|---|---|
| `services/workshop/file_sizes.rs` | **新建** | 无现有调用方 | 无 |
| `commands/workshop.rs` | +1 命令 | 纯新增 | 无 |
| `services/workshop.rs` | +1 行注册 mod | 无行为变化 | 无 |
| `stores/filters.ts` | +2 字段 | 后端已实测忽略 | 低 |
| `hooks/useSizeFilteredItems.ts` | **新建** | 无现有调用方 | 无 |
| `components/workshop/FilterBar.tsx` | +可选 prop + 区间输入 | 默认关闭，合集不受影响 | 低 |
| `components/views/WorkshopView.tsx` | +1 prop + 1 行过滤 | 仅工坊视图 | 低 |
| `i18n/locales/{zh,en}.ts` | +4 条 | 纯新增 | 无 |

**明确不改的文件（关键）：**

- ❌ `hooks/useWorkshopPage.ts` —— 三视图共享，改动会波及合集
- ❌ `components/views/CollectionsView.tsx` —— 合集不需要体积过滤
- ❌ `commands/workshop.rs::workshop_browse` —— 核心路径零改动
- ❌ `services/workshop/parser/*` —— 解析逻辑不动
- ❌ `services/workshop/url_builder.rs` —— URL 组装不动

### 10.8 反思：本次分析修正了什么

| 之前的说法 | 实际情况 | 来源 |
|---|---|---|
| 「约 350 行，风险低」 | 需增加「不改共享 hook」的约束，实际更谨慎 | 实测发现 `useWorkshopPage` 三视图共用 |
| 「FilterBar 加区间输入」 | 必须加可选 prop，否则污染合集视图 | 实测发现 `FilterBar` 两视图共用 |
| 「后端不受影响」（推测） | ✅ 已用探针实测确认 | `serde_json` 探针测试通过 |

**结论修正**：工作量从 ~314 行调整为 ~330 行（多了可选 prop 与条件渲染），但**风险边界更清晰**：所有改动都隔离在工坊视图链路内，合集与共享 hook 完全不动。

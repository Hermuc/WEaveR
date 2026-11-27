//! Steam Workshop scraping. The original Python app uses a logged-in
//! QWebEngineView to evaluate JS and read DOM; in the Rust port we hit the
//! very same HTML endpoints via `reqwest` + `scraper` and pull the exact
//! same fields out of the resulting markup. Login (cookies) is optional
//! and persisted on disk.

pub mod debug;
pub mod file_sizes;
pub mod parser;
pub mod size_index;
pub mod url_builder;
pub mod webview;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use regex::Regex;
use reqwest::cookie::Jar;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::constants::APP_VERSION;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkshopFilters {
    pub search: String,
    pub sort: String,
    pub days: String,
    pub category: String,
    pub type_tag: String,
    pub age_rating: String,
    pub resolution: String,
    pub misc_tags: Vec<String>,
    pub genre_tags: Vec<String>,
    pub excluded_misc_tags: Vec<String>,
    pub excluded_genre_tags: Vec<String>,
    pub asset_type: String,
    pub asset_genre: String,
    pub script_type: String,
    pub required_flags: Vec<String>,
    #[serde(default)]
    pub created_date_range_start: String,
    #[serde(default)]
    pub created_date_range_end: String,
    #[serde(default)]
    pub updated_date_range_start: String,
    #[serde(default)]
    pub updated_date_range_end: String,
    #[serde(default)]
    pub search_text_mode: u32,
    pub page: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkshopItem {
    pub pubfileid: String,
    pub title: String,
    pub preview_url: String,
    pub author: String,
    pub author_url: String,
    pub description: String,
    pub file_size: String,
    pub posted_date: String,
    pub updated_date: String,
    pub tags: serde_json::Value,
    pub rating_star_file: String,
    pub num_ratings: String,
    pub is_collection: bool,
    /// Collections this item appears in (parsed from `.parentCollection` on
    /// the details page). Empty if not yet fetched.
    #[serde(default)]
    pub collections: Vec<CollectionRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CollectionRef {
    pub id: String,
    pub title: String,
    pub item_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkshopPage {
    pub items: Vec<WorkshopItem>,
    pub current_page: u32,
    pub total_pages: u32,
    pub total_items: u64,
    /// Present only when a file-size filter produced this page. `None` on
    /// every unfiltered path, so existing callers see an unchanged shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_filter: Option<WorkshopSizeMeta>,
}

/// What the UI needs to explain a short page produced by a size filter.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkshopSizeMeta {
    /// Matches discovered so far across the scanned physical pages.
    pub discovered: u64,
    /// True when the scan stopped at the page budget with fewer results than
    /// a full page, so the UI must say "these are all we found".
    pub truncated: bool,
}

/// Snapshot of the Steam account that the Workshop client is scraping as.
/// Populated from `https://steamcommunity.com/my/home/` — mirrors whatever
/// Steam itself reports, so it matches the cookies the parser uses
/// regardless of which download account is selected in Settings.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SteamAccount {
    pub persona_name: String,
    pub account_name: String,
    pub steamid: String,
    pub profile_url: String,
    pub avatar_url: String,
}

/// What a session probe actually found.
///
/// `Unknown` exists because a failed HTTP call is not evidence that the user is
/// signed out. Startup treats `Unknown` as "keep the last known state" instead
/// of raising a false "login failed" error.
#[derive(Debug, Clone)]
pub enum AccountProbe {
    SignedIn(SteamAccount),
    SignedOut,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CollectionContents {
    pub collection_id: String,
    pub title: String,
    pub description: String,
    pub preview_url: String,
    pub author: String,
    pub author_url: String,
    pub items: Vec<WorkshopItem>,
    pub related_collections: Vec<WorkshopItem>,
    pub info: serde_json::Value,
}

pub struct WorkshopClient {
    client: Client,
    cookie_jar: Arc<Jar>,
    page_cache: Arc<Mutex<lru::LruCache<String, WorkshopPage>>>,
    item_cache: Arc<Mutex<lru::LruCache<String, WorkshopItem>>>,
    /// Size-filtered scan state, keyed by "filter + sort + search" so two
    /// different filters never share discovered ids.
    size_indexes: Arc<Mutex<lru::LruCache<String, size_index::SizeIndex>>>,
}

impl WorkshopClient {
    pub fn new(_cookies_dir: PathBuf) -> Self {
        let cookie_jar = Arc::new(Jar::default());

        let client = Client::builder()
            .cookie_provider(cookie_jar.clone())
            .gzip(true)
            .brotli(true)
            .timeout(WORKSHOP_REQUEST_TIMEOUT)
            .user_agent(format!(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36 WEave/{}",
                APP_VERSION
            ))
            .build()
            .expect("reqwest client");

        Self {
            client,
            cookie_jar,
            page_cache: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(20).expect("cache size > 0"),
            ))),
            item_cache: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(600).expect("cache ttl seconds > 0"),
            ))),
            size_indexes: Arc::new(Mutex::new(lru::LruCache::new(
                // A handful of concurrent filters is plenty; each entry is
                // just a list of ids plus a small page set.
                std::num::NonZeroUsize::new(8).expect("cache size > 0"),
            ))),
        }
    }

    pub fn client(&self) -> Client {
        self.client.clone()
    }

    pub fn cookie_jar(&self) -> Arc<Jar> {
        self.cookie_jar.clone()
    }

    pub fn clear_caches(&self) {
        self.page_cache.lock().clear();
        self.item_cache.lock().clear();
        // The size index caches ids discovered from those pages; stale pages
        // must not keep serving stale ids.
        self.size_indexes.lock().clear();
    }

    /// Ask Steam itself who we're logged in as, by following the redirect on
    /// `https://steamcommunity.com/my/` and pulling `g_rgProfileData` /
    /// `g_steamID` out of the returned profile HTML. Returns `None` when the
    /// session cookies don't resolve to a logged-in account.
    ///
    /// This powers the "parser login check" the user asked for — it's the
    /// single source of truth for which Steam account the Workshop scraper
    /// is currently acting as, independent of whatever's selected in the
    /// Settings → Download account UI.
    /// Download a Steam avatar and return it as a `data:image/…;base64,…` URL.
    ///
    /// Steam rotates its avatar CDNs, so a CSP host whitelist is fragile: the
    /// next rotation shows a broken image again. Inlining the bytes removes the
    /// network dependency from the renderer entirely.
    ///
    /// Returns `None` on any failure — the caller then keeps the plain URL and
    /// the CSP whitelist gets a chance to serve it.
    async fn fetch_avatar_data_url(&self, url: &str) -> Option<String> {
        if !url.starts_with("https://") {
            return None;
        }
        let resp = self.client.get(url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("image/jpeg")
            .to_string();
        // Guard against a surprise HTML error page being inlined as an image.
        if !content_type.starts_with("image/") {
            return None;
        }
        let bytes = resp.bytes().await.ok()?;
        // Avatars are ~5-40 KB; refuse anything absurd to avoid bloating state.
        if bytes.is_empty() || bytes.len() > 512 * 1024 {
            return None;
        }
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        Some(format!(
            "data:{};base64,{}",
            content_type,
            STANDARD.encode(&bytes)
        ))
    }

    /// Result of a session probe.
    ///
    /// The distinction matters at startup: conflating "the network call failed"
    /// with "nobody is signed in" is what produced a red "login failed" badge
    /// over an account that was in fact still signed in. Steam's profile page
    /// routinely takes 3-4s on this network, so a slow reply must not be read as
    /// an anonymous session.
    pub async fn probe_account(&self) -> AccountProbe {
        let resp = match self
            .client
            .get("https://steamcommunity.com/my/home/")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                log::warn!("[steam-session] probe request failed: {e}");
                return AccountProbe::Unknown;
            }
        };
        let account = self.current_account_from_response(resp).await;
        match account {
            Some(a) => AccountProbe::SignedIn(a),
            None => AccountProbe::SignedOut,
        }
    }

    pub async fn current_account(&self) -> Option<SteamAccount> {
        let resp = self
            .client
            .get("https://steamcommunity.com/my/home/")
            .send()
            .await
            .ok()?;
        self.current_account_from_response(resp).await
    }

    /// Parse a profile response into an account. `None` means genuinely
    /// anonymous (Steam redirected to the login page, or the page carried no
    /// user identity) — never "the call failed".
    async fn current_account_from_response(&self, resp: reqwest::Response) -> Option<SteamAccount> {
        // `/my` redirects to `/id/<vanity>` or `/profiles/<steamid>` when
        // logged in; anonymous sessions land on the login page.
        let final_url = resp.url().clone();
        let url_str = final_url.to_string();
        if url_str.contains("/login/") {
            return None;
        }
        let body = resp.text().await.ok()?;

        // Try `g_rgProfileData = { ... };` — this JSON is rendered into
        // every profile page and has both the vanity URL and the persona.
        let persona_name = Regex::new(r#""personaname"\s*:\s*"([^"]+)"""#)
            .ok()?
            .captures(&body)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .or_else(|| {
                // Fallback: on the /my/home page the display name appears in
                // a button in the header nav: <button ...>WE Workshop Manager</button>
                Regex::new(r#"<button[^>]*role="button"[^>]*>([^<]+)</button>"#)
                    .ok()
                    .and_then(|re| re.captures(&body))
                    .and_then(|c| c.get(1))
                    .map(|m| m.as_str().trim().to_string())
            });
        let steamid = Regex::new(r#""steamid"\s*:\s*"(\d+)""#)
            .ok()
            .and_then(|re| re.captures(&body))
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .or_else(|| {
                Regex::new(r#"g_steamID\s*=\s*"(\d+)""#)
                    .ok()
                    .and_then(|re| re.captures(&body))
                    .and_then(|c| c.get(1))
                    .map(|m| m.as_str().to_string())
            });
        // Fallback for the account login name: parse `AccountName` out of
        // the embedded JS, or from the header text "About the account: <span>name</span>".
        let account_name = Regex::new(r#"g_AccountName\s*=\s*"([^"]+)""#)
            .ok()
            .and_then(|re| re.captures(&body))
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .or_else(|| {
                Regex::new(r#">([^<]+)</span></a>"#)
                    .ok()
                    .and_then(|re| re.captures(&body))
                    .and_then(|c| c.get(1))
                    .map(|m| m.as_str().trim().to_string())
            });

        // Parse avatar: <img ... src="https://avatars...steamstatic.com/...">
        //
        // Steam serves avatars from a *rotating* set of CDNs
        // (avatars.fastly.steamstatic.com, avatars.st.dl.eccdnx.com,
        // avatars.akamai.steamstatic.com, …), so whitelisting hosts in the CSP
        // is a losing game — the next rotation silently breaks the image again.
        // Instead we inline the picture as a `data:` URL, which the CSP already
        // permits and which cannot break on a CDN change. If the download fails
        // we keep the original URL so the CSP whitelist can still serve it.
        let avatar_url = Regex::new(r#"<img[^>]+src="(https://avatars[^"]+)""#)
            .ok()
            .and_then(|re| re.captures(&body))
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        let avatar_url = match self.fetch_avatar_data_url(&avatar_url).await {
            Some(data_url) => data_url,
            None => avatar_url,
        };

        // If we couldn't even find a persona or a steamid, treat as
        // logged-out rather than returning an empty struct.
        if persona_name.is_none() && steamid.is_none() && account_name.is_none() {
            return None;
        }

        Some(SteamAccount {
            persona_name: persona_name.unwrap_or_default(),
            account_name: account_name.unwrap_or_default(),
            steamid: steamid.unwrap_or_default(),
            profile_url: url_str,
            avatar_url,
        })
    }

    pub async fn browse(&self, filters: &WorkshopFilters) -> anyhow::Result<WorkshopPage> {
        let url = url_builder::build_browse(filters);
        self.browse_url(&url, filters.page.max(1)).await
    }

    pub async fn browse_collections(
        &self,
        filters: &WorkshopFilters,
    ) -> anyhow::Result<WorkshopPage> {
        let url = url_builder::build_collections_browse(filters);
        self.browse_url(&url, filters.page.max(1)).await
    }

    pub async fn author_items(
        &self,
        profile_url: &str,
        filters: &WorkshopFilters,
    ) -> anyhow::Result<WorkshopPage> {
        let url = url_builder::build_author_items(profile_url, filters);
        self.browse_url(&url, filters.page.max(1)).await
    }

    pub async fn author_collections(
        &self,
        profile_url: &str,
        filters: &WorkshopFilters,
    ) -> anyhow::Result<WorkshopPage> {
        let url = url_builder::build_author_collections(profile_url, filters);
        self.browse_url(&url, filters.page.max(1)).await
    }

    /// Browse with a file-size filter, filling each logical page with matches.
    ///
    /// Steam gives 30 items per physical page and ignores size-related URL
    /// parameters, so a size filter cannot be applied to one page in isolation:
    /// we walk forward, keep what matches, and re-slice. The scan state is
    /// cached per filter so page 2 does not re-walk page 1.
    ///
    /// `filters.page` is the **logical** page. When no size bound is set this
    /// delegates straight to [`browse`], which is what keeps the unfiltered path
    /// byte-for-byte identical to before.
    pub async fn browse_sized(
        &self,
        filters: &WorkshopFilters,
        range: size_index::SizeRange,
        per_page: usize,
    ) -> anyhow::Result<WorkshopPage> {
        if !range.is_active() {
            return self.browse(filters).await;
        }

        let logical_page = filters.page.max(1);
        let per_page = per_page.max(1);
        let needed = logical_page as usize * per_page;

        // The scan key must capture everything that changes which ids Steam
        // would return. Rather than enumerate the filter fields (and silently
        // forget one — `age_rating` was missing, so switching from "Any" to
        // "Mature" reused the Any scan and showed its results), derive the key
        // from the actual browse URL with the page number stripped. That way
        // every tag, flag and date range is covered by construction.
        let key_filters = WorkshopFilters {
            page: 1,
            ..filters.clone()
        };
        let key = format!(
            "{}|{}|{}",
            url_builder::build_browse(&key_filters),
            range.min_bytes.map(|v| v.to_string()).unwrap_or_default(),
            range.max_bytes.map(|v| v.to_string()).unwrap_or_default(),
        );

        let mut index = {
            let mut cache = self.size_indexes.lock();
            cache.pop(&key).unwrap_or_default()
        };

        // Build the physical-page URL once; only `p` changes per page.
        let base_filters = WorkshopFilters {
            page: 1,
            ..filters.clone()
        };

        let result = index
            .fill(needed, |physical_page| {
                let mut page_filters = base_filters.clone();
                page_filters.page = physical_page;
                let url = url_builder::build_browse(&page_filters);
                async move {
                    let page = self
                        .browse_url(&url, physical_page)
                        .await
                        .map_err(|e| e.to_string())?;
                    // Warm the item cache with what this page already gave us.
                    // Without this, resolving a filtered page would fetch every
                    // detail page one by one — measured at ~840 ms each, so 30
                    // items would cost ~25 s.
                    {
                        let mut cache = self.item_cache.lock();
                        for item in &page.items {
                            cache.put(item.pubfileid.clone(), item.clone());
                        }
                    }

                    // An empty page means Steam ran out; a page whose items all
                    // fell outside the range is *not* the end and must be
                    // skipped past, or a sparse filter stops after one page.
                    let page_empty = page.items.is_empty();
                    let ids: Vec<String> = page.items.iter().map(|i| i.pubfileid.clone()).collect();
                    let sizes = file_sizes::fetch_file_sizes(&self.client, &ids).await;
                    // Keep only ids inside the range. Unknown sizes are kept by
                    // `accepts`, which is why they stay in this list.
                    let kept: Vec<String> = ids
                        .into_iter()
                        .filter(|id| range.accepts(sizes.get(id).copied()))
                        .collect();
                    Ok((kept, sizes, page_empty))
                }
            })
            .await;

        let page = index.page(logical_page, per_page);
        let discovered = index.matched_len();
        let truncated = index.truncated();

        // Put the state back even on error: a partially walked index is still
        // useful, and dropping it would make the next request start over.
        self.size_indexes.lock().put(key, index);

        // `fill` reports a plain String; this function speaks anyhow.
        result.map_err(anyhow::Error::msg)?;

        // Resolve the ids into full items. Prefer the page cache (already
        // parsed), then fall back to a detail fetch for anything missing.
        let mut items = Vec::with_capacity(page.ids.len());
        for id in &page.ids {
            if let Some(item) = self.cached_item(id) {
                items.push(item);
                continue;
            }
            if let Ok(item) = self.item_details(id).await {
                items.push(item);
            }
        }

        Ok(WorkshopPage {
            total_items: discovered as u64,
            total_pages: discovered.div_ceil(per_page).max(1) as u32,
            current_page: logical_page,
            items,
            size_filter: Some(WorkshopSizeMeta {
                discovered: discovered as u64,
                truncated,
            }),
        })
    }

    /// Cheap lookup in the parsed-page cache for one id.
    fn cached_item(&self, pubfileid: &str) -> Option<WorkshopItem> {
        // `item_cache` only holds detail pages; the browse cache holds pages.
        if let Some(item) = self.item_cache.lock().get(pubfileid) {
            return Some(item.clone());
        }
        None
    }

    async fn browse_url(&self, url: &str, page: u32) -> anyhow::Result<WorkshopPage> {
        if let Some(cached) = self.page_cache.lock().get(url) {
            return Ok(cached.clone());
        }
        let started = std::time::Instant::now();
        let resp = self.client.get(url).send().await?;
        let status = resp.status().as_u16();
        let html = resp.text().await?;
        let mut parsed = parser::parse_browse(&html, page);
        parsed.current_page = page.max(parsed.current_page);
        debug::record(
            "browse",
            url,
            status,
            started.elapsed().as_millis() as u64,
            &html,
            parsed.items.len() as u32,
        );
        self.page_cache.lock().put(url.to_string(), parsed.clone());
        Ok(parsed)
    }

    pub async fn item_details(&self, pubfileid: &str) -> anyhow::Result<WorkshopItem> {
        if let Some(cached) = self.item_cache.lock().get(pubfileid) {
            return Ok(cached.clone());
        }
        let url = format!("https://steamcommunity.com/sharedfiles/filedetails/?id={pubfileid}");
        let started = std::time::Instant::now();
        let resp = self.client.get(&url).send().await?;
        let status = resp.status().as_u16();

        // Check for rate limit error
        if status == 429 {
            return Err(anyhow::anyhow!(
                "Rate limit exceeded (429). Please try again later."
            ));
        }

        let html = resp.text().await?;
        let item = parser::parse_item_details(&html, pubfileid);
        debug::record(
            "item",
            &url,
            status,
            started.elapsed().as_millis() as u64,
            &html,
            1,
        );
        self.item_cache
            .lock()
            .put(pubfileid.to_string(), item.clone());
        Ok(item)
    }

    pub async fn collection_contents(
        &self,
        collection_id: &str,
    ) -> anyhow::Result<CollectionContents> {
        let url = url_builder::build_collection_url(collection_id);
        let started = std::time::Instant::now();
        let resp = self.client.get(&url).send().await?;
        let status = resp.status().as_u16();
        let html = resp.text().await?;
        let contents = parser::parse_collection_contents(&html, collection_id);
        debug::record(
            "collection",
            &url,
            status,
            started.elapsed().as_millis() as u64,
            &html,
            contents.items.len() as u32,
        );
        Ok(contents)
    }
}

// ── Timing constants ──────────────────────────────────────────
/// HTTP timeout for a workshop page request.
const WORKSHOP_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[cfg(test)]
mod size_key_tests {
    use crate::services::workshop::url_builder;
    use crate::workshop::WorkshopFilters;

    /// The scan cache key is derived from the browse URL with the page number
    /// pinned, so every filter that affects which items Steam returns is part of
    /// the key by construction.
    ///
    /// Regression: the key used to enumerate fields by hand and omitted
    /// `age_rating`, so switching from "Any" to "Mature" reused the previous
    /// scan and displayed its results — the Mature filter looked broken.
    fn key(filters: &WorkshopFilters) -> String {
        let pinned = WorkshopFilters {
            page: 1,
            ..filters.clone()
        };
        url_builder::build_browse(&pinned)
    }

    fn base() -> WorkshopFilters {
        WorkshopFilters {
            sort: "trend".into(),
            days: "7".into(),
            page: 1,
            ..Default::default()
        }
    }

    #[test]
    fn age_rating_changes_the_key() {
        let any = base();
        let mature = WorkshopFilters {
            age_rating: "Mature".into(),
            ..base()
        };
        assert_ne!(
            key(&any),
            key(&mature),
            "age rating must affect the scan key"
        );
    }

    #[test]
    fn other_tags_change_the_key() {
        let plain = base();
        for mutate in [
            (|f: &mut WorkshopFilters| f.category = "Scene".into()) as fn(&mut WorkshopFilters),
            |f: &mut WorkshopFilters| f.type_tag = "Video".into(),
            |f: &mut WorkshopFilters| f.resolution = "1920 x 1080".into(),
            |f: &mut WorkshopFilters| f.genre_tags.push("Anime".into()),
            |f: &mut WorkshopFilters| f.misc_tags.push("HDR".into()),
            |f: &mut WorkshopFilters| f.excluded_misc_tags.push("3D".into()),
            |f: &mut WorkshopFilters| f.search = "cat".into(),
            |f: &mut WorkshopFilters| f.days = "30".into(),
        ] {
            let mut changed = base();
            mutate(&mut changed);
            assert_ne!(
                key(&plain),
                key(&changed),
                "every filter must affect the key"
            );
        }
    }

    #[test]
    fn the_same_filters_produce_the_same_key() {
        // Otherwise every render would start a fresh scan.
        let filters = WorkshopFilters {
            age_rating: "Mature".into(),
            ..base()
        };
        assert_eq!(key(&filters), key(&filters));
    }

    #[test]
    fn the_page_number_does_not_change_the_key() {
        // Page is pinned to 1 inside the key: pages are scanned sequentially
        // into one index, so they must share a key.
        let first = base();
        let fifth = WorkshopFilters { page: 5, ..base() };
        assert_eq!(key(&first), key(&fifth));
    }
}

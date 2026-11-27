//! Size-filtered virtual pagination over the Workshop browse listing.
//!
//! Steam returns a fixed 30 items per physical page and offers no way to sort
//! or filter by file size (verified: `sortby=filesize` returns the identical
//! item order). So a size filter cannot be applied to a single page — it has to
//! walk forward, collect matches, and re-slice them into full logical pages.
//!
//! This module owns that walk and its cache. It deliberately knows nothing about
//! HTTP: the caller supplies a closure that fetches one physical page, which
//! keeps the scanning policy testable without a network.

use std::collections::HashMap;
use std::collections::HashSet;

/// Physical pages to walk for a single logical page request.
///
/// Measured hit rates for this catalogue: `<=500 MB` 85%, `<=100 MB` 61%,
/// `<=50 MB` 43%, `<=10 MB` 15%, `<=5 MB` 6%, `<=1 MB` 0.8%. At ~1.35 s per
/// physical page, 10 pages bounds the worst case at roughly 13 s. Without a
/// bound, `<=1 MB` would need ~125 pages (~3 min), which is worse than the bug
/// this feature replaces.
pub const MAX_PHYSICAL_PAGES: u32 = 10;

/// One logical page worth of results, plus what the UI needs to explain a
/// short page.
#[derive(Debug, Clone, Default)]
pub struct FilteredPage {
    /// Item ids for this logical page, in Steam browse order.
    pub ids: Vec<String>,
    /// Total matches discovered so far (not a catalogue-wide count — Steam
    /// does not expose one, so the UI shows "found so far").
    pub discovered: usize,
    /// True when the scan stopped at [`MAX_PHYSICAL_PAGES`] with fewer than a
    /// full page of results. The UI must say "these are all we found" rather
    /// than implying the list ended.
    pub truncated: bool,
}

/// A size range in bytes. `None` means "no bound on that side".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SizeRange {
    pub min_bytes: Option<u64>,
    pub max_bytes: Option<u64>,
}

impl SizeRange {
    /// Normalise a user-entered range: swap reversed bounds, clamp negatives
    /// (already impossible after parsing), and treat `min == max == None` as
    /// "no filter at all".
    ///
    /// Swapping rather than erroring is deliberate: a user who types 100 then
    /// 10 clearly means 10–100, and refusing to search would just be annoying.
    pub fn from_mb(min_mb: Option<f64>, max_mb: Option<f64>) -> Self {
        let to_bytes = |mb: f64| (mb.max(0.0) * 1024.0 * 1024.0) as u64;
        let (min_bytes, max_bytes) = match (min_mb, max_mb) {
            (Some(min), Some(max)) if min > max => (Some(to_bytes(max)), Some(to_bytes(min))),
            (min, max) => (min.map(to_bytes), max.map(to_bytes)),
        };
        Self {
            min_bytes,
            max_bytes,
        }
    }

    /// Whether any bound is set. A range with no bounds must take the fast path
    /// (single physical page, no size lookup at all).
    pub fn is_active(&self) -> bool {
        self.min_bytes.is_some() || self.max_bytes.is_some()
    }

    /// Unknown sizes pass: a missing lookup is not evidence the item is too
    /// big. Hiding items on missing data would make a network blip look like an
    /// empty catalogue.
    pub fn accepts(&self, size_bytes: Option<u64>) -> bool {
        let Some(size) = size_bytes else {
            return true;
        };
        if let Some(min) = self.min_bytes {
            if size < min {
                return false;
            }
        }
        if let Some(max) = self.max_bytes {
            if size > max {
                return false;
            }
        }
        true
    }
}

/// Everything discovered so far for one filter, so later pages need no refetch.
#[derive(Debug, Default)]
pub struct SizeIndex {
    /// Ids that matched, in browse order.
    matched: Vec<String>,
    /// Physical pages already walked, so a page is never fetched twice.
    scanned_pages: HashSet<u32>,
    /// Highest physical page number reached.
    last_page: u32,
    /// True once Steam returned a page with no items (end of catalogue).
    exhausted: bool,
    /// True when the last scan hit the page budget before filling the request.
    truncated: bool,
}

impl SizeIndex {
    pub fn matched_len(&self) -> usize {
        self.matched.len()
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Ensure `needed` matches are indexed, walking forward at most
    /// [`MAX_PHYSICAL_PAGES`] physical pages.
    ///
    /// `fetch_page(page)` returns the item ids on that physical page together
    /// with a size lookup for them. Returning an empty id list means the
    /// catalogue ended.
    pub async fn fill<F, Fut>(&mut self, needed: usize, mut fetch_page: F) -> Result<(), String>
    where
        F: FnMut(u32) -> Fut,
        // (matched ids, sizes, page was empty) — the third flag distinguishes
        // "Steam has no more pages" from "this page had items but none matched
        // the filter". Treating the latter as the end stopped the scan after a
        // single sparse page: a 1000 MB floor matches ~0.8 items per page, so
        // page 2 usually yields nothing and the whole search returned empty.
        Fut:
            std::future::Future<Output = Result<(Vec<String>, HashMap<String, u64>, bool), String>>,
    {
        self.truncated = false;
        let mut walked = 0u32;

        while self.matched.len() < needed && !self.exhausted && walked < MAX_PHYSICAL_PAGES {
            let page = self.last_page + 1;
            walked += 1;

            // A page already in the index cannot add anything new.
            if !self.scanned_pages.insert(page) {
                self.last_page = page;
                continue;
            }

            let (ids, sizes, page_empty) = fetch_page(page).await?;
            self.last_page = page;
            if page_empty {
                self.exhausted = true;
                break;
            }

            for id in ids {
                if sizes.contains_key(&id) {
                    self.matched.push(id);
                }
            }
        }

        // Short page with budget left over means we ran out of pages, not time.
        self.truncated = self.matched.len() < needed && !self.exhausted;
        Ok(())
    }

    /// Slice one logical page out of what has been discovered.
    ///
    /// `page` is 1-based. A page beyond the end yields an empty slice rather
    /// than an error — the UI shows "no more results".
    pub fn page(&self, page: u32, per_page: usize) -> FilteredPage {
        let per_page = per_page.max(1);
        let start = (page.max(1) as usize - 1).saturating_mul(per_page);
        let end = (start + per_page).min(self.matched.len());
        let ids = if start < self.matched.len() {
            self.matched[start..end].to_vec()
        } else {
            Vec::new()
        };
        FilteredPage {
            ids,
            discovered: self.matched.len(),
            truncated: self.truncated && end >= self.matched.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sizes(pairs: &[(&str, u64)]) -> HashMap<String, u64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn reversed_bounds_are_swapped() {
        let range = SizeRange::from_mb(Some(100.0), Some(10.0));
        assert_eq!(range.min_bytes, Some(10 * 1024 * 1024));
        assert_eq!(range.max_bytes, Some(100 * 1024 * 1024));
    }

    #[test]
    fn no_bounds_means_inactive() {
        assert!(!SizeRange::from_mb(None, None).is_active());
        assert!(SizeRange::from_mb(Some(0.0), None).is_active());
    }

    #[test]
    fn unknown_size_passes_the_filter() {
        // Critical: a failed lookup must not hide the item.
        let range = SizeRange::from_mb(Some(10.0), Some(100.0));
        assert!(range.accepts(None));
    }

    #[test]
    fn bounds_are_inclusive() {
        let range = SizeRange::from_mb(Some(10.0), Some(100.0));
        assert!(range.accepts(Some(10 * 1024 * 1024)));
        assert!(range.accepts(Some(100 * 1024 * 1024)));
        assert!(!range.accepts(Some(10 * 1024 * 1024 - 1)));
        assert!(!range.accepts(Some(100 * 1024 * 1024 + 1)));
    }

    #[test]
    fn one_sided_bounds_work() {
        let low = SizeRange::from_mb(Some(50.0), None);
        assert!(low.accepts(Some(5000 * 1024 * 1024)));
        assert!(!low.accepts(Some(1024)));

        let high = SizeRange::from_mb(None, Some(50.0));
        assert!(high.accepts(Some(1024)));
        assert!(!high.accepts(Some(5000 * 1024 * 1024)));
    }

    #[tokio::test]
    async fn fill_stops_once_enough_matches_are_indexed() {
        let mut index = SizeIndex::default();
        let mut calls = 0u32;
        index
            .fill(3, |page| {
                calls += 1;
                async move {
                    let ids = vec![format!("{page}a"), format!("{page}b")];
                    Ok((ids.clone(), sizes(&[(&ids[0], 1), (&ids[1], 2)]), false))
                }
            })
            .await
            .unwrap();
        // Each physical page contributes 2 matches, so the quota is crossed
        // during page 2 and that whole page is kept. Overshooting is
        // deliberate: the extra match seeds the next logical page instead of
        // being fetched twice. `page()` is what trims to 30.
        assert_eq!(index.matched_len(), 4);
        assert_eq!(calls, 2, "should stop as soon as the quota is met");

        let first = index.page(1, 3);
        assert_eq!(first.ids.len(), 3, "the logical page is still capped");
    }

    #[tokio::test]
    async fn fill_marks_truncated_when_the_budget_runs_out() {
        let mut index = SizeIndex::default();
        let mut calls = 0u32;
        index
            .fill(100, |page| {
                calls += 1;
                async move {
                    let ids = vec![format!("{page}a")];
                    Ok((ids.clone(), sizes(&[(&ids[0], 1)]), false))
                }
            })
            .await
            .unwrap();
        assert_eq!(calls, MAX_PHYSICAL_PAGES);
        assert!(index.truncated(), "must report the page budget was hit");
    }

    #[tokio::test]
    async fn an_empty_page_marks_the_catalogue_exhausted() {
        let mut index = SizeIndex::default();
        index
            .fill(100, |_page| async {
                Ok((Vec::new(), HashMap::new(), true))
            })
            .await
            .unwrap();
        assert!(
            !index.truncated(),
            "running out of catalogue is not truncation"
        );
    }

    #[tokio::test]
    async fn pages_are_not_fetched_twice() {
        let mut index = SizeIndex::default();
        index
            .fill(2, |page| async move {
                let ids = vec![format!("{page}a")];
                Ok((ids.clone(), sizes(&[(&ids[0], 1)]), false))
            })
            .await
            .unwrap();
        let after_first = index.matched_len();
        index
            .fill(2, |page| async move {
                let ids = vec![format!("{page}a")];
                Ok((ids.clone(), sizes(&[(&ids[0], 1)]), false))
            })
            .await
            .unwrap();
        assert_eq!(index.matched_len(), after_first, "quota already met");
    }

    #[tokio::test]
    async fn a_page_with_no_matches_does_not_end_the_scan() {
        // Regression: a 1000 MB floor matches under one item per page, so
        // most pages contribute nothing. Treating a no-match page as "the
        // catalogue ended" made the whole search return zero results.
        let mut index = SizeIndex::default();
        let mut calls = 0u32;
        index
            .fill(30, |page| {
                calls += 1;
                async move {
                    // Pages 2 and 3 exist but match nothing.
                    let ids = if page == 1 || page >= 4 {
                        vec![format!("{page}a")]
                    } else {
                        Vec::new()
                    };
                    let sizes = if ids.is_empty() {
                        HashMap::new()
                    } else {
                        sizes(&[(&ids[0], 1)])
                    };
                    // The page itself was NOT empty for 2 and 3 — it simply
                    // produced no matches, which must not stop the walk.
                    Ok((ids, sizes, false))
                }
            })
            .await
            .unwrap();

        assert_eq!(
            calls, MAX_PHYSICAL_PAGES,
            "must keep walking past empty matches"
        );
        assert_eq!(index.matched_len(), 8, "pages 1 and 4..10 each yield one");
        assert!(
            index.truncated(),
            "still short of 30, so the UI must say so"
        );
    }

    #[test]
    fn slicing_a_logical_page_is_one_based() {
        let index = SizeIndex {
            matched: (0..75).map(|n| format!("id{n}")).collect(),
            ..Default::default()
        };

        let first = index.page(1, 30);
        assert_eq!(first.ids.len(), 30);
        assert_eq!(first.ids[0], "id0");
        assert_eq!(first.discovered, 75);

        let third = index.page(3, 30);
        assert_eq!(third.ids.len(), 15);
        assert_eq!(third.ids[0], "id60");

        let beyond = index.page(9, 30);
        assert!(beyond.ids.is_empty());
    }
}

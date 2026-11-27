//! Collection-contents page parser.
//!
//! Split by section: header (title/description/preview), author, the `info`
//! dictionary (ratings, counts, dates, tag groups), the item tiles, and the
//! related-collections sidebar. `parse_collection_contents` only assembles them.

use scraper::Html;
use serde_json::Value;

use crate::workshop::{CollectionContents, CollectionRef, WorkshopItem};

use super::common::*;
use super::selectors;

pub fn parse_collection_contents(html: &str, collection_id: &str) -> CollectionContents {
    let doc = Html::parse_document(html);

    let mut contents = CollectionContents {
        collection_id: collection_id.to_string(),
        title: parse_title(&doc),
        description: parse_description(&doc),
        preview_url: parse_preview_url(&doc),
        ..Default::default()
    };
    let (author, author_url) = parse_author(&doc);
    contents.author = author;
    contents.author_url = author_url;
    contents.info = parse_info(&doc);

    // Items are collected first and seed `seen`, so the related sidebar never
    // repeats something already listed above it.
    let mut seen = std::collections::HashSet::new();
    contents.items = parse_items(&doc, collection_id, &mut seen);
    contents.related_collections = parse_related_collections(&doc, collection_id, &mut seen);
    contents
}

fn parse_title(doc: &Html) -> String {
    select_first(doc, ".workshopItemTitle")
        .map(text)
        .unwrap_or_default()
}

/// Full description, keeping newlines.
///
/// The original PyQt build kept the *full* description (truncating only for
/// display); `inner_text` strips whitespace and silently drops `<br>`-separated
/// lines, so a newline-preserving extractor is used instead.
fn parse_description(doc: &Html) -> String {
    select_first(doc, ".workshopItemDescription")
        .map(|el| inner_text_multiline(el))
        .unwrap_or_default()
}

/// Background image first (Steam uses it as the hero on collection pages), then
/// the standard og:image / preview slots.
fn parse_preview_url(doc: &Html) -> String {
    if let Some(bg) = select_first(doc, ".collectionBackgroundImage") {
        if let Some(src) = bg.value().attr("src") {
            return src.to_string();
        }
    }
    for sel in [
        "#previewImageMain",
        ".workshopItemPreviewImage",
        ".highlight_screenshot img",
        "meta[property=\"og:image\"]",
    ] {
        if let Some(el) = select_first(doc, sel) {
            let value = el
                .value()
                .attr("src")
                .or_else(|| el.value().attr("content"))
                .unwrap_or("");
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    String::new()
}

/// `(author, author_url)`.
///
/// Steam renders the collection author as
/// `<div class="friendBlockContent"> Author<br>Name </div>` with a sibling
/// `<a class="friendBlockLinkOverlay" href="...">`; older variants use
/// `.workshopItemAuthorName a`.
fn parse_author(doc: &Html) -> (String, String) {
    for sel in [".workshopItemAuthorName a", ".friendBlockContent a"] {
        if let Some(anchor) = select_first(doc, sel) {
            let url = anchor
                .value()
                .attr("href")
                .map(str::to_string)
                .unwrap_or_default();
            return (text(anchor), url);
        }
    }
    let url = select_first(doc, ".friendBlockLinkOverlay")
        .and_then(|a| a.value().attr("href"))
        .map(str::to_string)
        .unwrap_or_default();
    (String::new(), url)
}

/// The `info` dictionary: rating, counts, dates and tag groups.
fn parse_info(doc: &Html) -> Value {
    let mut info = serde_json::Map::new();

    if let Some(img) = select_first(doc, ".fileRatingDetails img") {
        if let Some(src) = img.value().attr("src") {
            let path = src.split('?').next().unwrap_or(src);
            let filename = path.rsplit('/').next().unwrap_or("");
            let stem = filename
                .trim_end_matches(".png")
                .trim_end_matches(".jpg")
                .trim_end_matches(".gif");
            if !stem.is_empty() {
                info.insert("rating_star_file".into(), Value::String(stem.to_string()));
            }
        }
    }

    if let Some(el) = select_first(doc, ".numRatings") {
        let digits: String = text(el).chars().filter(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            info.insert("num_ratings".into(), Value::String(digits));
        }
    }

    if let Some(el) = select_first(doc, ".rightSectionTopTitle") {
        if let Some(captures) = selectors::DIGITS.captures(&text(el)) {
            if let Ok(count) = captures[1].parse::<u64>() {
                info.insert(
                    "item_count".into(),
                    Value::Number(serde_json::Number::from(count)),
                );
            }
        }
    }

    insert_stat_rows(doc, &mut info);
    insert_tag_groups(doc, &mut info);
    Value::Object(info)
}

/// Map the stat label/value rows onto info keys.
fn insert_stat_rows(doc: &Html, info: &mut serde_json::Map<String, Value>) {
    let lefts: Vec<_> = doc.select(&selectors::STAT_LEFT).collect();
    let rights: Vec<_> = doc.select(&selectors::STAT_RIGHT).collect();
    for (left_el, right_el) in lefts.iter().zip(rights.iter()) {
        let left = text(*left_el);
        let right = text(*right_el);
        let right_lower = right.to_lowercase();
        let left_lower = left.to_lowercase();
        if right_lower.contains("unique") {
            info.insert("unique_visitors".into(), Value::String(left));
        } else if right_lower.contains("total") && right_lower.contains("favorit") {
            info.insert("total_favorited".into(), Value::String(left));
        } else if right_lower.contains("favorit") {
            info.insert("favorited".into(), Value::String(left));
        } else if right_lower.contains("subscriber") {
            info.insert("subscribers".into(), Value::String(left));
        } else if left_lower.contains("posted")
            || left_lower.contains("added")
            || left_lower.contains("created")
        {
            info.insert("posted_date".into(), Value::String(right));
        } else if left_lower.contains("updated") || left_lower.contains("changed") {
            info.insert("updated_date".into(), Value::String(right));
        }
    }
}

/// Collapse each `.workshopTags` block into `"Category": "Tag1, Tag2"`.
fn insert_tag_groups(doc: &Html, info: &mut serde_json::Map<String, Value>) {
    for block in doc.select(&selectors::TAG_BLOCK) {
        let Some(title_el) = block.select(&selectors::TAG_TITLE).next() else {
            continue;
        };
        let key = text(title_el)
            .trim_end_matches(|c: char| c == ':' || c.is_whitespace() || c == '\u{00a0}')
            .trim()
            .to_string();
        if key.is_empty() {
            continue;
        }
        let values: Vec<String> = block
            .select(&selectors::ANCHOR)
            .map(text)
            .filter(|v| !v.is_empty())
            .collect();
        if !values.is_empty() {
            info.insert(key, Value::String(values.join(", ")));
        }
    }
}

/// Item tiles, skipping the collection itself and anything already seen.
fn parse_items(
    doc: &Html,
    collection_id: &str,
    seen: &mut std::collections::HashSet<String>,
) -> Vec<WorkshopItem> {
    let mut items = Vec::new();
    for tile in doc.select(&selectors::COLLECTION_ITEM) {
        let Some(link) = tile.select(&selectors::BROWSE_LINK).next() else {
            continue;
        };
        let Some(id) = extract_pubfileid(link.value().attr("href").unwrap_or("")) else {
            continue;
        };
        if id == collection_id || !seen.insert(id.clone()) {
            continue;
        }

        let title = tile
            .select(&selectors::WORKSHOP_TITLE)
            .next()
            .map(text)
            .unwrap_or_default();
        let preview = tile
            .select(&selectors::COLLECTION_PREVIEW)
            .next()
            .and_then(|i| i.value().attr("src"))
            .or_else(|| {
                tile.select(&selectors::IMG)
                    .next()
                    .and_then(|i| i.value().attr("src"))
            })
            .unwrap_or("")
            .to_string();

        let (author, author_url) = match tile.select(&selectors::WORKSHOP_AUTHOR_ANCHOR).next() {
            Some(anchor) => {
                let url = anchor
                    .value()
                    .attr("href")
                    .map(str::to_string)
                    .unwrap_or_default();
                (text(anchor), url)
            }
            None => (String::new(), String::new()),
        };

        items.push(WorkshopItem {
            pubfileid: id,
            title,
            preview_url: preview,
            author,
            author_url,
            ..Default::default()
        });
    }
    items
}

/// Related-collections sidebar, skipping the collection itself and duplicates.
fn parse_related_collections(
    doc: &Html,
    collection_id: &str,
    seen: &mut std::collections::HashSet<String>,
) -> Vec<WorkshopItem> {
    let mut related = Vec::new();
    for entry in doc.select(&selectors::COLLECTION_RELATED) {
        let Some(link) = entry.select(&selectors::BROWSE_LINK).next() else {
            continue;
        };
        let Some(id) = extract_pubfileid(link.value().attr("href").unwrap_or("")) else {
            continue;
        };
        if id == collection_id || !seen.insert(id.clone()) {
            continue;
        }

        let title = entry
            .select(&selectors::WORKSHOP_TITLE)
            .next()
            .map(text)
            .unwrap_or_default();
        let preview = entry
            .select(&selectors::COLLECTION_PREVIEW)
            .next()
            .and_then(|i| i.value().attr("src"))
            .unwrap_or("")
            .to_string();

        let mut item_count: u32 = 0;
        if let Some(el) = entry.select(&selectors::COLLECTION_COUNT).next() {
            if let Some(captures) = selectors::DIGITS.captures(&text(el)) {
                if let Ok(count) = captures[1].parse::<u32>() {
                    item_count = count;
                }
            }
        }

        let collections = if item_count > 0 {
            vec![CollectionRef {
                id: id.clone(),
                title: String::new(),
                item_count,
            }]
        } else {
            Vec::new()
        };

        related.push(WorkshopItem {
            pubfileid: id,
            title,
            preview_url: preview,
            is_collection: true,
            collections,
            ..Default::default()
        });
    }
    related
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_title_and_preview() {
        let html = r#"<html><body>
            <div class="workshopItemTitle">My Collection</div>
            <img class="workshopItemPreviewImage" src="https://example.com/c.jpg">
        </body></html>"#;
        let c = parse_collection_contents(html, "999");
        assert_eq!(c.collection_id, "999");
        assert_eq!(c.title, "My Collection");
        assert_eq!(c.preview_url, "https://example.com/c.jpg");
    }

    #[test]
    fn prefers_the_collection_background_image() {
        let html = r#"<html><body>
            <img class="collectionBackgroundImage" src="https://example.com/bg.jpg">
            <img class="workshopItemPreviewImage" src="https://example.com/other.jpg">
        </body></html>"#;
        let c = parse_collection_contents(html, "1");
        assert_eq!(c.preview_url, "https://example.com/bg.jpg");
    }

    #[test]
    fn parses_the_author_link() {
        let html = r#"<html><body>
            <div class="workshopItemAuthorName"><a href="https://steamcommunity.com/id/me">Me</a></div>
        </body></html>"#;
        let c = parse_collection_contents(html, "2");
        assert_eq!(c.author, "Me");
        assert_eq!(c.author_url, "https://steamcommunity.com/id/me");
    }

    #[test]
    fn empty_html_yields_defaults() {
        let c = parse_collection_contents("<html></html>", "3");
        assert_eq!(c.collection_id, "3");
        assert!(c.title.is_empty());
        assert!(c.items.is_empty());
    }
}

//! Item-details page parser (`filedetails/?id=...`).
//!
//! The page is parsed field-family by field-family: each `parse_*` helper
//! owns one block of markup and returns only the values it is responsible for.
//! `parse_item_details` assembles them into a `WorkshopItem`. This keeps each
//! piece independently testable and stops the top-level function from growing
//! into a 200-line wall (it was one before this split).

use scraper::Html;
use serde_json::{Map, Value};

use crate::workshop::{CollectionRef, WorkshopItem};

use super::common::*;
use super::selectors;

pub fn parse_item_details(html: &str, pubfileid: &str) -> WorkshopItem {
    let doc = Html::parse_document(html);
    let (author, author_url) = parse_author(&doc);
    let (posted_date, updated_date) = parse_dates(&doc);
    let (rating_star_file, num_ratings) = parse_rating(&doc);

    WorkshopItem {
        pubfileid: pubfileid.to_string(),
        title: parse_title(&doc),
        description: parse_description(&doc),
        preview_url: parse_preview_url(&doc),
        author,
        author_url,
        file_size: parse_file_size(&doc),
        posted_date,
        updated_date,
        rating_star_file,
        num_ratings,
        tags: parse_tags(&doc),
        collections: parse_parent_collections(&doc),
        ..Default::default()
    }
}

/// `.workshopItemTitle`, falling back to the `<title>` tag with Steam's prefix
/// stripped.
fn parse_title(doc: &Html) -> String {
    if let Some(el) = select_first(doc, ".workshopItemTitle") {
        let title = text(el);
        if !title.is_empty() {
            return title;
        }
    }
    if let Some(el) = select_first(doc, "title") {
        return text(el).replace("Steam Workshop::", "").trim().to_string();
    }
    String::new()
}

/// `.workshopItemDescription`, truncated to 1000 characters (by char, not byte,
/// so multi-byte text is never cut mid-codepoint).
fn parse_description(doc: &Html) -> String {
    let Some(el) = select_first(doc, ".workshopItemDescription") else {
        return String::new();
    };
    let t = text(el);
    if t.len() <= 1000 {
        return t;
    }
    t.char_indices()
        .nth(1000)
        .map(|(idx, _)| t[..idx].to_string())
        .unwrap_or(t)
}

/// First non-empty preview image, trying the most specific selector first.
fn parse_preview_url(doc: &Html) -> String {
    for sel in [
        "#previewImageMain",
        ".workshopItemPreviewImageEnlarge img",
        ".workshopItemPreviewImage img",
        ".highlight_screenshot img",
        "meta[property=\"og:image\"]",
    ] {
        if let Some(el) = select_first(doc, sel) {
            let src = el
                .value()
                .attr("src")
                .or_else(|| el.value().attr("content"))
                .unwrap_or("");
            if !src.is_empty() {
                return src.to_string();
            }
        }
    }
    String::new()
}

/// `(author name, author profile URL)`. The name is the first non-empty direct
/// text child of `.friendBlockContent`; the URL comes from the linked form when
/// Steam renders one.
fn parse_author(doc: &Html) -> (String, String) {
    let author_url = select_first(
        doc,
        ".friendBlockContent a, .workshopItemAuthorName a, .creatorsBlock .friendBlock a",
    )
    .and_then(|el| el.value().attr("href"))
    .map(str::to_string)
    .unwrap_or_default();

    let author = select_first(doc, ".friendBlockContent")
        .and_then(|el| {
            el.children()
                .filter_map(|c| c.value().as_text().map(|t| t.trim().to_string()))
                .find(|s| !s.is_empty())
        })
        .unwrap_or_default();

    (author, author_url)
}

/// `.detailsStatRight` — the "File size" value as Steam prints it.
fn parse_file_size(doc: &Html) -> String {
    select_first(doc, ".detailsStatRight")
        .map(text)
        .unwrap_or_default()
}

/// `(posted, updated)` from the stat label/value columns.
fn parse_dates(doc: &Html) -> (String, String) {
    const STAT_KEYS: [&str; 4] = ["Posted", "Posted:", "Updated", "Updated:"];
    let labels: Vec<_> = doc.select(&selectors::STAT_LEFT).map(text).collect();
    let values: Vec<_> = doc.select(&selectors::STAT_RIGHT).map(text).collect();

    let mut posted = String::new();
    let mut updated = String::new();
    for (i, label) in labels.iter().enumerate() {
        let is_wanted = STAT_KEYS.iter().any(|k| {
            label
                .trim_end_matches(':')
                .eq_ignore_ascii_case(k.trim_end_matches(':'))
        });
        if !is_wanted {
            continue;
        }
        let value = values.get(i).cloned().unwrap_or_default();
        let lower = label.to_lowercase();
        if lower.contains("post") {
            posted = value;
        } else if lower.contains("updat") {
            updated = value;
        }
    }
    (posted, updated)
}

/// `(rating star image file, rating count)`.
fn parse_rating(doc: &Html) -> (String, String) {
    let star = doc
        .select(&selectors::RATING_IMG)
        .next()
        .and_then(|img| img.value().attr("src"))
        .map(str::to_string)
        .unwrap_or_default();

    let count = doc
        .select(&selectors::NUM_RATINGS)
        .next()
        .map(text)
        .map(|raw| {
            raw.chars()
                .filter(|c| c.is_ascii_digit() || *c == ',')
                .collect::<String>()
        })
        .unwrap_or_default();

    (star, count)
}

/// Workshop tags as a JSON array of `{tag, category?}` objects.
///
/// Steam renders `<div class="workshopTags">Category: <a>Tag1</a>, <a>Tag2</a>
/// </div>`. The category is the leading text node before the anchors; we skip
/// punctuation-only nodes so a stray "." or ", " never becomes a bogus header.
fn parse_tags(doc: &Html) -> Value {
    let mut tags = Vec::new();
    for block in doc.select(&selectors::TAG_BLOCK) {
        let category = block
            .children()
            .filter_map(|child| child.value().as_text())
            .map(|t| t.trim().trim_end_matches(':').trim())
            .find(|raw| !raw.is_empty() && raw.chars().any(|c| c.is_alphanumeric()))
            .unwrap_or("")
            .to_string();

        for anchor in block.select(&selectors::ANCHOR) {
            let value = text(anchor);
            if value.is_empty() {
                continue;
            }
            let mut map = Map::new();
            map.insert("tag".into(), Value::String(value));
            if !category.is_empty() {
                map.insert("category".into(), Value::String(category.clone()));
            }
            tags.push(Value::Object(map));
        }
    }
    Value::Array(tags)
}

/// Parent collections this item belongs to.
///
/// Steam emits `<div class="parentCollection" onClick="... id=12345 ...">` with
/// a title and a "N items" count inside.
fn parse_parent_collections(doc: &Html) -> Vec<CollectionRef> {
    let mut collections = Vec::new();
    for block in doc.select(&selectors::PARENT_COLLECTION) {
        let onclick = block
            .value()
            .attr("onClick")
            .or_else(|| block.value().attr("onclick"))
            .unwrap_or("");
        let Some(captures) = selectors::ID_EQ_DIGITS.captures(onclick) else {
            continue;
        };
        let id = captures[1].to_string();

        let title = block
            .select(&selectors::PARENT_COLLECTION_TITLE)
            .next()
            .map(text)
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "Collection".to_string());

        let item_count = block
            .select(&selectors::PARENT_COLLECTION_COUNT)
            .next()
            .map(text)
            .and_then(|s| selectors::DIGITS.captures(&s).map(|c| c[1].to_string()))
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);

        collections.push(CollectionRef {
            id,
            title,
            item_count,
        });
    }
    collections
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_title_and_description() {
        let html = r#"<html><body>
            <div class="workshopItemTitle">My Scene</div>
            <div class="workshopItemDescription">A nice wallpaper.</div>
        </body></html>"#;
        let item = parse_item_details(html, "42");
        assert_eq!(item.pubfileid, "42");
        assert_eq!(item.title, "My Scene");
        assert_eq!(item.description, "A nice wallpaper.");
    }

    #[test]
    fn falls_back_to_the_title_tag() {
        let html = r#"<html><head><title>Steam Workshop::Cool Thing</title></head>
            <body></body></html>"#;
        let item = parse_item_details(html, "7");
        assert_eq!(item.title, "Cool Thing");
    }

    #[test]
    fn truncates_an_overlong_description() {
        let long = "x".repeat(1500);
        let html = format!(
            r#"<html><body><div class="workshopItemTitle">T</div>
            <div class="workshopItemDescription">{long}</div></body></html>"#
        );
        let item = parse_item_details(&html, "1");
        assert_eq!(item.description.chars().count(), 1000);
    }

    #[test]
    fn picks_the_first_preview_image() {
        let html = r#"<html><body>
            <img id="previewImageMain" src="https://example.com/hero.jpg">
        </body></html>"#;
        let item = parse_item_details(html, "9");
        assert_eq!(item.preview_url, "https://example.com/hero.jpg");
    }

    #[test]
    fn missing_fields_default_cleanly() {
        let item = parse_item_details("<html><body></body></html>", "5");
        assert_eq!(item.pubfileid, "5");
        assert!(item.title.is_empty());
        assert!(item.preview_url.is_empty());
    }
}

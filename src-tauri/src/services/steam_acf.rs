//! Read-only access to Steam's per-app Workshop state file
//! (appworkshop_431960.acf).
//!
//! Two blocks matter for cleanup:
//!   * WorkshopItemsInstalled  - every item Steam has downloaded
//!   * WorkshopItemDetails     - subscription details; subscribedby is the
//!     Steam user who subscribed, and is absent once the user unsubscribes.
//!
//! An item that is installed but has no subscription is a leftover: Steam
//! removed the subscription bookkeeping but did not delete the folder.
//!
//! The file is Valve KeyValues: quoted strings, braces and // comments. We
//! only need two levels, so a small tokenizer is enough and keeps the crate
//! dependency-free.

use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct Detail {
    pub subscribed_by: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct WorkshopAcf {
    installed: HashSet<String>,
    details: HashMap<String, Detail>,
}

impl WorkshopAcf {
    /// Load the ACF belonging to a workshop content directory
    /// (.../steamapps/workshop/content/431960). Missing or unreadable files
    /// yield an empty map, which treats every item as "not residual" - the
    /// conservative choice.
    pub fn load_for_content_dir(content_dir: &Path) -> Self {
        let Some(steamapps) = content_dir
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
        else {
            return Self::default();
        };
        let path = steamapps.join("workshop").join("appworkshop_431960.acf");
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_text(&text),
            Err(_) => Self::default(),
        }
    }

    /// True when Steam downloaded the item but no longer has a subscription
    /// record for it.
    pub fn is_residual(&self, id: &str) -> bool {
        self.installed.contains(id) && !self.subscribed(id)
    }

    pub fn subscribed(&self, id: &str) -> bool {
        self.details
            .get(id)
            .and_then(|detail| detail.subscribed_by.as_deref())
            .map(|value| !value.trim().is_empty() && value != "0")
            .unwrap_or(false)
    }

    pub fn installed_ids(&self) -> &HashSet<String> {
        &self.installed
    }

    pub fn subscribed_ids(&self) -> Vec<&str> {
        self.details
            .iter()
            .filter(|(_, detail)| {
                detail
                    .subscribed_by
                    .as_deref()
                    .map(|value| !value.trim().is_empty() && value != "0")
                    .unwrap_or(false)
            })
            .map(|(id, _)| id.as_str())
            .collect()
    }

    pub fn from_text(text: &str) -> Self {
        let tokens = tokenize(text);
        let mut acf = WorkshopAcf::default();
        let mut index = 0;
        // Skip the outer key and opening brace; parse_block handles nesting.
        if matches!(tokens.get(index), Some(Token::Str(_))) {
            index += 1;
        }
        if matches!(tokens.get(index), Some(Token::Open)) {
            index += 1;
        }
        let mut path: Vec<String> = Vec::new();
        parse_block(&tokens, &mut index, &mut path, &mut acf);
        acf
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Str(String),
    Open,
    Close,
}

fn tokenize(text: &str) -> Vec<Token> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'{' => {
                out.push(Token::Open);
                i += 1;
            }
            b'}' => {
                out.push(Token::Close);
                i += 1;
            }
            b'"' => {
                i += 1;
                let mut value = String::new();
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' if i + 1 < bytes.len() => {
                            let next = bytes[i + 1];
                            if next == b'\\' || next == b'"' {
                                value.push(next as char);
                                i += 2;
                            } else {
                                value.push(bytes[i] as char);
                                i += 1;
                            }
                        }
                        b'"' => {
                            i += 1;
                            break;
                        }
                        other => {
                            value.push(other as char);
                            i += 1;
                        }
                    }
                }
                out.push(Token::Str(value));
            }
            _ => i += 1,
        }
    }
    out
}

fn parse_block(tokens: &[Token], index: &mut usize, path: &mut Vec<String>, acf: &mut WorkshopAcf) {
    while *index < tokens.len() {
        match &tokens[*index] {
            Token::Close => {
                *index += 1;
                return;
            }
            Token::Open => {
                *index += 1;
            }
            Token::Str(key) => {
                let key = key.clone();
                *index += 1;
                if matches!(tokens.get(*index), Some(Token::Open)) {
                    *index += 1;
                    let depth = path.len();
                    path.push(key);
                    // A direct child of WorkshopItemsInstalled / WorkshopItemDetails
                    // is an item id.
                    if path.len() == 2
                        && (path[0] == "WorkshopItemsInstalled" || path[0] == "WorkshopItemDetails")
                    {
                        let id = path[1].clone();
                        if path[0] == "WorkshopItemsInstalled" {
                            acf.installed.insert(id.clone());
                        }
                        acf.details.entry(id).or_default();
                    }
                    parse_block(tokens, index, path, acf);
                    path.truncate(depth);
                } else if let Some(Token::Str(value)) = tokens.get(*index) {
                    let value = value.clone();
                    *index += 1;
                    if path.len() == 2 && path[0] == "WorkshopItemDetails" && key == "subscribedby"
                    {
                        if let Some(detail) = acf.details.get_mut(&path[1]) {
                            detail.subscribed_by = Some(value);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
"AppWorkshop"
{
    "appid"        "431960"
    "WorkshopItemsInstalled"
    {
        "100"
        {
            "size"        "123"
            "timeupdated" "1"
        }
        "200"
        {
            "size"        "456"
            "timeupdated" "2"
        }
    }
    "WorkshopItemDetails"
    {
        "100"
        {
            "manifest"     "9"
            "subscribedby" "822413975"
        }
    }
}
"#;

    #[test]
    fn parses_installed_and_subscription_blocks() {
        let acf = WorkshopAcf::from_text(SAMPLE);
        assert_eq!(acf.installed_ids().len(), 2);
        assert!(acf.installed_ids().contains("100"));
        assert!(acf.installed_ids().contains("200"));
        assert!(!acf.is_residual("100"), "subscribed item is not residual");
        assert!(
            acf.is_residual("200"),
            "installed without subscribedby is residual"
        );
    }

    #[test]
    fn missing_subscribedby_is_treated_as_unsubscribed() {
        let text = r#"
"AppWorkshop"
{
    "WorkshopItemsInstalled"
    {
        "300" { "size" "1" }
    }
    "WorkshopItemDetails"
    {
        "300" { "manifest" "1" }
    }
}
"#;
        let acf = WorkshopAcf::from_text(text);
        assert!(acf.is_residual("300"));
    }

    #[test]
    fn zero_subscriber_is_treated_as_unsubscribed() {
        let text = r#"
"AppWorkshop"
{
    "WorkshopItemsInstalled" { "400" { "size" "1" } }
    "WorkshopItemDetails" { "400" { "subscribedby" "0" } }
}
"#;
        let acf = WorkshopAcf::from_text(text);
        assert!(acf.is_residual("400"));
    }

    #[test]
    fn empty_or_missing_acf_marks_nothing_residual() {
        let acf = WorkshopAcf::from_text("");
        assert!(!acf.is_residual("100"));
        assert_eq!(acf.installed_ids().len(), 0);
    }
}

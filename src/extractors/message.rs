//! Message intent extractor — simple keyword-based classifier (spec 6.10).
//!
//! Classifies user messages into intent categories and extracts
//! entities and date mentions. Uses simple string matching rather
//! than LLM-based classification for deterministic, injection-resistant
//! extraction.

use super::{ExtractedEntity, ExtractedMetadata, Extractor};

/// Message intent extractor using keyword matching (spec 6.10).
///
/// Classifies user messages into intent categories and extracts
/// entities and date mentions. Uses simple string matching rather
/// than LLM-based classification for deterministic, injection-resistant
/// extraction.
pub struct MessageIntentExtractor;

impl Extractor for MessageIntentExtractor {
    fn name(&self) -> &str {
        "extractor:message_intent"
    }

    fn extract(&self, text: &str) -> ExtractedMetadata {
        let lower = text.to_lowercase();

        let intent = detect_intent(&lower);
        let entities = extract_entities(text);
        let dates = extract_dates(text);

        ExtractedMetadata {
            intent,
            entities,
            dates_mentioned: dates,
            extra: serde_json::Value::Null,
        }
    }
}

/// Detect intent from lowercased message text (spec 6.10).
///
/// Priority order — first match wins:
/// 1. email_reply  2. email_send  3. email_check
/// 4. scheduling   5. github_check  6. web_browse
/// 7. admin_config  8. memory_save  9. None
fn detect_intent(lower: &str) -> Option<String> {
    // 1. "reply" + email-related keyword
    if lower.contains("reply") && has_email_keyword(lower) {
        return Some("email_reply".to_owned());
    }

    // 2. "send" + email-related keyword
    if lower.contains("send") && has_email_keyword(lower) {
        return Some("email_send".to_owned());
    }

    // 3. General email check
    if has_email_keyword(lower) {
        return Some("email_check".to_owned());
    }

    // 4. Scheduling / calendar
    if lower.contains("schedule")
        || lower.contains("meeting")
        || lower.contains("free busy")
        || lower.contains("freebusy")
        || lower.contains("calendar")
    {
        return Some("scheduling".to_owned());
    }

    // 5. GitHub
    if lower.contains("github") || lower.contains("pull request") || lower.contains("pr #") {
        return Some("github_check".to_owned());
    }

    // 6. Web browse
    if has_browse_keyword(lower) && has_web_keyword(lower) {
        return Some("web_browse".to_owned());
    }

    // 7. Admin / config
    if lower.contains("config")
        || lower.contains("setup")
        || lower.contains("integrate")
        || lower.contains("integration")
        || lower.contains("connect")
    {
        return Some("admin_config".to_owned());
    }

    // 8. Memory save (memory spec §4)
    if lower.contains("remember")
        || lower.contains("don't forget")
        || lower.contains("keep in mind")
        || lower.contains("note that")
        || lower.contains("save this")
    {
        return Some("memory_save".to_owned());
    }

    // 9. No specific intent detected
    None
}

/// Check if text contains an email-related keyword.
fn has_email_keyword(lower: &str) -> bool {
    lower.contains("email") || lower.contains("mail") || lower.contains("inbox")
}

/// Check if text contains a browse-related keyword.
fn has_browse_keyword(lower: &str) -> bool {
    lower.contains("browse") || lower.contains("visit") || lower.contains("open")
}

/// Check if text contains a web-related keyword.
fn has_web_keyword(lower: &str) -> bool {
    lower.contains("http")
        || lower.contains("url")
        || lower.contains("page")
        || lower.contains("site")
        || lower.contains("website")
}

/// Extract typed entities from message text (spec 6.10).
///
/// Looks for:
/// - Person names after "reply to" or "to " or "from "
/// - Email addresses (words containing "@")
/// - Message IDs (patterns like "msg_xxx" or "email_xxx")
fn extract_entities(text: &str) -> Vec<ExtractedEntity> {
    let mut entities = Vec::new();

    // Extract person names after "reply to", "to ", "from "
    let lower = text.to_lowercase();
    for trigger in &["reply to ", "to ", "from "] {
        if let Some(pos) = lower.find(trigger) {
            let after = &text[pos.saturating_add(trigger.len())..];
            if let Some(name) = extract_capitalized_word(after) {
                // Avoid duplicating the same person entity
                if !entities
                    .iter()
                    .any(|e: &ExtractedEntity| e.kind == "person" && e.value == name)
                {
                    entities.push(ExtractedEntity {
                        kind: "person".to_owned(),
                        value: name,
                    });
                }
            }
        }
    }

    // Extract email addresses (words containing "@")
    for word in text.split_whitespace() {
        let cleaned = word.trim_matches(|c: char| {
            !c.is_alphanumeric() && c != '@' && c != '.' && c != '_' && c != '-' && c != '+'
        });
        if cleaned.contains('@') && cleaned.len() > 2 {
            entities.push(ExtractedEntity {
                kind: "email_address".to_owned(),
                value: cleaned.to_owned(),
            });
        }
    }

    // Extract message IDs (msg_xxx or email_xxx)
    for word in text.split_whitespace() {
        let cleaned = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
        if (cleaned.starts_with("msg_") || cleaned.starts_with("email_"))
            && cleaned.len() > 4
            && cleaned[4..]
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_')
        {
            entities.push(ExtractedEntity {
                kind: "message_id".to_owned(),
                value: cleaned.to_owned(),
            });
        }
    }

    entities
}

/// Extract the first capitalized word from a string.
///
/// Returns `None` if no capitalized word is found.
fn extract_capitalized_word(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let word: String = trimmed.chars().take_while(|c| c.is_alphabetic()).collect();
    if word.is_empty() {
        return None;
    }
    // Check first char is uppercase
    let first = word.chars().next()?;
    if first.is_uppercase() {
        Some(word)
    } else {
        None
    }
}

/// Extract date/time references from message text (spec 6.10).
///
/// Looks for:
/// - "tomorrow", "today"
/// - "next monday", "next tuesday", etc.
/// - ISO date patterns (YYYY-MM-DD)
/// - "in X days/hours/minutes"
fn extract_dates(text: &str) -> Vec<String> {
    let mut dates = Vec::new();
    let lower = text.to_lowercase();

    // "tomorrow"
    if lower.contains("tomorrow") {
        dates.push("tomorrow".to_owned());
    }

    // "today"
    if lower.contains("today") {
        dates.push("today".to_owned());
    }

    // "next {weekday}"
    let weekdays = [
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
    ];
    for day in &weekdays {
        let pattern = format!("next {day}");
        if lower.contains(&pattern) {
            dates.push(pattern);
        }
    }

    // ISO date pattern YYYY-MM-DD
    // Simple scan: find 4 digits, dash, 2 digits, dash, 2 digits
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    if len >= 10 {
        let limit = len.saturating_sub(9);
        for i in 0..limit {
            if is_iso_date(&chars, i) {
                let date_str: String = chars[i..i.saturating_add(10)].iter().collect();
                dates.push(date_str);
            }
        }
    }

    // "in X days/hours/minutes"
    extract_relative_times(&lower, &mut dates);

    dates
}

/// Check if chars starting at `i` form a YYYY-MM-DD pattern.
fn is_iso_date(chars: &[char], i: usize) -> bool {
    // Need exactly 10 characters: YYYY-MM-DD
    let end = i.saturating_add(10);
    if end > chars.len() {
        return false;
    }
    // Positions 0-3: digits, 4: '-', 5-6: digits, 7: '-', 8-9: digits
    chars[i].is_ascii_digit()
        && chars[i.saturating_add(1)].is_ascii_digit()
        && chars[i.saturating_add(2)].is_ascii_digit()
        && chars[i.saturating_add(3)].is_ascii_digit()
        && chars[i.saturating_add(4)] == '-'
        && chars[i.saturating_add(5)].is_ascii_digit()
        && chars[i.saturating_add(6)].is_ascii_digit()
        && chars[i.saturating_add(7)] == '-'
        && chars[i.saturating_add(8)].is_ascii_digit()
        && chars[i.saturating_add(9)].is_ascii_digit()
}

/// Extract "in X days/hours/minutes" patterns from lowercased text.
fn extract_relative_times(lower: &str, dates: &mut Vec<String>) {
    let words: Vec<&str> = lower.split_whitespace().collect();
    let word_count = words.len();
    if word_count < 3 {
        return;
    }
    let limit = word_count.saturating_sub(2);
    for i in 0..limit {
        if words[i] == "in" {
            // Check if next word is a number
            if words[i.saturating_add(1)]
                .chars()
                .all(|c| c.is_ascii_digit())
                && !words[i.saturating_add(1)].is_empty()
            {
                let unit = words[i.saturating_add(2)];
                if unit.starts_with("day") || unit.starts_with("hour") || unit.starts_with("minute")
                {
                    dates.push(format!("in {} {}", words[i.saturating_add(1)], unit));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_email_check_intent() {
        let extractor = MessageIntentExtractor;
        let meta = extractor.extract("check my email");
        assert_eq!(meta.intent.as_deref(), Some("email_check"));
    }

    #[test]
    fn test_email_reply_intent() {
        let extractor = MessageIntentExtractor;
        let meta = extractor.extract("reply to Sarah's email");
        assert_eq!(meta.intent.as_deref(), Some("email_reply"));
        assert!(
            meta.entities
                .iter()
                .any(|e| e.kind == "person" && e.value == "Sarah"),
            "should extract person entity 'Sarah'"
        );
    }

    #[test]
    fn test_scheduling_intent() {
        let extractor = MessageIntentExtractor;
        let meta = extractor.extract("schedule a meeting for next Tuesday");
        assert_eq!(meta.intent.as_deref(), Some("scheduling"));
        assert!(
            meta.dates_mentioned.contains(&"next tuesday".to_owned()),
            "should extract 'next tuesday' date"
        );
    }

    #[test]
    fn test_github_intent() {
        let extractor = MessageIntentExtractor;
        let meta = extractor.extract("check my pull requests");
        assert_eq!(meta.intent.as_deref(), Some("github_check"));
    }

    #[test]
    fn test_admin_intent() {
        let extractor = MessageIntentExtractor;
        let meta = extractor.extract("let's setup Notion integration");
        assert_eq!(meta.intent.as_deref(), Some("admin_config"));
    }

    #[test]
    fn test_no_intent() {
        let extractor = MessageIntentExtractor;
        let meta = extractor.extract("hello");
        assert_eq!(meta.intent, None);
    }

    #[test]
    fn test_date_extraction_tomorrow() {
        let extractor = MessageIntentExtractor;
        let meta = extractor.extract("do this tomorrow");
        assert!(
            meta.dates_mentioned.contains(&"tomorrow".to_owned()),
            "should extract 'tomorrow'"
        );
    }

    #[test]
    fn test_date_extraction_iso() {
        let extractor = MessageIntentExtractor;
        let meta = extractor.extract("meeting on 2026-03-15");
        assert!(
            meta.dates_mentioned.contains(&"2026-03-15".to_owned()),
            "should extract ISO date '2026-03-15'"
        );
    }

    #[test]
    fn test_entity_extraction_email() {
        let extractor = MessageIntentExtractor;
        let meta = extractor.extract("from user@example.com about the project");
        assert!(
            meta.entities
                .iter()
                .any(|e| e.kind == "email_address" && e.value == "user@example.com"),
            "should extract email address entity"
        );
    }

    #[test]
    fn test_web_browse_intent() {
        let extractor = MessageIntentExtractor;
        let meta = extractor.extract("browse this website");
        assert_eq!(meta.intent.as_deref(), Some("web_browse"));
    }

    #[test]
    fn test_metadata_serialization() {
        let meta = ExtractedMetadata {
            intent: Some("email_check".to_owned()),
            entities: vec![ExtractedEntity {
                kind: "person".to_owned(),
                value: "Alice".to_owned(),
            }],
            dates_mentioned: vec!["tomorrow".to_owned()],
            extra: serde_json::Value::Null,
        };
        let json = serde_json::to_string(&meta).expect("should serialize");
        let deserialized: ExtractedMetadata =
            serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(deserialized.intent.as_deref(), Some("email_check"));
        assert_eq!(deserialized.entities.len(), 1);
        assert_eq!(deserialized.entities[0].value, "Alice");
        assert_eq!(deserialized.dates_mentioned.len(), 1);
    }

    #[test]
    fn test_email_send_intent() {
        let extractor = MessageIntentExtractor;
        let meta = extractor.extract("send an email to Bob");
        assert_eq!(meta.intent.as_deref(), Some("email_send"));
    }

    #[test]
    fn test_message_id_extraction() {
        let extractor = MessageIntentExtractor;
        let meta = extractor.extract("read msg_123abc please");
        assert!(
            meta.entities
                .iter()
                .any(|e| e.kind == "message_id" && e.value == "msg_123abc"),
            "should extract message_id entity"
        );
    }

    #[test]
    fn test_relative_time_extraction() {
        let extractor = MessageIntentExtractor;
        let meta = extractor.extract("do this in 3 days");
        assert!(
            meta.dates_mentioned.contains(&"in 3 days".to_owned()),
            "should extract 'in 3 days'"
        );
    }

    #[test]
    fn test_extractor_name() {
        let extractor = MessageIntentExtractor;
        assert_eq!(extractor.name(), "extractor:message_intent");
    }

    #[test]
    fn test_inbox_triggers_email_check() {
        let extractor = MessageIntentExtractor;
        let meta = extractor.extract("what's in my inbox");
        assert_eq!(meta.intent.as_deref(), Some("email_check"));
    }

    #[test]
    fn test_freebusy_triggers_scheduling() {
        let extractor = MessageIntentExtractor;
        let meta = extractor.extract("check freebusy for next week");
        assert_eq!(meta.intent.as_deref(), Some("scheduling"));
    }

    #[test]
    fn test_memory_save_intent_remember() {
        let extractor = MessageIntentExtractor;
        let meta = extractor.extract("remember that my flight to Bali is March 15th");
        assert_eq!(meta.intent.as_deref(), Some("memory_save"));
    }

    #[test]
    fn test_memory_save_intent_dont_forget() {
        let extractor = MessageIntentExtractor;
        let meta = extractor.extract("don't forget my passport expires in June");
        assert_eq!(meta.intent.as_deref(), Some("memory_save"));
    }

    #[test]
    fn test_memory_save_intent_note_that() {
        let extractor = MessageIntentExtractor;
        let meta = extractor.extract("note that the API key needs rotating next month");
        assert_eq!(meta.intent.as_deref(), Some("memory_save"));
    }
}

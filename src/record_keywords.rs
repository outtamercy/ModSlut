// MS-owned port of the useful *input* side of SkyGen's keyword rules.
//
// SkyGen writes SkyPatcher keywords. ModSlut reads record KWDA form IDs and
// turns them into portable sorting concepts. Same proven Skyrim facts; no
// shared file and no chance of one tool's config breaking the other.

use std::{collections::HashMap, path::PathBuf};

#[derive(Clone, Debug)]
pub struct KeywordRule {
    pub concept: String,
    pub confidence: u8,
}

#[derive(Clone, Debug)]
pub struct RecordKeywords {
    rules: HashMap<String, Vec<KeywordRule>>,
    fingerprint: u64,
}

const DEFAULT_RULES: &str = r#"# ModSlut record-keyword evidence (MS-owned copy).
#
# Format mirrors the useful bit of SkyGen's keyword grammar:
# filterByKeywords=master.esm|formid:concept=portable-concept:confidence=0..100
#
# These are evidence facts. They do NOT pick a separator unless the profile
# roadmap explicitly maps that concept.

[armor]
filterByKeywords=skyrim.esm|0006bbd2:concept=equipment-armor-heavy:confidence=96
filterByKeywords=skyrim.esm|0006bbd3:concept=equipment-armor-light:confidence=96
filterByKeywords=skyrim.esm|0006c0ec:concept=equipment-armor:confidence=92
filterByKeywords=skyrim.esm|0006c0ed:concept=equipment-armor:confidence=92
filterByKeywords=skyrim.esm|0006c0ee:concept=equipment-armor:confidence=92
filterByKeywords=skyrim.esm|0006c0ef:concept=equipment-armor:confidence=92

# Weapon evidence deliberately stays empty for now. SkyGen's weapon rules use
# animation-type logic, not the two guessed vanilla form IDs that were here.
# Add only verified KWDA IDs once we have the matching SkyGen provenance.
"#;

fn fnv(text: &str) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for b in text.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("modslut_record_keywords.ini")))
        .unwrap_or_else(|| PathBuf::from("modslut_record_keywords.ini"))
}

fn parse(text: &str) -> HashMap<String, Vec<KeywordRule>> {
    let mut rules: HashMap<String, Vec<KeywordRule>> = HashMap::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix("filterByKeywords=") else {
            continue;
        };
        let Some((key, tail)) = rest.split_once(":concept=") else {
            continue;
        };
        let (concept, confidence) = match tail.split_once(":confidence=") {
            Some((concept, score)) => (concept.trim(), score.trim().parse().unwrap_or(80)),
            None => (tail.trim(), 80),
        };
        if key.trim().is_empty() || concept.is_empty() {
            continue;
        }
        rules
            .entry(key.trim().to_lowercase())
            .or_default()
            .push(KeywordRule {
                concept: concept.to_string(),
                confidence,
            });
    }
    rules
}

impl RecordKeywords {
    pub fn load() -> Self {
        let text = std::fs::read_to_string(path()).unwrap_or_else(|_| DEFAULT_RULES.to_string());
        Self {
            rules: parse(&text),
            fingerprint: fnv(&text),
        }
    }

    pub fn lookup(&self, key: &str) -> Option<&[KeywordRule]> {
        self.rules.get(&key.to_lowercase()).map(Vec::as_slice)
    }

    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_skygen_style_keyword_evidence() {
        let rules = parse(
            "filterByKeywords=Skyrim.esm|0006BBD2:concept=equipment-armor-heavy:confidence=96",
        );
        let rule = &rules["skyrim.esm|0006bbd2"][0];
        assert_eq!(rule.concept, "equipment-armor-heavy");
        assert_eq!(rule.confidence, 96);
    }
}

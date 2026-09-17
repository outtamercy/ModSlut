// Cached, structural content evidence for ModSlut.
//
// This is intentionally facts-first. It stores what plugins contain (at the
// useful top-level signature level) and derives portable concepts with a
// confidence score. Placement remains the roadmap/user's job; the index must
// never quietly turn a guess into a move.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use crate::plugins::PluginInfo;
use crate::record_keywords::RecordKeywords;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    pub concept: String,
    pub confidence: u8,
}

#[derive(Clone, Debug)]
pub struct ModContent {
    pub facts: Vec<Fact>,
    pub plugins: usize,
}

#[derive(Clone, Debug)]
pub struct ContentIndex {
    pub path: PathBuf,
    pub from_cache: bool,
    entries: HashMap<String, ModContent>,
}

impl ContentIndex {
    pub fn get(&self, mod_name: &str) -> Option<&ModContent> {
        self.entries.get(mod_name)
    }
    pub fn summary(&self) -> String {
        let with_facts = self
            .entries
            .values()
            .filter(|entry| !entry.facts.is_empty())
            .count();
        format!(
            "{} plugin-bearing mod(s), {with_facts} with structural evidence",
            self.entries.len()
        )
    }
}

pub fn cache_path(modlist: &Path) -> PathBuf {
    let legacy_name = format!(
        "modslut_content_index_{}.cache",
        crate::profile_key(modlist)
    );
    let legacy = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(&legacy_name)))
        .unwrap_or_else(|| modlist.with_file_name(legacy_name));
    crate::migrate_profile_file(modlist, "content_index.cache", legacy)
}

fn add(facts: &mut Vec<Fact>, concept: &str, confidence: u8) {
    if let Some(existing) = facts.iter_mut().find(|fact| fact.concept == concept) {
        existing.confidence = existing.confidence.max(confidence);
    } else {
        facts.push(Fact {
            concept: concept.to_string(),
            confidence,
        });
    }
}

fn facts_for(infos: &[PluginInfo], keyword_rules: &RecordKeywords) -> Vec<Fact> {
    let mut signatures: HashMap<&str, u32> = HashMap::new();
    let mut non_base_masters = 0usize;
    for info in infos {
        for (sig, count) in &info.records {
            *signatures.entry(sig.as_str()).or_default() += count;
        }
        non_base_masters += info
            .masters
            .iter()
            .filter(|master| {
                !matches!(
                    master.as_str(),
                    "skyrim.esm"
                        | "update.esm"
                        | "dawnguard.esm"
                        | "hearthfires.esm"
                        | "dragonborn.esm"
                        | "skyrimvr.esm"
                )
            })
            .count();
    }
    let count = |sig: &str| *signatures.get(sig).unwrap_or(&0);
    let total: u32 = signatures.values().sum();
    let enough = |n: u32, floor: u32, percent: u32| {
        n >= floor && total > 0 && n.saturating_mul(100) >= total.saturating_mul(percent)
    };
    let npc = count("NPC_");
    let faces = count("HDPT").saturating_add(count("TXST"));
    let equipment = count("ARMO")
        .saturating_add(count("WEAP"))
        .saturating_add(count("ARMA"));
    let world = count("CELL")
        .saturating_add(count("WRLD"))
        .saturating_add(count("REFR"))
        .saturating_add(count("LAND"));
    let magic = count("SPEL")
        .saturating_add(count("PERK"))
        .saturating_add(count("MGEF"));
    let quests = count("QUST")
        .saturating_add(count("DIAL"))
        .saturating_add(count("SCEN"));
    let mut facts = Vec::new();
    // Exact KWDA facts are better than broad record-signature ratios, but an
    // enormous patch can touch one vanilla armor record incidentally. Keep a
    // one-off hit as a visible hint; require a small ARMO body before it earns
    // routing-grade confidence. This stays evidence-first, not guess-first.
    for info in infos {
        for (key, keyword_count) in &info.keywords {
            if *keyword_count == 0 {
                continue;
            }
            if let Some(rules) = keyword_rules.lookup(key) {
                for rule in rules {
                    let confidence =
                        if rule.concept.starts_with("equipment-armor") && count("ARMO") < 4 {
                            rule.confidence.min(55)
                        } else {
                            rule.confidence
                        };
                    add(&mut facts, &rule.concept, confidence);
                }
            }
        }
    }
    // A single CELL/ARMO/NPC_ is often a tiny compatibility edit. Demand a
    // meaningful count and share of the actual records before emitting a
    // content concept. This makes the index useful evidence, not a synonym
    // for "the plugin exists".
    if enough(world, 12, 35) {
        add(&mut facts, "worldspace", 86);
    }
    if enough(npc, 12, 20) {
        add(
            &mut facts,
            if faces >= 12 { "npc-appearance" } else { "npc" },
            if faces >= 12 { 92 } else { 78 },
        );
    }
    if enough(equipment, 12, 25) {
        add(&mut facts, "equipment", 84);
    }
    if enough(quests, 8, 18) {
        add(&mut facts, "quests-dialogue", 80);
    }
    if enough(magic, 10, 22) {
        add(&mut facts, "magic-gameplay", 82);
    }
    if enough(count("COBJ"), 10, 30) && equipment < 12 {
        add(&mut facts, "crafting", 72);
    }
    // Several non-base masters is meaningful compatibility evidence, but not
    // enough to choose a generic Patch section on its own.
    if non_base_masters >= 2 {
        add(&mut facts, "compatibility", 64);
    }
    facts.sort_by(|a, b| {
        b.confidence
            .cmp(&a.confidence)
            .then_with(|| a.concept.cmp(&b.concept))
    });
    facts
}

fn derive(
    modlist: &Path,
    census: &[(String, String, PluginInfo)],
    keyword_rules: &RecordKeywords,
) -> ContentIndex {
    let mut by_mod: HashMap<String, Vec<PluginInfo>> = HashMap::new();
    for (name, _, info) in census {
        by_mod.entry(name.clone()).or_default().push(info.clone());
    }
    let entries = by_mod
        .into_iter()
        .map(|(name, infos)| {
            let plugins = infos.len();
            (
                name,
                ModContent {
                    facts: facts_for(&infos, keyword_rules),
                    plugins,
                },
            )
        })
        .collect();
    ContentIndex {
        path: cache_path(modlist),
        from_cache: false,
        entries,
    }
}

fn save(index: &ContentIndex, fingerprint: u64) {
    let mut out = format!("#modslut-content-index-v4 {fingerprint:016x}\n");
    for (name, entry) in &index.entries {
        let facts = entry
            .facts
            .iter()
            .map(|fact| format!("{}:{}", fact.concept, fact.confidence))
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&format!("{name}\t{}\t{facts}\n", entry.plugins));
    }
    let _ = crate::ensure_profile_data_parent(&index.path);
    let _ = std::fs::write(&index.path, out);
}

fn load(modlist: &Path, fingerprint: u64) -> Option<ContentIndex> {
    let path = cache_path(modlist);
    let text = std::fs::read_to_string(&path).ok()?;
    let mut lines = text.lines();
    if lines.next()? != format!("#modslut-content-index-v4 {fingerprint:016x}") {
        return None;
    }
    let mut entries = HashMap::new();
    for line in lines {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 3 {
            return None;
        }
        let facts = fields[2]
            .split(',')
            .filter_map(|item| {
                let (concept, score) = item.rsplit_once(':')?;
                Some(Fact {
                    concept: concept.to_string(),
                    confidence: score.parse().ok()?,
                })
            })
            .collect();
        entries.insert(
            fields[0].to_string(),
            ModContent {
                plugins: fields[1].parse().ok()?,
                facts,
            },
        );
    }
    Some(ContentIndex {
        path,
        from_cache: true,
        entries,
    })
}

pub fn load_or_build(
    modlist: &Path,
    fingerprint: u64,
    census: &[(String, String, PluginInfo)],
) -> ContentIndex {
    let keyword_rules = RecordKeywords::load();
    let fingerprint = fingerprint ^ keyword_rules.fingerprint();
    if let Some(index) = load(modlist, fingerprint) {
        return index;
    }
    let index = derive(modlist, census, &keyword_rules);
    save(&index, fingerprint);
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npc_appearance_beats_a_generic_npc_fact() {
        let info = PluginInfo {
            plugin: "faces.esp".into(),
            masters: vec![],
            is_esm: false,
            is_esl: false,
            record_count: 0,
            groups: vec![],
            records: vec![
                ("NPC_".into(), 12),
                ("HDPT".into(), 12),
                ("TXST".into(), 12),
            ],
            keywords: vec![],
        };
        let facts = facts_for(&[info], &RecordKeywords::load());
        assert_eq!(
            facts.first().map(|fact| fact.concept.as_str()),
            Some("npc-appearance")
        );
    }

    #[test]
    fn multiple_non_base_masters_only_mark_compatibility() {
        let info = PluginInfo {
            plugin: "patch.esp".into(),
            masters: vec!["skyrim.esm".into(), "lux.esp".into(), "city.esp".into()],
            is_esm: false,
            is_esl: false,
            record_count: 0,
            groups: vec![],
            records: vec![],
            keywords: vec![],
        };
        assert!(facts_for(&[info], &RecordKeywords::load())
            .iter()
            .any(|fact| fact.concept == "compatibility"));
    }

    #[test]
    fn one_tiny_patch_edit_does_not_become_a_content_category() {
        let info = PluginInfo {
            plugin: "tiny.esp".into(),
            masters: vec![],
            is_esm: false,
            is_esl: false,
            record_count: 0,
            groups: vec![],
            records: vec![("NPC_".into(), 1), ("ARMO".into(), 1), ("CELL".into(), 1)],
            keywords: vec![],
        };
        assert!(facts_for(&[info], &RecordKeywords::load()).is_empty());
    }

    #[test]
    fn skygen_ported_armor_keyword_needs_a_real_armor_body_for_routing_confidence() {
        let info = PluginInfo {
            plugin: "armor.esp".into(),
            masters: vec!["skyrim.esm".into()],
            is_esm: false,
            is_esl: false,
            record_count: 1,
            groups: vec![],
            records: vec![("ARMO".into(), 4)],
            keywords: vec![
                ("skyrim.esm|0006bbd2".into(), 1), // ArmorHeavy
                ("skyrim.esm|0006c0ec".into(), 1), // ArmorCuirass
            ],
        };
        let facts = facts_for(&[info], &RecordKeywords::load());
        assert!(facts
            .iter()
            .any(|fact| { fact.concept == "equipment-armor-heavy" && fact.confidence == 96 }));
        assert!(facts.iter().any(|fact| fact.concept == "equipment-armor"));
    }

    #[test]
    fn one_off_vanilla_armor_keyword_stays_a_hint() {
        let info = PluginInfo {
            plugin: "patch.esp".into(),
            masters: vec!["skyrim.esm".into()],
            is_esm: false,
            is_esl: false,
            record_count: 1,
            groups: vec![],
            records: vec![("ARMO".into(), 1)],
            keywords: vec![("skyrim.esm|0006bbd2".into(), 1)],
        };
        let facts = facts_for(&[info], &RecordKeywords::load());
        assert!(facts
            .iter()
            .any(|fact| { fact.concept == "equipment-armor-heavy" && fact.confidence == 55 }));
    }
}

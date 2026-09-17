// Profile-local separator roadmap.
//
// Separators are layout targets, not a universal taxonomy. This file gives
// ModSlut an explicit, inspectable bridge from a profile's labels to portable
// concepts before any sorter is allowed to interpret them. It deliberately
// does not decide where a mod belongs; that requires independent evidence.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

pub const FILE_PREFIX: &str = "modslut_section_map_";

pub struct SectionMap {
    pub path: PathBuf,
    pub created: bool,
    // The file is deliberately boring INI rather than a clever schema. The
    // important bit is that the user can see and correct every bridge from a
    // portable concept to one of their separators.
    mappings: HashMap<String, String>,
    protected: HashSet<String>,
    // Context rows are part of MO2's presentation but not sortable shelves:
    // for example a profile-title wrapper or Game Folder. They are inferred
    // only when this profile roadmap is created, then remain editable here.
    context: HashSet<String>,
    terminal: HashSet<String>,
    ambiguous: HashSet<String>,
    multi_destination: HashSet<String>,
    // Repeated concept rows form a profile-owned routing tree, not one global
    // shelf. Keep those discovered destinations so the roadmap UI can show
    // the actual branches without deciding which layouts deserve special
    // treatment in the executable.
    branch_destinations: HashMap<String, Vec<String>>,
}

impl SectionMap {
    /// Return a destination only when this category has one unambiguous,
    /// user-reviewable roadmap entry.  We do not fuzzy-match separator names
    /// here: "fix", "patch", and friends are how a useful sorter turns into
    /// a very confident junk drawer.
    pub fn destination_for_category(&self, category: &str) -> Option<&str> {
        let whole = normalize_key(category);
        if let Some(destination) = self.mappings.get(&whole) {
            return Some(destination);
        }

        let mut destinations = HashSet::new();
        for word in words(category) {
            if let Some(destination) = self.mappings.get(&word) {
                destinations.insert(destination.as_str());
            }
        }
        (destinations.len() == 1).then(|| destinations.into_iter().next().expect("one destination"))
    }

    /// An exact roadmap concept is an intentional profile decision.  This is
    /// distinct from a category/fuzzy route: a protected optional or
    /// pick-one shelf may be used only through this narrow door, never as a
    /// convenient catch-all destination.
    pub fn explicit_destination_for(&self, concept: &str) -> Option<&str> {
        self.mappings
            .get(&normalize_key(concept))
            .map(String::as_str)
    }

    pub fn is_protected(&self, label: &str) -> bool {
        self.protected.contains(&normalize_key(label)) || protected(label)
    }

    pub fn is_terminal(&self, label: &str) -> bool {
        self.terminal.contains(&normalize_key(label))
    }

    pub fn is_context(&self, label: &str) -> bool {
        self.context.contains(&normalize_key(label))
    }

    pub fn mapping_count(&self) -> usize {
        self.mappings.len()
    }

    pub fn ambiguous_count(&self) -> usize {
        self.ambiguous.len()
    }

    /// Concepts the GUI should put in front of the user. Armor needs three
    /// homes in a sensible MO2 layout, so the old one-size-fits-all `armor`
    /// bridge is intentionally not offered as a destination decision.
    pub fn review_concepts(&self) -> Vec<String> {
        let mut concepts: HashSet<String> = self
            .ambiguous
            .iter()
            .filter(|concept| concept.as_str() != "armor")
            .filter(|concept| !self.multi_destination.contains(*concept))
            .cloned()
            .collect();
        for concept in ["armor-base", "armor-converted", "armor-new"] {
            if !self.mappings.contains_key(concept) {
                concepts.insert(concept.to_string());
            }
        }
        // These come from plugin/title evidence rather than a separator's
        // spelling. A layout can call its ENB shelf anything it likes, so
        // generation cannot safely guess; the review window can ask once and
        // then every matching mod gets the same profile-local bridge.
        for concept in [
            "compatibility",
            "worldspace",
            "interior",
            "enb",
            "community-shaders",
        ] {
            if !self.mappings.contains_key(concept) && !self.multi_destination.contains(concept) {
                concepts.insert(concept.to_string());
            }
        }
        let mut concepts: Vec<String> = concepts.into_iter().collect();
        concepts.sort();
        concepts
    }

    pub fn reviewed_mappings(&self) -> Vec<(String, String)> {
        let mut rows: Vec<(String, String)> = self
            .mappings
            .iter()
            .map(|(concept, destination)| (concept.clone(), destination.clone()))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    }

    pub fn is_automatic_destination(&self, label: &str) -> bool {
        if self.is_protected(label) {
            return false;
        }
        let label = label.to_ascii_lowercase();
        // WACCF is a deliberately scoped fixes stack, not a general armor
        // shelf. A user can still explicitly pin something there.
        !label.contains("waccf")
            && !label.contains("weapons, armour, clothing, and clutter fixes")
            && !label.contains("weapons armor clothing and clutter fixes")
    }

    pub fn multi_destination_concepts(&self) -> Vec<String> {
        let mut concepts: Vec<String> = self.multi_destination.iter().cloned().collect();
        concepts.sort();
        concepts
    }

    pub fn branch_routes(&self) -> Vec<(String, Vec<String>)> {
        let mut routes: Vec<(String, Vec<String>)> = self
            .branch_destinations
            .iter()
            .map(|(concept, destinations)| (concept.clone(), destinations.clone()))
            .collect();
        routes.sort_by(|a, b| a.0.cmp(&b.0));
        routes
    }

    /// The profile-owned leaves for a multi-destination concept.  Sorters may
    /// use these as an allow-list when they have narrower evidence, but must
    /// never turn the branch itself into one arbitrary global destination.
    pub fn branch_destinations_for(&self, concept: &str) -> Option<&[String]> {
        self.branch_destinations
            .get(&normalize_key(concept))
            .map(Vec::as_slice)
    }
}

/// Replace every competing map row for one portable concept with the choice
/// made in the GUI. The file remains plain INI on purpose: this just saves the
/// same review a user could have made by hand, without making them spelunk for
/// duplicate entries.
pub fn save_reviewed_mapping(path: &Path, concept: &str, destination: &str) -> std::io::Result<()> {
    let wanted = normalize_key(concept);
    let text = std::fs::read_to_string(path)?;
    let mut section = "";
    let mut kept: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = &line[1..line.len() - 1];
        }
        let replace = section.eq_ignore_ascii_case("map")
            && !line.starts_with('#')
            && !line.starts_with(';')
            && line
                .split_once('=')
                .is_some_and(|(key, _)| normalize_key(key) == wanted);
        let remove_multi =
            section.eq_ignore_ascii_case("multi-destination") && normalize_key(line) == wanted;
        if !replace && !remove_multi {
            kept.push(raw.to_string());
        }
    }
    let map_start = kept
        .iter()
        .position(|line| line.trim().eq_ignore_ascii_case("[map]"))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "roadmap has no [map] section",
            )
        })?;
    let insert_at = kept
        .iter()
        .enumerate()
        .skip(map_start + 1)
        .find(|(_, line)| {
            let line = line.trim();
            line.starts_with('[') && line.ends_with(']')
        })
        .map(|(idx, _)| idx)
        .unwrap_or(kept.len());
    kept.insert(insert_at, format!("{wanted} = {destination}"));
    std::fs::write(path, format!("{}\n", kept.join("\n")))
}

pub fn save_multi_destination(path: &Path, concept: &str) -> std::io::Result<()> {
    let wanted = normalize_key(concept);
    let text = std::fs::read_to_string(path)?;
    let mut section = "";
    let mut kept = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = &line[1..line.len() - 1];
        }
        let map_row = section.eq_ignore_ascii_case("map")
            && line
                .split_once('=')
                .is_some_and(|(key, _)| normalize_key(key) == wanted);
        let multi_row =
            section.eq_ignore_ascii_case("multi-destination") && normalize_key(line) == wanted;
        if !map_row && !multi_row {
            kept.push(raw.to_string());
        }
    }
    let insert_at = kept
        .iter()
        .position(|line| line.trim().eq_ignore_ascii_case("[multi-destination]"));
    match insert_at {
        Some(start) => {
            let end = kept
                .iter()
                .enumerate()
                .skip(start + 1)
                .find(|(_, line)| {
                    let line = line.trim();
                    line.starts_with('[') && line.ends_with(']')
                })
                .map(|(idx, _)| idx)
                .unwrap_or(kept.len());
            kept.insert(end, wanted);
        }
        None => {
            kept.push(String::new());
            kept.push("[multi-destination]".into());
            kept.push(wanted);
        }
    }
    std::fs::write(path, format!("{}\n", kept.join("\n")))
}

pub fn path_for(modlist: &Path) -> PathBuf {
    // Maps live beside ModSlut, not inside MO2's profile folders. They still
    // need to be profile-specific: different profiles routinely have totally
    // different separator vocabularies.
    let profile = modlist
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("default");
    let safe_profile: String = profile
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let legacy_name = format!("{FILE_PREFIX}{safe_profile}.ini");
    let legacy = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(&legacy_name)))
        .unwrap_or_else(|| modlist.with_file_name(legacy_name));
    crate::migrate_profile_file(modlist, "section_map.ini", legacy)
}

// These are portable semantic anchors, not a list-specific layout. We only
// emit a mapping if the exact word or phrase is in the separator label.
const CONCEPTS: &[(&str, &[&str])] = &[
    ("nsfw", &["nsfw"]),
    ("ai", &["ai"]),
    ("immersion", &["immersion"]),
    ("dialogue", &["dialogue"]),
    ("perk", &["perk"]),
    ("combat", &["combat"]),
    ("magic", &["magic"]),
    ("alternate-start", &["alternate start", "alternate-start"]),
    ("effects", &["effects", "effect"]),
    ("fire", &["fire"]),
    ("lighting", &["lighting"]),
    ("enb", &["enb"]),
    (
        "community-shaders",
        &["community shaders", "community shader"],
    ),
    ("interior", &["interior"]),
    ("compatibility", &["compatibility"]),
    ("worldspace", &["worldspace"]),
    ("map", &["map"]),
    ("patches", &["patch collection", "patches"]),
    ("animations", &["animations", "animation"]),
    ("body", &["body"]),
    ("armor", &["armor", "armour"]),
    (
        "armor-base",
        &[
            "armor and weapons retextures and tweaks",
            "armour retextures",
        ],
    ),
    (
        "armor-converted",
        &["armor conversions", "armour conversions"],
    ),
    (
        "armor-new",
        &["new armors and clothing", "new armour and clothing"],
    ),
    ("open-composite", &["open composite", "opencomposite"]),
    ("vr-controller-bindings", &["controller bindings"]),
    ("weapons", &["weapons", "weapon"]),
    ("audio", &["audio", "sound"]),
    ("interface", &["interface", "ui"]),
    ("quests", &["quests", "quest"]),
    ("landscape", &["landscape"]),
    ("cities", &["cities", "city", "town"]),
];

fn protected(label: &str) -> bool {
    let label = label.to_ascii_lowercase();
    [
        "output",
        "end of list",
        "testing",
        "test",
        "optional",
        "pick one",
    ]
    .iter()
    .any(|needle| label.contains(needle))
}

fn has_phrase(label: &str, phrase: &str) -> bool {
    let label = words(label);
    let phrase = words(phrase);
    !phrase.is_empty() && label.windows(phrase.len()).any(|window| window == phrase)
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| word.to_ascii_lowercase())
        .collect()
}

fn normalize_key(text: &str) -> String {
    words(text).join("-")
}

fn parse_roadmap(
    path: &Path,
) -> std::io::Result<(
    HashMap<String, String>,
    HashSet<String>,
    HashSet<String>,
    HashSet<String>,
    HashSet<String>,
    HashSet<String>,
    HashMap<String, Vec<String>>,
)> {
    let text = std::fs::read_to_string(path)?;
    let mut section = "";
    let mut candidates: HashMap<String, HashSet<String>> = HashMap::new();
    let mut protected_labels = HashSet::new();
    let mut context_labels = HashSet::new();
    let mut terminal_labels = HashSet::new();
    let mut multi_destination = HashSet::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = &line[1..line.len() - 1];
            continue;
        }
        match section.to_ascii_lowercase().as_str() {
            "map" => {
                if let Some((concept, destination)) = line.split_once('=') {
                    let concept = normalize_key(concept);
                    let destination = destination.trim();
                    if !concept.is_empty() && !destination.is_empty() {
                        candidates
                            .entry(concept)
                            .or_default()
                            .insert(destination.to_string());
                    }
                }
            }
            "protected" => {
                protected_labels.insert(normalize_key(line));
            }
            "context" => {
                context_labels.insert(normalize_key(line));
            }
            "terminal" => {
                terminal_labels.insert(normalize_key(line));
            }
            "multi-destination" => {
                let concept = normalize_key(line);
                if !concept.is_empty() {
                    multi_destination.insert(concept);
                }
            }
            _ => {}
        }
    }
    // Repeated `concept = separator` rows are not "last one wins". That's
    // how every patch category quietly ended up in the last patch separator
    // in a big list. Hold an ambiguous concept until a human picks one.
    let mut mappings = HashMap::new();
    let ambiguous = HashSet::new();
    let mut branch_destinations = HashMap::new();
    for (concept, destinations) in candidates {
        if destinations.len() == 1 {
            mappings.insert(
                concept,
                destinations.into_iter().next().expect("one destination"),
            );
        } else {
            let mut destinations: Vec<String> = destinations.into_iter().collect();
            destinations.sort();
            multi_destination.insert(concept.clone());
            branch_destinations.insert(concept, destinations);
        }
    }
    Ok((
        mappings,
        protected_labels,
        context_labels,
        ambiguous,
        terminal_labels,
        multi_destination,
        branch_destinations,
    ))
}

fn roadmap_text(labels: &[String]) -> String {
    let mut out = String::from(
        "# ModSlut separator roadmap - generated once per MO2 profile.\n#\n# This file maps PORTABLE CONCEPTS to your profile's separator labels.\n# ModSlut reads this before it plans moves. Edit the generated entries or add\n# your own. A separator with no entry is intentionally not a destination.\n# Protected separators are never automatic destinations.\n\n[map]\n",
    );
    let mut candidates: HashMap<&str, Vec<&str>> = HashMap::new();
    for label in labels {
        if protected(label) {
            continue;
        }
        for (concept, aliases) in CONCEPTS {
            if aliases.iter().any(|alias| has_phrase(label, alias)) {
                candidates.entry(concept).or_default().push(label);
            }
        }
    }
    for (concept, _) in CONCEPTS {
        if let Some(destinations) = candidates.get(concept) {
            if destinations.len() == 1 {
                out.push_str(&format!("{concept} = {}\n", destinations[0]));
            }
        }
    }
    out.push_str("\n# Concepts with several plausible separator targets are deliberately\n# not auto-mapped. Pick ONE target below and add it to [map] if you want it.\n[needs-review]\n");
    for (concept, _) in CONCEPTS {
        if let Some(destinations) = candidates.get(concept).filter(|d| d.len() > 1) {
            out.push_str(&format!("# {concept}: {}\n", destinations.join(" | ")));
        }
    }
    out.push_str("\n[protected]\n");
    for label in labels.iter().filter(|label| protected(label)) {
        out.push_str(label);
        out.push('\n');
    }
    out.push_str("\n# Non-sortable MO2 presentation rows for this profile.\n[context]\n");
    if let Some(wrapper) = labels.last() {
        out.push_str(wrapper);
        out.push('\n');
    }
    for label in labels
        .iter()
        .filter(|label| normalize_key(label) == "game-folder")
    {
        if labels.last() != Some(label) {
            out.push_str(label);
            out.push('\n');
        }
    }
    out.push_str("\n[terminal]\n");
    for label in labels
        .iter()
        .filter(|label| normalize_key(label) == "end-of-list")
    {
        out.push_str(label);
        out.push('\n');
    }
    out.push_str("\n# Concepts that deliberately have NO global destination. They need\n# narrower evidence (city name, interface file, patch parentage) or a user rule.\n[multi-destination]\n");
    let mut branch_concepts: Vec<&str> = candidates
        .iter()
        .filter_map(|(concept, destinations)| (destinations.len() > 1).then_some(*concept))
        .collect();
    branch_concepts.sort();
    for concept in branch_concepts {
        out.push_str(concept);
        out.push('\n');
    }
    out.push_str("\n[unmapped]\n");
    for label in labels.iter().filter(|label| !protected(label)) {
        if !CONCEPTS
            .iter()
            .any(|(_, aliases)| aliases.iter().any(|alias| has_phrase(label, alias)))
        {
            out.push_str("# ");
            out.push_str(label);
            out.push('\n');
        }
    }
    out
}

// Existing user maps are authoritative and never rewritten. Creating the file
// is separate from using it for placement, so it can be reviewed first.
pub fn load_or_create(modlist: &Path, labels: &[String]) -> std::io::Result<SectionMap> {
    let path = path_for(modlist);
    let created = !path.is_file();
    if created {
        crate::ensure_profile_data_parent(&path)?;
        std::fs::write(&path, roadmap_text(labels))?;
    } else {
        // Compatibility migration for roadmaps generated before boundaries
        // were explicit. This is profile-local data, not an executable
        // fallback: once written, the user can rename/remove either row.
        let text = std::fs::read_to_string(&path)?;
        let has_context = text
            .lines()
            .any(|line| line.trim().eq_ignore_ascii_case("[context]"));
        let has_terminal = text
            .lines()
            .any(|line| line.trim().eq_ignore_ascii_case("[terminal]"));
        if !has_context || !has_terminal {
            let mut updated = text;
            if !updated.ends_with('\n') {
                updated.push('\n');
            }
            if !has_context {
                updated.push_str(
                    "\n# Non-sortable MO2 presentation rows for this profile.\n[context]\n",
                );
                if let Some(wrapper) = labels.last() {
                    updated.push_str(wrapper);
                    updated.push('\n');
                }
                for label in labels
                    .iter()
                    .filter(|label| normalize_key(label) == "game-folder")
                {
                    if labels.last() != Some(label) {
                        updated.push_str(label);
                        updated.push('\n');
                    }
                }
            }
            if !has_terminal {
                // This is only a one-time import hint for old profiles. The
                // result is explicit profile-local data from here on; the
                // sorter has no built-in terminal label.
                updated.push_str("\n# Last shelf: nothing is sorted beyond it.\n[terminal]\n");
                for label in labels
                    .iter()
                    .filter(|label| normalize_key(label) == "end-of-list")
                {
                    updated.push_str(label);
                    updated.push('\n');
                }
            }
            std::fs::write(&path, updated)?;
        }
    }
    let (mappings, protected, context, ambiguous, terminal, multi_destination, branch_destinations) =
        parse_roadmap(&path)?;
    Ok(SectionMap {
        path,
        created,
        mappings,
        protected,
        context,
        terminal,
        ambiguous,
        multi_destination,
        branch_destinations,
    })
}

/// Import a roadmap pack into this profile. Packs name exact separators, so a
/// mismatched list is rejected rather than half-imported into nonsense. The
/// previous local roadmap is retained beside it as a plain INI backup.
pub fn import_for_profile(
    modlist: &Path,
    source: &Path,
    labels: &[String],
) -> std::io::Result<usize> {
    let (mappings, _, _, _, _, _, _) = parse_roadmap(source)?;
    let unknown: Vec<String> = mappings
        .values()
        .filter(|destination| {
            !labels
                .iter()
                .any(|label| label.eq_ignore_ascii_case(destination))
        })
        .cloned()
        .collect();
    if !unknown.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "pack names separators this profile does not have: {}",
                unknown.join(", ")
            ),
        ));
    }
    let target = path_for(modlist);
    crate::ensure_profile_data_parent(&target)?;
    if target.exists() {
        std::fs::copy(&target, target.with_extension("before-import.ini"))?;
    }
    std::fs::copy(source, &target)?;
    Ok(mappings.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_explicit_label_concepts_and_protects_operational_rows() {
        let labels = vec![
            "Ya Filthy Animal - NSFW".into(),
            "Combat & Magic".into(),
            "Outputs".into(),
            "End of List".into(),
        ];
        let map = roadmap_text(&labels);
        assert!(map.contains("nsfw = Ya Filthy Animal - NSFW"));
        assert!(map.contains("combat = Combat & Magic"));
        assert!(map.contains("magic = Combat & Magic"));
        assert!(map.contains("[protected]\nOutputs\nEnd of List"));
        assert!(!map.contains("output = Outputs"));
        assert!(!has_phrase("Main Files", "ai"));
    }

    #[test]
    fn category_needs_one_clear_roadmap_destination() {
        let map = SectionMap {
            path: PathBuf::new(),
            created: false,
            mappings: HashMap::from([
                ("combat".into(), "Combat & Magic".into()),
                ("magic".into(), "Combat & Magic".into()),
                ("audio".into(), "Audio".into()),
            ]),
            protected: HashSet::new(),
            context: HashSet::new(),
            terminal: HashSet::new(),
            ambiguous: HashSet::new(),
            multi_destination: HashSet::new(),
            branch_destinations: HashMap::new(),
        };
        assert_eq!(
            map.destination_for_category("Combat & Magic"),
            Some("Combat & Magic")
        );
        assert_eq!(map.destination_for_category("Combat Audio"), None);
    }

    #[test]
    fn patch_rows_become_a_branch_not_a_fake_global_home() {
        let dir = std::env::temp_dir().join(format!("modslut-map-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("roadmap.ini");
        std::fs::write(
            &path,
            "[map]\npatches = Patch Hub\npatches = Engines Patches\n",
        )
        .unwrap();
        let (mappings, _, _, ambiguous, _, multi, branches) = parse_roadmap(&path).unwrap();
        assert!(!mappings.contains_key("patches"));
        assert!(!ambiguous.contains("patches"));
        assert!(multi.contains("patches"));
        assert_eq!(branches["patches"], ["Engines Patches", "Patch Hub"]);
        std::fs::remove_file(path).ok();
        std::fs::remove_dir(dir).ok();
    }

    #[test]
    fn generated_end_of_list_is_a_profile_terminal() {
        let map = roadmap_text(&["Gameplay".into(), "End of List".into()]);
        assert!(map.contains("[terminal]\nEnd of List"));
    }

    #[test]
    fn gui_review_replaces_ambiguous_rows_without_touching_other_concepts() {
        let path =
            std::env::temp_dir().join(format!("modslut-map-review-{}.ini", std::process::id()));
        std::fs::write(
            &path,
            "[map]\narmor = A\narmor = B\nmagic = Magic\n\n[protected]\nEnd of List\n",
        )
        .unwrap();
        save_reviewed_mapping(&path, "armor", "B").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("armor = B"));
        assert!(!text.contains("armor = A"));
        assert!(text.contains("magic = Magic"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn multi_destination_review_suppresses_a_generic_default() {
        let path =
            std::env::temp_dir().join(format!("modslut-map-multi-{}.ini", std::process::id()));
        std::fs::write(&path, "[map]\ncities = Whiterun\ncities = Riften\n").unwrap();
        save_multi_destination(&path, "cities").unwrap();
        let (mappings, _, _, ambiguous, _, multi, _) = parse_roadmap(&path).unwrap();
        assert!(!mappings.contains_key("cities"));
        assert!(!ambiguous.contains("cities"));
        assert!(multi.contains("cities"));
        std::fs::remove_file(path).ok();
    }
}

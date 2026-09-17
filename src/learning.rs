// Profile-local manual-placement learning.
//
// MS cannot honestly infer whether an arbitrary current separator is a human
// decision or leftover chaos.  It *can* know what it wrote last time.  The
// baseline is recorded after an explicit Sort (and after Apply). A later
// *section* change is a concrete human correction, not a guess we teach
// ourselves. Opening, reloading, and background re-plans never advance it.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

fn key(text: &str) -> String {
    text.to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

fn operational(section: &str) -> bool {
    let section = section.to_ascii_lowercase();
    section.contains("end of list") || section.contains("output")
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Placement {
    section: String,
    // This is the exact MO2 left-pane priority (top row is highest). It is
    // intentionally captured alongside a learned section so a manual reorder
    // is visible as a backbone update, never mistaken for random churn.
    priority: usize,
}

fn placements(text: &str) -> HashMap<String, Placement> {
    let rows: Vec<&str> = text
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let total = rows.len();
    let mut pending = Vec::<(String, usize)>::new();
    let mut out = HashMap::new();
    for (index, line) in rows.into_iter().enumerate() {
        let bare = line.trim_start_matches(['+', '-', '*']).trim();
        let priority = total - index;
        if let Some(section) = bare.strip_suffix("_separator") {
            let section = section.trim();
            if !operational(section) {
                for (name, priority) in pending.drain(..) {
                    out.insert(
                        key(&name),
                        Placement {
                            section: section.to_string(),
                            priority,
                        },
                    );
                }
            } else {
                pending.clear();
            }
        } else if !bare.is_empty() {
            pending.push((bare.to_string(), priority));
        }
    }
    out
}

pub fn baseline_path(modlist: &Path) -> PathBuf {
    crate::profile_data_path(modlist, "applied_baseline.ini")
}

// This is deliberately separate from the Apply baseline. It lets a user
// preview a sort, close MS, arrange their list in MO2, then have the next
// explicit Sort recognise those changed homes even though they never applied
// MS's previous preview.
pub fn observation_path(modlist: &Path) -> PathBuf {
    crate::profile_data_path(modlist, "sort_observation.ini")
}

pub fn homes_path(modlist: &Path) -> PathBuf {
    crate::profile_data_path(modlist, "learned_homes.ini")
}

#[derive(Clone, Debug, Default)]
pub struct LearningIndex {
    homes: HashMap<String, String>,
    /// New, concrete section moves seen since the last explicit Sort/Apply.
    pub observed_corrections: usize,
    /// Manual moves that stayed in one separator but changed the MO2 backbone.
    /// These need no separate rule: the current priority manifest is already
    /// the next sort's input order.
    pub observed_priority_adjustments: usize,
    /// All saved homes currently influencing this profile.
    pub corrections: usize,
}

impl LearningIndex {
    // Reads only. The caller decides whether this explicit Sort should then
    // advance the observation snapshot.
    pub fn observe(modlist: &Path, current: &str) -> Self {
        let observation = observation_path(modlist);
        let path = if observation.exists() {
            observation
        } else {
            baseline_path(modlist)
        };
        // Explicitly learned homes work immediately, including before the
        // first MS Apply has created a comparison baseline.
        let mut homes = load_saved_homes(modlist);
        let Ok(previous) = std::fs::read_to_string(path) else {
            let corrections = homes.len();
            return Self {
                homes,
                corrections,
                observed_corrections: 0,
                observed_priority_adjustments: 0,
            };
        };
        let previous = placements(&previous);
        let current = placements(current);
        let mut observed_corrections = 0usize;
        let mut observed_priority_adjustments = 0usize;
        for (name, placement) in current {
            if previous
                .get(&name)
                .is_some_and(|old| old.section != placement.section)
            {
                if homes.get(&name).is_none_or(|old| old != &placement.section) {
                    homes.insert(name, placement.section);
                    observed_corrections += 1;
                }
            } else if previous
                .get(&name)
                .is_some_and(|old| old.priority != placement.priority)
            {
                observed_priority_adjustments += 1;
            }
        }
        let corrections = homes.len();
        Self {
            homes,
            corrections,
            observed_corrections,
            observed_priority_adjustments,
        }
    }

    pub fn destination(&self, mod_name: &str, sections: &[String]) -> Option<String> {
        let home = self.homes.get(&key(mod_name))?;
        sections
            .iter()
            .find(|section| key(section) == key(home))
            .cloned()
    }

    /// Turn explicit, profile-local placement pins into durable learned
    /// homes.  This is intentionally a one-way lesson: removing the rule
    /// later does not make MS forget what the user explicitly taught it.
    /// Built-in rules are never consulted here.
    pub fn learn_profile_pins(
        &mut self,
        modlist: &Path,
        sections: &[String],
    ) -> std::io::Result<usize> {
        let mut changed = 0usize;
        for path in crate::user_rule_files(Some(modlist)) {
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            for line in text.lines() {
                let line = line.trim();
                let Some(body) = line.strip_prefix(['!', '<', '^']) else {
                    continue;
                };
                let Some((name, target)) = body.split_once('=') else {
                    continue;
                };
                let name = name.trim();
                let target = target.trim();
                if name.is_empty() || target.is_empty() || operational(target) {
                    continue;
                }
                // Store the live separator spelling so a later lookup is
                // robust to case and spacing in the user's rule.
                let Some(section) = sections.iter().find(|s| s.eq_ignore_ascii_case(target)) else {
                    continue;
                };
                let name = key(name);
                if self.homes.get(&name).is_none_or(|old| old != section) {
                    self.homes.insert(name, section.clone());
                    changed += 1;
                }
            }
        }
        if changed > 0 {
            save_homes(modlist, &self.homes)?;
            self.corrections = self.homes.len();
        }
        Ok(changed)
    }
}

fn load_saved_homes(modlist: &Path) -> HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(homes_path(modlist)) else {
        return HashMap::new();
    };
    text.lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(name, section)| (key(name.trim()), section.trim().to_string()))
        .filter(|(name, section)| !name.is_empty() && !section.is_empty() && !operational(section))
        .collect()
}

// Explicitly bless a current separator. This is for an already curated shelf
// imported from a guide or hand-sorted before MS ever had a baseline.
pub fn learn_section(modlist: &Path, section: &str, mods: &[String]) -> std::io::Result<usize> {
    if operational(section) {
        return Ok(0);
    }
    let mut homes = load_saved_homes(modlist);
    let mut changed = 0usize;
    for name in mods {
        let name = key(name);
        if !name.is_empty() && homes.get(&name).is_none_or(|old| old != section) {
            homes.insert(name, section.to_string());
            changed += 1;
        }
    }
    save_homes(modlist, &homes)?;
    Ok(changed)
}

/// Explicitly bless every managed shelf currently visible in the profile.
/// This is deliberately separate from `learn_section`: a right-click on one
/// separator should never accidentally teach the whole list.
pub fn learn_all(modlist: &Path, sections: &[(String, Vec<String>)]) -> std::io::Result<usize> {
    let mut homes = load_saved_homes(modlist);
    let mut changed = 0usize;
    for (section, mods) in sections {
        if operational(section) || section.eq_ignore_ascii_case("Game Folder") {
            continue;
        }
        for name in mods {
            let name = key(name);
            if !name.is_empty() && homes.get(&name).is_none_or(|old| old != section) {
                homes.insert(name, section.clone());
                changed += 1;
            }
        }
    }
    save_homes(modlist, &homes)?;
    Ok(changed)
}

/// Forget the durable lesson too.  The GUI calls this from Clear Metadata so
/// users always have a straightforward way back from an accidental pin.
pub fn forget_home(modlist: &Path, mod_name: &str) -> std::io::Result<bool> {
    let mut homes = load_saved_homes(modlist);
    if homes.remove(&key(mod_name)).is_none() {
        return Ok(false);
    }
    save_homes(modlist, &homes)?;
    Ok(true)
}

fn save_homes(modlist: &Path, homes: &HashMap<String, String>) -> std::io::Result<()> {
    let path = homes_path(modlist);
    crate::ensure_profile_data_parent(&path)?;
    let mut rows: Vec<_> = homes.iter().collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let mut text = "# Exact homes explicitly learned from this profile.\n".to_string();
    for (name, home) in rows {
        text.push_str(&format!("{name}\t{home}\n"));
    }
    std::fs::write(path, text)
}

// Only a user-approved write becomes the next comparison point.
pub fn save_baseline(modlist: &Path, text: &str) -> std::io::Result<()> {
    let path = baseline_path(modlist);
    crate::ensure_profile_data_parent(&path)?;
    std::fs::write(path, text)
}

/// Capture the actual MO2 layout only after the user explicitly asks MS to
/// sort. This never records MS's proposed output, so a preview cannot teach
/// itself its own guesses.
pub fn save_observation(modlist: &Path, text: &str) -> std::io::Result<()> {
    let path = observation_path(modlist);
    crate::ensure_profile_data_parent(&path)?;
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_section_is_a_manual_correction() {
        let before = "# This file was automatically generated by Mod Organizer.\n+A\n+Old_separator\n+End of List_separator\n";
        let after = "# This file was automatically generated by Mod Organizer.\n+A\n+New_separator\n+End of List_separator\n";
        let old = placements(before);
        let new = placements(after);
        assert_ne!(
            old.get("a").map(|p| &p.section),
            new.get("a").map(|p| &p.section)
        );
    }

    #[test]
    fn profile_pin_is_a_durable_learned_home() {
        let root = std::env::temp_dir().join(format!("modslut-learning-{}", std::process::id()));
        let modlist = root.join("modlist.txt");
        let rules = crate::profile_data_path(&modlist, "rules.txt");
        crate::ensure_profile_data_parent(&rules).unwrap();
        std::fs::write(&rules, "!A Mod = A Section\n").unwrap();
        let mut learning = LearningIndex::default();
        assert_eq!(
            learning
                .learn_profile_pins(&modlist, &["A Section".into()])
                .unwrap(),
            1
        );
        assert_eq!(
            learning.destination("A Mod", &["A Section".into()]),
            Some("A Section".into())
        );
        // The rule may disappear later: its user-approved placement survives.
        std::fs::write(&rules, "").unwrap();
        assert_eq!(
            LearningIndex::observe(&modlist, "").destination("A Mod", &["A Section".into()]),
            Some("A Section".into())
        );
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(rules.parent().unwrap());
    }
}

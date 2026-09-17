// User-local reference profiles.
//
// A reference is not a public rule pack. It is a private record of where a
// human deliberately put exact mod names in another profile. That lets a
// Mayhem VR list benefit from Mayhem AE / Synergy VR without baking either
// author's layout into ModSlut's Nexus build.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default)]
pub struct ReferenceIndex {
    entries: HashMap<String, Vec<String>>, // normalized mod -> source sections
}

fn key(text: &str) -> String {
    text.to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

fn operational_section(section: &str) -> bool {
    let section = section.to_ascii_lowercase();
    section.contains("end of list")
        || section.contains("output")
        || section.contains("testing")
        || section.contains("game folder")
        || section.contains("creation kit")
}

pub fn path_for(modlist: &Path) -> PathBuf {
    let legacy_name = format!(
        "modslut_private_reference_{}.ini",
        crate::profile_key(modlist)
    );
    let legacy = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(&legacy_name)))
        .unwrap_or_else(|| modlist.with_file_name(legacy_name));
    crate::migrate_profile_file(modlist, "private_reference.ini", legacy)
}

impl ReferenceIndex {
    pub fn load(modlist: &Path) -> Self {
        let path = path_for(modlist);
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let mut entries: HashMap<String, Vec<String>> = HashMap::new();
        for line in text.lines() {
            let Some((name, section)) = line.split_once('\t') else {
                continue;
            };
            let name = name.trim();
            let section = section.trim();
            if !name.is_empty() && !section.is_empty() && !operational_section(section) {
                entries
                    .entry(key(name))
                    .or_default()
                    .push(section.to_string());
            }
        }
        Self { entries }
    }

    pub fn source_count(&self) -> usize {
        self.entries.len()
    }

    /// Return a destination only if every usable human reference agrees on
    /// one actual section in the active profile. Different layouts are fine;
    /// a reference label that doesn't exist here is simply not a vote.
    pub fn destination(&self, mod_name: &str, sections: &[String]) -> Option<(String, u8)> {
        let rows = self.entries.get(&key(mod_name))?;
        let mut votes: HashMap<String, usize> = HashMap::new();
        for source_section in rows {
            if let Some(destination) = sections
                .iter()
                .find(|section| key(section) == key(source_section))
            {
                *votes.entry(destination.clone()).or_default() += 1;
            }
        }
        if votes.len() != 1 {
            return None;
        }
        let (destination, count) = votes.into_iter().next().expect("one vote target");
        Some((destination, if count >= 2 { 99 } else { 92 }))
    }
}

/// Append a human-sorted source profile to the active profile's private
/// reference file. Both source and target live outside the shipped defaults.
pub fn import_source(target_modlist: &Path, source_modlist: &Path) -> std::io::Result<usize> {
    let text = std::fs::read_to_string(source_modlist)?;
    let mut bucket: Vec<String> = Vec::new();
    let mut rows: Vec<(String, String)> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        let bare = line.trim_start_matches(['+', '-']).trim();
        if let Some(label) = bare.strip_suffix("_separator") {
            let label = label.trim();
            for name in bucket.drain(..) {
                if !operational_section(label) {
                    rows.push((name, label.to_string()));
                }
            }
        } else if (line.starts_with('+') || line.starts_with('-')) && !bare.is_empty() {
            bucket.push(bare.to_string());
        }
    }
    let path = path_for(target_modlist);
    let mut out = if path.is_file() {
        std::fs::read_to_string(&path)?
    } else {
        "# ModSlut private human-reference data. Do not ship with ModSlut itself.\n".to_string()
    };
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("# source: {}\n", source_modlist.display()));
    let mut existing: HashSet<String> = out
        .lines()
        .filter(|line| line.contains('\t'))
        .map(str::to_string)
        .collect();
    let mut added = 0usize;
    for (name, section) in &rows {
        let row = format!("{name}\t{section}");
        if existing.insert(row.clone()) {
            out.push_str(&row);
            out.push('\n');
            added += 1;
        }
    }
    crate::ensure_profile_data_parent(&path)?;
    std::fs::write(path, out)?;
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_unanimous_existing_section_wins() {
        let index = ReferenceIndex {
            entries: HashMap::from([
                (key("A Mod"), vec!["Whiterun".into(), "Whiterun".into()]),
                (key("B Mod"), vec!["Whiterun".into(), "Riften".into()]),
            ]),
        };
        let sections = vec!["Whiterun".into(), "Riften".into()];
        assert_eq!(
            index.destination("A Mod", &sections),
            Some(("Whiterun".into(), 99))
        );
        assert_eq!(index.destination("B Mod", &sections), None);
    }

    #[test]
    fn operational_sections_never_become_reference_evidence() {
        assert!(operational_section("End of List"));
        assert!(operational_section("Outputs"));
        assert!(!operational_section("Whiterun"));
    }
}

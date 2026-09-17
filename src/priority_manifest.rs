//! ModSlut-owned priority manifest.
//!
//! MO2's `modlist.txt` is deliberately left untouched. Its on-disk order is
//! the reverse of the order users see in MO2's left pane. This small
//! profile-local mirror writes the familiar visible order instead: top row
//! first, with position 1. It is rebuilt only when MO2 updates modlist.txt.

use std::{collections::HashMap, fs, io, path::Path, time::UNIX_EPOCH};

#[derive(Clone, Debug, Default)]
pub struct PriorityManifest {
    pub priorities: HashMap<String, usize>,
    pub refreshed: bool,
}

fn key(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

fn entry_name(row: &str) -> &str {
    row.trim().trim_start_matches(['+', '-', '*']).trim()
}

/// A timestamp is the deliberately lightweight freshness contract: ModSlut
/// reads its cached manifest first, compares this value to MO2's current
/// `modlist.txt` modification time, and regenerates only if it changed.
fn modlist_timestamp(modlist: &Path) -> io::Result<u128> {
    Ok(fs::metadata(modlist)?
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos())
}

fn parse(text: &str, wanted_timestamp: u128) -> Option<HashMap<String, usize>> {
    let mut format_v4 = false;
    let mut saved_timestamp: Option<u128> = None;
    let mut values = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "format=4" {
            format_v4 = true;
        } else if let Some(value) = line.strip_prefix("modlist_modified_unix_nanos=") {
            saved_timestamp = value.trim().parse().ok();
        } else if let Some((value, name)) = line.split_once('\t') {
            let priority = value.trim().parse().ok()?;
            values.insert(key(entry_name(name)), priority);
        }
    }
    (format_v4 && saved_timestamp? == wanted_timestamp && !values.is_empty()).then_some(values)
}

pub fn load_or_refresh(modlist: &Path, source: &str) -> io::Result<PriorityManifest> {
    let path = crate::profile_data_path(modlist, "priority_manifest.ini");
    // Read the profile-local cache before deciding whether it is stale. The
    // only freshness comparison is MO2's own file time/date stamp.
    let cached = fs::read_to_string(&path).ok();
    let wanted_timestamp = modlist_timestamp(modlist)?;
    if let Some(old) = cached {
        if let Some(priorities) = parse(&old, wanted_timestamp) {
            return Ok(PriorityManifest {
                priorities,
                refreshed: false,
            });
        }
    }

    // A separator is a real MO2 row with a real priority. Keep every row in
    // the ledger rather than treating separators as invisible structure.
    let rows: Vec<String> = source
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|row| !row.is_empty())
        .map(ToString::to_string)
        .collect();
    let total = rows.len();
    let priorities = rows
        .iter()
        .enumerate()
        .map(|(index, row)| (key(entry_name(row)), total - index))
        .collect::<HashMap<_, _>>();
    crate::ensure_profile_data_parent(&path)?;
    let mut saved = format!(
        "# ModSlut priority manifest; generated from modlist.txt.\n# Rows are written in MO2's visible top-to-bottom order: position 1 first.\n# The left column includes every mod and separator; the right preserves MO2's + enabled, - disabled, and * foreign markers.\nformat=4\nmodlist_modified_unix_nanos={wanted_timestamp}\n"
    );
    // Disk order is bottom-to-top from the user's point of view. The map
    // retains the matching MO2 position, while the ledger becomes readable
    // without mentally reversing it.
    for row in rows.iter().rev() {
        saved.push_str(&priorities[&key(entry_name(row))].to_string());
        saved.push('\t');
        saved.push_str(row);
        saved.push('\n');
    }
    fs::write(path, saved)?;
    Ok(PriorityManifest {
        priorities,
        refreshed: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_mo2_order_starts_at_one_and_separators_count() {
        let source = "# header\n+High\n-Low\n+Section_separator\n";
        let rows: Vec<_> = source.lines().skip(1).collect();
        let total = rows.len();
        let priorities: HashMap<_, _> = rows
            .iter()
            .enumerate()
            .map(|(i, row)| (key(entry_name(row)), total - i))
            .collect();
        // `modlist.txt` is stored in reverse display order, so its final row
        // is position 1 in the user-facing manifest.
        assert_eq!(priorities["high"], 3);
        assert_eq!(priorities["low"], 2);
        assert_eq!(priorities["section_separator"], 1);
        let visible: Vec<_> = rows.iter().rev().copied().collect();
        assert_eq!(visible[0], "+Section_separator");
        assert_eq!(visible[1], "-Low");
    }
}

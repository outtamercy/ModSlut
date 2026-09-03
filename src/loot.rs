// loot.rs - read LOOT's LOCAL masterlist.yaml. no download, no online
// access: loot already keeps a fresh copy at
//   %LOCALAPPDATA%\LOOT\games\<game>\masterlist.yaml
// users just update their masterlist in loot before running modslut.
//
// we read PLUGIN entries only, and only their hard "after" edges:
//   - name: 'SomePlugin.esp'
//     after:
//       - 'OtherPlugin.esp'
// regex names (loot uses them for wildcards) and conditional edges are
// skipped - a guess is worse than a miss. mapping plugin -> owning mod
// happens in the caller via the census provider map.

use std::collections::HashMap;
use std::path::PathBuf;

pub struct LootData {
    // literal lowercase plugin name -> literal lowercase after-targets
    pub after: HashMap<String, Vec<String>>,
    pub path: PathBuf,
}

fn is_regex_name(n: &str) -> bool {
    n.contains(['*', '?', '(', ')', '[', ']', '\\', '|', '^', '$', '+'])
}

fn unquote(s: &str) -> &str {
    s.trim().trim_matches(|c| c == '\'' || c == '"')
}

// minimal yaml walk for loot's regular structure. entries are
// "  - name: 'x.esp'"; "after:" at 4 spaces; items at 6 spaces, either
// "      - 'y.esp'" or "      - name: 'y.esp'" (+ optional condition lines,
// which make the edge conditional -> we drop the whole entry's edges then).
fn parse_masterlist(text: &str) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    let mut in_plugins = false;
    let mut cur_name: Option<String> = None;
    let mut cur_after: Vec<String> = Vec::new();
    let mut cur_conditional = false;
    let mut in_after = false;

    fn flush(
        out: &mut HashMap<String, Vec<String>>,
        name: &mut Option<String>,
        after: &mut Vec<String>,
        conditional: &mut bool,
    ) {
        if let (Some(n), false) = (name.take(), *conditional) {
            if !after.is_empty() && !is_regex_name(&n) {
                out.entry(n).or_default().extend(after.drain(..));
            } else {
                after.clear();
            }
        } else {
            after.clear();
        }
        *conditional = false;
    }

    for raw in text.lines() {
        let indent = raw.len() - raw.trim_start().len();
        let line = raw.trim();
        if indent == 0 {
            if line.is_empty() || line.starts_with('#') {
                continue; // blanks and banner comments are not keys
            }
            in_plugins = line == "plugins:";
            flush(&mut out, &mut cur_name, &mut cur_after, &mut cur_conditional);
            in_after = false;
            continue;
        }
        if !in_plugins {
            continue;
        }
        if indent == 2 && line.starts_with("- name:") {
            flush(&mut out, &mut cur_name, &mut cur_after, &mut cur_conditional);
            in_after = false;
            let n = unquote(line.trim_start_matches("- name:")).to_lowercase();
            cur_name = Some(n);
            continue;
        }
        if cur_name.is_none() {
            continue;
        }
        if indent == 4 {
            if line.starts_with("condition:") {
                cur_conditional = true; // entry-level condition: skip entirely
                in_after = false;
                continue;
            }
            in_after = line.starts_with("after:");
            if in_after {
                // inline flow style: after: [ 'X.esp', 'Y.esp' ] (group
                // anchors like *mainGroup are not plugins - skip)
                if let Some(l) = line.find('[') {
                    let r = line.rfind(']').unwrap_or(line.len());
                    if r > l {
                        for item in line[l + 1..r].split(',') {
                            let t = unquote(item).to_lowercase();
                            if !t.is_empty() && !t.starts_with('*') && !is_regex_name(&t) {
                                cur_after.push(t);
                            }
                        }
                    }
                }
            }
            continue;
        }
        if in_after && indent >= 6 && line.starts_with("- ") {
            let item = line.trim_start_matches("- ");
            let target = if let Some(rest) = item.strip_prefix("name:") {
                unquote(rest)
            } else {
                unquote(item)
            };
            let t = target.to_lowercase();
            if !t.is_empty() && !is_regex_name(&t) {
                cur_after.push(t);
            }
            continue;
        }
        if in_after && indent >= 8 && line.starts_with("condition:") {
            cur_conditional = true; // conditional edge: drop the entry's edges
        }
    }
    flush(&mut out, &mut cur_name, &mut cur_after, &mut cur_conditional);
    for v in out.values_mut() {
        v.sort();
        v.dedup();
    }
    out
}

// loot's games/ folder name is a SETTING: loot autodetects installs and the
// user can rename the entry, so %LOCALAPPDATA%\LOOT\settings.yaml holds the
// authoritative game-type -> folder mapping:
//   games:
//     - type: SkyrimVR
//       folder: Skyrim VR
// returns folder names for this game's types, most preferred first.
fn folders_from_settings(base: &std::path::Path, game: &crate::game::GameInfo) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(base.join("LOOT").join("settings.yaml")) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    let mut cur_type: Option<String> = None;
    for raw in text.lines() {
        let indent = raw.len() - raw.trim_start().len();
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if indent == 2 && line.starts_with("- ") {
            cur_type = None; // new list item; a type: on the same line is rare
            let rest = line.trim_start_matches("- ");
            if let Some(t) = rest.strip_prefix("type:") {
                cur_type = Some(unquote(t).to_string());
            }
            continue;
        }
        if indent == 2 && line.starts_with("type:") {
            cur_type = Some(unquote(line.trim_start_matches("type:")).to_string());
            continue;
        }
        if indent >= 4 && line.starts_with("type:") {
            cur_type = Some(unquote(line.trim_start_matches("type:")).to_string());
            continue;
        }
        if indent >= 4 && line.starts_with("folder:") {
            if let Some(t) = &cur_type {
                if game.loot_types.iter().any(|want| want.eq_ignore_ascii_case(t)) {
                    let f = unquote(line.trim_start_matches("folder:")).to_string();
                    if !f.is_empty() && !out.contains(&f) {
                        out.push(f);
                    }
                }
            }
        }
    }
    out
}

pub fn load(game: &crate::game::GameInfo) -> Option<LootData> {
    // explicit override for testing / nonstandard installs
    if let Some(p) = std::env::var_os("MODSLUT_LOOT") {
        let p = PathBuf::from(p);
        if p.is_file() {
            let text = std::fs::read_to_string(&p).ok()?;
            return Some(LootData { after: parse_masterlist(&text), path: p });
        }
    }
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
    let games = base.join("LOOT").join("games");
    // 1. loot's own settings: the folder name it autodetected / the user set
    for folder in folders_from_settings(&base, game) {
        let p = games.join(&folder).join("masterlist.yaml");
        if p.is_file() {
            if let Ok(text) = std::fs::read_to_string(&p) {
                return Some(LootData { after: parse_masterlist(&text), path: p });
            }
        }
    }
    // 2. this game's well-known loot folder names, in preference order
    for folder in game.loot_folders {
        let p = games.join(folder).join("masterlist.yaml");
        if p.is_file() {
            if let Ok(text) = std::fs::read_to_string(&p) {
                return Some(LootData { after: parse_masterlist(&text), path: p });
            }
        }
    }
    // 3. renamed / custom game entries: any folder under games/ whose name
    //    matches the game's scan key and that actually has a masterlist
    if let Ok(rd) = std::fs::read_dir(&games) {
        let mut candidates: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.to_lowercase().contains(game.loot_scan_key))
            })
            .collect();
        candidates.sort(); // deterministic
        for dir in candidates {
            let p = dir.join("masterlist.yaml");
            if p.is_file() {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    return Some(LootData { after: parse_masterlist(&text), path: p });
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_mapped_afters() {
        let yaml = "globals: []\nplugins:\n  - name: 'A.esp'\n    after:\n      - 'B.esp'\n      - name: 'C.esp'\n  - name: 'D.esp'\n";
        let m = parse_masterlist(yaml);
        assert_eq!(m.get("a.esp").unwrap(), &vec!["b.esp".to_string(), "c.esp".to_string()]);
        assert!(!m.contains_key("d.esp"));
    }

    #[test]
    fn settings_yaml_maps_game_type_to_folder() {
        let dir = std::env::temp_dir().join(format!("modslut-loot-test-{}", std::process::id()));
        let loot_dir = dir.join("LOOT");
        std::fs::create_dir_all(&loot_dir).unwrap();
        std::fs::write(
            loot_dir.join("settings.yaml"),
            "games:\n  - type: SkyrimVR\n    folder: Skyrim VR\n    path: 'C:\\Games\\SkyrimVR'\n  - type: Fallout4\n    folder: Fallout4\n",
        )
        .unwrap();
        let game = crate::game::info(crate::game::Game::SkyrimSeVr);
        let folders = folders_from_settings(&dir, &game);
        assert_eq!(folders, vec!["Skyrim VR".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn skips_regex_and_conditional() {
        let yaml = "plugins:\n  - name: 'Patch.*\\.esp'\n    after:\n      - 'B.esp'\n  - name: 'E.esp'\n    after:\n      - name: 'F.esp'\n        condition: 'active(\"X.esp\")'\n";
        let m = parse_masterlist(yaml);
        assert!(m.is_empty(), "got {m:?}");
    }
}

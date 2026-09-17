// loot.rs - read LOOT's local masterlist.yaml and its companion metadata.
// LOOT keeps its active copy at
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
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// The same local facts that LOOT exposes in its General Information card.
/// The masterlist is a file download, not a git checkout in modern LOOT, so
/// its blob SHA and update timestamp live beside it in a tiny TOML file.
#[derive(Clone, Debug, Default)]
pub struct MasterlistStatus {
    pub revision: Option<String>,
    pub updated: Option<String>,
    pub source: Option<String>,
}

fn quoted_value(line: &str, key: &str) -> Option<String> {
    let value = line.trim().strip_prefix(key)?.trim_start();
    let value = value.strip_prefix('=').unwrap_or(value).trim();
    Some(value.trim_matches(|c| c == '\'' || c == '"').to_string())
}

/// Read metadata without parsing masterlist YAML again. This also lets the
/// UI tell the user which URL an explicit update will use.
pub fn masterlist_status(masterlist: &Path) -> MasterlistStatus {
    let mut status = MasterlistStatus::default();
    let metadata_path = masterlist.with_file_name("masterlist.yaml.metadata.toml");
    if let Ok(text) = std::fs::read_to_string(metadata_path) {
        for line in text.lines() {
            if status.revision.is_none() {
                status.revision = quoted_value(line, "blob_sha1");
            }
            if status.updated.is_none() {
                status.updated = quoted_value(line, "update_timestamp");
            }
        }
    }
    let Some(folder) = masterlist
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
    else {
        return status;
    };
    let Some(base) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) else {
        return status;
    };
    let Ok(text) = std::fs::read_to_string(base.join("LOOT").join("settings.toml")) else {
        return status;
    };
    let mut matching_folder = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "[[games]]" {
            matching_folder = false;
            continue;
        }
        if let Some(value) = quoted_value(trimmed, "folder") {
            matching_folder = value.eq_ignore_ascii_case(folder);
            continue;
        }
        if matching_folder && status.source.is_none() {
            status.source = quoted_value(trimmed, "masterlistSource");
        }
    }
    status
}

fn today_utc() -> String {
    // Civil-date conversion for the metadata stamp. Keep this dependency-free:
    // MS must not pull a web/date crate merely to write LOOT's YYYY-MM-DD.
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
        / 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += if month <= 2 { 1 } else { 0 };
    format!("{year:04}-{month:02}-{day:02}")
}

fn sha1_from_certutil(path: &Path) -> Result<String, String> {
    let output = Command::new("certutil.exe")
        .args(["-hashfile"])
        .arg(path)
        .arg("SHA1")
        .output()
        .map_err(|error| format!("could not start certutil for masterlist hash: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "certutil could not hash downloaded masterlist: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| {
            line.chars()
                .filter(|c| c.is_ascii_hexdigit())
                .collect::<String>()
        })
        .find(|candidate| candidate.len() == 40)
        .map(|hash| hash.to_ascii_lowercase())
        .ok_or_else(|| "certutil returned no SHA-1 for downloaded masterlist".into())
}

/// Update LOOT's *existing configured* masterlist in place. libloot loads and
/// evaluates metadata, but does not fetch it; this is the small frontend layer
/// LOOT itself supplies. A clicked update always leaves a dated rollback copy.
pub fn update_masterlist(masterlist: &Path, source: &str) -> Result<MasterlistStatus, String> {
    if !source.starts_with("https://") {
        return Err(
            "LOOT masterlist source is not a secure HTTPS URL; update was not attempted".into(),
        );
    }
    let temporary = masterlist.with_file_name("masterlist.yaml.modslut-download");
    let _ = std::fs::remove_file(&temporary);
    let output = Command::new("curl.exe")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
        ])
        .arg(source)
        .arg("--output")
        .arg(&temporary)
        .output()
        .map_err(|error| format!("could not start curl for LOOT masterlist update: {error}"))?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!(
            "masterlist download failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let preview = std::fs::read_to_string(&temporary)
        .map_err(|error| format!("could not read downloaded masterlist: {error}"))?;
    if preview.len() < 512 || !preview.contains("plugins:") {
        let _ = std::fs::remove_file(&temporary);
        return Err("download did not look like a LOOT masterlist; existing file was kept".into());
    }
    let revision = sha1_from_certutil(&temporary)?;
    let backup_suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let backup = masterlist.with_file_name(format!(
        "masterlist.yaml.modslut-backup-{}-{backup_suffix}",
        today_utc()
    ));
    std::fs::copy(masterlist, &backup).map_err(|error| {
        format!(
            "could not create rollback copy {}: {error}",
            backup.display()
        )
    })?;
    std::fs::copy(&temporary, masterlist)
        .map_err(|error| format!("could not replace local masterlist: {error}"))?;
    let _ = std::fs::remove_file(&temporary);
    let metadata = masterlist.with_file_name("masterlist.yaml.metadata.toml");
    std::fs::write(
        metadata,
        format!(
            "blob_sha1 = '{revision}'\nupdate_timestamp = '{}'\n",
            today_utc()
        ),
    )
    .map_err(|error| {
        format!("masterlist updated, but metadata stamp could not be written: {error}")
    })?;
    let mut status = masterlist_status(masterlist);
    status.source = Some(source.to_string());
    Ok(status)
}

pub struct LootData {
    // literal lowercase plugin name -> literal lowercase after-targets
    pub after: HashMap<String, Vec<String>>,
    // literal plugin -> its LOOT group. Group membership is ordering data,
    // not a category: it belongs to the masterlist backbone, never the
    // separator matcher.
    pub groups: HashMap<String, String>,
    // group -> groups it must load after. Kept separately from plugin edges
    // so the caller can build a compact group-rank projection rather than
    // exploding one group rule into thousands of pairwise mod edges.
    pub group_after: HashMap<String, Vec<String>>,
    // Declaration order is a deterministic tiebreaker for unrelated LOOT
    // groups. HashMap iteration is not an ordering policy, obviously.
    pub group_order: Vec<String>,
    pub path: PathBuf,
}

#[derive(Default)]
struct MasterlistRules {
    after: HashMap<String, Vec<String>>,
    groups: HashMap<String, String>,
    group_after: HashMap<String, Vec<String>>,
    group_order: Vec<String>,
}

/// Produce stable low -> high ranks for LOOT groups. Direct group `after`
/// relations win; declaration order handles unrelated groups. Cycles remain
/// deterministic at the end rather than causing a bogus random order.
pub fn group_ranks(data: &LootData) -> HashMap<String, usize> {
    use std::collections::{BTreeSet, HashSet};
    let mut nodes: Vec<String> = data.group_order.clone();
    for group in data
        .groups
        .values()
        .chain(data.group_after.keys())
        .chain(data.group_after.values().flatten())
    {
        if !nodes.contains(group) {
            nodes.push(group.clone());
        }
    }
    if nodes.is_empty() {
        return HashMap::new();
    }
    let original: HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.clone(), i))
        .collect();
    let mut outgoing: HashMap<String, Vec<String>> = HashMap::new();
    let mut indegree: HashMap<String, usize> = nodes.iter().map(|n| (n.clone(), 0usize)).collect();
    let mut seen = HashSet::new();
    for (child, parents) in &data.group_after {
        for parent in parents {
            if !original.contains_key(parent)
                || !original.contains_key(child)
                || parent == child
                || !seen.insert((parent.clone(), child.clone()))
            {
                continue;
            }
            outgoing
                .entry(parent.clone())
                .or_default()
                .push(child.clone());
            *indegree.get_mut(child).expect("known group") += 1;
        }
    }
    let mut ready: BTreeSet<(usize, String)> = indegree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(n, _)| (original[n], n.clone()))
        .collect();
    let mut ordered = Vec::with_capacity(nodes.len());
    while let Some((_, name)) = ready.pop_first() {
        ordered.push(name.clone());
        for child in outgoing.get(&name).into_iter().flatten() {
            let degree = indegree.get_mut(child).expect("known group");
            *degree -= 1;
            if *degree == 0 {
                ready.insert((original[child], child.clone()));
            }
        }
    }
    for node in nodes {
        if !ordered.contains(&node) {
            ordered.push(node);
        }
    }
    ordered
        .into_iter()
        .enumerate()
        .map(|(i, group)| (group, i))
        .collect()
}

fn is_regex_name(n: &str) -> bool {
    n.contains(['*', '?', '(', ')', '[', ']', '\\', '|', '^', '$', '+'])
}

fn unquote(s: &str) -> &str {
    s.trim().trim_matches(|c| c == '\'' || c == '"')
}

// A deliberately small YAML walker for LOOT's stable masterlist shape. We
// read literal plugin `after` / `before` rules plus group membership and
// group `after` rules. Regex and conditional rules stay out: a missed edge is
// safer than pretending a condition happened to be true.
fn parse_masterlist(text: &str) -> MasterlistRules {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Scope {
        Other,
        Groups,
        Plugins,
    }
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum List {
        None,
        GroupAfter,
        PluginAfter,
        PluginBefore,
    }

    fn targets(line: &str, out: &mut Vec<String>) {
        let items: Vec<&str> = if let Some(left) = line.find('[') {
            let right = line.rfind(']').unwrap_or(line.len());
            line[left + 1..right].split(',').collect()
        } else if let Some(item) = line.strip_prefix("- ") {
            vec![item]
        } else {
            vec![]
        };
        for item in items {
            let item = item.trim();
            let item = item.strip_prefix("name:").unwrap_or(item);
            let target = unquote(item).to_ascii_lowercase();
            if !target.is_empty() && !target.starts_with('*') && !is_regex_name(&target) {
                out.push(target);
            }
        }
    }

    // LOOT names groups with YAML anchors (`&coreGroup Core Mods`) and
    // references them through aliases (`*coreGroup`). These are not plugin
    // wildcards: resolve them to their human group names before building the
    // group graph.
    fn parse_group_name(value: &str, aliases: &mut HashMap<String, String>) -> String {
        let value = unquote(value).trim();
        if let Some(rest) = value.strip_prefix('&') {
            if let Some((anchor, display)) = rest.split_once(char::is_whitespace) {
                let display = display.trim().to_ascii_lowercase();
                aliases.insert(anchor.to_ascii_lowercase(), display.clone());
                return display;
            }
        }
        value.to_ascii_lowercase()
    }

    fn group_targets(line: &str, aliases: &HashMap<String, String>, out: &mut Vec<String>) {
        let items: Vec<&str> = if let Some(left) = line.find('[') {
            let right = line.rfind(']').unwrap_or(line.len());
            line[left + 1..right].split(',').collect()
        } else if let Some(item) = line.strip_prefix("- ") {
            vec![item]
        } else {
            vec![]
        };
        for item in items {
            let item = item.trim();
            let item = item.strip_prefix("name:").unwrap_or(item);
            let item = unquote(item).trim().to_ascii_lowercase();
            if let Some(alias) = item.strip_prefix('*') {
                if let Some(group) = aliases.get(alias) {
                    out.push(group.clone());
                }
            } else if !item.is_empty() {
                out.push(item);
            }
        }
    }

    fn flush_group(
        rules: &mut MasterlistRules,
        name: &mut Option<String>,
        after: &mut Vec<String>,
        conditional: &mut bool,
    ) {
        if let (Some(name), false) = (name.take(), *conditional) {
            if !after.is_empty() {
                let row = rules.group_after.entry(name).or_default();
                row.append(after);
            }
        }
        after.clear();
        *conditional = false;
    }

    fn flush_plugin(
        rules: &mut MasterlistRules,
        name: &mut Option<String>,
        group: &mut Option<String>,
        after: &mut Vec<String>,
        before: &mut Vec<String>,
        conditional: &mut bool,
    ) {
        if let (Some(name), false) = (name.take(), *conditional) {
            if !is_regex_name(&name) {
                if let Some(group) = group.take() {
                    rules.groups.insert(name.clone(), group);
                }
                if !after.is_empty() {
                    rules.after.entry(name.clone()).or_default().append(after);
                }
                for target in before.drain(..) {
                    rules.after.entry(target).or_default().push(name.clone());
                }
            }
        }
        group.take();
        after.clear();
        before.clear();
        *conditional = false;
    }

    let mut rules = MasterlistRules::default();
    let mut scope = Scope::Other;
    let mut list = List::None;
    let mut group_name = None;
    let mut group_after = Vec::new();
    let mut group_conditional = false;
    let mut plugin_name = None;
    let mut plugin_group = None;
    let mut plugin_after = Vec::new();
    let mut plugin_before = Vec::new();
    let mut plugin_conditional = false;
    let mut group_aliases: HashMap<String, String> = HashMap::new();

    for raw in text.lines() {
        let indent = raw.len() - raw.trim_start().len();
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if indent == 0 {
            flush_group(
                &mut rules,
                &mut group_name,
                &mut group_after,
                &mut group_conditional,
            );
            flush_plugin(
                &mut rules,
                &mut plugin_name,
                &mut plugin_group,
                &mut plugin_after,
                &mut plugin_before,
                &mut plugin_conditional,
            );
            scope = match line {
                "groups:" => Scope::Groups,
                "plugins:" => Scope::Plugins,
                _ => Scope::Other,
            };
            list = List::None;
            continue;
        }
        match scope {
            Scope::Groups if indent == 2 && line.starts_with("- name:") => {
                flush_group(
                    &mut rules,
                    &mut group_name,
                    &mut group_after,
                    &mut group_conditional,
                );
                let parsed =
                    parse_group_name(line.trim_start_matches("- name:"), &mut group_aliases);
                if !rules.group_order.contains(&parsed) {
                    rules.group_order.push(parsed.clone());
                }
                group_name = Some(parsed);
                list = List::None;
            }
            Scope::Groups if group_name.is_some() && indent == 4 => {
                if line.starts_with("condition:") {
                    group_conditional = true;
                    list = List::None;
                } else if line.starts_with("after:") {
                    list = List::GroupAfter;
                    group_targets(line, &group_aliases, &mut group_after);
                } else {
                    list = List::None;
                }
            }
            Scope::Groups if list == List::GroupAfter && indent >= 6 && line.starts_with("- ") => {
                group_targets(line, &group_aliases, &mut group_after)
            }
            Scope::Groups
                if list == List::GroupAfter && indent >= 8 && line.starts_with("condition:") =>
            {
                group_conditional = true
            }

            Scope::Plugins if indent == 2 && line.starts_with("- name:") => {
                flush_plugin(
                    &mut rules,
                    &mut plugin_name,
                    &mut plugin_group,
                    &mut plugin_after,
                    &mut plugin_before,
                    &mut plugin_conditional,
                );
                plugin_name =
                    Some(unquote(line.trim_start_matches("- name:")).to_ascii_lowercase());
                list = List::None;
            }
            Scope::Plugins if plugin_name.is_some() && indent == 4 => {
                if line.starts_with("condition:") {
                    plugin_conditional = true;
                    list = List::None;
                } else if let Some(value) = line.strip_prefix("group:") {
                    let raw = unquote(value).trim().to_ascii_lowercase();
                    plugin_group = raw
                        .strip_prefix('*')
                        .and_then(|alias| group_aliases.get(alias).cloned())
                        .or_else(|| (!raw.is_empty()).then_some(raw));
                    list = List::None;
                } else if line.starts_with("after:") {
                    list = List::PluginAfter;
                    targets(line, &mut plugin_after);
                } else if line.starts_with("before:") {
                    list = List::PluginBefore;
                    targets(line, &mut plugin_before);
                } else {
                    list = List::None;
                }
            }
            Scope::Plugins
                if list == List::PluginAfter && indent >= 6 && line.starts_with("- ") =>
            {
                targets(line, &mut plugin_after)
            }
            Scope::Plugins
                if list == List::PluginBefore && indent >= 6 && line.starts_with("- ") =>
            {
                targets(line, &mut plugin_before)
            }
            Scope::Plugins
                if matches!(list, List::PluginAfter | List::PluginBefore)
                    && indent >= 8
                    && line.starts_with("condition:") =>
            {
                plugin_conditional = true
            }
            _ => {}
        }
    }
    flush_group(
        &mut rules,
        &mut group_name,
        &mut group_after,
        &mut group_conditional,
    );
    flush_plugin(
        &mut rules,
        &mut plugin_name,
        &mut plugin_group,
        &mut plugin_after,
        &mut plugin_before,
        &mut plugin_conditional,
    );
    for edges in rules.after.values_mut() {
        edges.sort();
        edges.dedup();
    }
    for edges in rules.group_after.values_mut() {
        edges.sort();
        edges.dedup();
    }
    rules
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
                if game
                    .loot_types
                    .iter()
                    .any(|want| want.eq_ignore_ascii_case(t))
                {
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
            let rules = parse_masterlist(&text);
            return Some(LootData {
                after: rules.after,
                groups: rules.groups,
                group_after: rules.group_after,
                group_order: rules.group_order,
                path: p,
            });
        }
    }
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
    let games = base.join("LOOT").join("games");
    // 1. loot's own settings: the folder name it autodetected / the user set
    for folder in folders_from_settings(&base, game) {
        let p = games.join(&folder).join("masterlist.yaml");
        if p.is_file() {
            if let Ok(text) = std::fs::read_to_string(&p) {
                let rules = parse_masterlist(&text);
                return Some(LootData {
                    after: rules.after,
                    groups: rules.groups,
                    group_after: rules.group_after,
                    group_order: rules.group_order,
                    path: p,
                });
            }
        }
    }
    // 2. this game's well-known loot folder names, in preference order
    for folder in game.loot_folders {
        let p = games.join(folder).join("masterlist.yaml");
        if p.is_file() {
            if let Ok(text) = std::fs::read_to_string(&p) {
                let rules = parse_masterlist(&text);
                return Some(LootData {
                    after: rules.after,
                    groups: rules.groups,
                    group_after: rules.group_after,
                    group_order: rules.group_order,
                    path: p,
                });
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
                    let rules = parse_masterlist(&text);
                    return Some(LootData {
                        after: rules.after,
                        groups: rules.groups,
                        group_after: rules.group_after,
                        group_order: rules.group_order,
                        path: p,
                    });
                }
            }
        }
    }
    None
}

/// LOOT stores the actual game install alongside the folder name in
/// settings.yaml. The folder is user-configurable, so this is a safer source
/// than trying to guess from a Steam library or an MO2 executable path.
///
/// This is deliberately optional: MS can still use its lightweight local
/// masterlist reader when LOOT has not recorded a usable install path.
pub fn game_install_path(game: &crate::game::GameInfo) -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
    // LOOT 0.25 stores configured games in settings.toml. Earlier ModSlut
    // mistakenly looked for the retired YAML layout, which made the GUI say
    // no games were installed even when LOOT was configured for many.
    if let Ok(text) = std::fs::read_to_string(base.join("LOOT").join("settings.toml")) {
        for block in text.split("[[games]]").skip(1) {
            let mut game_id = None;
            let mut folder = None;
            let mut path = None;
            for raw in block.lines() {
                let line = raw.trim();
                let Some((key, value)) = line.split_once('=') else {
                    continue;
                };
                let value = value.trim().trim_matches(|c| c == '\'' || c == '"');
                match key.trim() {
                    "gameId" => game_id = Some(value.to_string()),
                    "folder" => folder = Some(value.to_string()),
                    "path" => path = Some(PathBuf::from(value)),
                    _ => {}
                }
            }
            let matches = game_id.as_deref().is_some_and(|id| {
                game.loot_types
                    .iter()
                    .any(|want| want.eq_ignore_ascii_case(id))
            }) || folder.as_deref().is_some_and(|value| {
                game.loot_folders
                    .iter()
                    .any(|want| want.eq_ignore_ascii_case(value))
            });
            if matches {
                if let Some(path) = path.filter(|path| path.is_dir()) {
                    return Some(path);
                }
            }
        }
    }
    // Retain the older YAML reader for old LOOT installations.
    let text = std::fs::read_to_string(base.join("LOOT").join("settings.yaml")).ok()?;
    let mut current_type: Option<String> = None;
    for raw in text.lines() {
        let indent = raw.len() - raw.trim_start().len();
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if indent == 2 && line.starts_with("- ") {
            current_type = None;
            let rest = line.trim_start_matches("- ");
            if let Some(value) = rest.strip_prefix("type:") {
                current_type = Some(unquote(value).to_string());
            }
            continue;
        }
        if indent >= 2 && line.starts_with("type:") {
            current_type = Some(unquote(line.trim_start_matches("type:")).to_string());
            continue;
        }
        if indent >= 4 && line.starts_with("path:") {
            let matches_game = current_type.as_ref().is_some_and(|kind| {
                game.loot_types
                    .iter()
                    .any(|want| want.eq_ignore_ascii_case(kind))
            });
            if matches_game {
                let path = PathBuf::from(unquote(line.trim_start_matches("path:")).trim());
                if path.is_dir() {
                    return Some(path);
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
        assert_eq!(
            m.after.get("a.esp").unwrap(),
            &vec!["b.esp".to_string(), "c.esp".to_string()]
        );
        assert!(!m.after.contains_key("d.esp"));
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
        let game = crate::game::info(crate::game::Game::SkyrimVr);
        let folders = folders_from_settings(&dir, &game);
        assert_eq!(folders, vec!["Skyrim VR".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn skips_regex_and_conditional() {
        let yaml = "plugins:\n  - name: 'Patch.*\\.esp'\n    after:\n      - 'B.esp'\n  - name: 'E.esp'\n    after:\n      - name: 'F.esp'\n        condition: 'active(\"X.esp\")'\n";
        let m = parse_masterlist(yaml);
        assert!(m.after.is_empty(), "got {:?}", m.after);
    }

    #[test]
    fn parses_before_and_group_metadata_without_turning_groups_into_plugins() {
        let yaml = "groups:\n  - name: &defaultGroup default\n  - name: &lateGroup late\n    after: [*defaultGroup]\nplugins:\n  - name: 'Base.esp'\n    group: *defaultGroup\n  - name: 'Patch.esp'\n    group: *lateGroup\n    before: ['Later.esp']\n";
        let m = parse_masterlist(yaml);
        assert_eq!(m.groups.get("base.esp"), Some(&"default".to_string()));
        assert_eq!(m.groups.get("patch.esp"), Some(&"late".to_string()));
        assert_eq!(
            m.group_after.get("late"),
            Some(&vec!["default".to_string()])
        );
        assert_eq!(
            m.after.get("later.esp"),
            Some(&vec!["patch.esp".to_string()])
        );
    }

    #[test]
    fn group_ranks_follow_after_edges_and_keep_declaration_ties_stable() {
        let data = LootData {
            after: HashMap::new(),
            groups: HashMap::new(),
            group_after: HashMap::from([("late".into(), vec!["base".into()])]),
            group_order: vec!["base".into(), "side".into(), "late".into()],
            path: PathBuf::new(),
        };
        let ranks = group_ranks(&data);
        assert!(ranks["base"] < ranks["late"]);
        assert!(ranks["base"] < ranks["side"]);
    }
}

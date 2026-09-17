// libloot bridge - the real LOOT engine is compare-only for now.
//
// MS owns the MO2 left-pane roadmap. libloot owns plugin metadata, conditions
// and plugin sorting. Keeping that boundary explicit means a bad or incomplete
// virtual filesystem never gets to rearrange someone's folders.

use crate::game::{self, Game as MsGame};
use crate::plugins::PluginOwner;
use libloot::metadata::{select_message_content, Group, MessageType, PluginMetadata};
use libloot::{EvalMode, Game, GameType, MergeMode};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub struct Comparison {
    pub game_name: String,
    pub game_path_source: String,
    pub plugin_count: usize,
    pub active_full_plugins: usize,
    pub active_light_plugins: usize,
    pub dirty_plugins: usize,
    pub warning_count: usize,
    pub error_count: usize,
    pub total_messages: usize,
    pub general_messages: Vec<LootMessage>,
    // Evaluated LOOT Bash-tag suggestions projected from plugin files back to
    // their owning MO2 folders. They are display evidence only.
    pub bash_tags_by_owner: HashMap<String, Vec<String>>,
    // Actual LOOT load indexes by plugin filename. The GUI only displays an
    // index beside an MO2 folder when it owns one or more of these plugins;
    // pluginless folders and separators retain their meaningful MO2 Position.
    pub plugin_indexes: HashMap<String, String>,
    pub changed_positions: usize,
    pub masterlist: PathBuf,
    pub masterlist_status: crate::loot::MasterlistStatus,
    // libloot's solved plugin order, low -> high. MS projects these ranks
    // back to owning MO2 mods; it never writes plugins.txt from this path.
    pub sorted_plugins: Vec<String>,
    // Read by libloot while it loads the lightweight TES4 headers needed for
    // sorting anyway. This is deliberately not MS record extraction: it lets
    // the left-pane projection keep a patch after every declared master.
    pub plugin_masters: HashMap<String, Vec<String>>,
    // The separator graph is rebuilt in memory for every solve.  These
    // numbers make it obvious in the log that MS gave libloot the MO2
    // structure rather than merely reading libloot's final order back.
    pub separator_groups: usize,
    pub separator_memberships: usize,
    pub retained_loot_groups: usize,
}

#[derive(Clone, Debug)]
pub struct LootMessage {
    pub level: LootMessageLevel,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LootMessageLevel {
    Info,
    Warning,
    Error,
}

fn message_level(level: MessageType) -> LootMessageLevel {
    match level {
        MessageType::Say => LootMessageLevel::Info,
        MessageType::Warn => LootMessageLevel::Warning,
        MessageType::Error => LootMessageLevel::Error,
    }
}

fn visible_message(
    level: MessageType,
    content: &[libloot::metadata::MessageContent],
) -> Option<LootMessage> {
    let text = select_message_content(content, "en")?.text().trim();
    (!text.is_empty()).then(|| LootMessage {
        level: message_level(level),
        text: text.to_string(),
    })
}

#[derive(Default)]
struct SeparatorGraph {
    groups: Vec<Group>,
    group_for_section: HashMap<String, String>,
}

fn section_key(label: &str) -> String {
    label
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_terminal_section(label: &str) -> bool {
    matches!(section_key(label).as_str(), "endoflist" | "eol")
}

// Build the spine from the *actual* separator order: MO2's top row is the
// low-priority end, so every later separator loads after its predecessor.
// The group names are private, stable-for-this-solve IDs; the descriptions
// preserve the human separator labels shown in the MS graph editor.
fn separator_graph(labels: &[String], extra_edges: &[(String, String)]) -> SeparatorGraph {
    let mut graph = SeparatorGraph::default();
    for label in labels {
        let key = section_key(label);
        if key.is_empty() || graph.group_for_section.contains_key(&key) {
            continue;
        }
        let group_name = format!("modslut-separator-{:03}", graph.groups.len());
        graph.group_for_section.insert(key, group_name);
    }
    for (index, label) in labels.iter().enumerate() {
        let key = section_key(label);
        let Some(group_name) = graph.group_for_section.get(&key).cloned() else {
            continue;
        };
        // A duplicate separator label was intentionally ignored above, so it
        // must not manufacture a second group node here.
        if graph.groups.iter().any(|group| group.name() == group_name) {
            continue;
        }
        let mut after = index
            .checked_sub(1)
            .and_then(|previous| graph.group_for_section.get(&section_key(&labels[previous])))
            .cloned()
            .into_iter()
            .collect::<Vec<_>>();
        for (later, earlier) in extra_edges {
            if section_key(later) != key {
                continue;
            }
            if let Some(edge) = graph.group_for_section.get(&section_key(earlier)) {
                if !after.contains(edge) {
                    after.push(edge.clone());
                }
            }
        }
        graph.groups.push(
            Group::new(group_name.clone())
                .with_description(label.clone())
                .with_after_groups(after),
        );
    }
    graph
}

fn mo2_game_path(modlist: &Path) -> Option<PathBuf> {
    // MS is launched by MO2, so ModOrganizer.ini is the right fallback when
    // LOOT's own settings file was cleaned up, moved, or simply never made.
    let mut dir = modlist.parent();
    for _ in 0..6 {
        let here = dir?;
        let ini = here.join("ModOrganizer.ini");
        if let Ok(text) = std::fs::read_to_string(&ini) {
            for line in text.lines() {
                let line = line.trim();
                let Some(value) = line.strip_prefix("gamePath") else {
                    continue;
                };
                let value = value.trim_start_matches(['=', ' ']).trim();
                let value = value
                    .strip_prefix("@ByteArray(")
                    .and_then(|v| v.strip_suffix(')'))
                    .unwrap_or(value)
                    .trim_matches('"')
                    .replace("\\\\", "\\");
                let path = PathBuf::from(value);
                if path.is_dir() {
                    return Some(path);
                }
            }
        }
        dir = here.parent();
    }
    None
}

fn game_type(info: &game::GameInfo) -> Option<GameType> {
    match info.game {
        MsGame::Skyrim => Some(GameType::Skyrim),
        MsGame::SkyrimSe => Some(GameType::SkyrimSE),
        MsGame::SkyrimVr => Some(GameType::SkyrimVR),
        MsGame::Fallout4 => Some(GameType::Fallout4),
        MsGame::Fallout4Vr => Some(GameType::Fallout4VR),
        MsGame::Starfield => Some(GameType::Starfield),
        MsGame::Oblivion => Some(GameType::Oblivion),
        MsGame::OblivionRemastered => Some(GameType::OblivionRemastered),
        MsGame::Fallout3 => Some(GameType::Fallout3),
        MsGame::FalloutNv => Some(GameType::FalloutNV),
        MsGame::Morrowind => Some(GameType::Morrowind),
        MsGame::OpenMw => Some(GameType::OpenMW),
        // Enderal uses SkyrimSE's plugin format and LOOT metadata.
        MsGame::Enderal => Some(GameType::SkyrimSE),
        MsGame::Unknown => None,
    }
}

fn game_data_path(kind: GameType, game_path: &Path) -> PathBuf {
    match kind {
        GameType::Morrowind => game_path.join("Data Files"),
        GameType::OpenMW => game_path.join("resources").join("vfs"),
        GameType::OblivionRemastered => game_path
            .join("OblivionRemastered")
            .join("Content")
            .join("Dev")
            .join("ObvData")
            .join("Data"),
        _ => game_path.join("Data"),
    }
}

fn plugin_paths(
    kind: GameType,
    game_path: &Path,
    mods_dir: &Path,
    owners: &[PluginOwner],
    current_order: &[String],
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    // MO2's later folders win. Build a unique filename -> physical file map in
    // that order, after the game's Data directory, just like the virtual pane.
    let mut found: HashMap<String, PathBuf> = HashMap::new();
    let data = game_data_path(kind, game_path);
    if let Ok(entries) = std::fs::read_dir(&data) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| {
                matches!(
                    e.to_string_lossy().to_ascii_lowercase().as_str(),
                    "esp" | "esm" | "esl"
                )
            }) {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    found.insert(name.to_ascii_lowercase(), path);
                }
            }
        }
    }

    let mut data_paths = Vec::new();
    for owner in owners {
        let root = mods_dir.join(&owner.mod_name);
        if root.is_dir() {
            if !data_paths.contains(&root) {
                data_paths.push(root.clone());
            }
            // Most MO2 mod folders put plugins at the root. The filename is
            // the only thing libloot needs here; nested plugin layouts are
            // uncommon and will simply be absent from this first compare pass.
            if owner.path.is_file() {
                found.insert(owner.plugin.to_ascii_lowercase(), owner.path.clone());
            }
        }
    }
    // libloot sorts a live plugin load order. Give it exactly plugins.txt,
    // not every dormant FOMOD option / old patch / alternative master sitting
    // in MO2. Those disabled files still get MS's masterlist metadata pass;
    // they simply aren't valid input to LOOT's executable-order solver.
    let mut ordered = Vec::new();
    let mut seen = HashSet::new();
    for name in current_order {
        let key = name.to_ascii_lowercase();
        if let Some(path) = found.get(&key) {
            if seen.insert(key) {
                ordered.push(path.clone());
            }
        }
    }
    (ordered, data_paths)
}

/// Ask libloot for its plugin order without applying it anywhere. This is an
/// evidence feed for MS's own left-pane projection, not a second mod sorter.
pub fn compare(
    modlist: &Path,
    mods_dir: &Path,
    owners: &[PluginOwner],
    current_order: &[String],
    separator_labels: &[String],
    separator_edges: &[(String, String)],
    default_game: Option<MsGame>,
) -> Result<Comparison, String> {
    // libloot is a big dependency with its own sort graph. A bad masterlist
    // edge must degrade to a diagnostic, never kill the MS preview window.
    std::panic::catch_unwind(|| {
        compare_inner(
            modlist,
            mods_dir,
            owners,
            current_order,
            separator_labels,
            separator_edges,
            default_game,
        )
    })
    .map_err(|panic| {
        let message = panic
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic".to_string());
        format!("libloot panic contained: {message}")
    })?
}

fn compare_inner(
    modlist: &Path,
    mods_dir: &Path,
    owners: &[PluginOwner],
    current_order: &[String],
    separator_labels: &[String],
    separator_edges: &[(String, String)],
    default_game: Option<MsGame>,
) -> Result<Comparison, String> {
    let detected = game::detect_with_default(
        &owners
            .iter()
            .map(|owner| owner.plugin.clone())
            .collect::<Vec<_>>(),
        default_game,
    );
    let Some(kind) = game_type(&detected) else {
        return Err("unsupported game - libloot compare skipped".into());
    };
    let (game_path, game_path_source) = if let Some(path) =
        crate::loot::game_install_path(&detected)
    {
        (path, "LOOT settings".to_string())
    } else if let Some(path) = mo2_game_path(modlist) {
        (path, "MO2 ModOrganizer.ini".to_string())
    } else {
        return Err("no usable game install path in LOOT or MO2 - libloot compare skipped".into());
    };
    let Some(masterlist) = crate::loot::load(&detected).map(|data| data.path) else {
        return Err("no local LOOT masterlist - libloot compare skipped".into());
    };
    let (paths, additional_data_paths) =
        plugin_paths(kind, &game_path, mods_dir, owners, current_order);
    if paths.is_empty() {
        return Err(
            "no active physical plugin files resolved from MO2 - libloot compare skipped".into(),
        );
    }
    let mut game = Game::new(kind, &game_path).map_err(|e| format!("game handle: {e}"))?;
    game.set_additional_data_paths(additional_data_paths)
        .map_err(|e| format!("MO2 virtual data paths: {e}"))?;
    {
        let db = game.database();
        db.write()
            .map_err(|e| format!("metadata lock: {e}"))?
            .load_masterlist(&masterlist)
            .map_err(|e| format!("masterlist: {e}"))?;
    }
    // Separators are the MO2-side group graph. Build their chain afresh on
    // every sort so it follows the list if a user inserts, removes, or moves
    // a separator. This lives only in this Game handle: neither LOOT's
    // masterlist nor its userlist is edited.
    let graph = separator_graph(separator_labels, separator_edges);
    let active: HashSet<String> = paths
        .iter()
        .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
        .map(|name| name.to_ascii_lowercase())
        .collect();
    let mut winner = HashMap::<String, &PluginOwner>::new();
    for owner in owners {
        winner.insert(owner.plugin.to_ascii_lowercase(), owner);
    }
    let mut memberships = 0usize;
    let mut retained_loot_groups = 0usize;
    {
        let db = game.database();
        let mut db = db.write().map_err(|e| format!("metadata lock: {e}"))?;
        db.set_user_groups(graph.groups.clone());
        for plugin in &active {
            let Some(owner) = winner.get(plugin) else {
                continue;
            };
            // End of List is deliberately not a LOOT group membership. It is
            // a terminal MO2 holding area, never an instruction that a plugin
            // deserves to stay at the end forever.
            if is_terminal_section(&owner.section) {
                continue;
            }
            let Some(group) = graph.group_for_section.get(&section_key(&owner.section)) else {
                continue;
            };
            // A non-default masterlist group is LOOT's own stronger semantic
            // knowledge. Keep it intact; MS adds separator memberships where
            // LOOT has no group rather than erasing the upstream graph.
            let has_loot_group = db
                .plugin_metadata(
                    &owner.plugin,
                    MergeMode::WithoutUserMetadata,
                    EvalMode::Evaluate,
                )
                .map_err(|e| format!("masterlist metadata for {}: {e}", owner.plugin))?
                .and_then(|metadata| metadata.group().map(str::to_string))
                .is_some_and(|existing| !existing.eq_ignore_ascii_case(Group::DEFAULT_NAME));
            if has_loot_group {
                retained_loot_groups += 1;
                continue;
            }
            let mut metadata = PluginMetadata::new(&owner.plugin)
                .map_err(|e| format!("separator metadata for {}: {e}", owner.plugin))?;
            metadata.set_group(group.clone());
            db.set_plugin_user_metadata(metadata);
            memberships += 1;
        }
    }
    let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    game.load_plugin_headers(&refs)
        .map_err(|e| format!("plugin headers: {e}"))?;
    let plugin_masters = game
        .loaded_plugins()
        .into_iter()
        .map(|plugin| {
            let name = plugin.name().to_ascii_lowercase();
            let masters = plugin
                .masters()
                .unwrap_or_default()
                .into_iter()
                .map(|master| master.to_ascii_lowercase())
                .collect();
            (name, masters)
        })
        .collect();
    let loaded_plugins = game.loaded_plugins();
    let active_light_plugins = loaded_plugins
        .iter()
        .filter(|plugin| plugin.is_light_plugin())
        .count();
    let active_full_plugins = loaded_plugins.len().saturating_sub(active_light_plugins);
    // This is LOOT metadata as evaluated by libloot, not a home-grown scan.
    // It gives the General Information card the same warning/error/dirty
    // facts that will matter when MS writes plugin order later.
    let (
        dirty_plugins,
        warning_count,
        error_count,
        total_messages,
        general_messages,
        bash_tags_by_owner,
    ) = {
        let db_handle = game.database();
        let db = db_handle
            .read()
            .map_err(|e| format!("metadata lock: {e}"))?;
        let mut dirty = 0usize;
        let mut warnings = 0usize;
        let mut errors = 0usize;
        let mut total = 0usize;
        let mut bash_tags_by_owner = HashMap::<String, Vec<String>>::new();
        for plugin in &loaded_plugins {
            let Some(metadata) = db
                .plugin_metadata(
                    plugin.name(),
                    MergeMode::WithUserMetadata,
                    EvalMode::Evaluate,
                )
                .map_err(|e| format!("masterlist facts for {}: {e}", plugin.name()))?
            else {
                continue;
            };
            if !metadata.dirty_info().is_empty() {
                dirty += 1;
            }
            for message in metadata.messages() {
                total += 1;
                match message.message_type() {
                    MessageType::Warn => warnings += 1,
                    MessageType::Error => errors += 1,
                    MessageType::Say => {}
                }
            }
        }
        // Plugin headers can carry plain Bash tags, while the masterlist
        // contributes conditional Add/Remove suggestions. Show both under
        // their owning MO2 folder without letting either route a mod.
        for owner in owners {
            let mut tags: Vec<String> = game
                .plugin(&owner.plugin)
                .map(|plugin| {
                    plugin
                        .bash_tags()
                        .iter()
                        .map(|tag| format!("Header: {tag}"))
                        .collect()
                })
                .unwrap_or_default();
            if let Some(metadata) = db
                .plugin_metadata(
                    &owner.plugin,
                    MergeMode::WithUserMetadata,
                    EvalMode::Evaluate,
                )
                .map_err(|e| format!("Bash tags for {}: {e}", owner.plugin))?
            {
                tags.extend(metadata.tags().iter().map(|tag| {
                    format!(
                        "{} {}",
                        if tag.is_addition() { "Add" } else { "Remove" },
                        tag.name()
                    )
                }));
            }
            tags.sort();
            tags.dedup();
            if !tags.is_empty() {
                bash_tags_by_owner
                    .entry(owner.mod_name.clone())
                    .or_default()
                    .extend(tags);
            }
        }
        for tags in bash_tags_by_owner.values_mut() {
            tags.sort();
            tags.dedup();
        }
        let mut cards = Vec::new();
        for message in db
            .general_messages(MergeMode::WithUserMetadata, EvalMode::Evaluate)
            .map_err(|e| format!("general LOOT messages: {e}"))?
        {
            total += 1;
            match message.message_type() {
                MessageType::Warn => warnings += 1,
                MessageType::Error => errors += 1,
                MessageType::Say => {}
            }
            if let Some(card) = visible_message(message.message_type(), message.content()) {
                cards.push(card);
            }
        }
        (dirty, warnings, errors, total, cards, bash_tags_by_owner)
    };
    let input: Vec<&str> = paths
        .iter()
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
        .collect();
    let sorted = game
        .sort_plugins(&input)
        .map_err(|e| format!("sort: {e}"))?;
    let changed_positions = input
        .iter()
        .zip(sorted.iter())
        .filter(|(before, after)| !before.eq_ignore_ascii_case(after))
        .count();
    let masterlist_status = crate::loot::masterlist_status(&masterlist);
    let light_plugins: HashSet<String> = loaded_plugins
        .iter()
        .filter(|plugin| plugin.is_light_plugin())
        .map(|plugin| plugin.name().to_ascii_lowercase())
        .collect();
    let mut plugin_indexes = HashMap::new();
    let mut full_index = 0usize;
    let mut light_index = 0usize;
    for plugin in &sorted {
        let key = plugin.to_ascii_lowercase();
        let index = if light_plugins.contains(&key) {
            let value = format!("FE {light_index:03X}");
            light_index += 1;
            value
        } else {
            let value = format!("{full_index:02X}");
            full_index += 1;
            value
        };
        plugin_indexes.insert(key, index);
    }
    Ok(Comparison {
        game_name: kind.to_string(),
        game_path_source,
        plugin_count: sorted.len(),
        active_full_plugins,
        active_light_plugins,
        dirty_plugins,
        warning_count,
        error_count,
        total_messages,
        general_messages,
        bash_tags_by_owner,
        plugin_indexes,
        changed_positions,
        masterlist,
        masterlist_status,
        sorted_plugins: sorted
            .into_iter()
            .map(|name| name.to_ascii_lowercase())
            .collect(),
        plugin_masters,
        separator_groups: graph.groups.len(),
        separator_memberships: memberships,
        retained_loot_groups,
    })
}

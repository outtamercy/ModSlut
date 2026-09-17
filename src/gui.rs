// modslut gui - LOOT-flavored dark ui, built to be launched from inside MO2.
// finds the active profile's modlist.txt on its own, previews changes,
// and only writes when "apply changes" is clicked. mo2 refreshes on exit.

use crate::conflicts::ConflictIndex;
use crate::metadata::{HumanMetadata, Kind as HumanMetadataKind};
use crate::{
    active_mods, category_report, game, load_rules_for, parse, run, serialize, user_rule_files,
    Categories, Change, ChangeKind, Modlist,
};
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::Instant;

// one conflict scan at a time, process-wide. ConflictIndex::build walks
// every active mod's whole file tree and holds every relative path in
// memory - with a big list that's a few hundred MB per scan. concurrent
// scans (a reload per right-click pin while the ini is stale) freeze the
// machine, so reloads join a wait instead of stacking scans.
static SCAN_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

// ---- loot-ish palette ----
const BG: egui::Color32 = egui::Color32::from_rgb(0x2b, 0x2b, 0x2b);
const PANEL: egui::Color32 = egui::Color32::from_rgb(0x33, 0x33, 0x33);
const TOOLBAR: egui::Color32 = egui::Color32::from_rgb(0x22, 0x22, 0x22);
const MOVE_CLR: egui::Color32 = egui::Color32::from_rgb(0xf0, 0xc0, 0x60); // yellow
const REOR_CLR: egui::Color32 = egui::Color32::from_rgb(0x6c, 0xb6, 0xff); // blue
const PROM_CLR: egui::Color32 = egui::Color32::from_rgb(0x7f, 0xd6, 0x7f); // green
const SINK_CLR: egui::Color32 = egui::Color32::from_rgb(0xff, 0x9f, 0x6c); // orange
const FLOT_CLR: egui::Color32 = egui::Color32::from_rgb(0xd6, 0x9f, 0xff); // purple
const WARN_CLR: egui::Color32 = egui::Color32::from_rgb(0xff, 0x5f, 0x5f); // red
const RENAME_CLR: egui::Color32 = egui::Color32::from_rgb(0x6c, 0xe0, 0xd6); // teal
const DIM: egui::Color32 = egui::Color32::from_rgb(0xaa, 0xaa, 0xaa);
const APPLY: egui::Color32 = egui::Color32::from_rgb(0x3a, 0x6b, 0x3f); // accent green
const BTN: egui::Color32 = egui::Color32::from_rgb(0x44, 0x44, 0x44); // toolbar buttons

fn load_icon() -> Option<egui::IconData> {
    let img = image::load_from_memory(include_bytes!("../assets/icon.png")).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some(egui::IconData {
        rgba: rgba.into_raw(),
        width: w,
        height: h,
    })
}

// MO2 documents F5 as a complete profile refresh.  ModSlut is launched as
// an external executable rather than an MO2 plugin, so this is the narrowest
// supported hand-off available to us: locate the already-open MO2 window and
// post that documented shortcut without stealing foreground focus.
#[cfg(target_os = "windows")]
fn request_open_mo2_refresh() -> bool {
    type Hwnd = isize;
    unsafe extern "system" {
        fn EnumWindows(
            callback: Option<unsafe extern "system" fn(Hwnd, isize) -> i32>,
            data: isize,
        ) -> i32;
        fn GetWindowTextW(hwnd: Hwnd, text: *mut u16, max_count: i32) -> i32;
        fn PostMessageW(hwnd: Hwnd, message: u32, wparam: usize, lparam: isize) -> i32;
    }
    unsafe extern "system" fn visit(hwnd: Hwnd, data: isize) -> i32 {
        let mut text = [0u16; 512];
        let len = unsafe { GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32) };
        if len <= 0 {
            return 1;
        }
        let title = String::from_utf16_lossy(&text[..len as usize]).to_ascii_lowercase();
        if !title.contains("mod organizer") {
            return 1;
        }
        // WM_KEYDOWN / WM_KEYUP, VK_F5. MO2 performs its profile refresh
        // from this documented shortcut.
        unsafe {
            PostMessageW(hwnd, 0x0100, 0x74, 0);
            PostMessageW(hwnd, 0x0101, 0x74, 0);
            *(data as *mut bool) = true;
        }
        0
    }
    let mut posted = false;
    unsafe {
        EnumWindows(Some(visit), &mut posted as *mut bool as isize);
    }
    posted
}

#[cfg(not(target_os = "windows"))]
fn request_open_mo2_refresh() -> bool {
    false
}

fn kind_color(k: ChangeKind) -> egui::Color32 {
    match k {
        ChangeKind::Move => MOVE_CLR,
        ChangeKind::Separator => RENAME_CLR,
        ChangeKind::Reorder => REOR_CLR,
        ChangeKind::Promote => PROM_CLR,
        ChangeKind::Sink => SINK_CLR,
        ChangeKind::Float => FLOT_CLR,
        ChangeKind::Warn => WARN_CLR,
        ChangeKind::Rename => RENAME_CLR,
    }
}

fn kind_tag(k: ChangeKind) -> &'static str {
    match k {
        ChangeKind::Move => "MOVE",
        ChangeKind::Separator => "SEP",
        ChangeKind::Reorder => "REOR",
        ChangeKind::Promote => "PROM",
        ChangeKind::Sink => "SINK",
        ChangeKind::Float => "FLOT",
        ChangeKind::Warn => "WARN",
        ChangeKind::Rename => "REN",
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MetadataTab {
    Group,
    LoadAfter,
    LoadBefore,
    Requirements,
    Incompatibilities,
    Messages,
    BashTags,
    DirtyPluginInfo,
    CleanPluginInfo,
    Locations,
}

#[derive(Clone, Copy)]
enum MetadataAction {
    Save,
    Cancel,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    General,
    Game(game::Game),
}

#[derive(Clone)]
struct PluginRow {
    position: usize,
    index: String,
    name: String,
    owner: Option<String>,
}

// plugins.txt is MO2's real plugin-order ledger. Reorder only its active
// entries, preserving comments, inactive entries, and the user's spelling.
// This is built as a preview first and written only by Apply.
fn plugin_order_preview(text: &str, sorted: &[String]) -> Result<(String, usize), String> {
    let active: Vec<String> = text
        .lines()
        .filter_map(|line| {
            line.trim_start()
                .strip_prefix('*')
                .map(|name| name.trim().to_string())
        })
        .collect();
    if active.is_empty() || sorted.is_empty() {
        return Ok((text.to_string(), 0));
    }
    let display: HashMap<String, String> = active
        .iter()
        .map(|name| (name.to_ascii_lowercase(), name.clone()))
        .collect();
    let wanted: Vec<String> = sorted
        .iter()
        .filter_map(|name| display.get(&name.to_ascii_lowercase()).cloned())
        .collect();
    if wanted.len() != active.len()
        || wanted
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect::<std::collections::HashSet<_>>()
            .len()
            != active
                .iter()
                .map(|name| name.to_ascii_lowercase())
                .collect::<std::collections::HashSet<_>>()
                .len()
    {
        return Err(
            "libloot order and plugins.txt active entries disagree; not preparing a plugin write"
                .into(),
        );
    }
    let changed = active
        .iter()
        .zip(&wanted)
        .filter(|(before, after)| !before.eq_ignore_ascii_case(after))
        .count();
    let mut next = wanted.into_iter();
    let mut output = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('*') {
            let indent = &line[..line.len() - trimmed.len()];
            let name = next.next().expect("active entry count already validated");
            output.push_str(indent);
            output.push('*');
            output.push_str(&name);
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    Ok((output, changed))
}

impl MetadataTab {
    fn label(self) -> &'static str {
        match self {
            Self::Group => "Group",
            Self::LoadAfter => "Load After",
            Self::LoadBefore => "Load Before",
            Self::Requirements => "Requirements",
            Self::Incompatibilities => "Incompatibilities",
            Self::Messages => "Messages",
            Self::BashTags => "Bash Tags",
            Self::DirtyPluginInfo => "Dirty Plugin Info",
            Self::CleanPluginInfo => "Clean Plugin Info",
            Self::Locations => "Locations",
        }
    }

    fn all() -> [Self; 10] {
        [
            Self::Group,
            Self::LoadAfter,
            Self::LoadBefore,
            Self::Requirements,
            Self::Incompatibilities,
            Self::Messages,
            Self::BashTags,
            Self::DirtyPluginInfo,
            Self::CleanPluginInfo,
            Self::Locations,
        ]
    }
}

// ---- profile auto-detection ----
// launched from mo2, the exe could land almost anywhere (mo2 dir, game dir,
// some tools folder, a nolvus stock game folder...). so we hunt, walking up
// from the working directory:
//   1. modlist.txt right in the working directory
//   2. a ModOrganizer.ini -> selected_profile key -> profiles/<name>/modlist.txt
//      (this is the exact profile mo2 has loaded - beats any guessing)
//   3. <dir>/profiles/<anything>/modlist.txt, most recently written wins
fn find_modlist() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;

    let direct = cwd.join("modlist.txt");
    if direct.is_file() {
        return Some(direct);
    }

    let mut found: Vec<PathBuf> = Vec::new();
    let mut dir: Option<&Path> = Some(cwd.as_path());
    for _ in 0..5 {
        let Some(d) = dir else { break };

        // mo2 tells us exactly which profile is active - trust it over timestamps
        let ini = d.join("ModOrganizer.ini");
        if ini.is_file() {
            if let Some(name) = read_selected_profile(&ini) {
                let p = d.join("profiles").join(&name).join("modlist.txt");
                if p.is_file() {
                    return Some(p);
                }
            }
        }

        let prof = d.join("profiles");
        if prof.is_dir() {
            collect_profiles(&prof, &mut found);
        }
        // cwd might BE inside the profiles dir (e.g. launched from a profile folder)
        if d.file_name().is_some_and(|n| n == "profiles") {
            collect_profiles(d, &mut found);
        }
        dir = d.parent();
    }

    found
        .into_iter()
        .filter_map(|p| {
            std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .ok()
                .map(|t| (p, t))
        })
        .max_by_key(|(_, t)| *t)
        .map(|(p, _)| p)
}

// mo2 writes the active profile as `selected_profile = Name` or
// `selected_profile=@ByteArray(Name)` - handle both
fn read_selected_profile(ini: &Path) -> Option<String> {
    let text = std::fs::read_to_string(ini).ok()?;
    for line in text.lines() {
        let l = line.trim();
        if let Some(v) = l.strip_prefix("selected_profile") {
            let v = v.trim_start_matches(['=', ' ']).trim();
            let v = v
                .strip_prefix("@ByteArray(")
                .and_then(|s| s.strip_suffix(')'))
                .unwrap_or(v)
                .trim_matches('"')
                .trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn collect_profiles(profiles_dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(profiles_dir) {
        for e in rd.flatten() {
            let p = e.path().join("modlist.txt");
            if p.is_file() {
                out.push(p);
            }
        }
    }
}

// ---- per-user configuration ----
// One readable config file avoids settings.txt and ui.txt clobbering each
// other when a user changes window geometry and appearance in the same run.
// It deliberately belongs to the user, not an MO2 install or a profile.
fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("modslut").join("config.dat"))
}

fn legacy_settings_path() -> Option<PathBuf> {
    config_path().map(|p| p.with_file_name("settings.txt"))
}

fn legacy_ui_prefs_path() -> Option<PathBuf> {
    config_path().map(|p| p.with_file_name("ui.txt"))
}

fn read_config() -> HashMap<String, String> {
    let mut values = HashMap::new();
    // New config takes precedence. If it does not exist yet, merge both old
    // files once so updating ModSlut never throws away a user's layout.
    let paths = [
        config_path(),
        legacy_settings_path(),
        legacy_ui_prefs_path(),
    ];
    for path in paths.into_iter().flatten() {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in text.lines() {
            if let Some((key, value)) = line.split_once('=') {
                values
                    .entry(key.trim().to_string())
                    .or_insert_with(|| value.trim().to_string());
            }
        }
    }
    values
}

fn write_config(values: HashMap<String, String>) {
    let Some(path) = config_path() else { return };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let mut keys: Vec<_> = values.keys().collect();
    keys.sort();
    let mut text =
        String::from("# ModSlut per-user preferences. Safe to edit while ModSlut is closed.\n");
    for key in keys {
        if let Some(value) = values.get(key) {
            text.push_str(&format!("{key}={value}\n"));
        }
    }
    std::fs::write(path, text).ok();
}

#[derive(Default)]
struct ThemePack {
    name: String,
    panel: Option<egui::Color32>,
    toolbar: Option<egui::Color32>,
    accent: Option<egui::Color32>,
    card: Option<egui::Color32>,
    selected: Option<egui::Color32>,
    background: Option<PathBuf>,
}

fn theme_root() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|folder| folder.join("themes"))
}

fn theme_color(value: &str) -> Option<egui::Color32> {
    let raw = value
        .trim()
        .trim_matches('"')
        .trim()
        .trim_start_matches('#');
    if raw.len() != 3 && raw.len() != 6 && raw.len() != 8 {
        return None;
    }
    let expanded = if raw.len() == 3 {
        let chars: Vec<_> = raw.chars().collect();
        format!(
            "{}{}{}{}{}{}",
            chars[0], chars[0], chars[1], chars[1], chars[2], chars[2]
        )
    } else {
        raw.to_string()
    };
    let r = u8::from_str_radix(&expanded[0..2], 16).ok()?;
    let g = u8::from_str_radix(&expanded[2..4], 16).ok()?;
    let b = u8::from_str_radix(&expanded[4..6], 16).ok()?;
    let a = if raw.len() == 8 {
        u8::from_str_radix(&raw[6..8], 16).ok()?
    } else {
        255
    };
    Some(egui::Color32::from_rgba_unmultiplied(r, g, b, a))
}

// Imported QSS palettes often declare a very light, fully opaque widget
// background. Treat that as a tint rather than a literal card paint, so a
// theme never loses its background art behind a stack of gray slabs.
fn blend_theme_color(
    foreground: egui::Color32,
    background: egui::Color32,
    foreground_weight: u16,
) -> egui::Color32 {
    let foreground_weight = foreground_weight.min(100);
    let background_weight = 100 - foreground_weight;
    let mix =
        |a: u8, b: u8| ((a as u16 * foreground_weight + b as u16 * background_weight) / 100) as u8;
    egui::Color32::from_rgb(
        mix(foreground.r(), background.r()),
        mix(foreground.g(), background.g()),
        mix(foreground.b(), background.b()),
    )
}

// LOOT disables its table row grid and retains the header's section divider.
// Mirror that quiet one-line boundary here; rows reserve the same width but
// deliberately do not paint a repeated fence down the entire tree.
fn left_index_divider(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(2.0, ui.spacing().interact_size.y),
        egui::Sense::hover(),
    );
    ui.painter().vline(
        rect.center().x,
        rect.y_range(),
        egui::Stroke::new(2.0_f32, egui::Color32::from_white_alpha(185)),
    );
}

fn load_theme_pack(path: &Path) -> Option<ThemePack> {
    let text = std::fs::read_to_string(path.join("theme.toml")).ok()?;
    let mut pack = ThemePack::default();
    let mut background_name = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        match key.trim() {
            "name" => pack.name = value.to_string(),
            "panel_bg" | "background_overlay" => pack.panel = theme_color(value),
            "toolbar_bg" => pack.toolbar = theme_color(value),
            "accent" => pack.accent = theme_color(value),
            "card_bg" => pack.card = theme_color(value),
            "selection_bg" => pack.selected = theme_color(value),
            "background_image" => background_name = Some(value.to_string()),
            _ => {}
        }
    }
    if pack.name.trim().is_empty() {
        return None;
    }
    if let Some(image) = background_name {
        let candidate = path.join(image);
        if candidate.is_file() {
            pack.background = Some(candidate);
        }
    }
    Some(pack)
}

fn theme_packs() -> Vec<ThemePack> {
    let Some(root) = theme_root() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut packs: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter_map(|path| load_theme_pack(&path))
        .collect();
    packs.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
    });
    packs
}

fn bootstrap_theme_packs() {
    let Some(root) = theme_root() else { return };
    let builtins: [(&str, &str, &[u8], &str); 4] = [
        (
            "Skybound Nebula",
            "skybound-nebula.png",
            include_bytes!("../assets/themes/skybound-nebula.png"),
            "#18263ACD|#0D1625|#4A90E2|#14253AE6|#1E3D5B",
        ),
        (
            "Midnight Roses",
            "midnight-roses.png",
            include_bytes!("../assets/themes/midnight-roses.png"),
            "#2C1C3ACD|#180D28|#D672D8|#2A1638E6|#4D2762",
        ),
        (
            "Ember Coast",
            "ember-coast.png",
            include_bytes!("../assets/themes/ember-coast.png"),
            "#312525CD|#1C171C|#DC7C42|#322424E8|#5B3B32",
        ),
        (
            "Winterpine",
            "winterpine.png",
            include_bytes!("../assets/themes/winterpine.png"),
            "#1C2D36CD|#0E1B22|#66BDE8|#172A34E8|#2B4B5A",
        ),
    ];
    for (name, image, bytes, colors) in builtins {
        let folder = root.join(name);
        if std::fs::create_dir_all(&folder).is_err() {
            continue;
        }
        let [panel, toolbar, accent, card, selected] = colors.split('|').collect::<Vec<_>>()[..]
        else {
            continue;
        };
        let config = format!("[theme]\nname = \"{name}\"\nauthor = \"ModSlut\"\nversion = \"1.0\"\n\n[colors]\nbackground_image = \"{image}\"\npanel_bg = \"{panel}\"\ntoolbar_bg = \"{toolbar}\"\naccent = \"{accent}\"\ncard_bg = \"{card}\"\nselection_bg = \"{selected}\"\n");
        let config_path = folder.join("theme.toml");
        if !config_path.exists() {
            std::fs::write(config_path, config).ok();
        }
        let image_path = folder.join(image);
        if !image_path.exists() {
            std::fs::write(image_path, bytes).ok();
        }
    }
}

fn qss_hex_colours(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut colors = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' {
            let end = (i + 9).min(bytes.len());
            let raw = &text[i + 1..end];
            let length = if raw
                .get(..8)
                .is_some_and(|v| v.bytes().all(|c| c.is_ascii_hexdigit()))
            {
                8
            } else if raw
                .get(..6)
                .is_some_and(|v| v.bytes().all(|c| c.is_ascii_hexdigit()))
            {
                6
            } else {
                0
            };
            if length > 0 {
                let color = format!("#{}", &raw[..length]);
                if !colors.contains(&color) {
                    colors.push(color);
                }
                i += length;
            }
        }
        i += 1;
    }
    colors
}

fn qss_property_colours(text: &str, property: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case(property) {
            continue;
        }
        let value = value
            .trim()
            .trim_end_matches(';')
            .trim()
            .trim_matches(['\'', '"']);
        if let Some(color) = theme_color(value) {
            let raw = if value.len() == 4 {
                // Qt permits #RGB; normalize it.
                let c: Vec<_> = value[1..].chars().collect();
                format!("#{}{}{}{}{}{}", c[0], c[0], c[1], c[1], c[2], c[2])
            } else {
                value.to_string()
            };
            if !out.contains(&raw) && color != egui::Color32::TRANSPARENT {
                out.push(raw);
            }
        }
    }
    out
}

fn qss_background(source: &Path, text: &str, target: &Path) -> Option<String> {
    let start = text.find("url(")? + 4;
    let tail = &text[start..];
    let end = tail.find(')')?;
    let raw = tail[..end].trim().trim_matches(['\'', '"']);
    let image = source.parent()?.join(raw);
    if !image.is_file() {
        return None;
    }
    let extension = image.extension().and_then(|e| e.to_str()).unwrap_or("png");
    let name = format!("background.{extension}");
    if std::fs::copy(image, target.join(&name)).is_ok() {
        Some(name)
    } else {
        None
    }
}

fn import_mo2_qss(source: &Path) -> Result<String, String> {
    let text =
        std::fs::read_to_string(source).map_err(|e| format!("couldn't read stylesheet: {e}"))?;
    let raw_name = source
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("Imported MO2 Theme")
        .trim();
    let name = raw_name.replace(['_', '-'], " ");
    let safe: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let root = theme_root().ok_or("couldn't locate ModSlut's themes folder")?;
    let target = root.join(safe);
    std::fs::create_dir_all(&target).map_err(|e| format!("couldn't create theme pack: {e}"))?;
    let backgrounds = qss_property_colours(&text, "background-color");
    let selections = qss_property_colours(&text, "selection-background-color");
    let panel = backgrounds
        .get(0)
        .cloned()
        .unwrap_or_else(|| "#20242BDD".into());
    let toolbar = backgrounds
        .get(0)
        .cloned()
        .unwrap_or_else(|| "#141820".into());
    let accent = selections
        .get(0)
        .cloned()
        .or_else(|| backgrounds.get(2).cloned())
        .unwrap_or_else(|| "#4A90E2".into());
    // QSS has no semantic card-background property. A second generic
    // `background-color` can describe a scrollbar, button, or disabled
    // widget, so using it for cards made some imported themes gray slabs.
    // Cards inherit the panel tint and ModSlut supplies the translucency.
    let card = panel.clone();
    let selected = selections.get(0).cloned().unwrap_or_else(|| accent.clone());
    let background = qss_background(source, &text, &target)
        .map(|file| format!("background_image = \"{file}\"\n"))
        .unwrap_or_default();
    let toml = format!("[theme]\nname = \"{name}\"\nauthor = \"Imported from MO2 stylesheet\"\nversion = \"1.0\"\n\n[colors]\n{background}panel_bg = \"{panel}\"\ntoolbar_bg = \"{toolbar}\"\naccent = \"{accent}\"\ncard_bg = \"{card}\"\nselection_bg = \"{selected}\"\n");
    std::fs::write(target.join("theme.toml"), toml)
        .map_err(|e| format!("couldn't write theme.toml: {e}"))?;
    Ok(name)
}

fn find_qss_files(folder: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            find_qss_files(&path, out);
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("qss"))
        {
            out.push(path);
        }
    }
}

fn import_mo2_qss_folder(folder: &Path) -> Result<(usize, usize), String> {
    let mut files = Vec::new();
    find_qss_files(folder, &mut files);
    if files.is_empty() {
        return Err("that folder contains no .qss stylesheets".into());
    }
    let total = files.len();
    let imported = files
        .into_iter()
        .filter(|file| import_mo2_qss(file).is_ok())
        .count();
    Ok((imported, total))
}

fn qss_theme_pack(source: &Path, name: String) -> Option<ThemePack> {
    let text = std::fs::read_to_string(source).ok()?;
    // QSS often declares a light foreground before any panel paint. Mirror
    // only background/selection properties, never arbitrary hex literals.
    let backgrounds = qss_property_colours(&text, "background-color");
    let selections = qss_property_colours(&text, "selection-background-color");
    let color =
        |values: &[String], index: usize| values.get(index).and_then(|value| theme_color(value));
    let background = text.find("url(").and_then(|start| {
        let tail = &text[start + 4..];
        let end = tail.find(')')?;
        let path = source
            .parent()?
            .join(tail[..end].trim().trim_matches(['\'', '"']));
        path.is_file().then_some(path)
    });
    Some(ThemePack {
        name,
        panel: color(&backgrounds, 0),
        toolbar: color(&backgrounds, 0),
        accent: color(&selections, 0).or_else(|| color(&backgrounds, 2)),
        // QSS does not name cards. Mirror the panel and let ModSlut apply a
        // translucent card treatment instead of guessing from other widgets.
        card: color(&backgrounds, 0),
        selected: color(&selections, 0).or_else(|| color(&backgrounds, 0)),
        background,
    })
}

// Read only. ModSlut never edits ModOrganizer.ini, a QSS file, or MO2's
// theme state; this produces a private in-memory palette for this process.
fn active_mo2_theme(modlist: Option<&Path>) -> Option<ThemePack> {
    let mut roots = Vec::new();
    if let Some(file) = modlist {
        let mut cursor = file.parent();
        for _ in 0..5 {
            let Some(dir) = cursor else { break };
            roots.push(dir.to_path_buf());
            cursor = dir.parent();
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    for root in roots {
        let ini = root.join("ModOrganizer.ini");
        let Ok(text) = std::fs::read_to_string(&ini) else {
            continue;
        };
        let Some(style) = text
            .lines()
            .find_map(|line| line.trim().strip_prefix("style=").map(str::trim))
        else {
            continue;
        };
        let stylesheet = {
            let raw = PathBuf::from(style);
            if raw.is_absolute() {
                raw
            } else {
                root.join("stylesheets").join(raw)
            }
        };
        if let Some(pack) = qss_theme_pack(
            &stylesheet,
            format!("MO2 Mirror — {}", style.trim_end_matches(".qss")),
        ) {
            return Some(pack);
        }
    }
    None
}

fn load_settings() -> (f32, Option<game::Game>, String) {
    let values = read_config();
    let text = legacy_settings_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    // v0.14.7 wrote a bare font size. Retain it so an update never loses a
    // user's accessibility setting; new files are readable key=value lines.
    let legacy_font = text.trim().parse::<f32>().ok();
    let mut font = legacy_font;
    let mut default_game = None;
    let mut theme = "MO2 Mirror (read-only)".to_string();
    for (key, value) in values {
        match key.as_str() {
            "font_size" => font = value.parse::<f32>().ok(),
            "default_game" => default_game = game::from_key(&value),
            "theme" => {
                theme = if value == "SkyGen Blue" {
                    "Skybound Nebula".to_string()
                } else {
                    value
                }
            }
            _ => {}
        }
    }
    (
        font.filter(|&s| (10.0..=28.0).contains(&s)).unwrap_or(14.0),
        default_game,
        theme,
    )
}

fn load_font_size() -> f32 {
    load_settings().0
}

fn load_default_game() -> Option<game::Game> {
    load_settings().1
}
fn load_theme() -> String {
    load_settings().2
}

fn save_settings(size: f32, default_game: Option<game::Game>, theme: &str) {
    let mut values = read_config();
    values.insert("font_size".into(), format!("{size:.0}"));
    values.insert(
        "default_game".into(),
        default_game.map(game::key).unwrap_or("auto").to_string(),
    );
    values.insert("theme".into(), theme.to_string());
    write_config(values);
}

// MO2 keeps an install's provenance beside the mod itself. Only show a
// Source line for a real Nexus install: Creation Kit and cleaned Creation
// Club folders may have metadata too, but they are not Nexus sources. Bash
// tags are evaluated separately and can still appear on any active plugin.
fn source_from_meta(mods_dir: &Path, mod_name: &str) -> Option<String> {
    let text = std::fs::read_to_string(mods_dir.join(mod_name).join("meta.ini")).ok()?;
    let mut url = None;
    let mut repository = None;
    let mut mod_id = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("url=") {
            if !value.trim().is_empty() {
                url = Some(value.trim().to_string());
            }
        } else if let Some(value) = line.strip_prefix("repository=") {
            if !value.trim().is_empty() {
                repository = Some(value.trim().to_string());
            }
        } else if let Some(value) = line.strip_prefix("modid=") {
            if !value.trim().is_empty() {
                mod_id = Some(value.trim().to_string());
            }
        }
    }
    let url_is_nexus = url
        .as_deref()
        .is_some_and(|value| value.to_ascii_lowercase().contains("nexusmods.com"));
    let repository_is_nexus = repository
        .as_deref()
        .is_some_and(|value| value.to_ascii_lowercase().contains("nexus"));
    if url_is_nexus {
        return url;
    }
    // MO2 records repository=Nexus and modid=-1 for locally indexed folders
    // such as Creation Kit / cleaned Creation Club content. A Source line
    // needs a real positive Nexus id (or the URL handled above).
    repository_is_nexus
        .then_some(())
        .and_then(|_| mod_id)
        .and_then(|id| id.parse::<u64>().ok().filter(|id| *id > 0))
        .map(|id| format!("Nexus mod {id}"))
}

// The left panel is MS's equivalent of LOOT's Position column. Keep the
// exact priority MO2 gives each row (separators included), plus its familiar
// enabled/disabled/foreign marker, so the visible list and priority manifest
// describe the same backbone.
fn mo2_priority_view(
    text: &str,
) -> (
    HashMap<String, usize>,
    HashMap<String, char>,
    HashMap<String, usize>,
) {
    let rows: Vec<&str> = text
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|row| !row.is_empty())
        .collect();
    let total = rows.len();
    let mut mods = HashMap::new();
    let mut states = HashMap::new();
    let mut separators = HashMap::new();
    for (index, row) in rows.into_iter().enumerate() {
        let priority = total - index;
        let marker = row.chars().next().filter(|c| matches!(c, '+' | '-' | '*'));
        let bare = row.trim_start_matches(['+', '-', '*']).trim();
        if let Some(label) = bare.strip_suffix("_separator") {
            separators.insert(label.trim().to_string(), priority);
        } else if !bare.is_empty() {
            mods.insert(bare.to_string(), priority);
            if let Some(marker) = marker {
                states.insert(bare.to_string(), marker);
            }
        }
    }
    (mods, states, separators)
}

fn show_source_and_bash_tags(
    ui: &mut egui::Ui,
    source: Option<&String>,
    bash_tags: Option<&Vec<String>>,
) {
    if let Some(source) = source {
        ui.add_space(5.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Source:").color(DIM));
            if source.starts_with("http://") || source.starts_with("https://") {
                ui.hyperlink_to(source, source);
            } else {
                ui.label(egui::RichText::new(source).color(REOR_CLR));
            }
        });
    }
    if let Some(tags) = bash_tags.filter(|tags| !tags.is_empty()) {
        ui.add_space(7.0);
        egui::Frame::new()
            .stroke(egui::Stroke::new(1.0_f32, DIM))
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Bash Tags").color(DIM));
                ui.horizontal_wrapped(|ui| {
                    for tag in tags {
                        let color = if tag.starts_with("Remove ") {
                            WARN_CLR
                        } else {
                            PROM_CLR
                        };
                        ui.label(egui::RichText::new(tag).color(color));
                    }
                });
            });
    }
}

// LOOT messages use a little Markdown (notably the live "Latest LOOT thread"
// link). egui labels do not parse it, so render the useful inline link form
// instead of showing users the raw brackets and URL.
fn show_loot_message(ui: &mut egui::Ui, text: &str) {
    fn plain(text: &str) -> String {
        text.replace("**", "").replace("__", "")
    }

    ui.horizontal_wrapped(|ui| {
        let mut rest = text;
        while let Some(open) = rest.find('[') {
            let before = &rest[..open];
            if !before.is_empty() {
                ui.label(plain(before));
            }
            let tail = &rest[open + 1..];
            let Some(label_end) = tail.find("](") else {
                ui.label(plain(&rest[open..]));
                return;
            };
            let label = &tail[..label_end];
            let url_tail = &tail[label_end + 2..];
            let Some(url_end) = url_tail.find(')') else {
                ui.label(plain(&rest[open..]));
                return;
            };
            let url = &url_tail[..url_end];
            if url.starts_with("https://") || url.starts_with("http://") {
                ui.hyperlink_to(plain(label), url);
            } else {
                ui.label(plain(&rest[open..=open + label_end + 2 + url_end]));
            }
            rest = &url_tail[url_end + 1..];
        }
        if !rest.is_empty() {
            ui.label(plain(rest));
        }
    });
}

// ---- layout prefs: left panel width + window geometry, saved between runs ----
#[derive(Clone, Copy, Default)]
struct UiPrefs {
    panel: Option<f32>,
    size: Option<(f32, f32)>,
    pos: Option<(f32, f32)>,
    // Settings is a separate egui window, so it needs its own remembered
    // bounds. Reusing the main viewport bounds was what made it reopen at a
    // seemingly arbitrary height after a user had resized it.
    settings_size: Option<(f32, f32)>,
    settings_pos: Option<(f32, f32)>,
    user_rules_only: bool,
}

fn load_ui_prefs() -> UiPrefs {
    let mut out = UiPrefs::default();
    for (k, v) in read_config() {
        match k.as_str() {
            "panel" => out.panel = v.parse::<f32>().ok().filter(|w| *w >= 200.0),
            "size" => {
                if let Some((w, h)) = v.split_once('x') {
                    if let (Ok(w), Ok(h)) = (w.parse::<f32>(), h.parse::<f32>()) {
                        if w >= 400.0 && h >= 300.0 {
                            out.size = Some((w, h));
                        }
                    }
                }
            }
            "pos" => {
                if let Some((x, y)) = v.split_once(',') {
                    if let (Ok(x), Ok(y)) = (x.parse::<f32>(), y.parse::<f32>()) {
                        out.pos = Some((x, y));
                    }
                }
            }
            "settings_size" => {
                if let Some((w, h)) = v.split_once('x') {
                    if let (Ok(w), Ok(h)) = (w.parse::<f32>(), h.parse::<f32>()) {
                        if w >= 300.0 && h >= 180.0 {
                            out.settings_size = Some((w, h));
                        }
                    }
                }
            }
            "settings_pos" => {
                if let Some((x, y)) = v.split_once(',') {
                    if let (Ok(x), Ok(y)) = (x.parse::<f32>(), y.parse::<f32>()) {
                        out.settings_pos = Some((x, y));
                    }
                }
            }
            "user_rules_only" => out.user_rules_only = v == "1",
            _ => {}
        }
    }
    out
}

fn save_ui_prefs(prefs: &UiPrefs) {
    let mut values = read_config();
    if let Some(w) = prefs.panel {
        values.insert("panel".into(), format!("{w:.0}"));
    }
    if let Some((w, h)) = prefs.size {
        values.insert("size".into(), format!("{w:.0}x{h:.0}"));
    }
    if let Some((x, y)) = prefs.pos {
        values.insert("pos".into(), format!("{x:.0},{y:.0}"));
    }
    if let Some((w, h)) = prefs.settings_size {
        values.insert("settings_size".into(), format!("{w:.0}x{h:.0}"));
    }
    if let Some((x, y)) = prefs.settings_pos {
        values.insert("settings_pos".into(), format!("{x:.0},{y:.0}"));
    }
    values.insert(
        "user_rules_only".into(),
        if prefs.user_rules_only { "1" } else { "0" }.into(),
    );
    write_config(values);
}

const USER_RULES_TEMPLATE: &str = "\
# modslut user rules - checked BEFORE the built-in rules, so anything you
# put here wins. save the file, then hit \"reload\" in modslut.
#
# syntax:
#   !Exact Mod Name = Section Label     force one mod into a section
#   @MO2 Category = Section Label       route a whole mo2 category
#                                       (see the \"categories\" view for names)
#   keyword !exclusion = Section Label  lowercase substring match, first hit wins
#   >After Name = Before Name            After loads later than Before
#   <Mod Name = Section Label           sink: pin to the TOP of that section in mo2
#                                       (loses everything in-section - base replacers)
#   ^Mod Name = Section Label           float: pin to the BOTTOM of that section
#                                       (wins everything in-section)
#
# directives (a bare !line with no '=', put it anywhere):
#   !proven-only                        only proven moves: your rules, loot,
#                                       conflict data, plugin masters - no
#                                       category/keyword guesses
#   !rename-separators                  let modslut retitle separators to
#                                       canonical concepts (off by default)
#   !dump = Section Label               mark a separator as a waiting room:
#                                       its contents get filed out, nothing
#                                       ever moves IN (e.g. !dump = End of List)
#
# section labels must match your separator names exactly. examples:
#   !Some Sexlab Mod - Argonian Addon = Skin and Body - Argonians and Khajiits
#   @Clothing = Clothing and Jewelry
#   <Glacierslab SSE = Glaciers, Ice, Snow and Ash
#
# lines below are appended by the right-click menu in the gui:
";

// ---- app ----

pub fn run_gui() {
    let prefs = load_ui_prefs();
    let (w, h) = prefs.size.unwrap_or((1100.0, 700.0));
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(format!("ModSlut v{}", crate::VERSION))
        .with_inner_size([w, h]);
    if let Some((x, y)) = prefs.pos {
        viewport = viewport.with_position([x, y]);
    }
    if let Some(icon) = load_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        // we persist geometry ourselves (ui.txt) - eframe's built-in
        // persistence isn't reliable everywhere
        persist_window: false,
        ..Default::default()
    };
    if let Err(e) = eframe::run_native(
        "ModSlut",
        options,
        Box::new(move |_cc| Ok(Box::new(ModslutApp::new(prefs)))),
    ) {
        eprintln!("couldn't start the gui: {e}");
    }
}

struct ModslutApp {
    file: Option<PathBuf>,
    plan: Vec<Change>,
    sorted: Option<String>,        // serialized output, ready to write
    plugin_sorted: Option<String>, // plugins.txt preview, written only by Apply
    plugin_rows: Vec<PluginRow>,   // libloot's solved order, low -> high
    selected: Option<usize>,
    written: bool,
    status: String,
    font_size: f32,
    cats_report: Option<String>,
    show_cats: bool,
    trace: String,
    content_index: Option<crate::content_index::ContentIndex>,
    conflicts: Option<Arc<ConflictIndex>>,
    scan_rx: Option<mpsc::Receiver<ConflictIndex>>,
    sections: Vec<String>,
    // right-click "send to section…" picker: (mod name, current section)
    picker_for: Option<(String, String)>,
    picker_filter: String,
    promote_target: String,
    metadata_tab: MetadataTab,
    human_metadata: HumanMetadata,
    metadata_message: String,
    metadata_rule_draft: Option<String>,
    // LOOT permits several ordering rows. Keep these separate from the
    // one-choice Group draft so Load After/Before can accumulate edges.
    metadata_load_order_drafts: Vec<String>,
    metadata_human_drafts: Vec<(HumanMetadataKind, String)>,
    // ui layout prefs (persisted): left panel width + last window rect
    changes_width: f32,
    win_rect: Option<egui::Rect>,
    // full modlist snapshot (current state, pre-run) for the loot-style
    // whole-list view: section -> mod names in current order
    layout: Vec<(String, Vec<String>)>,
    parking_mods: Vec<String>,
    mod_priorities: HashMap<String, usize>,
    mod_states: HashMap<String, char>,
    separator_priorities: HashMap<String, usize>,
    // a plain mod row clicked (no pending change): (name, section)
    selected_mod: Option<(String, String)>,
    left_search: String,
    right_search: String,
    // The browser is repainted for input, hover, and background-job status.
    // Keep its derived lookup data out of that hot path: a 2k-mod list must
    // not rebuild a change index or lowercase every label sixty times/sec.
    left_change_map: HashMap<String, Vec<usize>>,
    left_search_terms: HashMap<String, String>,
    // where the auto-written debug_sort.log landed (or why it didn't)
    debug_note: Option<String>,
    // "user rules only" mode (persisted): built-in cascade + guess tiers off
    user_rules_only: bool,
    // current profile's unresolved separator concepts; only UI state, never
    // persisted because the roadmap INI is the source of truth.
    show_roadmap: bool,
    roadmap_path: Option<PathBuf>,
    roadmap_ambiguous: Vec<String>,
    roadmap_reviewed: Vec<(String, String)>,
    roadmap_multi: Vec<String>,
    roadmap_branches: Vec<(String, Vec<String>)>,
    roadmap_destinations: Vec<String>,
    // Non-sortable presentation rows from this profile's roadmap [context].
    // The left pane and LOOT-style cards use these same boundaries as run().
    roadmap_context: HashSet<String>,
    roadmap_picks: HashMap<String, String>,
    // A separator can also be a LOOT-style order group. Kept profile-local
    // because it describes this list's design, not a universal taxonomy.
    show_groups: bool,
    show_settings: bool,
    settings_rect: Option<egui::Rect>,
    settings_page: SettingsPage,
    settings_font_draft: f32,
    theme: String,
    settings_theme_draft: String,
    theme_background: Option<egui::TextureHandle>,
    theme_texture_name: String,
    theme_card_fill: egui::Color32,
    theme_card_selected: egui::Color32,
    theme_card_border: egui::Color32,
    default_game: Option<game::Game>,
    settings_game_draft: Option<game::Game>,
    groups_path: Option<PathBuf>,
    group_edges: Vec<(String, String)>, // (later, earlier)
    group_later: String,
    group_earlier: String,
    // Sort work is deliberately queued for one paint. A 2k-mod analysis can
    // take a while, and Windows otherwise makes the app look dead before the
    // existing synchronous sorter gets its first bite.
    sort_waiting: bool,
    sort_rx: Option<mpsc::Receiver<Box<ModslutApp>>>,
    // MO2-local provenance for LOOT-style cards, indexed only during Sort.
    mod_sources: HashMap<String, String>,
    // Evaluated LOOT Bash tags, reverse-projected from plugins to MO2 mods.
    mod_bash_tags: HashMap<String, Vec<String>>,
    // LOOT's real plugin index, reverse-projected only to owning MO2 mods.
    mod_plugin_indexes: HashMap<String, String>,
    // Bump this whenever the separator browser is collapsed as a whole. The
    // salt makes every header begin a fresh, closed collapsing state.
    collapse_generation: u64,
    // Kept as an internal escape hatch while card metadata settles; normal
    // use is deliberately card-only rather than showing a duplicate preview.
    show_detail_preview: bool,
    // Displayed in the LOOT-style picker. It is evidence, not an override:
    // libloot still detects the real game from the current plugin set.
    detected_game: String,
    // Filled by the authoritative libloot preview. Keeping this separate from
    // ownership records matters: one MO2 mod may provide several plugins.
    active_plugin_count: Option<usize>,
    active_full_plugin_count: Option<usize>,
    active_light_plugin_count: Option<usize>,
    dirty_plugin_count: Option<usize>,
    loot_warning_count: Option<usize>,
    loot_error_count: Option<usize>,
    loot_message_count: Option<usize>,
    loot_general_messages: Vec<crate::libloot_adapter::LootMessage>,
    masterlist_revision: Option<String>,
    masterlist_updated: Option<String>,
    masterlist_source: Option<String>,
    masterlist_path: Option<PathBuf>,
    masterlist_update_rx: Option<mpsc::Receiver<Result<crate::loot::MasterlistStatus, String>>>,
}

impl ModslutApp {
    fn request_mo2_refresh(&mut self) {
        if request_open_mo2_refresh() {
            self.status.push_str("; MO2 refresh requested");
        } else {
            self.status
                .push_str("; MO2 window was not found for automatic refresh");
        }
    }

    fn new(prefs: UiPrefs) -> Self {
        bootstrap_theme_packs();
        let mut app = Self {
            file: None,
            plan: Vec::new(),
            sorted: None,
            plugin_sorted: None,
            plugin_rows: Vec::new(),
            selected: None,
            written: false,
            status: String::new(),
            font_size: load_font_size(),
            cats_report: None,
            show_cats: false,
            trace: String::new(),
            content_index: None,
            conflicts: None,
            scan_rx: None,
            sections: Vec::new(),
            picker_for: None,
            picker_filter: String::new(),
            promote_target: String::new(),
            metadata_tab: MetadataTab::Group,
            human_metadata: HumanMetadata::default(),
            metadata_message: String::new(),
            metadata_rule_draft: None,
            metadata_load_order_drafts: Vec::new(),
            metadata_human_drafts: Vec::new(),
            changes_width: prefs.panel.unwrap_or(430.0),
            win_rect: None,
            layout: Vec::new(),
            parking_mods: Vec::new(),
            mod_priorities: HashMap::new(),
            mod_states: HashMap::new(),
            separator_priorities: HashMap::new(),
            selected_mod: None,
            left_search: String::new(),
            right_search: String::new(),
            left_change_map: HashMap::new(),
            left_search_terms: HashMap::new(),
            debug_note: None,
            user_rules_only: prefs.user_rules_only,
            show_roadmap: false,
            roadmap_path: None,
            roadmap_ambiguous: Vec::new(),
            roadmap_reviewed: Vec::new(),
            roadmap_multi: Vec::new(),
            roadmap_branches: Vec::new(),
            roadmap_destinations: Vec::new(),
            roadmap_context: HashSet::new(),
            roadmap_picks: HashMap::new(),
            show_groups: false,
            show_settings: false,
            settings_rect: match (prefs.settings_pos, prefs.settings_size) {
                (Some((x, y)), Some((w, h))) => Some(egui::Rect::from_min_size(
                    egui::pos2(x, y),
                    egui::vec2(w, h),
                )),
                _ => None,
            },
            settings_page: SettingsPage::General,
            settings_font_draft: load_font_size(),
            theme: load_theme(),
            settings_theme_draft: load_theme(),
            theme_background: None,
            theme_texture_name: String::new(),
            theme_card_fill: egui::Color32::from_rgb(42, 42, 42),
            theme_card_selected: egui::Color32::from_rgb(48, 56, 63),
            theme_card_border: egui::Color32::from_gray(70),
            default_game: load_default_game(),
            settings_game_draft: load_default_game(),
            groups_path: None,
            group_edges: Vec::new(),
            group_later: String::new(),
            group_earlier: String::new(),
            sort_waiting: false,
            sort_rx: None,
            mod_sources: HashMap::new(),
            mod_bash_tags: HashMap::new(),
            mod_plugin_indexes: HashMap::new(),
            collapse_generation: 0,
            show_detail_preview: false,
            detected_game: "Auto-detect game".into(),
            active_plugin_count: None,
            active_full_plugin_count: None,
            active_light_plugin_count: None,
            dirty_plugin_count: None,
            loot_warning_count: None,
            loot_error_count: None,
            loot_message_count: None,
            loot_general_messages: Vec::new(),
            masterlist_revision: None,
            masterlist_updated: None,
            masterlist_source: None,
            masterlist_path: None,
            masterlist_update_rx: None,
        };
        match find_modlist() {
            // Launch must be inert. Do not parse, sort, scan caches, or even
            // manufacture a roadmap until the user explicitly clicks Sort
            // Mods. A 2k-mod list is not a cute startup chore.
            Some(p) => app.open_idle(p),
            None => {
                app.status =
                    "couldn't find an mo2 profile from here - use 'open modlist.txt…'".into()
            }
        }
        app
    }

    fn open_idle(&mut self, path: PathBuf) {
        self.file = Some(path.clone());
        self.plan.clear();
        self.left_change_map.clear();
        self.left_search_terms.clear();
        self.sorted = None;
        self.plugin_sorted = None;
        self.plugin_rows.clear();
        self.selected = None;
        self.selected_mod = None;
        self.left_search.clear();
        self.right_search.clear();
        self.trace.clear();
        self.debug_note = None;
        self.cats_report = None;
        self.mod_sources.clear();
        self.mod_bash_tags.clear();
        self.mod_plugin_indexes.clear();
        self.show_cats = false;
        self.conflicts = None;
        self.scan_rx = None;
        self.active_plugin_count = None;
        self.active_full_plugin_count = None;
        self.active_light_plugin_count = None;
        self.dirty_plugin_count = None;
        self.loot_warning_count = None;
        self.loot_error_count = None;
        self.loot_message_count = None;
        self.loot_general_messages.clear();
        self.masterlist_revision = None;
        self.masterlist_updated = None;
        self.masterlist_source = None;
        self.masterlist_path = None;
        self.masterlist_update_rx = None;
        self.roadmap_path = None;
        self.roadmap_ambiguous.clear();
        self.roadmap_reviewed.clear();
        self.roadmap_multi.clear();
        self.roadmap_branches.clear();
        self.roadmap_destinations.clear();
        self.roadmap_context.clear();
        // Reading modlist.txt is the one safe startup action: it lets the
        // window show the user's actual list and active profile. No roadmap,
        // rule loading, file crawl, cache read, or sort plan happens here.
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let ml = parse(&text);
                (
                    self.mod_priorities,
                    self.mod_states,
                    self.separator_priorities,
                ) = mo2_priority_view(&text);
                self.layout = ml
                    .sections
                    .iter()
                    .map(|section| {
                        (
                            section.label.clone(),
                            section
                                .mods
                                .iter()
                                .map(|mod_entry| mod_entry.name.clone())
                                .collect(),
                        )
                    })
                    .collect();
                self.parking_mods = ml
                    .parking
                    .iter()
                    .filter(|line| !line.trim().starts_with('#'))
                    .map(|line| {
                        line.trim()
                            .trim_start_matches(['+', '-', '*'])
                            .trim()
                            .to_string()
                    })
                    .filter(|line| !line.is_empty())
                    .collect();
                self.sections = ml
                    .sections
                    .iter()
                    .map(|section| section.label.clone())
                    .collect();
                self.refresh_left_browser_cache();
                // LOOT can show source links before its user presses Sort
                // Plugins because these are ordinary install metadata, not a
                // sorting result. Do the same for MO2 folders: this is a
                // lightweight read of meta.ini only, and never touches the
                // modlist, rules, roadmap, or conflict cache.
                if let Some(mods_dir) = ConflictIndex::mods_dir_of(&path) {
                    for (_, mods) in &self.layout {
                        for name in mods {
                            if let Some(source) = source_from_meta(&mods_dir, name) {
                                self.mod_sources.insert(name.clone(), source);
                            }
                        }
                    }
                }
                // This is intentionally the one bit of profile setup we do
                // at launch. A roadmap is a tiny text bridge for separator
                // names, not a mod analysis; creating it lets the user
                // review their layout before asking MS to sort anything.
                match crate::section_map::load_or_create(&path, &self.sections) {
                    Ok(roadmap) => {
                        self.roadmap_path = Some(roadmap.path.clone());
                        self.roadmap_ambiguous = roadmap.review_concepts();
                        self.roadmap_reviewed = roadmap.reviewed_mappings();
                        self.roadmap_multi = roadmap.multi_destination_concepts();
                        self.roadmap_branches = roadmap.branch_routes();
                        self.roadmap_destinations = self
                            .sections
                            .iter()
                            .filter(|label| roadmap.is_automatic_destination(label))
                            .cloned()
                            .collect();
                        self.roadmap_context = self
                            .sections
                            .iter()
                            .filter(|label| roadmap.is_context(label))
                            .cloned()
                            .collect();
                    }
                    Err(e) => self.debug_note = Some(format!("separator roadmap unavailable: {e}")),
                }
                // Do not leave the LOOT-style card stream blank until a
                // sort. The top visible MOD is the final disk child, except
                // for profile-defined [context] presentation rows.
                self.selected_mod = self.layout.iter().rev().find_map(|(section, mods)| {
                    (!self.roadmap_context.contains(section))
                        .then(|| mods.last().map(|name| (name.clone(), section.clone())))
                        .flatten()
                });
            }
            Err(e) => {
                self.layout.clear();
                self.parking_mods.clear();
                self.sections.clear();
                self.status = format!("couldn't read {}: {e}", path.display());
                return;
            }
        }
        self.status = format!(
            "ready: {} — click Sort Mods when you want MS to do work",
            path.parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("active profile")
        );
    }

    /// Rebuild only when the list or plan changes, never from `update()`.
    /// Keys preserve the displayed spelling while values are the normalized
    /// terms used by the left search field.
    fn refresh_left_browser_cache(&mut self) {
        self.left_change_map.clear();
        for (index, change) in self.plan.iter().enumerate() {
            self.left_change_map
                .entry(change.name.clone())
                .or_default()
                .push(index);
        }
        self.left_search_terms.clear();
        for (section, mods) in &self.layout {
            self.left_search_terms
                .insert(section.clone(), section.to_ascii_lowercase());
            for name in mods {
                self.left_search_terms
                    .insert(name.clone(), name.to_ascii_lowercase());
            }
        }
        for name in &self.parking_mods {
            self.left_search_terms
                .insert(name.clone(), name.to_ascii_lowercase());
        }
    }

    // LOOT calls this "Discard Sorted Load Order".  MS has only built an
    // in-memory preview at this point, so discarding is a read-only reload of
    // the current MO2 file: no rules, plugins, or modlist changes are saved.
    fn discard_sorted_preview(&mut self) {
        if let Some(path) = self.file.clone() {
            self.open_idle(path);
            self.status = "sorted ModSlut preview discarded; nothing was written".into();
        }
    }

    fn load_and_preview(&mut self, path: PathBuf) {
        self.load_preview(path, false, false, false, false);
    }

    // A completed conflict crawl only adds conflict evidence. It must not
    // erase the already-evaluated LOOT facts from the completed Sort—the old
    // refresh did exactly that, leaving the real Index and Bash-tag columns
    // blank a moment after they had been populated.
    fn refresh_after_conflict_scan(&mut self, path: PathBuf) {
        let sources = self.mod_sources.clone();
        let bash_tags = self.mod_bash_tags.clone();
        let indexes = self.mod_plugin_indexes.clone();
        let plugin_sorted = self.plugin_sorted.clone();
        let plugin_rows = self.plugin_rows.clone();
        self.load_and_preview(path);
        self.mod_sources = sources;
        self.mod_bash_tags = bash_tags;
        self.mod_plugin_indexes = indexes;
        self.plugin_sorted = plugin_sorted;
        self.plugin_rows = plugin_rows;
    }

    fn draw_sort_loader(&self, ctx: &egui::Context) {
        if !self.sort_waiting {
            return;
        }

        // The old Windows slash spinner, in red because MS is currently
        // sweating through somebody's giant modlist. Unlike the first pass,
        // this really animates: sorting lives on a worker thread now.
        let rect = ctx.screen_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("modslut_sort_wait"),
        ));
        painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(150));
        let center = rect.center();
        let radius = 43.0;
        // Old Windows spinners had a calm little churn, not a coffee-fueled
        // blur. Keep this readable while a large list is being indexed.
        let phase = ctx.input(|input| input.time) as f32 * 2.0;
        for i in 0..16 {
            let angle = std::f32::consts::TAU * i as f32 / 16.0 + phase;
            let pos = center + egui::vec2(angle.cos() * radius, angle.sin() * radius);
            let glow = i as f32 / 15.0;
            let color = egui::Color32::from_rgb(
                (92.0 + 163.0 * glow) as u8,
                (5.0 + 48.0 * glow) as u8,
                (9.0 + 33.0 * glow) as u8,
            );
            // Radial bars, not tangent bars. Tangent bars politely connect
            // into a hula hoop, which is not the vintage Windows look.
            let radial = egui::vec2(angle.cos(), angle.sin());
            painter.line_segment(
                [pos - radial * 8.0, pos + radial * 8.0],
                egui::Stroke::new(5.0_f32, color),
            );
        }
        painter.text(
            center,
            egui::Align2::CENTER_CENTER,
            "Loading",
            egui::FontId::proportional(self.font_size * 1.05),
            egui::Color32::WHITE,
        );
        painter.text(
            center + egui::vec2(0.0, 88.0),
            egui::Align2::CENTER_TOP,
            "Please wait",
            egui::FontId::proportional(self.font_size * 1.55),
            egui::Color32::WHITE,
        );
        painter.text(
            center + egui::vec2(0.0, 121.0),
            egui::Align2::CENTER_TOP,
            "ModSlut is mapping the mess…",
            egui::FontId::proportional(self.font_size * 0.9),
            DIM,
        );
    }

    fn start_sort_worker(&mut self, path: PathBuf, solve_mods: bool, solve_plugins: bool) {
        if self.sort_rx.is_some() {
            return;
        }
        self.sort_waiting = true;
        self.status = match (solve_mods, solve_plugins) {
            (true, true) => "sorting mods and plugins - please wait…".into(),
            (true, false) => "sorting mods - please wait…".into(),
            (false, true) => "sorting plugins - please wait…".into(),
            (false, false) => return,
        };
        let user_rules_only = self.user_rules_only;
        let (tx, rx) = mpsc::channel();
        self.sort_rx = Some(rx);
        std::thread::spawn(move || {
            // A worker gets its own throwaway app state. It only reads and
            // plans; the UI thread remains the only place that can apply a
            // modlist write.
            let mut worker = ModslutApp::new(UiPrefs {
                user_rules_only,
                ..UiPrefs::default()
            });
            worker.user_rules_only = user_rules_only;
            // `load_preview` builds the complete plan state. Calling
            // `open_idle` first used to reread every meta.ini merely to
            // throw that inventory away a moment later in the real analysis.
            // Keep the file context needed for profile-local keywords, but
            // avoid that duplicate 2k-folder pass.
            worker.file = Some(path.clone());
            worker.analyze_and_preview(path, solve_mods, solve_plugins);
            let _ = tx.send(Box::new(worker));
        });
    }

    fn start_masterlist_update(&mut self) {
        if self.masterlist_update_rx.is_some() {
            return;
        }
        let Some(masterlist) = self.masterlist_path.clone() else {
            self.status = "sort once first so ModSlut can identify LOOT's active masterlist".into();
            return;
        };
        let Some(source) = self.masterlist_source.clone() else {
            self.status = "LOOT has no configured masterlist source for this game".into();
            return;
        };
        self.status = "updating LOOT masterlist - please wait…".into();
        let (tx, rx) = mpsc::channel();
        self.masterlist_update_rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(crate::loot::update_masterlist(&masterlist, &source));
        });
    }

    // Opening MS must be cheap.  The explicit Sort Mods action below asks
    // for the expensive plugin/content/conflict work; ordinary reloads only
    // consume fresh caches and rebuild the visible preview.
    fn analyze_and_preview(&mut self, path: PathBuf, solve_mods: bool, solve_plugins: bool) {
        self.load_preview(path, true, true, solve_mods, solve_plugins);
    }

    fn load_preview(
        &mut self,
        path: PathBuf,
        analyze: bool,
        record_observation: bool,
        solve_mods: bool,
        solve_plugins: bool,
    ) {
        let preview_started = Instant::now();
        self.plan.clear();
        self.sorted = None;
        self.selected = None;
        self.selected_mod = None;
        self.debug_note = None;
        self.written = false;
        self.human_metadata = HumanMetadata::load(&path);

        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("couldn't read {}: {e}", path.display());
                return;
            }
        };
        let mut ml: Modlist = parse(&text);
        (
            self.mod_priorities,
            self.mod_states,
            self.separator_priorities,
        ) = mo2_priority_view(&text);
        // `modlist.txt` itself remains untouched. MS mirrors it into a
        // profile-local priority manifest and reads that validated backbone
        // before any rule or libloot projection gets to rearrange a section.
        let priority_note = match crate::priority_manifest::load_or_refresh(&path, &text) {
            Ok(manifest) => {
                for section in &mut ml.sections {
                    section.mods.sort_by_key(|entry| {
                        std::cmp::Reverse(
                            *manifest
                                .priorities
                                .get(&entry.name.to_ascii_lowercase())
                                .unwrap_or(&0),
                        )
                    });
                }
                Some(format!(
                    "priority manifest: {} folder priorities {}",
                    manifest.priorities.len(),
                    if manifest.refreshed {
                        "refreshed from modlist.txt"
                    } else {
                        "validated"
                    },
                ))
            }
            Err(error) => Some(format!(
                "priority manifest unavailable: {error}; using live modlist order"
            )),
        };
        // Initialize the profile roadmap before rules or sort passes. It
        // records separator vocabulary but makes no placement decision yet.
        let section_labels: Vec<String> = ml.sections.iter().map(|s| s.label.clone()).collect();
        // The base group graph is always the separator order. Extra edges are
        // optional profile knowledge created in the Game > Edit Groups UI.
        // Reading it here is deliberately non-mutating: opening/reloading a
        // list must not create or rewrite a group file.
        let group_path = crate::groups::path_for(&path);
        let group_edges = if group_path.exists() {
            crate::groups::parse(&group_path, &section_labels).unwrap_or_default()
        } else {
            Vec::new()
        };
        let roadmap = match crate::section_map::load_or_create(&path, &section_labels) {
            Ok(map) => Some(map),
            Err(e) => {
                self.debug_note = Some(format!("separator roadmap unavailable: {e}"));
                None
            }
        };
        let roadmap_note = roadmap.as_ref().and_then(|map| {
            map.created
                .then(|| format!("separator roadmap created: {}", map.path.display()))
        });
        self.roadmap_path = roadmap.as_ref().map(|map| map.path.clone());
        self.roadmap_ambiguous = roadmap
            .as_ref()
            .map(|map| map.review_concepts())
            .unwrap_or_default();
        self.roadmap_reviewed = roadmap
            .as_ref()
            .map(|map| map.reviewed_mappings())
            .unwrap_or_default();
        self.roadmap_multi = roadmap
            .as_ref()
            .map(|map| map.multi_destination_concepts())
            .unwrap_or_default();
        self.roadmap_branches = roadmap
            .as_ref()
            .map(|map| map.branch_routes())
            .unwrap_or_default();
        self.roadmap_destinations = roadmap
            .as_ref()
            .map(|map| {
                section_labels
                    .iter()
                    .filter(|label| map.is_automatic_destination(label))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        self.roadmap_context = roadmap
            .as_ref()
            .map(|map| {
                section_labels
                    .iter()
                    .filter(|label| map.is_context(label))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        self.roadmap_picks.clear();
        let (mut rules, user_files) = load_rules_for(None, Some(&path), self.user_rules_only);
        if self.user_rules_only {
            // "apply user rules only": the built-in cascade AND the guess
            // tiers stay out - user pins + proven data (loot/conflict/
            // census/family) are the only things allowed to move a mod
            rules.proven_only = true;
        }
        let cats = Categories::discover(&path);

        // conflict index: fresh cache loads instantly; a stale/missing one
        // gets rebuilt on a background thread so the window stays alive, and
        // the preview re-runs with conflict data once the scan lands
        self.conflicts = None;
        self.scan_rx = None;
        if let Some(mods_dir) = ConflictIndex::mods_dir_of(&path) {
            // fresh AND stamped for this instance (a cache next to the exe
            // may belong to a different instance - that's a rescan, not a load)
            let cached = if ConflictIndex::is_fresh(&path) {
                ConflictIndex::ini_path(&path)
                    .and_then(|ini| ConflictIndex::load_checked(&ini, &mods_dir))
            } else {
                None
            };
            if let Some(ci) = cached {
                self.conflicts = Some(Arc::new(ci));
            } else if !analyze {
                // No startup crawl. Sort Mods is the user's explicit "go do
                // work" button; until then a missing conflict cache simply
                // means the corresponding guardrail is unavailable.
            } else if SCAN_ACTIVE.swap(true, std::sync::atomic::Ordering::SeqCst) {
                // a scan is ALREADY walking the mods folder (kicked off by a
                // previous reload). spawning another full walk per reload is
                // how we get N parallel scans x hundreds of MB of path lists
                // = a frozen machine. wait for the in-flight one to finish;
                // it saves conflict.ini, so the next reload picks it up fresh.
                self.status =
                    "conflict scan still running in background - reload in a moment to use it"
                        .into();
            } else {
                let active = active_mods(&ml);
                let ini_path = ConflictIndex::ini_path(&path);
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    // panic-safe: even if the scan dies, the flag resets so
                    // the next reload can try again instead of waiting forever
                    struct ResetFlag;
                    impl Drop for ResetFlag {
                        fn drop(&mut self) {
                            SCAN_ACTIVE.store(false, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                    let _guard = ResetFlag;
                    let ci = ConflictIndex::build(&mods_dir, &active);
                    if let Some(ini) = ini_path {
                        ci.save(&ini, &mods_dir).ok();
                    }
                    tx.send(ci).ok();
                });
                self.scan_rx = Some(rx);
            }
        }

        self.cats_report = cats
            .as_ref()
            .map(|c| category_report(&ml, c, &rules, roadmap.as_ref()));
        self.show_cats = false;
        let mut log = String::new();
        // snapshot the current layout BEFORE run() applies its moves, so the
        // full-list view shows where mods actually sit right now
        self.layout = ml
            .sections
            .iter()
            .map(|s| {
                (
                    s.label.clone(),
                    s.mods.iter().map(|m| m.name.clone()).collect(),
                )
            })
            .collect();
        self.parking_mods = ml
            .parking
            .iter()
            .filter(|l| !l.trim().starts_with('#'))
            .map(|l| {
                l.trim()
                    .trim_start_matches(['+', '-', '*'])
                    .trim()
                    .to_string()
            })
            .filter(|l| !l.is_empty())
            .collect();
        // (name, section) for every ticked mod - the platform guard's input
        let enabled: Vec<(String, String)> = ml
            .sections
            .iter()
            .flat_map(|s| {
                s.mods
                    .iter()
                    .filter(|m| m.raw.trim_start().starts_with('+'))
                    .map(|m| (m.name.clone(), s.label.clone()))
            })
            .collect();
        // Cheap inventory only: filename -> MO2 owner. libloot reads the
        // active plugin headers itself, so MS must not crack open every
        // plugin's records just to sort the left pane.
        let owner_sources: Vec<(String, String)> = ml
            .sections
            .iter()
            .flat_map(|s| s.mods.iter().map(|m| (m.name.clone(), s.label.clone())))
            .collect();
        let plugins_path = path.with_file_name("plugins.txt");
        let plugin_text = std::fs::read_to_string(&plugins_path).ok();
        let plugin_order: Vec<String> = plugin_text
            .as_deref()
            .map(|t| {
                t.lines()
                    .filter_map(|l| l.trim().strip_prefix('*').map(|s| s.trim().to_lowercase()))
                    .collect()
            })
            .unwrap_or_default();
        let plugin_owners = if analyze {
            ConflictIndex::mods_dir_of(&path)
                .map(|mods_dir| crate::plugins::plugin_owners(&owner_sources, &mods_dir))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let ownership_elapsed = preview_started.elapsed();
        // Provenance is intentionally a Sort-time read, just like the rest
        // of the meaningful work. Opening MS remains inert; after a preview,
        // the right-side LOOT-style cards can show the same local source line
        // for pluginless folders and plugin owners alike.
        self.mod_sources.clear();
        self.mod_bash_tags.clear();
        self.mod_plugin_indexes.clear();
        self.plugin_sorted = None;
        self.plugin_rows.clear();
        if analyze && solve_plugins {
            if let Some(mods_dir) = ConflictIndex::mods_dir_of(&path) {
                for (name, _) in &owner_sources {
                    if let Some(source) = source_from_meta(&mods_dir, name) {
                        self.mod_sources.insert(name.clone(), source);
                    }
                }
            }
        }
        // Content extraction is deliberately offline/optional now. Normal
        // sorting is libloot + names/keywords/roadmap/user knowledge only.
        self.content_index = None;

        let mut plan = Vec::new();
        let mut trace = String::new();
        if let Some(note) = priority_note {
            let _ = writeln!(trace, "{note}");
        }
        let (kw, _kw_files) = crate::Keywords::load(self.file.as_deref());
        let reference = crate::reference::ReferenceIndex::load(&path);
        // A Sort compares the current MO2 layout to the last explicit Sort
        // (or Apply). This learns deliberate *section* moves made while MS
        // was closed, but never blesses MS's own proposed output.
        let mut learning = crate::learning::LearningIndex::observe(&path, &text);
        match learning.learn_profile_pins(&path, &section_labels) {
            Ok(n) if n > 0 => {
                let _ = writeln!(trace, "manual-learning: learned {n} explicit profile pin(s) as durable homes; removing the pin will not forget them");
            }
            Ok(_) => {}
            Err(e) => {
                let _ = writeln!(
                    trace,
                    "manual-learning: couldn't save explicit profile pins as learned homes ({e})"
                );
            }
        }
        if learning.observed_corrections > 0 {
            let _ = writeln!(trace, "manual-learning: captured {} manual section correction(s) since the last Sort/Apply", learning.observed_corrections);
        }
        if learning.observed_priority_adjustments > 0 {
            let _ = writeln!(trace, "manual-learning: observed {} manual in-section priority adjustment(s); the refreshed priority backbone preserves them", learning.observed_priority_adjustments);
        }
        if learning.corrections > 0 {
            let _ = writeln!(
                trace,
                "manual-learning: {} saved profile home(s) active",
                learning.corrections
            );
        }
        if record_observation {
            match crate::learning::save_observation(&path, &text) {
                Ok(()) => {
                    let _ = writeln!(
                        trace,
                        "manual-learning: recorded this Sort's MO2 layout for the next comparison"
                    );
                }
                Err(e) => {
                    let _ = writeln!(
                        trace,
                        "manual-learning: couldn't record Sort observation ({e})"
                    );
                }
            }
        }
        // libloot solves the real plugin graph first.  Project its final
        // plugin positions back to MO2 owning mods (the inverse of
        // PluginSync's mod-priority -> plugin-order trick). A mod may ship
        // several plugins, so its latest plugin rank is the safe placement
        // anchor: patches win.
        let mut libloot_mod_rank = HashMap::<String, usize>::new();
        let mut libloot_dependencies = HashMap::<String, Vec<String>>::new();
        let mut libloot_note = None::<String>;
        let libloot_started = Instant::now();
        if analyze && solve_plugins {
            if let Some(mods_dir) = ConflictIndex::mods_dir_of(&path) {
                match crate::libloot_adapter::compare(
                    &path,
                    &mods_dir,
                    &plugin_owners,
                    &plugin_order,
                    &section_labels,
                    &group_edges,
                    self.default_game,
                ) {
                    Ok(result) => {
                        self.detected_game = result.game_name.clone();
                        self.active_plugin_count = Some(result.plugin_count);
                        self.active_full_plugin_count = Some(result.active_full_plugins);
                        self.active_light_plugin_count = Some(result.active_light_plugins);
                        self.dirty_plugin_count = Some(result.dirty_plugins);
                        self.loot_warning_count = Some(result.warning_count);
                        self.loot_error_count = Some(result.error_count);
                        self.loot_message_count = Some(result.total_messages);
                        self.loot_general_messages = result.general_messages.clone();
                        self.mod_bash_tags = result.bash_tags_by_owner.clone();
                        let mut indexes_by_owner = HashMap::<String, Vec<String>>::new();
                        for owner in &plugin_owners {
                            if let Some(index) = result.plugin_indexes.get(&owner.plugin) {
                                indexes_by_owner
                                    .entry(owner.mod_name.clone())
                                    .or_default()
                                    .push(index.clone());
                            }
                        }
                        self.mod_plugin_indexes = indexes_by_owner
                            .into_iter()
                            .map(|(owner, mut indexes)| {
                                indexes.sort();
                                indexes.dedup();
                                (owner, indexes.join(", "))
                            })
                            .collect();
                        self.masterlist_revision = result.masterlist_status.revision.clone();
                        self.masterlist_updated = result.masterlist_status.updated.clone();
                        self.masterlist_source = result.masterlist_status.source.clone();
                        self.masterlist_path = Some(result.masterlist.clone());
                        // Later MO2 folders win duplicate plugin names. Use
                        // that same winner when translating libloot's header
                        // master links back to their owning left-pane mods.
                        let mut owner_of_plugin = HashMap::<String, String>::new();
                        for owner in &plugin_owners {
                            owner_of_plugin.insert(owner.plugin.clone(), owner.mod_name.clone());
                        }
                        for (rank, plugin) in result.sorted_plugins.iter().enumerate() {
                            for owner in plugin_owners
                                .iter()
                                .filter(|owner| owner.plugin.eq_ignore_ascii_case(plugin))
                            {
                                libloot_mod_rank
                                    .entry(owner.mod_name.clone())
                                    .and_modify(|old| *old = (*old).max(rank))
                                    .or_insert(rank);
                            }
                        }
                        for (plugin, masters) in &result.plugin_masters {
                            let Some(child) = owner_of_plugin.get(plugin) else {
                                continue;
                            };
                            let parents = libloot_dependencies.entry(child.clone()).or_default();
                            for master in masters {
                                if master.starts_with("cc") {
                                    continue;
                                }
                                if let Some(parent) = owner_of_plugin.get(master) {
                                    if parent != child {
                                        parents.push(parent.clone());
                                    }
                                }
                            }
                        }
                        libloot_dependencies.retain(|_, parents| {
                            parents.sort();
                            parents.dedup();
                            !parents.is_empty()
                        });
                        // libloot works case-insensitively and returns its
                        // normalised filename keys. Keep MO2's spelling for
                        // the LOOT-style table: it is easier to compare to
                        // the user's plugin list and looks like LOOT.
                        let plugin_display: HashMap<String, String> = plugin_text
                            .as_deref()
                            .map(|text| {
                                text.lines()
                                    .filter_map(|line| line.trim_start().strip_prefix('*'))
                                    .map(|name| name.trim())
                                    .filter(|name| !name.is_empty())
                                    .map(|name| (name.to_ascii_lowercase(), name.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default();
                        self.plugin_rows = result
                            .sorted_plugins
                            .iter()
                            .enumerate()
                            .map(|(position, plugin)| PluginRow {
                                position: position + 1,
                                index: result
                                    .plugin_indexes
                                    .get(plugin)
                                    .cloned()
                                    .unwrap_or_default(),
                                name: plugin_display
                                    .get(&plugin.to_ascii_lowercase())
                                    .cloned()
                                    .unwrap_or_else(|| plugin.clone()),
                                owner: owner_of_plugin.get(plugin).cloned(),
                            })
                            .collect();
                        if let Some(text) = plugin_text.as_deref() {
                            match plugin_order_preview(text, &result.sorted_plugins) {
                                Ok((preview, changed)) => {
                                    if changed > 0 {
                                        self.plugin_sorted = Some(preview);
                                    }
                                }
                                Err(reason) => {
                                    let _ = writeln!(trace, "plugin-order preview held: {reason}");
                                }
                            }
                        }
                        libloot_note = Some(format!(
                            "\n--- libloot engine (authoritative plugin projection) ---\n  game: {} ({})\n  masterlist: {}\n  LOOT facts: {} full + {} light active plugin(s), {} dirty, {} warning(s), {} error(s), {} total message(s)\n  separator graph: {} fluid node(s), {} membership/memberships injected; {} existing LOOT group(s) retained\n  plugin order: {} active file(s), {} position(s) differ from the active MO2 baseline\n  projected {} owning mod rank(s) and {} declared-master mod link(s) from {} cheap filename ownership record(s)\n  plugin contents were not extracted; pluginless mods use names, keywords, roadmaps, and user knowledge\n  no plugins.txt, modlist.txt, or LOOT metadata changes were made by libloot",
                            result.game_name,
                            result.game_path_source,
                            result.masterlist.display(),
                            result.active_full_plugins,
                            result.active_light_plugins,
                            result.dirty_plugins,
                            result.warning_count,
                            result.error_count,
                            result.total_messages,
                            result.separator_groups,
                            result.separator_memberships,
                            result.retained_loot_groups,
                            result.plugin_count,
                            result.changed_positions,
                            libloot_mod_rank.len(),
                            libloot_dependencies.values().map(Vec::len).sum::<usize>(),
                            plugin_owners.len(),
                        ));
                    }
                    Err(reason) => {
                        libloot_note =
                            Some(format!("\n--- libloot engine ---\n  skipped: {reason}"));
                    }
                }
            }
        }
        let libloot_elapsed = libloot_started.elapsed();
        let core_sort_started = Instant::now();
        if solve_mods {
            run(
                &mut ml,
                &rules,
                &mut log,
                &mut plan,
                cats.as_ref(),
                self.conflicts.as_deref(),
                None,
                &kw,
                roadmap.as_ref(),
                None,
                (!self.user_rules_only).then_some(&reference),
                (!self.user_rules_only).then_some(&learning),
                (!libloot_mod_rank.is_empty()).then_some(&libloot_mod_rank),
                (!libloot_dependencies.is_empty()).then_some(&libloot_dependencies),
                self.default_game,
                false,
                &mut trace,
            );
        } else {
            let _ = writeln!(
                trace,
                "ModSlut section sorting: skipped (Sort Plugins only)"
            );
        }
        let core_sort_elapsed = core_sort_started.elapsed();
        let _ = writeln!(
            trace,
            "\n--- timing ---\n  setup + plugin ownership walk: {ownership_elapsed:?}\n  libloot header/metadata/sort: {libloot_elapsed:?}\n  ModSlut routing + priority constraints: {core_sort_elapsed:?}",
        );
        if let Some(note) = libloot_note {
            let _ = writeln!(trace, "{note}");
        }
        if reference.source_count() > 0 {
            let _ = writeln!(
                trace,
                "private reference: {} exact mod(s) have human placement evidence",
                reference.source_count()
            );
        }
        self.trace = trace;
        if let Some(index) = &self.content_index {
            let _ = writeln!(
                self.trace,
                "\n--- content index ({}) ---\n  cache: {} ({})",
                index.summary(),
                if index.from_cache { "hit" } else { "built" },
                index.path.display(),
            );
        }
        self.sections = ml.sections.iter().map(|s| s.label.clone()).collect();

        // platform guard: ae/oldrim skse plugins living in a vr profile.
        // display-only WARN rows - they never touch the serialized output
        let mut n_warns = 0usize;
        let platform_scan_started = Instant::now();
        if analyze {
            if let Some(mods_dir) = ConflictIndex::mods_dir_of(&path) {
                for w in crate::platform_scan(&enabled, &mods_dir) {
                    n_warns += 1;
                    let _ = writeln!(
                        self.trace,
                        "WARN  {} [{}] {}",
                        w.mod_name,
                        w.kind.label(),
                        w.dll
                    );
                    plan.push(Change {
                        kind: ChangeKind::Warn,
                        name: w.mod_name,
                        detail: format!("[{}] {}", w.kind.label(), w.dll),
                        section: w.section,
                    });
                }
            }
        }
        if analyze {
            let _ = writeln!(
                self.trace,
                "  VR DLL guard walk: {:?}\n  total foreground sort: {:?}\n  note: a stale conflict cache is rebuilt separately in the background and may still be walking files after this plan appears",
                platform_scan_started.elapsed(),
                preview_started.elapsed(),
            );
        }

        // the debug trace is always on disk, no button required - nobody
        // remembers to click "debug log" before reporting a bug
        if !self.trace.is_empty() {
            let p = crate::debug_log_path();
            self.debug_note = match std::fs::write(&p, &self.trace) {
                Ok(_) => Some(p.display().to_string()),
                Err(e) => Some(format!("(debug log write failed: {e})")),
            };
        }

        // The browser and right-hand cards must now describe the planned
        // result, not the disk-order snapshot that existed before Sort Mods.
        // `mo2_priority_view` deliberately converts native disk order into
        // MO2's human-visible position numbering.
        let preview = serialize(&ml);
        self.layout = ml
            .sections
            .iter()
            .map(|section| {
                (
                    section.label.clone(),
                    section.mods.iter().map(|m| m.name.clone()).collect(),
                )
            })
            .collect();
        (
            self.mod_priorities,
            self.mod_states,
            self.separator_priorities,
        ) = mo2_priority_view(&preview);
        self.sorted = solve_mods.then_some(preview);
        self.file = Some(path);
        self.plan = plan;
        self.refresh_left_browser_cache();
        let mut cat_note = if cats.is_some() {
            " (mo2 categories loaded)".to_string()
        } else {
            " (no mo2 categories found - keyword rules only)".to_string()
        };
        if !user_files.is_empty() {
            cat_note += &format!(" + {} user rules file(s)", user_files.len());
        }
        if let Some(ci) = &self.conflicts {
            cat_note += &format!(" + {} conflict pair(s)", ci.pairs.len());
        } else if self.scan_rx.is_some() {
            cat_note += " - conflict scan running in background, preview will refresh";
        }
        if n_warns > 0 {
            cat_note += &format!(" + {n_warns} warning(s)");
        }
        if let Some(note) = &self.debug_note {
            cat_note += &format!(" · log: {note}");
        }
        if let Some(note) = roadmap_note {
            cat_note += &format!(" · {note}");
        }
        self.status = match (self.plan.len(), self.plugin_sorted.is_some()) {
            (0, false) => format!("list's already clean{cat_note}"),
            (0, true) => format!(
                "plugin order preview ready{cat_note} - review, then apply if it looks good"
            ),
            (n, _) => {
                format!("{n} change(s) ready{cat_note} - review, then apply if it looks good")
            }
        };
    }

    // Writes the reviewed MO2 folder and plugin previews with sibling .bak
    // files. Both previews are inert until this explicit Apply action.
    fn apply(&mut self, write_mods: bool, write_plugins: bool) {
        let Some(file) = self.file.as_ref() else {
            return;
        };
        let sorted = self.sorted.clone();
        if write_mods && sorted.is_none() {
            return;
        }
        let plugin_preview = write_plugins.then(|| self.plugin_sorted.clone()).flatten();
        if write_plugins && plugin_preview.is_none() {
            return;
        }
        let plugins = file.with_file_name("plugins.txt");
        if let Some(preview) = plugin_preview {
            let plugin_bak = plugins.with_file_name("plugins.txt.bak");
            if let Err(e) = std::fs::copy(&plugins, &plugin_bak) {
                self.status = format!("plugin backup failed ({e}) - not touching either order");
                return;
            }
            if let Err(e) = std::fs::write(&plugins, preview) {
                self.status = format!(
                    "couldn't write {}: {e} - modlist untouched",
                    plugins.display()
                );
                return;
            }
        }
        if !write_mods {
            self.written = true;
            self.status = "applied plugin order; requested MO2 refresh (F5)".into();
            self.request_mo2_refresh();
            return;
        }
        let bak = file.with_file_name(format!(
            "{}.bak",
            file.file_name().unwrap_or_default().to_string_lossy()
        ));
        if let Err(e) = std::fs::copy(file, &bak) {
            self.status = format!("backup failed ({e}) - not touching anything");
            return;
        }
        let sorted = sorted.expect("checked mod preview");
        match std::fs::write(file, &sorted) {
            Ok(()) => {
                self.written = true;
                self.status = match crate::learning::save_baseline(file, &sorted)
                    .and_then(|()| crate::learning::save_observation(file, &sorted))
                {
                    Ok(()) => format!(
                        "applied {}. Sort baseline saved; requested MO2 refresh (F5)",
                        if write_plugins { "mods and plugins" } else { "mods" }
                    ),
                    Err(e) => format!(
                        "applied, but couldn't save the learning baseline ({e}). backup at {} - close this window and mo2 will refresh",
                        bak.display()
                    ),
                };
            }
            Err(e) => self.status = format!("couldn't write {}: {e}", file.display()),
        }
        if self.written {
            self.request_mo2_refresh();
        }
    }

    fn plugin_order_table(&mut self, ui: &mut egui::Ui) {
        if self.plugin_rows.is_empty() {
            return;
        }
        let changed = self.plugin_sorted.is_some();
        ui.label(
            egui::RichText::new(format!(
                "Plugins · {} active{}",
                self.plugin_rows.len(),
                if changed {
                    " · sorted preview ready"
                } else {
                    ""
                }
            ))
            .strong()
            .color(if changed {
                REOR_CLR
            } else {
                egui::Color32::WHITE
            }),
        );
        ui.label(egui::RichText::new("libloot order · low → high").color(DIM));
        egui::Grid::new("solved_plugin_grid")
            .num_columns(3)
            .spacing([12.0, 3.0])
            .striped(true)
            .show(ui, |ui| {
                ui.add_sized(
                    egui::vec2(54.0, 18.0),
                    egui::Label::new(egui::RichText::new("Position").strong()),
                );
                ui.add_sized(
                    egui::vec2(60.0, 18.0),
                    egui::Label::new(egui::RichText::new("Index").strong()),
                );
                ui.add_sized(
                    egui::vec2(200.0, 18.0),
                    egui::Label::new(egui::RichText::new("Plugin Name").strong()),
                );
                ui.end_row();
                let needle = self.right_search.trim().to_ascii_lowercase();
                for row in self.plugin_rows.clone() {
                    if !needle.is_empty()
                        && !row.name.to_ascii_lowercase().contains(&needle)
                        && !row
                            .owner
                            .as_deref()
                            .is_some_and(|owner| owner.to_ascii_lowercase().contains(&needle))
                    {
                        continue;
                    }
                    ui.add_sized(
                        egui::vec2(54.0, 18.0),
                        egui::Label::new(
                            egui::RichText::new(row.position.to_string())
                                .monospace()
                                .color(DIM),
                        ),
                    );
                    ui.add_sized(
                        egui::vec2(60.0, 18.0),
                        egui::Label::new(
                            egui::RichText::new(&row.index).monospace().color(REOR_CLR),
                        ),
                    );
                    let mut response = ui.add_sized(
                        egui::vec2(200.0, 18.0),
                        egui::Label::new(&row.name)
                            .truncate()
                            .sense(egui::Sense::click()),
                    );
                    if let Some(owner) = row.owner.as_deref() {
                        response = response.on_hover_text(format!(
                            "MO2 owner: {owner}\nRight-click for ModSlut actions."
                        ));
                    } else {
                        response = response.on_hover_text("Game Folder plugin (unmanaged by MO2).");
                    }
                    let section = row.owner.as_deref().and_then(|owner| {
                        self.layout.iter().find_map(|(section, mods)| {
                            mods.iter()
                                .any(|name| name == owner)
                                .then(|| section.clone())
                        })
                    });
                    self.plugin_row_menu(
                        &response,
                        &row.name,
                        row.owner.as_deref(),
                        section.as_deref(),
                    );
                    ui.end_row();
                }
            });
    }

    // "apply user rules only": write back a modlist where ONLY the user's
    // own profile-named ModSlut rules have been applied - no built-ins, no
    // guesses, no loot/family/census/conflict passes. for applying your pins
    // without mass-accepting the whole computed plan. Returns true when it
    // is safe for the caller to close the window afterwards.
    fn apply_user_rules_only(&mut self) -> bool {
        let Some(file) = self.file.clone() else {
            return false;
        };
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("couldn't read {}: {e}", file.display());
                return false;
            }
        };
        let (rules, user_files) = load_rules_for(None, Some(&file), true);
        if user_files.is_empty() {
            self.status = "no profile-named ModSlut rules found - nothing of yours to apply".into();
            return false;
        }
        let mut ml: Modlist = parse(&text);
        let section_labels: Vec<String> = ml.sections.iter().map(|s| s.label.clone()).collect();
        let roadmap = crate::section_map::load_or_create(&file, &section_labels).ok();
        let cats = Categories::discover(&file);
        let (kw, _) = crate::Keywords::load(Some(file.as_path()));
        let mut log = String::new();
        let mut plan = Vec::new();
        let mut trace = String::new();
        let n = run(
            &mut ml,
            &rules,
            &mut log,
            &mut plan,
            cats.as_ref(),
            None,
            None,
            &kw,
            roadmap.as_ref(),
            None,
            None,
            None,
            None,
            None,
            self.default_game,
            true,
            &mut trace,
        );
        let _ = std::fs::write(crate::debug_log_path(), &trace);
        if n == 0 {
            self.status = "your user rules change nothing - the list already matches them".into();
            return true;
        }
        let bak = file.with_file_name(format!(
            "{}.bak",
            file.file_name().unwrap_or_default().to_string_lossy()
        ));
        if let Err(e) = std::fs::copy(&file, &bak) {
            self.status = format!("backup failed ({e}) - not touching anything");
            return false;
        }
        match std::fs::write(&file, serialize(&ml)) {
            Ok(()) => {
                let baseline = serialize(&ml);
                let learning_note = match crate::learning::save_baseline(&file, &baseline)
                    .and_then(|()| crate::learning::save_observation(&file, &baseline))
                {
                    Ok(()) => " Sort baseline saved for manual-placement learning.".to_string(),
                    Err(e) => format!(" learning baseline failed ({e})."),
                };
                self.status = format!(
                    "applied {n} user-rule change(s) only.{learning_note} backup at {} - close this window and mo2 will refresh",
                    bak.display(),
                );
                // refresh the preview against the new on-disk state
                self.load_and_preview(file);
                true
            }
            Err(e) => {
                self.status = format!("couldn't write {}: {e}", file.display());
                false
            }
        }
    }

    // LOOT-style context menu: opening the editor is harmless. Rules and
    // human metadata are drafted inline and written only through Save.
    fn row_menu(&mut self, resp: &egui::Response, name: &str, section: &str) {
        let mut clear = false;
        let plugins: Vec<String> = self
            .plugin_rows
            .iter()
            .filter(|row| row.owner.as_deref() == Some(name))
            .map(|row| row.name.clone())
            .collect();
        let source = self.mod_sources.get(name).cloned();
        let bash_tags = self.mod_bash_tags.get(name).cloned().unwrap_or_default();
        let card_text = format!(
            "Mod: {name}\nSection: {section}{}{}{}",
            if plugins.is_empty() {
                String::new()
            } else {
                format!("\nPlugin(s): {}", plugins.join(", "))
            },
            source
                .as_ref()
                .map(|value| format!("\nSource: {value}"))
                .unwrap_or_default(),
            if bash_tags.is_empty() {
                String::new()
            } else {
                format!("\nBash Tags: {}", bash_tags.join(", "))
            },
        );
        resp.context_menu(|ui| {
            ui.label(egui::RichText::new(name).strong().small());
            ui.separator();
            if ui.button("Edit Metadata…").clicked() {
                self.picker_for = Some((name.to_string(), section.to_string()));
                self.picker_filter.clear();
                self.promote_target.clear();
                self.metadata_tab = MetadataTab::Group;
                self.metadata_rule_draft = None;
                self.metadata_load_order_drafts.clear();
                self.metadata_human_drafts.clear();
                self.metadata_message.clear();
                ui.close_menu();
            }
            if !plugins.is_empty()
                && ui
                    .button(if plugins.len() == 1 {
                        "Copy Plugin Name"
                    } else {
                        "Copy Plugin Names"
                    })
                    .clicked()
            {
                ui.ctx().copy_text(plugins.join("\n"));
                ui.close_menu();
            }
            if ui.button("Copy Card Content").clicked() {
                ui.ctx().copy_text(card_text.clone());
                ui.close_menu();
            }
            if ui.button("Copy Metadata").clicked() {
                ui.ctx().copy_text(format!(
                    "{}{}",
                    source
                        .as_ref()
                        .map(|value| format!("Source: {value}"))
                        .unwrap_or_default(),
                    if bash_tags.is_empty() {
                        String::new()
                    } else {
                        format!("\nBash Tags: {}", bash_tags.join(", "))
                    },
                ));
                ui.close_menu();
            }
            if ui.button("Clear User Metadata…").clicked() {
                clear = true;
                ui.close_menu();
            }
        });
        if clear {
            self.clear_metadata(name);
        }
    }

    // The right-hand plugin rows get the same actions LOOT exposes. A Game
    // Folder plugin has no ModSlut-owned metadata to edit or clear, but it
    // still supports copying its solved card information.
    fn plugin_row_menu(
        &mut self,
        resp: &egui::Response,
        plugin: &str,
        owner: Option<&str>,
        section: Option<&str>,
    ) {
        let mut clear = false;
        let source = owner.and_then(|name| self.mod_sources.get(name)).cloned();
        let bash_tags = owner
            .and_then(|name| self.mod_bash_tags.get(name))
            .cloned()
            .unwrap_or_default();
        let card_text = format!(
            "Plugin: {plugin}\nMO2 owner: {}{}{}",
            owner.unwrap_or("Game Folder"),
            source
                .as_ref()
                .map(|value| format!("\nSource: {value}"))
                .unwrap_or_default(),
            if bash_tags.is_empty() {
                String::new()
            } else {
                format!("\nBash Tags: {}", bash_tags.join(", "))
            },
        );
        resp.context_menu(|ui| {
            ui.label(egui::RichText::new(plugin).strong().small());
            ui.separator();
            if let (Some(owner), Some(section)) = (owner, section) {
                if ui.button("Edit Metadata…").clicked() {
                    self.picker_for = Some((owner.to_string(), section.to_string()));
                    self.picker_filter.clear();
                    self.promote_target.clear();
                    self.metadata_tab = MetadataTab::Group;
                    self.metadata_rule_draft = None;
                    self.metadata_load_order_drafts.clear();
                    self.metadata_human_drafts.clear();
                    self.metadata_message.clear();
                    ui.close_menu();
                }
            }
            if ui.button("Copy Plugin Name").clicked() {
                ui.ctx().copy_text(plugin.to_string());
                ui.close_menu();
            }
            if ui.button("Copy Card Content").clicked() {
                ui.ctx().copy_text(card_text.clone());
                ui.close_menu();
            }
            if ui.button("Copy Metadata").clicked() {
                ui.ctx().copy_text(format!(
                    "{}{}",
                    source
                        .as_ref()
                        .map(|value| format!("Source: {value}"))
                        .unwrap_or_default(),
                    if bash_tags.is_empty() {
                        String::new()
                    } else {
                        format!("\nBash Tags: {}", bash_tags.join(", "))
                    },
                ));
                ui.close_menu();
            }
            if owner.is_some() && ui.button("Clear User Metadata…").clicked() {
                clear = true;
                ui.close_menu();
            }
        });
        if clear {
            if let Some(owner) = owner {
                self.clear_metadata(owner);
            }
        }
    }

    fn append_human_metadata(&mut self, kind: HumanMetadataKind, mod_name: &str, value: &str) {
        let Some(file) = self.file.clone() else {
            return;
        };
        match crate::metadata::append(&file, kind, mod_name, value) {
            Ok(()) => {
                self.human_metadata = HumanMetadata::load(&file);
                self.status = format!(
                    "human metadata saved for {mod_name} — reviewable at {}",
                    self.human_metadata.path.display()
                );
            }
            Err(e) => self.status = format!("couldn't save human metadata: {e}"),
        }
    }

    // LOOT gets this bit right: metadata belongs beside the item being
    // edited, not in a floating mystery box halfway across the screen.
    // Keep the controls intentionally ModSlut-sized. They edit a draft;
    // Save is the only path that writes inspectable rules or metadata.
    fn inline_metadata_editor(
        &mut self,
        ui: &mut egui::Ui,
        mod_name: &str,
        cur_sec: &str,
    ) -> Option<MetadataAction> {
        let mut action = None;
        // These tabs store advice, not an automatic left-pane move. A human
        // can say "requires X" without ModSlut pretending that means it knows
        // which separator X's assets belong in. Cute little distinction, huge
        // difference when a list is on fire.
        egui::Frame::group(ui.style())
            .fill(TOOLBAR)
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Edit Metadata").strong());
                    ui.label(egui::RichText::new(format!("· {mod_name}")).color(DIM));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("cancel").clicked() {
                            action = Some(MetadataAction::Cancel);
                        }
                    });
                });
                ui.label(
                    egui::RichText::new(format!("current section: {cur_sec}"))
                        .color(DIM)
                        .italics(),
                );
                ui.add_space(4.0);

                // The tab names deliberately mirror LOOT. Group/ordering
                // apply to left-pane mods today; the plugin-only tabs are
                // the shared metadata shell for the plugin sorter later.
                ui.horizontal_wrapped(|ui| {
                    for tab in MetadataTab::all() {
                        if ui
                            .selectable_label(self.metadata_tab == tab, tab.label())
                            .clicked()
                        {
                            self.metadata_tab = tab;
                            self.picker_filter.clear();
                            self.promote_target.clear();
                            self.metadata_message.clear();
                        }
                    }
                });
                ui.separator();

                match self.metadata_tab {
                    MetadataTab::Group => {
                        ui.label(egui::RichText::new("MO2 section (ModSlut group)").strong());
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("keep here").clicked() {
                                self.metadata_rule_draft = Some(format!("!{mod_name} = {cur_sec}"));
                            }
                            if ui.button("float").on_hover_text("bottom of this section; wins in-section").clicked() {
                                self.metadata_rule_draft = Some(format!("^{mod_name} = {cur_sec}"));
                            }
                            if ui.button("sink").on_hover_text("top of this section; loses in-section").clicked() {
                                self.metadata_rule_draft = Some(format!("<{mod_name} = {cur_sec}"));
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("find section:");
                            ui.text_edit_singleline(&mut self.picker_filter);
                        });
                        let needle = self.picker_filter.to_ascii_lowercase();
                        let matches: Vec<String> = self.sections.iter()
                            .filter(|sec| needle.is_empty() || sec.to_ascii_lowercase().contains(&needle))
                            .take(14)
                            .cloned()
                            .collect();
                        if matches.is_empty() { ui.label(egui::RichText::new("no matching section").color(DIM)); }
                        ui.horizontal_wrapped(|ui| {
                            for sec in matches {
                                if ui.button(&sec).clicked() { self.metadata_rule_draft = Some(format!("!{mod_name} = {sec}")); }
                            }
                        });
                    }
                    MetadataTab::LoadAfter | MetadataTab::LoadBefore => {
                        let after = self.metadata_tab == MetadataTab::LoadAfter;
                        ui.label(egui::RichText::new(if after { "This mod loads after:" } else { "This mod loads before:" }).strong());
                        let prefix = format!(">{mod_name} = ");
                        let suffix = format!(" = {mod_name}");
                        let pending: Vec<(usize, String)> = self
                            .metadata_load_order_drafts
                            .iter()
                            .enumerate()
                            .filter_map(|(index, edge)| {
                                if after {
                                    edge.strip_prefix(&prefix).map(|target| (index, target.to_string()))
                                } else {
                                    edge.strip_prefix('>')
                                        .and_then(|body| body.strip_suffix(&suffix))
                                        .map(|target| (index, target.to_string()))
                                }
                            })
                            .collect();
                        if pending.is_empty() {
                            ui.label(egui::RichText::new("No rows yet — this user-metadata table starts empty.").color(DIM));
                        } else {
                            let mut delete = None;
                            egui::Grid::new(if after { "load_after_rows" } else { "load_before_rows" })
                                .num_columns(2)
                                .show(ui, |ui| {
                                    ui.label(egui::RichText::new("Mod / plugin owner").strong());
                                    ui.label(egui::RichText::new("Action").strong());
                                    ui.end_row();
                                    for (index, target) in &pending {
                                        ui.label(target);
                                        if ui.small_button("delete row").clicked() { delete = Some(*index); }
                                        ui.end_row();
                                    }
                                });
                            if let Some(index) = delete {
                                self.metadata_load_order_drafts.remove(index);
                            }
                        }
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label("add a new row:");
                            ui.text_edit_singleline(&mut self.promote_target);
                        });
                        let needle = self.promote_target.to_ascii_lowercase();
                        if needle.trim().is_empty() {
                            ui.label(egui::RichText::new("type part of a mod name to pick an exact dependency").color(DIM));
                        } else {
                            let mut matches: Vec<String> = self.layout.iter()
                                .flat_map(|(_, mods)| mods.iter())
                                .chain(self.parking_mods.iter())
                                .filter(|candidate| candidate.as_str() != mod_name)
                                .filter(|candidate| candidate.to_ascii_lowercase().contains(needle.trim()))
                                .take(14)
                                .cloned()
                                .collect();
                            matches.sort_by_key(|candidate| candidate.to_ascii_lowercase());
                            matches.dedup();
                            ui.horizontal_wrapped(|ui| {
                                for candidate in matches {
                                    // >After = Before: invert the names for
                                    // the user-facing Load Before tab.
                                    let edge = if after {
                                        format!(">{mod_name} = {candidate}")
                                    } else {
                                        format!(">{candidate} = {mod_name}")
                                    };
                                    if ui.button(format!("Add {candidate}")).clicked()
                                        && !self.metadata_load_order_drafts.contains(&edge)
                                    {
                                        self.metadata_load_order_drafts.push(edge);
                                    }
                                }
                            });
                        }
                    }
                    MetadataTab::Requirements | MetadataTab::Incompatibilities => {
                        let (kind, title, hint) = if self.metadata_tab == MetadataTab::Requirements {
                            (
                                HumanMetadataKind::Requirement,
                                "Dependencies / Requirements",
                                "add a mod this depends on",
                            )
                        } else {
                            (HumanMetadataKind::Incompatibility, "Incompatible with", "find the incompatible mod")
                        };
                        ui.label(egui::RichText::new(title).strong());
                        let saved = self.human_metadata.values(mod_name, kind);
                        if saved.is_empty() {
                            ui.label(egui::RichText::new(
                                "No rows yet — this user-metadata table starts empty; detected plugin masters are not imported here.",
                            ).color(DIM));
                        }
                        for value in saved {
                            ui.label(egui::RichText::new(format!("• {value}")).color(DIM));
                        }
                        ui.horizontal(|ui| {
                            ui.label(hint);
                            ui.text_edit_singleline(&mut self.promote_target);
                        });
                        let needle = self.promote_target.trim().to_ascii_lowercase();
                        if needle.is_empty() {
                            ui.label(egui::RichText::new("type part of an installed mod name, then pick the exact one").color(DIM));
                        } else {
                            let mut matches: Vec<String> = self.layout.iter()
                                .flat_map(|(_, mods)| mods.iter())
                                .chain(self.parking_mods.iter())
                                .filter(|candidate| candidate.as_str() != mod_name)
                                .filter(|candidate| candidate.to_ascii_lowercase().contains(&needle))
                                .take(14)
                                .cloned()
                                .collect();
                            matches.sort_by_key(|candidate| candidate.to_ascii_lowercase());
                            matches.dedup();
                            ui.horizontal_wrapped(|ui| {
                                for candidate in matches {
                                    if ui.button(&candidate).clicked() {
                                        self.metadata_human_drafts.push((kind, candidate));
                                    }
                                }
                            });
                        }
                    }
                    MetadataTab::Messages => {
                        ui.label(egui::RichText::new("Human note").strong());
                        for value in self.human_metadata.values(mod_name, HumanMetadataKind::Message) {
                            ui.label(egui::RichText::new(format!("• {value}")).color(DIM));
                        }
                        ui.horizontal(|ui| {
                            ui.text_edit_singleline(&mut self.metadata_message)
                                .on_hover_text("A review note, warning, or gotcha. This never becomes an automatic move.");
                            if ui.button("add note").clicked() {
                                let note = self.metadata_message.trim().to_string();
                                if !note.is_empty() {
                                    self.metadata_human_drafts.push((HumanMetadataKind::Message, note));
                                    self.metadata_message.clear();
                                }
                            }
                        });
                    }
                    MetadataTab::BashTags
                    | MetadataTab::DirtyPluginInfo
                    | MetadataTab::CleanPluginInfo
                    | MetadataTab::Locations => {
                        ui.label(egui::RichText::new("LOOT metadata slot").strong());
                        ui.label(egui::RichText::new(
                            "This will show read-only facts from the local LOOT masterlist. Bash tags stay Tier 4 conflict evidence; they do not get to randomly fling folders around your left pane.",
                        ).color(DIM));
                    }
                }
                ui.separator();
                if let Some(rule) = &self.metadata_rule_draft {
                    ui.label(egui::RichText::new(format!("pending rule: {rule}")).color(DIM));
                }
                if !self.metadata_load_order_drafts.is_empty() {
                    ui.label(egui::RichText::new(format!("{} pending load-order row(s)", self.metadata_load_order_drafts.len())).color(DIM));
                }
                if !self.metadata_human_drafts.is_empty() {
                    ui.label(egui::RichText::new(format!("{} pending metadata entr{}", self.metadata_human_drafts.len(), if self.metadata_human_drafts.len() == 1 { "y" } else { "ies" })).color(DIM));
                }
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        action = Some(MetadataAction::Save);
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(MetadataAction::Cancel);
                    }
                });
            });
        match action {
            Some(MetadataAction::Save) => {
                if let Some(rule) = self.metadata_rule_draft.take() {
                    self.append_user_rule(rule);
                }
                for rule in std::mem::take(&mut self.metadata_load_order_drafts) {
                    self.append_user_rule(rule);
                }
                for (kind, value) in std::mem::take(&mut self.metadata_human_drafts) {
                    self.append_human_metadata(kind, mod_name, &value);
                }
                Some(MetadataAction::Save)
            }
            Some(MetadataAction::Cancel) => {
                self.metadata_rule_draft = None;
                self.metadata_load_order_drafts.clear();
                self.metadata_human_drafts.clear();
                Some(MetadataAction::Cancel)
            }
            None => None,
        }
    }

    // a mod with a pending change: kind tag + name, destination on its own
    // line underneath
    fn change_row(&mut self, ui: &mut egui::Ui, i: usize, section: &str) {
        let (kind, name, detail) = {
            // defensive: a stale index after a plan rebuild must never panic
            let Some(c) = self.plan.get(i) else { return };
            (c.kind, c.name.clone(), c.detail.clone())
        };
        let priority = self.mod_priorities.get(&name).copied().unwrap_or(0);
        let marker = self.mod_states.get(&name).copied().unwrap_or(' ');
        let selected = self.selected == Some(i);
        let frame = egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(6, 5))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_sized(
                        egui::vec2(66.0, 0.0),
                        egui::Label::new(
                            egui::RichText::new(format!("{priority:<4}"))
                                .monospace()
                                .color(DIM),
                        ),
                    );
                    ui.add_sized(
                        egui::vec2(34.0, 0.0),
                        egui::Label::new(egui::RichText::new(marker).monospace().color(DIM)),
                    );
                    // Match the header's divider width without drawing a
                    // row-by-row fence. LOOT's table only draws column rules
                    // in its header when its row grid is disabled.
                    ui.allocate_space(egui::vec2(1.0, 0.0));
                    ui.add_sized(
                        egui::vec2(64.0, 0.0),
                        egui::Label::new(
                            egui::RichText::new(
                                self.mod_plugin_indexes
                                    .get(&name)
                                    .cloned()
                                    .unwrap_or_else(|| "—".to_string()),
                            )
                            .monospace()
                            .color(
                                if self.mod_plugin_indexes.contains_key(&name) {
                                    REOR_CLR
                                } else {
                                    DIM
                                },
                            ),
                        ),
                    );
                    ui.label(
                        egui::RichText::new(kind_tag(kind))
                            .monospace()
                            .color(kind_color(kind)),
                    );
                    ui.label(egui::RichText::new(&name).color(egui::Color32::WHITE));
                    ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
                });
                if !detail.is_empty() {
                    ui.label(egui::RichText::new(format!("-> {detail}")).color(DIM));
                }
            });
        let resp = frame
            .response
            .interact(egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if resp.hovered() && !selected {
            ui.painter()
                .rect_filled(resp.rect, 2.0, egui::Color32::from_white_alpha(12));
        }
        if selected {
            ui.painter()
                .rect_filled(resp.rect, 2.0, egui::Color32::from_white_alpha(20));
            ui.painter().rect_stroke(
                resp.rect,
                2.0,
                egui::Stroke::new(1.0_f32, kind_color(kind)),
                egui::StrokeKind::Inside,
            );
        }
        if resp.clicked() {
            self.selected = Some(i);
            self.selected_mod = None;
        }
        self.row_menu(&resp, &name, section);
        if self
            .picker_for
            .as_ref()
            .is_some_and(|(picked, _)| picked == &name)
        {
            if self.inline_metadata_editor(ui, &name, section).is_some() {
                self.picker_for = None;
            }
        }
    }

    // a mod with no pending change: dim row, still fully clickable and
    // right-clickable so rules can be written against it
    fn plain_row(&mut self, ui: &mut egui::Ui, name: &str, section: &str) {
        let priority = self.mod_priorities.get(name).copied().unwrap_or(0);
        let marker = self.mod_states.get(name).copied().unwrap_or(' ');
        let selected = self.selected_mod.as_ref().is_some_and(|(n, _)| n == name);
        let frame = egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(6, 3))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_sized(
                        egui::vec2(66.0, 0.0),
                        egui::Label::new(
                            egui::RichText::new(format!("{priority:<4}"))
                                .monospace()
                                .color(DIM),
                        ),
                    );
                    ui.add_sized(
                        egui::vec2(34.0, 0.0),
                        egui::Label::new(egui::RichText::new(marker).monospace().color(DIM)),
                    );
                    ui.allocate_space(egui::vec2(1.0, 0.0));
                    ui.add_sized(
                        egui::vec2(64.0, 0.0),
                        egui::Label::new(
                            egui::RichText::new(
                                self.mod_plugin_indexes
                                    .get(name)
                                    .cloned()
                                    .unwrap_or_else(|| "—".to_string()),
                            )
                            .monospace()
                            .color(
                                if self.mod_plugin_indexes.contains_key(name) {
                                    REOR_CLR
                                } else {
                                    DIM
                                },
                            ),
                        ),
                    );
                    ui.label(egui::RichText::new(name).color(egui::Color32::WHITE));
                    ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
                });
            });
        let resp = frame
            .response
            .interact(egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if resp.hovered() && !selected {
            ui.painter()
                .rect_filled(resp.rect, 2.0, egui::Color32::from_white_alpha(12));
        }
        if selected {
            ui.painter()
                .rect_filled(resp.rect, 2.0, egui::Color32::from_white_alpha(20));
            ui.painter().rect_stroke(
                resp.rect,
                2.0,
                egui::Stroke::new(1.0_f32, DIM),
                egui::StrokeKind::Inside,
            );
        }
        if resp.clicked() {
            self.selected_mod = Some((name.to_string(), section.to_string()));
            self.selected = None;
        }
        self.row_menu(&resp, name, section);
        if self
            .picker_for
            .as_ref()
            .is_some_and(|(picked, _)| picked == name)
        {
            if self.inline_metadata_editor(ui, name, section).is_some() {
                self.picker_for = None;
            }
        }
    }

    // LOOT's main pane is a stream of plugin cards, rather than a detail
    // panel that stays empty until a sort. ModSlut mirrors that for every
    // MO2 folder, including pluginless mods and separators' children.
    fn installed_mod_cards(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("installed mods")
                .strong()
                .size(self.font_size * 1.05),
        );
        ui.label(egui::RichText::new(
            "Visible MO2 priority order. Source is available immediately; Bash tags and real plugin indexes appear after LOOT/libloot evaluates active plugins.",
        ).color(DIM));
        let groups = self.layout.clone();
        let needle = self.right_search.trim().to_ascii_lowercase();
        // Keep one natural right-pane scrollbar, but do not build 2,000
        // rich cards (and their source/tag widgets) on every frame. The
        // outer ScrollArea owns scrolling; offscreen cards reserve a stable
        // approximate height and only nearby cards become widgets.
        const CARD_ESTIMATED_HEIGHT: f32 = 94.0;
        let visible_clip = ui.clip_rect();
        // Same visible order as MO2: last serialized section/mod is
        // position 1, so show it first in the cards too.
        for (section, mods) in groups.into_iter().rev() {
            if self.roadmap_context.contains(&section) {
                continue;
            }
            for name in mods.into_iter().rev() {
                if !needle.is_empty() && !name.to_ascii_lowercase().contains(&needle) {
                    continue;
                }
                let selected = self
                    .selected_mod
                    .as_ref()
                    .is_some_and(|(selected_name, _)| selected_name == &name);
                let editing = self
                    .picker_for
                    .as_ref()
                    .is_some_and(|(picked, _)| picked == &name);
                let card_top = ui.cursor().min.y;
                let near_viewport = card_top + CARD_ESTIMATED_HEIGHT >= visible_clip.top() - 320.0
                    && card_top <= visible_clip.bottom() + 320.0;
                if !near_viewport && !selected && !editing {
                    ui.add_space(CARD_ESTIMATED_HEIGHT + 4.0);
                    continue;
                }
                let position = self.mod_priorities.get(&name).copied().unwrap_or(0);
                let state = self.mod_states.get(&name).copied().unwrap_or(' ');
                let index = self
                    .mod_plugin_indexes
                    .get(&name)
                    .cloned()
                    .unwrap_or_default();
                // Allocate the whole card width first. A Frame alone
                // shrink-wraps to text, which was the tiny-box look.
                let card = ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::Frame::new()
                            .fill(if selected {
                                self.theme_card_selected
                            } else {
                                self.theme_card_fill
                            })
                            .stroke(egui::Stroke::new(1.0_f32, self.theme_card_border))
                            .inner_margin(egui::Margin::symmetric(10, 7))
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!("Position {position}"))
                                            .monospace()
                                            .color(DIM),
                                    );
                                    ui.label(egui::RichText::new(state).monospace().color(DIM));
                                    if !index.is_empty() {
                                        ui.label(
                                            egui::RichText::new(format!("Index {index}"))
                                                .monospace()
                                                .color(REOR_CLR),
                                        );
                                    }
                                    ui.label(
                                        egui::RichText::new(&name).color(egui::Color32::WHITE),
                                    );
                                });
                                ui.label(
                                    egui::RichText::new(format!("Section: {section}"))
                                        .color(DIM)
                                        .size(self.font_size * 0.95),
                                );
                                show_source_and_bash_tags(
                                    ui,
                                    self.mod_sources.get(&name),
                                    self.mod_bash_tags.get(&name),
                                );
                            })
                    },
                );
                let response = card.response.interact(egui::Sense::click());
                if response.clicked() {
                    self.selected_mod = Some((name.clone(), section.clone()));
                    self.selected = None;
                }
                self.row_menu(&response, &name, &section);
                // LOOT expands metadata immediately below the card
                // that owns the selected plugin. Bring that card
                // into the one shared right-pane scroll view first,
                // then render the editor under it.
                if editing {
                    response.scroll_to_me(Some(egui::Align::Center));
                    if self.inline_metadata_editor(ui, &name, &section).is_some() {
                        self.picker_for = None;
                    }
                }
                ui.add_space(4.0);
            }
        }
    }

    // untick a mod in modlist.txt immediately (+ -> -), with a .bak backup.
    // mo2 picks it up when this process exits, same as apply.
    fn disable_mod(&mut self, name: &str) {
        let Some(file) = self.file.clone() else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(&file) else {
            self.status = format!("couldn't read {}", file.display());
            return;
        };
        let mut out = String::new();
        let mut hit = false;
        for line in text.lines() {
            let t = line.trim();
            if !hit && t.starts_with('+') && t[1..].trim() == name {
                out.push('-');
                out.push_str(&line[t.find('+').unwrap() + 1..]);
                hit = true;
            } else {
                out.push_str(line);
            }
            out.push('\n');
        }
        if !hit {
            self.status = format!("{name} isn't enabled - nothing to disable");
            return;
        }
        std::fs::copy(&file, file.with_extension("txt.bak")).ok();
        match std::fs::write(&file, out) {
            Ok(()) => {
                self.status = format!("disabled {name} - mo2 will show it unticked on exit");
                self.load_and_preview(file);
            }
            Err(e) => self.status = format!("couldn't write {}: {e}", file.display()),
        }
    }

    fn append_user_rule(&mut self, line: String) {
        let Some(file) = &self.file else { return };
        let Some(path) = user_rule_files(Some(file)).into_iter().next() else {
            self.status = "couldn't figure out where user rules live".into();
            return;
        };
        if let Err(e) = crate::ensure_profile_data_parent(&path) {
            self.status = format!("couldn't create profile rules folder: {e}");
            return;
        }
        if !path.exists() {
            // create silently - opening notepad mid-right-click would be weird
            std::fs::write(&path, USER_RULES_TEMPLATE).ok();
        }
        let mut text = std::fs::read_to_string(&path).unwrap_or_default();
        // re-pinning a mod REPLACES its old rule. rules are checked
        // first-hit-wins, so appending "!mod = B" under an old "!mod = A"
        // silently keeps A - the user clicks pin, nothing changes, and the
        // file fills up with contradicting duplicates (their live file had
        // six pins for one mod pointing at three different sections)
        if let Some(name) = line.get(1..).and_then(|r| r.split(" = ").next()) {
            let name = name.trim().to_lowercase();
            text = text
                .lines()
                .filter(|l| {
                    let l = l.trim_start();
                    let is_rule = l.starts_with('!') || l.starts_with('^') || l.starts_with('<');
                    if !is_rule {
                        return true;
                    }
                    match l.get(1..).and_then(|r| r.split(" = ").next()) {
                        Some(n) => n.trim().to_lowercase() != name,
                        None => true,
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
        }
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&line);
        text.push('\n');
        match std::fs::write(&path, text) {
            Ok(()) => {
                let p = self.file.clone().unwrap();
                self.load_and_preview(p);
                self.status = format!("rule added: {line}");
            }
            Err(e) => self.status = format!("couldn't write {}: {e}", path.display()),
        }
    }

    fn learn_section(&mut self, section: &str, mods: &[String]) {
        let Some(file) = self.file.clone() else {
            return;
        };
        match crate::learning::learn_section(&file, section, mods) {
            Ok(n) if n > 0 => {
                self.status = format!(
                    "learned {n} exact homes in [{section}] - old references can quit arguing now"
                );
                self.load_and_preview(file);
            }
            Ok(_) => {
                self.status = "nothing new to learn here (or this is an operational shelf).".into()
            }
            Err(e) => self.status = format!("couldn't save learned homes: {e}"),
        }
    }

    fn learn_all_homes(&mut self) {
        let Some(file) = self.file.clone() else {
            return;
        };
        match crate::learning::learn_all(&file, &self.layout) {
            Ok(n) if n > 0 => {
                self.status = format!("learned {n} exact homes across the managed modlist");
                self.load_and_preview(file);
            }
            Ok(_) => {
                self.status = "the managed modlist's current homes are already learned.".into()
            }
            Err(e) => self.status = format!("couldn't save learned homes: {e}"),
        }
    }

    fn clear_metadata(&mut self, name: &str) {
        let Some(file) = self.file.clone() else {
            return;
        };
        let mut changed = false;
        if let Some(path) = user_rule_files(Some(&file)).into_iter().next() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let kept: Vec<&str> = text
                    .lines()
                    .filter(|line| {
                        let line = line.trim_start();
                        let Some(body) = line.strip_prefix(['!', '^', '<', '>']) else {
                            return true;
                        };
                        let Some((left, right)) = body.split_once('=') else {
                            return true;
                        };
                        // An ordering edge belongs to either endpoint; a
                        // placement directive belongs to its left side.
                        let left = left.trim().eq_ignore_ascii_case(name);
                        let right = right.trim().eq_ignore_ascii_case(name);
                        !(left || (line.starts_with('>') && right))
                    })
                    .collect();
                let updated = format!("{}\n", kept.join("\n"));
                if updated != text {
                    if std::fs::write(&path, updated).is_ok() {
                        changed = true;
                    }
                }
            }
        }
        match crate::metadata::clear_for(&file, name) {
            Ok(human_changed) => changed |= human_changed,
            Err(e) => {
                self.status = format!("couldn't clear human metadata: {e}");
                return;
            }
        }
        match crate::learning::forget_home(&file, name) {
            Ok(forgot) => changed |= forgot,
            Err(e) => {
                self.status = format!("couldn't clear learned home: {e}");
                return;
            }
        }
        self.human_metadata = HumanMetadata::load(&file);
        self.status = if changed {
            format!("cleared saved metadata for {name}")
        } else {
            format!("{name} has no saved metadata")
        };
        if changed {
            self.load_and_preview(file);
        }
    }

    // User rules live beside ModSlut, named for the active profile. Create a
    // commented template on first use, then open it in the OS editor.
    fn open_user_rules(&mut self) {
        let Some(file) = &self.file else { return };
        let Some(path) = user_rule_files(Some(file)).into_iter().next() else {
            self.status = "couldn't figure out where to put user rules".into();
            return;
        };
        if let Err(e) = crate::ensure_profile_data_parent(&path) {
            self.status = format!("couldn't create profile rules folder: {e}");
            return;
        }
        if !path.exists() {
            if let Err(e) = std::fs::write(&path, USER_RULES_TEMPLATE) {
                self.status = format!("couldn't create {}: {e}", path.display());
                return;
            }
        }
        // windows: hand it to notepad; elsewhere: xdg-open
        let opened = if cfg!(target_os = "windows") {
            std::process::Command::new("notepad.exe")
                .arg(&path)
                .spawn()
                .is_ok()
        } else {
            std::process::Command::new("xdg-open")
                .arg(&path)
                .spawn()
                .is_ok()
        };
        self.status = if opened {
            format!("editing {} - save, then hit reload", path.display())
        } else {
            format!("couldn't open an editor - file is at {}", path.display())
        };
    }

    fn import_private_reference(&mut self) {
        let Some(target) = self.file.clone() else {
            return;
        };
        let Some(source) = rfd::FileDialog::new()
            .set_title("choose a human-sorted reference modlist.txt")
            .add_filter("modlist", &["txt"])
            .pick_file()
        else {
            return;
        };
        match crate::reference::import_source(&target, &source) {
            Ok(rows) => {
                self.load_and_preview(target);
                self.status = format!(
                    "private reference imported: {rows} placements from {}",
                    source.display()
                );
            }
            Err(e) => self.status = format!("couldn't import private reference: {e}"),
        }
    }

    fn open_roadmap_editor(&mut self) {
        let Some(file) = self.file.clone() else {
            return;
        };
        match crate::section_map::load_or_create(&file, &self.sections) {
            Ok(map) => {
                self.roadmap_path = Some(map.path.clone());
                self.roadmap_ambiguous = map.review_concepts();
                self.roadmap_reviewed = map.reviewed_mappings();
                self.roadmap_multi = map.multi_destination_concepts();
                self.roadmap_branches = map.branch_routes();
                self.roadmap_destinations = self.sections.clone();
                self.roadmap_context = self
                    .sections
                    .iter()
                    .filter(|label| map.is_context(label))
                    .cloned()
                    .collect();
                self.show_roadmap = true;
            }
            Err(e) => self.status = format!("couldn't open separator roadmap: {e}"),
        }
    }

    fn import_roadmap(&mut self) {
        let Some(file) = self.file.clone() else {
            return;
        };
        let Some(source) = rfd::FileDialog::new()
            .set_title("Import ModSlut separator roadmap")
            .add_filter("roadmap", &["ini"])
            .pick_file()
        else {
            return;
        };
        match crate::section_map::import_for_profile(&file, &source, &self.sections) {
            Ok(rows) => {
                self.status = format!("roadmap imported: {rows} mapping(s). Previous local copy kept as section_map.before-import.ini.");
                self.open_roadmap_editor();
            }
            Err(e) => self.status = format!("roadmap import refused: {e}"),
        }
    }

    fn open_groups_editor(&mut self) {
        let Some(file) = self.file.clone() else {
            return;
        };
        match crate::groups::load_or_create(&file, &self.sections) {
            Ok((path, edges)) => {
                self.groups_path = Some(path);
                self.group_edges = edges;
                self.group_later.clear();
                self.group_earlier.clear();
                self.show_groups = true;
            }
            Err(e) => self.status = format!("couldn't open group graph: {e}"),
        }
    }

    fn open_settings(&mut self) {
        self.settings_font_draft = self.font_size;
        self.settings_game_draft = self.default_game;
        self.settings_theme_draft = self.theme.clone();
        self.settings_page = SettingsPage::General;
        self.show_settings = true;
    }

    fn import_mo2_theme(&mut self) {
        let Some(source) = rfd::FileDialog::new()
            .set_title("Import an MO2 stylesheet as a ModSlut theme")
            .add_filter("MO2 stylesheet", &["qss"])
            .pick_file()
        else {
            return;
        };
        match import_mo2_qss(&source) {
            Ok(name) => {
                self.settings_theme_draft = name.clone();
                self.status = format!(
                    "imported MO2 stylesheet as theme pack '{name}' — press Save to activate it"
                );
            }
            Err(error) => self.status = format!("MO2 theme import failed: {error}"),
        }
    }

    fn import_all_mo2_themes(&mut self) {
        let Some(folder) = rfd::FileDialog::new()
            .set_title("Choose MO2's stylesheets folder to import every theme")
            .pick_folder()
        else {
            return;
        };
        match import_mo2_qss_folder(&folder) {
            Ok((imported, total)) => {
                self.status = format!("imported {imported} of {total} MO2 stylesheets as self-contained ModSlut theme packs");
            }
            Err(error) => self.status = format!("MO2 theme import failed: {error}"),
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = self.show_settings;
        let mut save = false;
        let mut cancel = false;
        let mut window = egui::Window::new("Settings")
            .id(egui::Id::new("modslut-settings-window"))
            .open(&mut open)
            .resizable(true)
            // `default_*` affects first open only. The old interior had a
            // 230px sidebar plus several 540/270px minimum widgets, which
            // silently made the window's real minimum about 860px and caused
            // a smaller drag to snap back. Keep a pleasant first size, but
            // let the content genuinely contract.
            .default_width(720.0)
            .default_height(440.0)
            .min_width(300.0)
            .min_height(260.0)
            // Let a smaller settings dialog scroll rather than allowing a
            // child panel to silently grow the parent again.
            .scroll([false, true]);
        if let Some(rect) = self.settings_rect {
            // This is intentionally a *default* rect, not fixed/current:
            // egui keeps a live drag fluid while this preserves it next run.
            window = window.default_rect(rect);
        }
        let response = window.show(ctx, |ui| {
                // Child labels/combo text must wrap or clip inside the
                // Window, never become a hidden content-derived minimum.
                ui.set_min_width(0.0);
                ui.set_min_height(0.0);
                // LOOT-style settings tree. The fixed-width navigation is
                // allocated inside this already-bounded window, so it cannot
                // force a resize back to the old giant minimum width.
                ui.horizontal_top(|ui| {
                    let nav_width = ui.available_width().clamp(120.0, 190.0);
                    ui.vertical(|ui| {
                        ui.set_width(nav_width);
                            ui.selectable_value(&mut self.settings_page, SettingsPage::General, "General");
                            ui.separator();
                            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                                for &candidate in game::CATALOG {
                                    let info = game::info(candidate);
                                    if crate::loot::game_install_path(&info).is_some() {
                                        ui.selectable_value(&mut self.settings_page, SettingsPage::Game(candidate), info.name);
                                    }
                                }
                            });
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_width(ui.available_width());
                        ui.set_max_width(ui.available_width());
                        egui::ScrollArea::vertical().show(ui, |ui| match self.settings_page {
                            SettingsPage::General => {
                                ui.heading("General");
                                ui.add_space(10.0);
                                ui.horizontal_wrapped(|ui| {
                                    ui.label("Default Game");
                                    let selected = self.settings_game_draft.map(|game| game::info(game).name).unwrap_or("Auto-detect from active profile");
                                    egui::ComboBox::from_id_salt("settings-default-game")
                                        .selected_text(selected)
                                        .width(ui.available_width().min(240.0))
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(&mut self.settings_game_draft, None, "Auto-detect from active profile");
                                            for &candidate in game::CATALOG {
                                                let info = game::info(candidate);
                                                if crate::loot::game_install_path(&info).is_some() {
                                                    ui.selectable_value(&mut self.settings_game_draft, Some(candidate), info.name);
                                                }
                                            }
                                        });
                                });
                                ui.add_space(10.0);
                                ui.horizontal_wrapped(|ui| {
                                    ui.label("Theme");
                                    egui::ComboBox::from_id_salt("settings-theme")
                                        .selected_text(&self.settings_theme_draft)
                                        .width(ui.available_width().min(240.0))
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(&mut self.settings_theme_draft, "MO2 Mirror (read-only)".to_string(), "MO2 Mirror (read-only)");
                                            ui.selectable_value(&mut self.settings_theme_draft, "ModSlut Dark".to_string(), "ModSlut Dark");
                                            for pack in theme_packs() {
                                                ui.selectable_value(&mut self.settings_theme_draft, pack.name.clone(), pack.name);
                                            }
                                        });
                                });
                                ui.horizontal_wrapped(|ui| {
                                    if ui.button("Import one MO2 theme…").clicked() {
                                        self.import_mo2_theme();
                                    }
                                    if ui.button("Import all MO2 themes…").clicked() {
                                        self.import_all_mo2_themes();
                                    }
                                });
                                ui.label(egui::RichText::new("Imports one stylesheet or every .qss below MO2's stylesheets folder as portable ModSlut packs. MO2 is read-only.").color(DIM).small());
                                ui.horizontal_wrapped(|ui| {
                                    ui.label("Font size");
                                    egui::ComboBox::from_id_salt("settings-font-size")
                                        .selected_text(format!("{:.0} px", self.settings_font_draft))
                                        .width(150.0)
                                        .show_ui(ui, |ui| for size in [10.0_f32, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 18.0, 20.0, 22.0, 24.0, 26.0, 28.0] {
                                            ui.selectable_value(&mut self.settings_font_draft, size, format!("{size:.0} px"));
                                        });
                                });
                                ui.add_space(14.0);
                                ui.label(egui::RichText::new("Themes are self-contained: palettes and optional background art ship with ModSlut. Installed games come directly from LOOT's configured paths.").color(DIM));
                            }
                            SettingsPage::Game(selected) => {
                                let info = game::info(selected);
                                ui.heading(info.name);
                                ui.add_space(10.0);
                                egui::Grid::new("settings-game-details").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                                    ui.label("LOOT folder"); ui.monospace(info.loot_folders.first().copied().unwrap_or("—")); ui.end_row();
                                    ui.label("Main master plugin"); ui.monospace(info.base_masters.first().copied().unwrap_or("—")); ui.end_row();
                                    ui.label("LOOT game type"); ui.monospace(info.loot_types.first().copied().unwrap_or("—")); ui.end_row();
                                    ui.label("Install path");
                                    ui.add(egui::Label::new(crate::loot::game_install_path(&info).map(|path| path.display().to_string()).unwrap_or_else(|| "not configured".to_string())).wrap());
                                    ui.end_row();
                                });
                                ui.add_space(14.0);
                                ui.label(egui::RichText::new("Game detection is read-only here. ModSlut uses LOOT's configured game location and masterlist identity; the active MO2 profile still wins whenever its root master is recognizable.").color(DIM));
                            }
                        });
                    });
                });
                ui.separator();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() { cancel = true; }
                    if ui.button("Save").clicked() { save = true; }
                });
            });
        if let Some(response) = response {
            let rect = response.response.rect;
            let changed = self.settings_rect.is_none_or(|old| {
                (old.min - rect.min).length_sq() > 1.0
                    || (old.size() - rect.size()).length_sq() > 1.0
            });
            self.settings_rect = Some(rect);
            if changed {
                save_ui_prefs(&UiPrefs {
                    panel: Some(self.changes_width),
                    size: self.win_rect.map(|r| (r.width(), r.height())),
                    pos: self.win_rect.map(|r| (r.min.x, r.min.y)),
                    settings_size: Some((rect.width(), rect.height())),
                    settings_pos: Some((rect.min.x, rect.min.y)),
                    user_rules_only: self.user_rules_only,
                });
            }
        }
        self.show_settings = open && !cancel;
        if save {
            self.font_size = self.settings_font_draft;
            self.default_game = self.settings_game_draft;
            if self.theme != self.settings_theme_draft {
                self.theme_background = None;
            }
            self.theme = self.settings_theme_draft.clone();
            save_settings(self.font_size, self.default_game, &self.theme);
            self.status = format!(
                "settings saved: {}; font size {:.0} px; default game {}",
                self.theme,
                self.font_size,
                self.default_game
                    .map(|game| game::info(game).name)
                    .unwrap_or("auto-detect")
            );
            self.show_settings = false;
        }
    }

    fn import_groups(&mut self) {
        let Some(file) = self.file.clone() else {
            return;
        };
        let Some(source) = rfd::FileDialog::new()
            .set_title("Import ModSlut separator groups")
            .add_filter("group graph", &["ini"])
            .pick_file()
        else {
            return;
        };
        match crate::groups::import_for_profile(&file, &source, &self.sections) {
            Ok(edges) => {
                self.status = format!("group graph imported: {edges} edge(s). Previous local copy kept as groups.before-import.ini.");
                self.open_groups_editor();
            }
            Err(e) => self.status = format!("group import refused: {e}"),
        }
    }

    fn clear_groups(&mut self) {
        let Some(file) = self.file.clone() else {
            return;
        };
        let path = crate::groups::path_for(&file);
        match crate::groups::clear(&path) {
            Ok(()) => {
                self.group_edges.clear();
                self.status = "separator group graph reset to the current separator order; previous links saved as groups.before-clear.ini".into();
            }
            Err(e) => self.status = format!("couldn't clear group graph: {e}"),
        }
    }

    fn groups_editor_window(&mut self, ctx: &egui::Context) {
        if !self.show_groups {
            return;
        }
        let mut open = self.show_groups;
        let labels = self.sections.clone();
        let mut remove: Option<usize> = None;
        let mut save = false;
        let mut cancel = false;
        egui::Window::new("Groups Editor")
            .open(&mut open)
            .resizable(true)
            .default_size(egui::vec2(900.0, 620.0))
            .min_size(egui::vec2(560.0, 360.0))
            // Never let a remembered oversized window escape the desktop.
            // The body scrolls instead, so this remains usable on laptops
            // and at large accessibility font sizes.
            .max_size(egui::vec2(
                (ctx.available_rect().width() - 24.0).max(560.0),
                (ctx.available_rect().height() - 24.0).max(360.0),
            ))
            .vscroll(true)
            .show(ctx, |ui| {
                ui.label("This is the live MO2 separator spine that ModSlut gives libloot when it sorts plugins.");
                ui.label(egui::RichText::new("Each node expands naturally as mods enter it. The arrows are ordering links, not fixed priority numbers.").color(DIM));
                ui.separator();
                ui.label(egui::RichText::new("Current separator graph").strong());
                // Mirror LOOT's Groups Editor: a single, scrollable node
                // spine with circular group nodes, arrowheads, and labels
                // below.  MO2's own separator order is the base graph; blue
                // edges are the optional profile-local `after` links.
                ui.horizontal_top(|ui| {
                    let controls_w = 258.0;
                    let canvas_w = (ui.available_width() - controls_w - 14.0).max(360.0);
                    ui.vertical(|ui| {
                        egui::ScrollArea::horizontal()
                            .id_salt("loot-style-separator-spine")
                            .max_width(canvas_w)
                            .max_height(300.0)
                            .show(ui, |ui| {
                                let spacing = 172.0;
                                let width = (labels.len().max(4) as f32 * spacing + 54.0).max(canvas_w);
                                let (response, painter) = ui.allocate_painter(
                                    egui::vec2(width, 270.0),
                                    egui::Sense::hover(),
                                );
                                let rect = response.rect;
                                let line_y = rect.center().y - 8.0;
                                let point = |index: usize| egui::pos2(rect.left() + 34.0 + index as f32 * spacing, line_y);
                                let arrow = |from: egui::Pos2, to: egui::Pos2, color: egui::Color32, painter: &egui::Painter| {
                                    let direction = (to - from).normalized();
                                    let start = from + direction * 17.0;
                                    let end = to - direction * 21.0;
                                    painter.line_segment([start, end], egui::Stroke::new(2.0_f32, color));
                                    let side = egui::vec2(-direction.y, direction.x) * 5.0;
                                    painter.add(egui::Shape::convex_polygon(
                                        vec![end, end - direction * 11.0 + side, end - direction * 11.0 - side],
                                        color,
                                        egui::Stroke::NONE,
                                    ));
                                };
                                for index in 0..labels.len().saturating_sub(1) {
                                    arrow(point(index), point(index + 1), DIM, &painter);
                                }
                                // Custom connections are laid above the
                                // spine, keeping the normal LOOT-like route
                                // immediately readable.
                                for (edge_index, (later, earlier)) in self.group_edges.iter().enumerate() {
                                    let Some(from) = labels.iter().position(|name| name == earlier) else { continue; };
                                    let Some(to) = labels.iter().position(|name| name == later) else { continue; };
                                    let rise = 28.0 + (edge_index % 3) as f32 * 16.0;
                                    let start = point(from);
                                    let end = point(to);
                                    let mid_a = egui::pos2(start.x, line_y - rise);
                                    let mid_b = egui::pos2(end.x, line_y - rise);
                                    painter.line_segment([start, mid_a], egui::Stroke::new(2.0_f32, REOR_CLR));
                                    painter.line_segment([mid_a, mid_b], egui::Stroke::new(2.0_f32, REOR_CLR));
                                    arrow(mid_b, end, REOR_CLR, &painter);
                                }
                                for (index, label) in labels.iter().enumerate() {
                                    let center = point(index);
                                    painter.circle_filled(center, 15.0, if index == 0 { REOR_CLR } else { egui::Color32::LIGHT_GRAY });
                                    let visible = if label.chars().count() > 22 {
                                        format!("{}…", label.chars().take(21).collect::<String>())
                                    } else { label.clone() };
                                    painter.text(
                                        egui::pos2(center.x, center.y + 30.0),
                                        egui::Align2::CENTER_TOP,
                                        visible,
                                        egui::FontId::proportional(13.0),
                                        egui::Color32::WHITE,
                                    );
                                }
                            });
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        let mut group_name_placeholder = String::new();
                        ui.add_space(54.0);
                        ui.label("Group name");
                        ui.add_enabled(false, egui::TextEdit::singleline(&mut group_name_placeholder).desired_width(236.0))
                            .on_hover_text("MO2 separators are the groups, so group names are managed in MO2.");
                        ui.add_enabled(false, egui::Button::new("Add a new group")).on_hover_text("Create the separator in MO2, then reload ModSlut.");
                        ui.add_enabled(false, egui::Button::new("Rename current group")).on_hover_text("Rename the separator in MO2 to preserve its list design.");
                        ui.separator();
                        ui.label(egui::RichText::new("Extra ordering link").strong());
                        egui::ComboBox::from_id_salt("group-later")
                            .selected_text(if self.group_later.is_empty() { "later separator…" } else { &self.group_later })
                            .width(236.0)
                            .show_ui(ui, |ui| for label in &labels {
                                if ui.selectable_label(self.group_later == *label, label).clicked() { self.group_later = label.clone(); }
                            });
                        ui.label("after");
                        egui::ComboBox::from_id_salt("group-earlier")
                            .selected_text(if self.group_earlier.is_empty() { "earlier separator…" } else { &self.group_earlier })
                            .width(236.0)
                            .show_ui(ui, |ui| for label in &labels {
                                if ui.selectable_label(self.group_earlier == *label, label).clicked() { self.group_earlier = label.clone(); }
                            });
                        if ui.add_enabled(!self.group_later.is_empty() && !self.group_earlier.is_empty() && self.group_later != self.group_earlier, egui::Button::new("Add link")).clicked() {
                            let edge = (self.group_later.clone(), self.group_earlier.clone());
                            if !self.group_edges.contains(&edge) { self.group_edges.push(edge); }
                            self.group_later.clear();
                            self.group_earlier.clear();
                        }
                        ui.separator();
                        if ui.button("Auto arrange groups").clicked() {
                            self.status = "the live graph already follows the current MO2 separator order".into();
                        }
                    });
                });
                if !self.group_edges.is_empty() {
                    ui.label(egui::RichText::new(format!("{} additional profile link(s) are applied below the baseline chain.", self.group_edges.len())).color(DIM));
                }
                ui.separator();
                egui::ScrollArea::vertical().max_height(100.0).show(ui, |ui| {
                    if self.group_edges.is_empty() { ui.label(egui::RichText::new("No custom group links yet. That's fine; the current separator order remains the baseline.").color(DIM)); }
                    for (idx, (later, earlier)) in self.group_edges.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.monospace(later);
                            ui.label("→ after →");
                            ui.monospace(earlier);
                            if ui.small_button("remove").clicked() { remove = Some(idx); }
                        });
                    }
                });
                ui.separator();
                if ui.button("Save").clicked() { save = true; }
                if ui.button("Cancel").clicked() { cancel = true; }
                if ui.button("Clear custom links").on_hover_text("Restore the normal separator-order chain. A groups.before-clear.ini backup is kept.").clicked() {
                    if let Some(path) = self.groups_path.clone() {
                        match crate::groups::clear(&path) {
                            Ok(()) => {
                                self.group_edges.clear();
                                self.status = "separator group graph reset; backup saved as groups.before-clear.ini".into();
                            }
                            Err(e) => self.status = format!("couldn't clear group graph: {e}"),
                        }
                    }
                }
            });
        self.show_groups = open && !cancel;
        if let Some(idx) = remove {
            self.group_edges.remove(idx);
        }
        if save {
            let Some(path) = self.groups_path.clone() else {
                return;
            };
            match crate::groups::save(&path, &self.group_edges) {
                Ok(()) => self.status = format!("group graph saved: {} link(s). The graph is ready for the ordering pass; no mods were moved.", self.group_edges.len()),
                Err(e) => self.status = format!("couldn't save group graph: {e}"),
            }
        }
    }

    // The generated roadmap is intentionally conservative: when a concept
    // has multiple plausible separators, only a human gets to settle it.
    // This little review window saves that choice as one normal [map] line.
    fn roadmap_review_window(&mut self, ctx: &egui::Context) {
        if !self.show_roadmap {
            return;
        }
        let mut open = self.show_roadmap;
        let mut reviewed: Option<(String, String)> = None;
        let concepts = self.roadmap_ambiguous.clone();
        let reviewed_rows = self.roadmap_reviewed.clone();
        let multi_rows = self.roadmap_multi.clone();
        let branch_rows = self.roadmap_branches.clone();
        let destinations = self.roadmap_destinations.clone();
        let mut mark_multi: Option<String> = None;
        egui::Window::new("Separator roadmap review")
            .open(&mut open)
            .resizable(true)
            .default_width(670.0)
            .show(ctx, |ui| {
                ui.label("These concepts matched more than one separator, so ModSlut refused to guess.");
                ui.label(egui::RichText::new("Pick the one actual home for a concept. This saves only that roadmap decision; no mods are moved yet.").color(DIM));
                ui.separator();
                if concepts.is_empty() {
                    ui.label("Nothing ambiguous is waiting for review. Nice. Suspiciously nice.");
                }
                egui::ScrollArea::vertical().max_height(410.0).show(ui, |ui| {
                    for concept in &concepts {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(concept).strong().monospace());
                            let selected = self.roadmap_picks.get(concept).cloned().unwrap_or_else(|| "choose a separator…".into());
                            egui::ComboBox::from_id_salt(("roadmap-choice", concept))
                                .selected_text(selected)
                                .width(405.0)
                                .show_ui(ui, |ui| {
                                    for destination in &destinations {
                                        if ui.selectable_label(false, destination).clicked() {
                                            reviewed = Some((concept.clone(), destination.clone()));
                                        }
                                    }
                                });
                            if ui.small_button("leave unresolved").on_hover_text(
                                "This concept has several legitimate homes. MS will wait for narrower evidence or a user rule."
                            ).clicked() {
                                mark_multi = Some(concept.clone());
                            }
                        });
                    }
                    ui.add_space(10.0);
                    ui.collapsing("Already mapped (click one to change it)", |ui| {
                        for (concept, current) in &reviewed_rows {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(concept).monospace().color(DIM));
                                egui::ComboBox::from_id_salt(("roadmap-mapped", concept))
                                    .selected_text(current)
                                    .width(405.0)
                                    .show_ui(ui, |ui| {
                                        for destination in &destinations {
                                            if ui.selectable_label(false, destination).clicked() {
                                                reviewed = Some((concept.clone(), destination.clone()));
                                            }
                                        }
                                    });
                            });
                        }
                    });
                    ui.collapsing("Intentionally multi-destination", |ui| {
                        for concept in &multi_rows {
                            ui.label(egui::RichText::new(concept).monospace().color(DIM));
                        }
                    });
                    ui.collapsing("Routing branches (not one global home)", |ui| {
                        ui.label(egui::RichText::new(
                            "These are the usable shelves for a split concept. A narrower matcher picks a route; MS will not flatten them into one junk-drawer destination.",
                        ).color(DIM));
                        for (concept, destinations) in &branch_rows {
                            ui.add_space(5.0);
                            ui.label(egui::RichText::new(concept).strong().monospace());
                            for destination in destinations {
                                ui.label(format!("  ├─ {destination}"));
                            }
                        }
                    });
                });
            });
        self.show_roadmap = open;
        if let Some(concept) = mark_multi {
            let Some(path) = self.roadmap_path.clone() else {
                self.status = "roadmap path vanished somehow - reload and try again".into();
                return;
            };
            match crate::section_map::save_multi_destination(&path, &concept) {
                Ok(()) => {
                    self.status = format!("[{concept}] marked multi-destination. MS will not fake a global home for it.");
                    self.show_roadmap = false;
                    if let Some(file) = self.file.clone() {
                        self.load_and_preview(file);
                    }
                }
                Err(e) => self.status = format!("couldn't save roadmap {}: {e}", path.display()),
            }
        } else if let Some((concept, destination)) = reviewed {
            let Some(path) = self.roadmap_path.clone() else {
                self.status = "roadmap path vanished somehow - reload and try again".into();
                return;
            };
            match crate::section_map::save_reviewed_mapping(&path, &concept, &destination) {
                Ok(()) => {
                    self.status = format!("roadmap saved: [{concept}] -> [{destination}]. now it can be used by the sorter.");
                    self.show_roadmap = false;
                    if let Some(file) = self.file.clone() {
                        self.load_and_preview(file);
                    }
                }
                Err(e) => self.status = format!("couldn't save roadmap {}: {e}", path.display()),
            }
        }
    }
}

impl eframe::App for ModslutApp {
    // save layout prefs whenever the window closes (any path: x, exit,
    // apply & quit)
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let prefs = UiPrefs {
            panel: Some(self.changes_width),
            size: self.win_rect.map(|r| (r.width(), r.height())),
            pos: self.win_rect.map(|r| (r.min.x, r.min.y)),
            settings_size: self.settings_rect.map(|r| (r.width(), r.height())),
            settings_pos: self.settings_rect.map(|r| (r.min.x, r.min.y)),
            user_rules_only: self.user_rules_only,
        };
        save_ui_prefs(&prefs);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Pick up a completed background plan without ever blocking the UI.
        // Preserve the window-only preferences from the live app; the worker
        // is intentionally disposable planning state.
        if let Some(rx) = self.sort_rx.as_ref() {
            match rx.try_recv() {
                Ok(worker) => {
                    let font_size = self.font_size;
                    let changes_width = self.changes_width;
                    let win_rect = self.win_rect;
                    let settings_rect = self.settings_rect;
                    *self = *worker;
                    self.font_size = font_size;
                    self.changes_width = changes_width;
                    self.win_rect = win_rect;
                    self.settings_rect = settings_rect;
                    self.sort_waiting = false;
                    self.sort_rx = None;
                }
                // The worker does the expensive sorting. Repainting the
                // entire browser at 60 fps while it runs only steals CPU
                // from it, especially on a 2k-mod profile. Twenty fps keeps
                // the UI and status responsive without competing for a core.
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(50))
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.sort_rx = None;
                    self.sort_waiting = false;
                    self.status =
                        "sort worker quit early - nothing was written; try Sort Mods again".into();
                }
            }
        }

        // Masterlist downloads are intentionally separate from the sort
        // worker. They update only LOOT's local cache and never touch the
        // MO2 profile, so the result can refresh the General Information
        // card without discarding a useful ModSlut preview.
        if let Some(rx) = self.masterlist_update_rx.as_ref() {
            match rx.try_recv() {
                Ok(Ok(status)) => {
                    self.masterlist_revision = status.revision;
                    self.masterlist_updated = status.updated;
                    self.masterlist_source = status.source;
                    self.masterlist_update_rx = None;
                    self.status =
                        "LOOT masterlist updated; a rollback copy was saved beside it".into();
                }
                Ok(Err(error)) => {
                    self.masterlist_update_rx = None;
                    self.status = format!("LOOT masterlist update failed: {error}");
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.masterlist_update_rx = None;
                    self.status =
                        "LOOT masterlist updater quit early; existing masterlist was left alone"
                            .into();
                }
            }
        }

        // Persist on each actual move/resize, not just on_exit: MO2 can end
        // an external tool without giving eframe a final shutdown callback.
        ctx.input(|i| {
            if let Some(r) = i.viewport().outer_rect {
                let changed = self.win_rect.is_none_or(|old| {
                    (old.min - r.min).length_sq() > 1.0 || (old.size() - r.size()).length_sq() > 1.0
                });
                self.win_rect = Some(r);
                if changed {
                    save_ui_prefs(&UiPrefs {
                        panel: Some(self.changes_width),
                        size: Some((r.width(), r.height())),
                        pos: Some((r.min.x, r.min.y)),
                        settings_size: self.settings_rect.map(|r| (r.width(), r.height())),
                        settings_pos: self.settings_rect.map(|r| (r.min.x, r.min.y)),
                        user_rules_only: self.user_rules_only,
                    });
                }
            }
        });

        // a background conflict scan finishing invalidates the preview -
        // reload, which now picks the fresh conflict.ini up synchronously.
        // keep repainting while it's running so the spinner/status stays live.
        if self.scan_rx.is_some() {
            let done = self
                .scan_rx
                .as_ref()
                .is_some_and(|rx| rx.try_recv().is_ok());
            if done {
                self.scan_rx = None;
                if let Some(p) = self.file.clone() {
                    self.refresh_after_conflict_scan(p);
                }
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
        }

        // Theme packs are intentionally ordinary folders next to ModSlut.exe:
        // `themes/<name>/theme.toml` plus its image. A changed selection gets
        // a fresh texture immediately; malformed packs fall back safely.
        let selected_pack = if self.theme == "MO2 Mirror (read-only)" {
            active_mo2_theme(self.file.as_deref())
        } else {
            theme_packs()
                .into_iter()
                .find(|pack| pack.name == self.theme)
        };
        if self.theme_texture_name != self.theme {
            self.theme_background = None;
            self.theme_texture_name = self.theme.clone();
        }
        let background_asset: Option<(&str, &[u8])> = match self.theme.as_str() {
            "Skybound Nebula" | "SkyGen Blue" => Some((
                "modslut-skybound-nebula",
                include_bytes!("../assets/themes/skybound-nebula.png"),
            )),
            "Midnight Roses" => Some((
                "modslut-midnight-roses",
                include_bytes!("../assets/themes/midnight-roses.png"),
            )),
            "Ember Coast" => Some((
                "modslut-ember-coast",
                include_bytes!("../assets/themes/ember-coast.png"),
            )),
            "Winterpine" => Some((
                "modslut-winterpine",
                include_bytes!("../assets/themes/winterpine.png"),
            )),
            _ => None,
        };
        if let Some(path) = selected_pack
            .as_ref()
            .and_then(|pack| pack.background.as_ref())
        {
            if self.theme_background.is_none() {
                if let Ok(image) = image::open(path) {
                    let rgba = image.to_rgba8();
                    let size = [rgba.width() as usize, rgba.height() as usize];
                    self.theme_background = Some(ctx.load_texture(
                        format!("modslut-theme-{}", self.theme),
                        egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()),
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
        } else if let Some((texture_name, asset)) = background_asset {
            if self.theme_background.is_none() {
                if let Ok(image) = image::load_from_memory(asset) {
                    let rgba = image.to_rgba8();
                    let size = [rgba.width() as usize, rgba.height() as usize];
                    self.theme_background = Some(ctx.load_texture(
                        texture_name,
                        egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()),
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
        } else {
            self.theme_background = None;
        }

        let fs = self.font_size;
        let (
            mut theme_panel,
            mut theme_toolbar,
            mut theme_accent,
            mut card_fill,
            mut card_selected,
        ) = match self.theme.as_str() {
            "Skybound Nebula" | "SkyGen Blue" => (
                egui::Color32::from_rgb(0x18, 0x26, 0x3a),
                egui::Color32::from_rgb(0x0d, 0x16, 0x25),
                egui::Color32::from_rgb(0x4a, 0x90, 0xe2),
                egui::Color32::from_rgba_unmultiplied(0x14, 0x25, 0x3a, 230),
                egui::Color32::from_rgb(0x1e, 0x3d, 0x5b),
            ),
            "Midnight Roses" => (
                egui::Color32::from_rgb(0x2c, 0x1c, 0x3a),
                egui::Color32::from_rgb(0x18, 0x0d, 0x28),
                egui::Color32::from_rgb(0xd6, 0x72, 0xd8),
                egui::Color32::from_rgba_unmultiplied(0x2a, 0x16, 0x38, 230),
                egui::Color32::from_rgb(0x4d, 0x27, 0x62),
            ),
            "Ember Coast" => (
                egui::Color32::from_rgb(0x31, 0x25, 0x25),
                egui::Color32::from_rgb(0x1c, 0x17, 0x1c),
                egui::Color32::from_rgb(0xdc, 0x7c, 0x42),
                egui::Color32::from_rgba_unmultiplied(0x32, 0x24, 0x24, 232),
                egui::Color32::from_rgb(0x5b, 0x3b, 0x32),
            ),
            "Winterpine" => (
                egui::Color32::from_rgb(0x1c, 0x2d, 0x36),
                egui::Color32::from_rgb(0x0e, 0x1b, 0x22),
                egui::Color32::from_rgb(0x66, 0xbd, 0xe8),
                egui::Color32::from_rgba_unmultiplied(0x17, 0x2a, 0x34, 232),
                egui::Color32::from_rgb(0x2b, 0x4b, 0x5a),
            ),
            _ => (
                BG,
                TOOLBAR,
                APPLY,
                egui::Color32::from_rgb(42, 42, 42),
                egui::Color32::from_rgb(48, 56, 63),
            ),
        };
        if let Some(pack) = selected_pack {
            if let Some(color) = pack.panel {
                theme_panel = color;
            }
            if let Some(color) = pack.toolbar {
                theme_toolbar = color;
            }
            if let Some(color) = pack.accent {
                theme_accent = color;
            }
            if let Some(color) = pack.card {
                card_fill = color;
            }
            if let Some(color) = pack.selected {
                card_selected = color;
            }
        }
        // Cards are deliberately translucent: background art remains a
        // theme, not a wallpaper hidden behind opaque gray rectangles. Blend
        // arbitrary imported QSS grays back toward the panel too; otherwise
        // a valid but light MO2 `background-color` still reads as opaque.
        // Keep this deliberately low even for legacy imported packs that
        // contain a guessed card_bg from an older ModSlut release.
        card_fill = blend_theme_color(card_fill, theme_panel, 12);
        card_selected = blend_theme_color(card_selected, theme_panel, 42);
        self.theme_card_fill =
            egui::Color32::from_rgba_unmultiplied(card_fill.r(), card_fill.g(), card_fill.b(), 78);
        self.theme_card_selected = egui::Color32::from_rgba_unmultiplied(
            card_selected.r(),
            card_selected.g(),
            card_selected.b(),
            136,
        );
        self.theme_card_border = theme_accent.linear_multiply(0.72);
        ctx.style_mut(|s| {
            s.visuals = egui::Visuals::dark();
            s.visuals.window_fill = theme_panel;
            s.visuals.panel_fill = theme_panel;
            s.visuals.extreme_bg_color = theme_toolbar;
            s.visuals.selection.bg_fill = theme_accent;
            s.visuals.hyperlink_color = theme_accent;
            // user-scaled type
            s.text_styles
                .insert(egui::TextStyle::Body, egui::FontId::proportional(fs));
            s.text_styles
                .insert(egui::TextStyle::Button, egui::FontId::proportional(fs));
            s.text_styles
                .insert(egui::TextStyle::Monospace, egui::FontId::monospace(fs));
        });

        // ---- top toolbar ----
        egui::TopBottomPanel::top("toolbar")
            .frame(egui::Frame::NONE.fill(theme_toolbar).inner_margin(egui::Margin::symmetric(12, 10)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let btn = |label: &str| {
                        egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE))
                            .fill(theme_toolbar)
                    };
                    // A warning alone is useful review information but is not
                    // something that can be applied. Plugin previews are
                    // produced by libloot and apply with their own backup.
                    let pending_mods = self.plan.iter().any(|change| change.kind != ChangeKind::Warn);
                    let pending_plugins = self.plugin_sorted.is_some();
                    let ready = (pending_mods && self.sorted.is_some() || pending_plugins)
                        && !self.written;
                    let apply_label = match (pending_mods, pending_plugins) {
                        (true, true) => "apply both",
                        (false, true) => "apply plugins",
                        (true, false) => "apply mods",
                        (false, false) => "review warnings",
                    };
                    // Windows-style menus keep secondary actions discoverable
                    // without turning the title bar into a button wall.
                    ui.menu_button("File", |ui| {
                        // LOOT-compatible file-menu vocabulary. Actions that
                        // would require a settings or masterlist service stay
                        // visibly unavailable until MS can perform them for
                        // real; a menu item must never be a decorative lie.
                        if ui.button("Settings…").clicked() {
                            self.open_settings();
                            ui.close_menu();
                        }
                        if ui.add_enabled(
                            self.masterlist_path.is_some()
                                && self.masterlist_source.is_some()
                                && self.masterlist_update_rx.is_none(),
                            egui::Button::new("Update Masterlist"),
                        ).on_hover_text("Downloads LOOT's configured source for this active game, then makes a rollback copy of the old local masterlist.").clicked() {
                            self.start_masterlist_update();
                            ui.close_menu();
                        }
                        ui.add_enabled(false, egui::Button::new("Backup LOOT Data"))
                            .on_hover_text("Reserved for a safe profile-data backup flow.");
                        ui.add_enabled(false, egui::Button::new("Open LOOT Data Folder"))
                            .on_hover_text("Reserved for the game-data inspection panel.");
                        ui.separator();
                        if ui.button("Open modlist.txt…").clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .set_title("open modlist.txt")
                                .add_filter("modlist", &["txt"])
                                .pick_file()
                            {
                                self.open_idle(path);
                            }
                            ui.close_menu();
                        }
                        if ui.add_enabled(self.file.is_some(), egui::Button::new("Reload")).clicked() {
                            if let Some(path) = self.file.clone() { self.open_idle(path); }
                            ui.close_menu();
                        }
                        if self.debug_note.is_some() && self.file.is_some() && ui.button("Open debug log").clicked() {
                            let p = crate::debug_log_path();
                            let opened = if cfg!(target_os = "windows") {
                                std::process::Command::new("notepad.exe").arg(&p).spawn().is_ok()
                            } else {
                                std::process::Command::new("xdg-open").arg(&p).spawn().is_ok()
                            };
                            if !opened { self.status = format!("log is at {}", p.display()); }
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Exit").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            ui.close_menu();
                        }
                    });
                    ui.menu_button("Rules", |ui| {
                        if ui.add_enabled(self.file.is_some(), egui::Button::new("Open user rules")).clicked() {
                            self.open_user_rules();
                            ui.close_menu();
                        }
                        if ui.add_enabled(self.file.is_some(), egui::Button::new("Edit separator roadmap…")).clicked() {
                            self.open_roadmap_editor();
                            ui.close_menu();
                        }
                        if ui.add_enabled(self.file.is_some(), egui::Button::new("Import separator roadmap…")).clicked() {
                            self.import_roadmap();
                            ui.close_menu();
                        }
                        if ui.add_enabled(self.file.is_some(), egui::Button::new("Import private reference modlist…")).clicked() {
                            self.import_private_reference();
                            ui.close_menu();
                        }
                        if self.cats_report.is_some()
                            && ui.button(if self.show_cats { "Show changes" } else { "Show categories" }).clicked()
                        {
                            self.show_cats = !self.show_cats;
                            ui.close_menu();
                        }
                        let mut uro = self.user_rules_only;
                        if ui.checkbox(&mut uro, "Preview user rules only").on_hover_text(
                            "Preview with built-in rules and guesses off. This does not save anything."
                        ).changed() {
                            self.user_rules_only = uro;
                            if let Some(path) = self.file.clone() { self.load_and_preview(path); }
                        }
                    });
                        ui.menu_button("Game", |ui| {
                        if ui.add_enabled(self.file.is_some(), egui::Button::new("Edit Groups…")).clicked() {
                            self.open_groups_editor();
                            ui.close_menu();
                        }
                        if ui.add_enabled(self.file.is_some(), egui::Button::new("Import Groups…")).clicked() {
                            self.import_groups();
                            ui.close_menu();
                        }
                        if ui.add_enabled(self.file.is_some(), egui::Button::new("Clear Group Links")).clicked() {
                            self.clear_groups();
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.add_enabled(self.file.is_some() && !self.sort_waiting, egui::Button::new("Sort Plugins")).clicked() {
                            if let Some(path) = self.file.clone() { self.start_sort_worker(path, false, true); }
                            ui.close_menu();
                        }
                        if ui.add_enabled(
                            self.masterlist_path.is_some()
                                && self.masterlist_source.is_some()
                                && self.masterlist_update_rx.is_none(),
                            egui::Button::new("Update Masterlist"),
                        ).on_hover_text("Downloads LOOT's configured source for this active game, then makes a rollback copy of the old local masterlist.").clicked() {
                            self.start_masterlist_update();
                            ui.close_menu();
                        }
                        ui.add_enabled(false, egui::Button::new("Search Cards…")).on_hover_text("Reserved for LOOT metadata cards.");
                        ui.separator();
                        ui.add_enabled(false, egui::Button::new("Copy Load Order")).on_hover_text("Reserved for the plugin-order review panel.");
                        ui.add_enabled(false, egui::Button::new("Copy Content")).on_hover_text("Reserved for the plugin-order review panel.");
                        if ui.add_enabled(self.file.is_some(), egui::Button::new("Refresh Content")).clicked() {
                            if let Some(path) = self.file.clone() { self.load_and_preview(path); }
                            ui.close_menu();
                        }
                        ui.add_enabled(false, egui::Button::new("Fix Ambiguous Load Order")).on_hover_text("Ambiguous ordering is reviewed in ModSlut before anything is applied.");
                        ui.add_enabled(false, egui::Button::new("Redate Plugins…")).on_hover_text("Not applicable to MO2 left-pane sorting.");
                        ui.add_enabled(false, egui::Button::new("Clear All User Metadata…")).on_hover_text("Will clear only explicit ModSlut metadata after a separate confirmation flow is added.");
                        ui.separator();
                        ui.label(egui::RichText::new("The group spine follows the current MO2 separator order.").color(DIM));
                        ui.label(egui::RichText::new("Extra links are profile-local and never modify LOOT.").color(DIM));
                    });
                    ui.menu_button(if ready { "Apply" } else { "Sort" }, |ui| {
                        if !ready && ui
                            .add_enabled(self.file.is_some() && !self.sort_waiting, egui::Button::new("Sort Mods"))
                            .on_hover_text("Plan only MO2 mod/separator placement. It does not sort plugins.txt.")
                            .clicked()
                        {
                            if let Some(path) = self.file.clone() {
                                self.start_sort_worker(path, true, false);
                            }
                            ui.close_menu();
                        }
                        if !ready && ui
                            .add_enabled(self.file.is_some() && !self.sort_waiting, egui::Button::new("Sort Plugins"))
                            .on_hover_text("Run libloot and prepare only a plugins.txt preview. It does not alter the ModSlut left-pane plan.")
                            .clicked()
                        {
                            if let Some(path) = self.file.clone() {
                                self.start_sort_worker(path, false, true);
                            }
                            ui.close_menu();
                        }
                        if !ready && ui
                            .add_enabled(self.file.is_some() && !self.sort_waiting, egui::Button::new("Sort Both"))
                            .on_hover_text("Build both the ModSlut left-pane plan and libloot's plugins.txt preview.")
                            .clicked()
                        {
                            if let Some(path) = self.file.clone() {
                                self.start_sort_worker(path, true, true);
                            }
                            ui.close_menu();
                        }
                        if !ready { ui.separator(); }
                        if ready && ui
                            .button("Discard Sorted Load Order")
                            .on_hover_text("Throw away this preview and reload MO2's current modlist. Nothing is written.")
                            .clicked()
                        {
                            self.discard_sorted_preview();
                            ui.close_menu();
                        }
                        if ready { ui.separator(); }
                        if pending_mods && ui.button("Apply Mods & quit").clicked() {
                            self.apply(true, false);
                            if self.written {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            ui.close_menu();
                        }
                        if pending_plugins && ui.button("Apply Plugins & quit").clicked() {
                            self.apply(false, true);
                            if self.written {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            ui.close_menu();
                        }
                        if pending_mods && pending_plugins && ui.button("Apply Both & quit").clicked() {
                            self.apply(true, true);
                            if self.written {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(
                                self.file.is_some() && !self.written,
                                egui::Button::new("Apply user rules only & quit"),
                            )
                            .on_hover_text(
                                "Discard the ModSlut sort preview, apply only explicit profile user rules, then close so MO2 refreshes.",
                            )
                            .clicked()
                        {
                            if self.apply_user_rules_only() {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            ui.close_menu();
                        }
                    });
                    ui.menu_button("Help", |ui| {
                        ui.label(format!("ModSlut v{}", crate::VERSION));
                        ui.label("Preview first. Saving always creates a backup.");
                    });
                    ui.separator();

                    // Same visual anchor as LOOT's game picker. It reports
                    // the game libloot identified from root masters; choosing
                    // a different game is intentionally not offered yet,
                    // because a cosmetic override must never feed the solver
                    // the wrong masterlist.
                    egui::ComboBox::from_id_salt("detected-game-picker")
                        .selected_text(&self.detected_game)
                        .width(225.0)
                        .show_ui(ui, |ui| {
                            ui.label(egui::RichText::new("Detected from active root masters").color(DIM));
                            ui.separator();
                            for game in ["Skyrim SE/AE/VR", "Fallout 4", "Starfield", "Enderal", "Oblivion", "Fallout 3 / New Vegas"] {
                                ui.add_enabled(false, egui::Button::new(game));
                            }
                        });
                    ui.separator();

                    let primary_label = if ready { apply_label } else { "sort both" };
                    let primary_enabled = if ready {
                        !self.written && (pending_mods || pending_plugins)
                    } else {
                        self.file.is_some() && !self.sort_waiting
                    };
                    if ui
                        .add_enabled(
                            primary_enabled,
                            egui::Button::new(
                                egui::RichText::new(primary_label)
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            )
                        .fill(theme_accent),
                        )
                        .on_hover_text(if ready {
                            "Apply the reviewed ModSlut sort, create a backup, then close so MO2 refreshes."
                        } else {
                            "Refresh missing/stale evidence, then build a safe sort preview. Nothing is written."
                        })
                        .clicked()
                    {
                        if ready {
                            self.apply(pending_mods, pending_plugins);
                            if self.written {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        } else if let Some(path) = self.file.clone() {
                            self.start_sort_worker(path, true, true);
                        }
                    }
                    if ready && ui
                        .button("discard sorted load order")
                        .on_hover_text("Discard this preview; it has not changed your MO2 files.")
                        .clicked()
                    {
                        self.discard_sorted_preview();
                    }
                    if ui.add_enabled(self.file.is_some(), btn("reload")).clicked() {
                        if let Some(path) = self.file.clone() { self.load_and_preview(path); }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(f) = &self.file {
                            let profile = f
                                .parent()
                                .and_then(|p| p.file_name())
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            ui.label(
                                egui::RichText::new(format!("active profile: {profile}"))
                                    .color(egui::Color32::WHITE),
                            )
                            .on_hover_text(f.display().to_string());
                        }
                    });

                    // grab any empty toolbar strip to drag the whole window -
                    // handy when the native titlebar is offscreen or cramped
                    let strip = ui.available_rect_before_wrap();
                    if strip.width() > 0.0 && strip.height() > 0.0 {
                        let dresp = ui.interact(
                            strip,
                            ui.id().with("win_drag_strip"),
                            egui::Sense::drag(),
                        );
                        if dresp.drag_started() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                        }
                    }
                });
            });

        self.roadmap_review_window(ctx);
        self.groups_editor_window(ctx);
        self.settings_window(ctx);

        // ---- bottom status bar ----
        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::NONE
                    .fill(theme_toolbar)
                    .inner_margin(egui::Margin::symmetric(14, 8)),
            )
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(&self.status).color(DIM).italics());
            });

        // ---- main area: manual left/right split ----
        // egui's built-in panel resize state kept snapping back on this list,
        // so we own the divider ourselves (and persist it to disk)
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                let full = ui.available_rect_before_wrap();
                let min_w = 240.0_f32;
                let max_w = (full.width() - 240.0).max(min_w);
                self.changes_width = self.changes_width.clamp(min_w, max_w);
                let x = full.min.x + self.changes_width;
                let left_rect = egui::Rect::from_min_max(full.min, egui::pos2(x, full.max.y));
                let right_rect =
                    egui::Rect::from_min_max(egui::pos2(x + 6.0, full.min.y), full.max);

                if let Some(background) = &self.theme_background {
                    ui.painter().image(
                        background.id(),
                        full,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }
                let overlay = if self.theme_background.is_some() {
                    // The texture is cached and smoothly sampled once. Keep
                    // the panel tint light enough that the original artwork
                    // remains crisp, while preserving a little soft depth.
                    egui::Color32::from_rgba_unmultiplied(theme_panel.r(), theme_panel.g(), theme_panel.b(), 170)
                } else { theme_panel };
                ui.painter().rect_filled(left_rect, 0.0, overlay);
                ui.painter().rect_filled(right_rect, 0.0, overlay);

                // draggable divider - 6px grab strip with resize cursor
                let sep_rect = egui::Rect::from_min_max(
                    egui::pos2(x, full.min.y),
                    egui::pos2(x + 6.0, full.max.y),
                );
                let sresp =
                    ui.interact(sep_rect, ui.id().with("split_drag"), egui::Sense::drag());
                if sresp.hovered() || sresp.dragged() {
                    ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                if sresp.dragged() {
                    if let Some(p) = sresp.interact_pointer_pos() {
                        self.changes_width = (p.x - full.min.x).clamp(min_w, max_w);
                    }
                }
                ui.painter().vline(
                    x + 3.0,
                    full.y_range(),
                    egui::Stroke::new(
                        1.0_f32,
                        if sresp.hovered() || sresp.dragged() { DIM } else { TOOLBAR },
                    ),
                );

                // left: change list
                let mut left_ui = ui.new_child(
                    egui::UiBuilder::new().max_rect(left_rect.shrink2(egui::vec2(14.0, 10.0))),
                );
                left_ui.set_clip_rect(left_rect);
                (|ui: &mut egui::Ui| {
                let total_mods: usize =
                    self.layout.iter().map(|(_, m)| m.len()).sum::<usize>()
                        + self.parking_mods.len();
                ui.label(
                    egui::RichText::new(format!(
                        "{total_mods} mods · {} change(s)",
                        self.plan.len()
                    ))
                    .strong()
                    .size(self.font_size * 1.1),
                );
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Search mods:");
                    ui.add_sized(
                        egui::vec2((ui.available_width() - 120.0).max(120.0), 0.0),
                        egui::TextEdit::singleline(&mut self.left_search)
                            .hint_text("mod or separator"),
                    );
                });
                // Use a real header strip rather than loose labels. The
                // fixed cells below deliberately mirror the widths used by
                // every child row; the lone section rule is the same quiet
                // State/Index boundary LOOT keeps with row grids disabled.
                let header_width = ui.available_width();
                egui::Frame::NONE
                    .fill(egui::Color32::from_black_alpha(26))
                    .inner_margin(egui::Margin::symmetric(0, 2))
                    .show(ui, |ui| {
                        ui.set_min_width(header_width - 2.0);
                        ui.horizontal(|ui| {
                            let header_height = ui.spacing().interact_size.y;
                            // Separator children inherit the tree's disclosure
                            // gutter before their framed cells. Reserve that same
                            // gutter in the header so Position, State and Index all
                            // share the rows' actual column starts.
                            ui.add_space(35.0);
                            ui.add_sized(
                                egui::vec2(66.0, header_height),
                                egui::Label::new(
                                    egui::RichText::new("Position")
                                        .size(self.font_size)
                                        .color(egui::Color32::WHITE),
                                ).wrap_mode(egui::TextWrapMode::Extend),
                            );
                            ui.add_sized(
                                egui::vec2(34.0, header_height),
                                egui::Label::new(
                                    egui::RichText::new("State")
                                        .size(self.font_size)
                                        .color(egui::Color32::WHITE),
                                ).wrap_mode(egui::TextWrapMode::Extend),
                            );
                            left_index_divider(ui);
                            // `add_sized`'s label starts before the short
                            // monospace index glyphs in the data rows. Nudge
                            // just the header text to their true column start;
                            // the cell is shortened by the same amount below
                            // so the Mod / separator column does not move.
                            ui.add_space(17.0);
                            // The index value is rendered in a 64 px data
                            // cell, but its short text occupies the leading
                            // 32 px after the header inset. Use that visible
                            // column edge here so the
                            // final label begins exactly with the mod names.
                            ui.add_sized(
                                egui::vec2(32.0, header_height),
                                egui::Label::new(
                                    egui::RichText::new("Index")
                                        .size(self.font_size)
                                        .color(egui::Color32::WHITE),
                                ).wrap_mode(egui::TextWrapMode::Extend),
                            );
                            ui.add_sized(
                                egui::vec2(ui.available_width(), header_height),
                                egui::Label::new(
                                    egui::RichText::new("Mod / separator")
                                        .size(self.font_size)
                                        .color(egui::Color32::WHITE),
                                ).wrap_mode(egui::TextWrapMode::Extend),
                            );
                        });
                    });
                // A native table header separates itself from the body with a
                // single rule, not a floating rectangle around every label.
                ui.separator();
                if self.file.is_none() {
                    ui.add_space(16.0);
                    ui.label("run me from mo2's executable dropdown,");
                    ui.label("or open a modlist.txt by hand.");
                    return;
                }
                // loot-style whole-list view: every section, every mod.
                // sections with pending changes open automatically; the rest
                // stay collapsed but every row is clickable/right-clickable
                // so rules can be written against ANY mod, not just movers.
                let left_needle = self.left_search.trim().to_ascii_lowercase();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        let mut groups: Vec<(String, Vec<String>)> = self.layout.clone();
                        if !self.parking_mods.is_empty() {
                            groups.push(("parking lot".to_string(), self.parking_mods.clone()));
                        }
                        // `modlist.txt` stores a separator after the mods it
                        // owns, while MO2 presents the same data in reverse:
                        // separator first, then its children.  Keep storage
                        // order for safe serialization, but mirror MO2 here.
                        for (sec, mods) in groups.into_iter().rev() {
                            if self.roadmap_context.contains(&sec) {
                                continue;
                            }
                            let section_matches = self
                                .left_search_terms
                                .get(&sec)
                                .is_some_and(|term| term.contains(&left_needle));
                            let visible_mods: Vec<&String> = mods
                                .iter()
                                .filter(|name| {
                                    left_needle.is_empty()
                                        || section_matches
                                        || self
                                            .left_search_terms
                                            .get(name.as_str())
                                            .is_some_and(|term| term.contains(&left_needle))
                                })
                                .collect();
                            if !left_needle.is_empty() && visible_mods.is_empty() {
                                continue;
                            }
                            let separator_priority = self.separator_priorities.get(&sec).copied();
                            let n_changes: usize = mods
                                .iter()
                                .map(|m| self.left_change_map.get(m.as_str()).map_or(0, |v| v.len()))
                                .sum();
                            let section_prefix = separator_priority
                                .map(|priority| format!("{priority:<4}  "))
                                .unwrap_or_default();
                            let head = if n_changes > 0 {
                                egui::RichText::new(format!(
                                    "{section_prefix}{sec}  ·  {} mods · {n_changes} change(s)",
                                    visible_mods.len()
                                ))
                                .strong()
                                .color(egui::Color32::WHITE)
                            } else {
                                egui::RichText::new(format!("{section_prefix}{sec}  ·  {} mods", visible_mods.len()))
                                    .color(DIM)
                            };
                            let collapsed = egui::CollapsingHeader::new(head)
                                .id_salt(format!("sec::{sec}::{}", self.collapse_generation))
                                // This is the primary browser, not merely a
                                // change review. Show installed rows before
                                // Sort Mods is ever pressed.
                                // A fresh salt is used for Collapse all; it
                                // must therefore start closed, not inherit
                                // the ordinary initial-open preference.
                                .default_open(self.collapse_generation == 0)
                                .show(ui, |ui| {
                                    for m in visible_mods.into_iter().rev() {
                                        match self.left_change_map.get(m.as_str()) {
                                            Some(idxs) => {
                                                for i in idxs.clone() {
                                                    self.change_row(ui, i, &sec);
                                                }
                                            }
                                            None => self.plain_row(ui, m, &sec),
                                        }
                                    }
                                });
                            collapsed.header_response.context_menu(|ui| {
                                if ui.button("Collapse all separators").clicked() {
                                    self.collapse_generation = self.collapse_generation.wrapping_add(1);
                                    ui.close_menu();
                                }
                                if ui.button("Learn homes in this separator").clicked() {
                                    self.learn_section(&sec, &mods);
                                    ui.close_menu();
                                }
                                if ui.button("Learn homes in entire modlist").clicked() {
                                    self.learn_all_homes();
                                    ui.close_menu();
                                }
                                ui.label("Both save exact profile-local homes; neither moves anything.");
                            });
                        }
                    });
                })(&mut left_ui);

                // right: summary + detail
                let mut right_ui = ui.new_child(
                    egui::UiBuilder::new().max_rect(right_rect.shrink2(egui::vec2(16.0, 12.0))),
                );
                right_ui.set_clip_rect(right_rect);
                egui::ScrollArea::vertical()
                    .id_salt("loot_style_right_pane")
                    .auto_shrink([false, false])
                    .show(&mut right_ui, |ui| {
                // categories view: which mo2/nexus categories your mods carry
                // and which separator each one maps to
                if self.show_cats {
                    if let Some(rep) = &self.cats_report {
                        ui.label(
                            egui::RichText::new("categories in use -> separator mapping")
                                .strong()
                                .size(self.font_size * 1.1),
                        );
                        ui.label(
                            egui::RichText::new(
                                "@ = pinned user rule · roadmap = reviewed separator bridge · exact = same-name separator · everything else stays put",
                            )
                            .color(DIM)
                            .italics(),
                        );
                        ui.add_space(6.0);
                        ui.add(
                            egui::TextEdit::multiline(&mut rep.as_str())
                                .font(egui::TextStyle::Monospace)
                                .desired_width(f32::INFINITY)
                                .interactive(true),
                        );
                    }
                    return;
                }

                ui.label(egui::RichText::new("general information").strong().size(self.font_size * 1.1));
                ui.add_space(6.0);

                if self.file.is_none() {
                    ui.label("no modlist loaded.");
                    return;
                }

                let moves = self.plan.iter().filter(|c| c.kind == ChangeKind::Move).count();
                let separator_moves = self
                    .plan
                    .iter()
                    .filter(|c| c.kind == ChangeKind::Separator)
                    .count();
                let reors = self
                    .plan
                    .iter()
                    .filter(|c| c.kind == ChangeKind::Reorder)
                    .count();
                let proms = self
                    .plan
                    .iter()
                    .filter(|c| c.kind == ChangeKind::Promote)
                    .count();
                let sinks = self
                    .plan
                    .iter()
                    .filter(|c| c.kind == ChangeKind::Sink)
                    .count();
                let total_mods = self
                    .layout
                    .iter()
                    .map(|(_, mods)| mods.len())
                    .sum::<usize>()
                    + self.parking_mods.len();
                let active_plugins = self
                    .active_plugin_count
                    .map(|count| count.to_string())
                    .unwrap_or_else(|| "—".into());
                let masterlist_revision = self
                    .masterlist_revision
                    .as_deref()
                    .map(|revision| revision.chars().take(7).collect::<String>())
                    .unwrap_or_else(|| "—".into());
                let masterlist_updated = self
                    .masterlist_updated
                    .as_deref()
                    .unwrap_or("—");
                let loot_count = |count: Option<usize>| count.map(|value| value.to_string()).unwrap_or_else(|| "—".into());

                // LOOT's General Information card is the base. ModSlut's
                // review totals occupy the spare facts row rather than
                // replacing LOOT's warning/error/plugin health summary.
                egui::Grid::new("summary").num_columns(6).spacing([16.0, 4.0]).show(ui, |ui| {
                    ui.label("masterlist revision ID");
                    ui.label(masterlist_revision);
                    ui.label("warnings");
                    ui.label(loot_count(self.loot_warning_count));
                    ui.label("active full plugins");
                    ui.label(loot_count(self.active_full_plugin_count));
                    ui.end_row();
                    ui.label("masterlist update date");
                    ui.label(masterlist_updated);
                    ui.label("errors");
                    ui.label(loot_count(self.loot_error_count));
                    ui.label("active light plugins");
                    ui.label(loot_count(self.active_light_plugin_count));
                    ui.end_row();
                    ui.label(egui::RichText::new("ModSlut changes").color(MOVE_CLR));
                    ui.label(format!("{}", self.plan.len()));
                    ui.label("total messages");
                    ui.label(loot_count(self.loot_message_count));
                    ui.label("dirty plugins");
                    ui.label(loot_count(self.dirty_plugin_count));
                    ui.end_row();
                    ui.label(egui::RichText::new("section / separator moves").color(MOVE_CLR));
                    ui.horizontal(|ui| {
                        ui.label(format!("{moves}").to_string());
                        ui.label(egui::RichText::new("/").color(DIM));
                        ui.label(egui::RichText::new(format!("{separator_moves}")).color(RENAME_CLR));
                    });
                    ui.label("total plugins");
                    ui.label(active_plugins);
                    ui.label("total mods");
                    ui.label(format!("{total_mods}"));
                    ui.end_row();
                    ui.label(egui::RichText::new("in-section reorders").color(REOR_CLR));
                    ui.label(format!("{reors}"));
                    ui.label(egui::RichText::new("vr/variant promotions").color(PROM_CLR));
                    ui.label(format!("{proms}"));
                    ui.label(egui::RichText::new("base-replacer sinks").color(SINK_CLR));
                    ui.label(format!("{sinks}"));
                    ui.end_row();
                });

                if let Some(source) = &self.masterlist_source {
                    ui.add_space(5.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.hyperlink_to("Open configured LOOT masterlist source", source)
                            .on_hover_text("Opens the same official source that Update Masterlist uses.");
                        ui.label(egui::RichText::new("· LOOT and libloot data, sorting, and messages by the LOOT team.").color(DIM));
                    });
                }
                ui.horizontal_wrapped(|ui| {
                    ui.hyperlink_to("LOOT project & latest updates", "https://loot.github.io/");
                    ui.label(egui::RichText::new("· ModSlut adds the MO2 left-pane roadmap and pluginless-mod layer.").color(DIM));
                });

                for message in &self.loot_general_messages {
                    ui.add_space(8.0);
                    let (fill, border) = match message.level {
                        crate::libloot_adapter::LootMessageLevel::Info => (egui::Color32::from_rgb(47, 58, 66), REOR_CLR),
                        crate::libloot_adapter::LootMessageLevel::Warning => (egui::Color32::from_rgb(100, 76, 24), MOVE_CLR),
                        crate::libloot_adapter::LootMessageLevel::Error => (egui::Color32::from_rgb(105, 47, 45), egui::Color32::from_rgb(255, 132, 125)),
                    };
                    egui::Frame::new()
                        .fill(fill)
                        .stroke(egui::Stroke::new(1.0_f32, border))
                        .inner_margin(egui::Margin::symmetric(10, 7))
                        .show(ui, |ui| show_loot_message(ui, &message.text));
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("Search cards / plugins:");
                    ui.add_sized(
                        egui::vec2((ui.available_width() - 160.0).max(140.0), 0.0),
                        egui::TextEdit::singleline(&mut self.right_search)
                            .hint_text("plugin, MO2 owner, or mod name"),
                    );
                });
                ui.separator();

                // ModSlut already has an MO2 left-pane navigator. Unlike
                // standalone LOOT it does not need to duplicate a second
                // narrow plugin list here: use the entire right pane for
                // LOOT-style cards, including pluginless folders.
                self.installed_mod_cards(ui);

                // Each installed-mod card now carries its own detail and
                // context menu, as LOOT does. Keep no duplicate selection
                // preview underneath the card stream.
                if self.show_detail_preview {
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);

                if let Some(i) = self.selected {
                    // plan can shrink after a reload (e.g. a rule vetoed the
                    // selected change) - a stale index must not crash the ui
                    let Some(c) = self.plan.get(i) else {
                        self.selected = None;
                        return;
                    };
                    ui.label(
                        egui::RichText::new(&c.name)
                            .strong()
                            .size(self.font_size * 1.1)
                            .color(kind_color(c.kind)),
                    );
                    ui.label(egui::RichText::new(kind_tag(c.kind)).monospace().color(DIM));
                    ui.add_space(4.0);
                    ui.label(&c.detail);
                    show_source_and_bash_tags(
                        ui,
                        self.mod_sources.get(&c.name),
                        self.mod_bash_tags.get(&c.name),
                    );
                    ui.add_space(8.0);
                    let blurb = match c.kind {
                        ChangeKind::Move => "this mod's name matched a rule for a different section.",
                        ChangeKind::Separator => {
                            "this managed Creation Club shelf is pinned directly below the profile context rows."
                        }
                        ChangeKind::Reorder => {
                            "sorted so patches sit below their parents in mo2 - patches win."
                        }
                        ChangeKind::Promote => {
                            "vr/variant wins conflicts, so it moves above its se sibling."
                        }
                        ChangeKind::Sink => {
                            "base replacer pinned to the top of its section - everything below overwrites it."
                        }
                        ChangeKind::Float => {
                            "pinned to the bottom of its section - it wins every conflict there."
                        }
                        ChangeKind::Warn => {
                            if c.detail.starts_with("[master order]") {
                                "this mod's plugin loads before its master. if a pin or rule of yours put it here, that's your call - otherwise it's a sorter bug, report it."
                            } else {
                                "platform mismatch - this skse plugin wasn't built for skyrim vr."
                            }
                        }
                        ChangeKind::Rename => {
                            "separator renamed so rules can find it - your name stays, a concept tag gets appended."
                        }
                    };
                    ui.label(egui::RichText::new(blurb).color(DIM).italics());
                    if let Some(content) = self
                        .content_index
                        .as_ref()
                        .and_then(|index| index.get(&c.name))
                    {
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("content evidence").strong());
                        let facts = if content.facts.is_empty() {
                            "plugin has no high-confidence structural signature yet".to_string()
                        } else {
                            content
                                .facts
                                .iter()
                                .map(|fact| format!("{} ({}%)", fact.concept, fact.confidence))
                                .collect::<Vec<_>>()
                                .join(" · ")
                        };
                        ui.label(egui::RichText::new(facts).color(DIM));
                    }
                    if c.kind == ChangeKind::Warn && !c.detail.starts_with("[master order]") {
                        ui.add_space(8.0);
                        if ui
                            .button("disable this mod (untick in mo2)")
                            .on_hover_text("flips + to - in modlist.txt right now, with a .bak backup")
                            .clicked()
                        {
                            let name = c.name.clone();
                            self.disable_mod(&name);
                        }
                        ui.label(
                            egui::RichText::new(
                                "if there's a vr build of this mod, install that instead and this warning goes away",
                            )
                            .color(DIM)
                            .italics()
                            .small(),
                        );
                    }
                } else if let Some((name, sec)) = self.selected_mod.clone() {
                    // A card gives ordinary MO2 folders the same immediate
                    // visual affordance as LOOT plugin cards. Bash tags
                    // appear here once libloot has supplied them during Sort.
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgb(42, 42, 42))
                        .stroke(egui::Stroke::new(1.0_f32, egui::Color32::from_gray(92)))
                        .inner_margin(egui::Margin::symmetric(12, 10))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(&name)
                                    .strong()
                                    .size(self.font_size * 1.15)
                                    .color(egui::Color32::WHITE),
                            );
                            let position = self.mod_priorities.get(&name).copied().unwrap_or(0);
                            let state = self.mod_states.get(&name).copied().unwrap_or(' ');
                            ui.label(egui::RichText::new(format!("Position {position}  ·  {state}  ·  [{sec}]")).color(DIM));
                            show_source_and_bash_tags(
                                ui,
                                self.mod_sources.get(&name),
                                self.mod_bash_tags.get(&name),
                            );
                            ui.add_space(5.0);
                            ui.label("No pending change — ModSlut is preserving this mod's current MO2 home.");
                        });
                    if let Some(content) = self
                        .content_index
                        .as_ref()
                        .and_then(|index| index.get(&name))
                    {
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("content evidence").strong());
                        let facts = if content.facts.is_empty() {
                            "plugin has no high-confidence structural signature yet".to_string()
                        } else {
                            content
                                .facts
                                .iter()
                                .map(|fact| format!("{} ({}%)", fact.concept, fact.confidence))
                                .collect::<Vec<_>>()
                                .join(" · ")
                        };
                        ui.label(egui::RichText::new(facts).color(DIM));
                    }
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(
                            "right-click it in the list to pin, float, sink, or send it to another section.",
                        )
                        .color(DIM)
                        .italics(),
                    );
                } else {
                    let warnings: Vec<&Change> = self.plan.iter().filter(|change| change.kind == ChangeKind::Warn).collect();
                    if warnings.is_empty() {
                        ui.label(egui::RichText::new("click a mod on the left for its source and details.").color(DIM));
                    } else {
                        ui.label(egui::RichText::new(format!("{} warning(s) need attention", warnings.len())).strong().color(WARN_CLR));
                        ui.add_space(6.0);
                        for warning in warnings {
                            egui::Frame::NONE
                                .fill(egui::Color32::from_rgb(0x63, 0x38, 0x32))
                                .inner_margin(egui::Margin::symmetric(10, 8))
                                .show(ui, |ui| {
                                    ui.label(egui::RichText::new(&warning.name).strong().color(egui::Color32::WHITE));
                                    ui.label(&warning.detail);
                                });
                            ui.add_space(5.0);
                        }
                    }
                }
                }
                    });
            });

        self.draw_sort_loader(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_selected_profile_both_formats() {
        let dir = std::env::temp_dir();
        let p1 = dir.join("mo2test1.ini");
        let p2 = dir.join("mo2test2.ini");
        let mut f = std::fs::File::create(&p1).unwrap();
        writeln!(f, "[General]\nselected_profile=@ByteArray(My VR Profile)").unwrap();
        let mut f = std::fs::File::create(&p2).unwrap();
        writeln!(f, "[General]\nselected_profile = Plain Name").unwrap();
        assert_eq!(read_selected_profile(&p1).as_deref(), Some("My VR Profile"));
        assert_eq!(read_selected_profile(&p2).as_deref(), Some("Plain Name"));
    }

    #[test]
    fn plugin_preview_reorders_active_rows_and_keeps_inactive_rows() {
        let source = "# This file was automatically generated by Mod Organizer.\n*Alpha.esm\n-Beta.esp\n*Gamma.esp\n";
        let (preview, changed) =
            plugin_order_preview(source, &["gamma.esp".into(), "alpha.esm".into()]).unwrap();
        assert_eq!(changed, 2);
        assert_eq!(
            preview,
            "# This file was automatically generated by Mod Organizer.\n*Gamma.esp\n-Beta.esp\n*Alpha.esm\n"
        );
    }
}

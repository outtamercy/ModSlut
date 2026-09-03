// modslut gui - LOOT-flavored dark ui, built to be launched from inside MO2.
// finds the active profile's modlist.txt on its own, previews changes,
// and only writes when "apply changes" is clicked. mo2 refreshes on exit.

use crate::conflicts::ConflictIndex;
use std::fmt::Write as _;
use crate::{active_mods, category_report, load_rules_for, parse, run, serialize, user_rule_files, Categories, Change, ChangeKind, Modlist};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};

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

fn kind_color(k: ChangeKind) -> egui::Color32 {
    match k {
        ChangeKind::Move => MOVE_CLR,
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
        ChangeKind::Reorder => "REOR",
        ChangeKind::Promote => "PROM",
        ChangeKind::Sink => "SINK",
        ChangeKind::Float => "FLOT",
        ChangeKind::Warn => "WARN",
        ChangeKind::Rename => "REN",
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

// ---- per-user settings ----
// font size lives in the os config dir (%APPDATA%/modslut on windows,
// ~/.config/modslut elsewhere) so every user keeps their own preference,
// and nothing gets written into the mo2 install itself.
fn settings_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
        })
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("modslut").join("settings.txt"))
}

fn load_font_size() -> f32 {
    settings_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| t.trim().parse::<f32>().ok())
        .filter(|&s| (10.0..=28.0).contains(&s))
        .unwrap_or(14.0)
}

fn save_font_size(size: f32) {
    if let Some(p) = settings_path() {
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        std::fs::write(p, format!("{size:.0}")).ok();
    }
}

// ---- layout prefs: left panel width + window geometry, saved between runs ----
#[derive(Clone, Copy, Default)]
struct UiPrefs {
    panel: Option<f32>,
    size: Option<(f32, f32)>,
    pos: Option<(f32, f32)>,
    user_rules_only: bool,
}

fn ui_prefs_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
        })
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("modslut").join("ui.txt"))
}

fn load_ui_prefs() -> UiPrefs {
    let mut out = UiPrefs::default();
    let Some(text) = ui_prefs_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return out;
    };
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        match k.trim() {
            "panel" => out.panel = v.trim().parse::<f32>().ok().filter(|w| *w >= 200.0),
            "size" => {
                if let Some((w, h)) = v.trim().split_once('x') {
                    if let (Ok(w), Ok(h)) = (w.parse::<f32>(), h.parse::<f32>()) {
                        if w >= 400.0 && h >= 300.0 {
                            out.size = Some((w, h));
                        }
                    }
                }
            }
            "pos" => {
                if let Some((x, y)) = v.trim().split_once(',') {
                    if let (Ok(x), Ok(y)) = (x.parse::<f32>(), y.parse::<f32>()) {
                        out.pos = Some((x, y));
                    }
                }
            }
            "user_rules_only" => out.user_rules_only = v.trim() == "1",
            _ => {}
        }
    }
    out
}

fn save_ui_prefs(prefs: &UiPrefs) {
    if let Some(p) = ui_prefs_path() {
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let mut text = String::new();
        if let Some(w) = prefs.panel {
            text.push_str(&format!("panel={w:.0}\n"));
        }
        if let Some((w, h)) = prefs.size {
            text.push_str(&format!("size={w:.0}x{h:.0}\n"));
        }
        if let Some((x, y)) = prefs.pos {
            text.push_str(&format!("pos={x:.0},{y:.0}\n"));
        }
        if prefs.user_rules_only {
            text.push_str("user_rules_only=1\n");
        }
        std::fs::write(p, text).ok();
    }
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
#   >Winner Name = Loser Name           winner sorts above loser (wins conflicts)
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
    sorted: Option<String>, // serialized output, ready to write
    selected: Option<usize>,
    written: bool,
    status: String,
    font_size: f32,
    cats_report: Option<String>,
    show_cats: bool,
    trace: String,
    conflicts: Option<Arc<ConflictIndex>>,
    scan_rx: Option<mpsc::Receiver<ConflictIndex>>,
    sections: Vec<String>,
    // right-click "send to section…" picker: (mod name, current section)
    picker_for: Option<(String, String)>,
    picker_filter: String,
    promote_target: String,
    // ui layout prefs (persisted): left panel width + last window rect
    changes_width: f32,
    win_rect: Option<egui::Rect>,
    // full modlist snapshot (current state, pre-run) for the loot-style
    // whole-list view: section -> mod names in current order
    layout: Vec<(String, Vec<String>)>,
    parking_mods: Vec<String>,
    // a plain mod row clicked (no pending change): (name, section)
    selected_mod: Option<(String, String)>,
    // where the auto-written debug_sort.log landed (or why it didn't)
    debug_note: Option<String>,
    // "user rules only" mode (persisted): built-in cascade + guess tiers off
    user_rules_only: bool,
}

impl ModslutApp {
    fn new(prefs: UiPrefs) -> Self {
        let mut app = Self {
            file: None,
            plan: Vec::new(),
            sorted: None,
            selected: None,
            written: false,
            status: String::new(),
            font_size: load_font_size(),
            cats_report: None,
            show_cats: false,
            trace: String::new(),
            conflicts: None,
            scan_rx: None,
            sections: Vec::new(),
            picker_for: None,
            picker_filter: String::new(),
            promote_target: String::new(),
            changes_width: prefs.panel.unwrap_or(430.0),
            win_rect: None,
            layout: Vec::new(),
            parking_mods: Vec::new(),
            selected_mod: None,
            debug_note: None,
            user_rules_only: prefs.user_rules_only,
        };
        match find_modlist() {
            Some(p) => app.load_and_preview(p),
            None => {
                app.status =
                    "couldn't find an mo2 profile from here - use 'open modlist.txt…'".into()
            }
        }
        app
    }

    fn load_and_preview(&mut self, path: PathBuf) {
        self.plan.clear();
        self.sorted = None;
        self.selected = None;
        self.selected_mod = None;
        self.debug_note = None;
        self.written = false;

        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("couldn't read {}: {e}", path.display());
                return;
            }
        };
        let mut ml: Modlist = parse(&text);
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
                ConflictIndex::ini_path(&path).and_then(|ini| ConflictIndex::load_checked(&ini, &mods_dir))
            } else {
                None
            };
            if let Some(ci) = cached {
                self.conflicts = Some(Arc::new(ci));
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

        self.cats_report = cats.as_ref().map(|c| category_report(&ml, c, &rules));
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

        // plugin census BEFORE the sort: run() uses it for the
        // plugin-master family pass (a mod may never land in a section
        // that loads before the mod providing its plugin's masters)
        let census_data: Option<(
            Vec<crate::plugins::MasterViolation>,
            Vec<(String, String, crate::plugins::PluginInfo)>,
        )> = ConflictIndex::mods_dir_of(&path).and_then(|mods_dir| {
            let load_order: Vec<String> = std::fs::read_to_string(path.with_file_name("plugins.txt"))
                .map(|t| {
                    t.lines()
                        .filter_map(|l| {
                            let l = l.trim();
                            l.strip_prefix('*').map(|s| s.trim().to_lowercase())
                        })
                        .collect()
                })
                .unwrap_or_default();
            if load_order.is_empty() {
                None
            } else {
                // manifest: same enabled-mod set as last scan = load the
                // cache; any +/- of mods (or a toggle) forces a rescan
                let fp = crate::plugins::census_fingerprint(&enabled);
                let cache = crate::plugins::census_cache_path(&path);
                let cached = crate::plugins::load_census(&cache, fp).map(|entries| {
                    // sections are re-attached from the CURRENT modlist -
                    // a sort never makes the cache stale
                    let sec_of: std::collections::HashMap<&str, &str> = enabled
                        .iter()
                        .map(|(n, s)| (n.as_str(), s.as_str()))
                        .collect();
                    entries
                        .into_iter()
                        .filter_map(|(m, info)| {
                            sec_of
                                .get(m.as_str())
                                .map(|s| (m.clone(), s.to_string(), info))
                        })
                        .collect::<Vec<_>>()
                });
                match cached {
                    Some(census) => {
                        let violations = crate::plugins::violations_from_census(&census, &load_order);
                        Some((violations, census))
                    }
                    None => {
                        let (violations, census) =
                            crate::plugins::master_violations(&enabled, &mods_dir, &load_order);
                        crate::plugins::save_census(&cache, fp, &census);
                        Some((violations, census))
                    }
                }
            }
        });

        let mut plan = Vec::new();
        let mut trace = String::new();
        let (kw, _kw_files) = crate::Keywords::load(self.file.as_deref());
        run(
            &mut ml,
            &rules,
            &mut log,
            &mut plan,
            cats.as_ref(),
            self.conflicts.as_deref(),
            census_data.as_ref().map(|(_, c)| c.as_slice()),
            &kw,
            false,
            &mut trace,
        );
        self.trace = trace;
        self.sections = ml.sections.iter().map(|s| s.label.clone()).collect();

        // platform guard: ae/oldrim skse plugins living in a vr profile.
        // display-only WARN rows - they never touch the serialized output
        let mut n_warns = 0usize;
        if let Some(mods_dir) = ConflictIndex::mods_dir_of(&path) {
            for w in crate::platform_scan(&enabled, &mods_dir) {
                n_warns += 1;
                let _ = writeln!(self.trace, "WARN  {} [{}] {}", w.mod_name, w.kind.label(), w.dll);
                plan.push(Change {
                    kind: ChangeKind::Warn,
                    name: w.mod_name,
                    detail: format!("[{}] {}", w.kind.label(), w.dll),
                    section: w.section,
                });
            }

            // census was parsed before run() - here it becomes trace +
            // WARN rows for master-order violations in plugins.txt
            if let Some((violations, census)) = &census_data {
                let _ = writeln!(
                    self.trace,
                    "\n--- plugin census ({} plugin(s) parsed) ---",
                    census.len()
                );
                for (mod_name, _sec, info) in census {
                    let _ = writeln!(
                        self.trace,
                        "  {} ({}){}{} | masters: {} | {}",
                        info.plugin,
                        mod_name,
                        if info.is_esm { " esm" } else { "" },
                        if info.is_esl { " esl" } else { "" },
                        if info.masters.is_empty() {
                            "-".to_string()
                        } else {
                            info.masters.join(", ")
                        },
                        info.census_line()
                    );
                }
                for v in violations {
                    n_warns += 1;
                    let _ = writeln!(
                        self.trace,
                        "WARN  {} [{} loads before its master {}]",
                        v.mod_name, v.plugin, v.master
                    );
                    plan.push(Change {
                        kind: ChangeKind::Warn,
                        name: v.mod_name.clone(),
                        detail: format!(
                            "[master order] {} loads before its master {}",
                            v.plugin, v.master
                        ),
                        section: v.section.clone(),
                    });
                }
            }
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

        self.sorted = Some(serialize(&ml));
        self.file = Some(path);
        self.plan = plan;
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
        self.status = match self.plan.len() {
            0 => format!("list's already clean{cat_note}"),
            n => format!("{n} change(s) ready{cat_note} - review, then apply if it looks good"),
        };
    }

    // writes straight back to the profile's modlist.txt, with a .bak backup.
    // mo2 picks the change up as soon as this process closes.
    fn apply(&mut self) {
        let (Some(file), Some(sorted)) = (&self.file, &self.sorted) else {
            return;
        };
        let bak = file.with_file_name(format!(
            "{}.bak",
            file.file_name().unwrap_or_default().to_string_lossy()
        ));
        if let Err(e) = std::fs::copy(file, &bak) {
            self.status = format!("backup failed ({e}) - not touching anything");
            return;
        }
        match std::fs::write(file, sorted) {
            Ok(()) => {
                self.written = true;
                self.status = format!(
                    "applied. backup at {} - close this window and mo2 will refresh",
                    bak.display()
                );
            }
            Err(e) => self.status = format!("couldn't write {}: {e}", file.display()),
        }
    }

    // "apply user rules only": write back a modlist where ONLY the user's
    // own modslut_rules.txt rules have been applied - no built-ins, no
    // guesses, no loot/family/census/conflict passes. for saving your pins
    // without mass-accepting the whole computed plan.
    fn apply_user_rules_only(&mut self) {
        let Some(file) = self.file.clone() else {
            return;
        };
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("couldn't read {}: {e}", file.display());
                return;
            }
        };
        let (rules, user_files) = load_rules_for(None, Some(&file), true);
        if user_files.is_empty() {
            self.status = "no modslut_rules.txt found - nothing of yours to apply".into();
            return;
        }
        let mut ml: Modlist = parse(&text);
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
            true,
            &mut trace,
        );
        let _ = std::fs::write(crate::debug_log_path(), &trace);
        if n == 0 {
            self.status = "your user rules change nothing - the list already matches them".into();
            return;
        }
        let bak = file.with_file_name(format!(
            "{}.bak",
            file.file_name().unwrap_or_default().to_string_lossy()
        ));
        if let Err(e) = std::fs::copy(&file, &bak) {
            self.status = format!("backup failed ({e}) - not touching anything");
            return;
        }
        match std::fs::write(&file, serialize(&ml)) {
            Ok(()) => {
                self.status = format!(
                    "applied {n} user-rule change(s) only. backup at {} - close this window and mo2 will refresh",
                    bak.display()
                );
                // refresh the preview against the new on-disk state
                self.load_and_preview(file);
            }
            Err(e) => self.status = format!("couldn't write {}: {e}", file.display()),
        }
    }

    // append one rule line to the user rules file (creating it from the
    // template if needed), then reload so the preview reflects it
    // loot-style right-click menu, shared by changed and unchanged rows:
    // every choice appends one line to modslut_rules.txt
    fn row_menu(&mut self, resp: &egui::Response, name: &str, section: &str) {
        let mut rule: Option<String> = None;
        resp.context_menu(|ui| {
            ui.label(egui::RichText::new(name).strong().small());
            ui.separator();
            if ui
                .button(format!("never move (pin to [{section}])"))
                .clicked()
            {
                rule = Some(format!("!{name} = {section}"));
                ui.close_menu();
            }
            if ui
                .button(format!("float: bottom of [{section}] (wins in-section)"))
                .clicked()
            {
                rule = Some(format!("^{name} = {section}"));
                ui.close_menu();
            }
            if ui
                .button(format!("sink: top of [{section}] (loses in-section)"))
                .clicked()
            {
                rule = Some(format!("<{name} = {section}"));
                ui.close_menu();
            }
            if ui.button("edit rules…").clicked() {
                self.picker_for = Some((name.to_string(), section.to_string()));
                self.picker_filter.clear();
                ui.close_menu();
            }
        });
        if let Some(r) = rule {
            self.append_user_rule(r);
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
        let selected = self.selected == Some(i);
        let frame = egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(6, 5))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
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
    }

    // a mod with no pending change: dim row, still fully clickable and
    // right-clickable so rules can be written against it
    fn plain_row(&mut self, ui: &mut egui::Ui, name: &str, section: &str) {
        let selected = self
            .selected_mod
            .as_ref()
            .is_some_and(|(n, _)| n == name);
        let frame = egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(6, 3))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(name).color(DIM));
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
    }

    // untick a mod in modlist.txt immediately (+ -> -), with a .bak backup.
    // mo2 picks it up when this process exits, same as apply.
    fn disable_mod(&mut self, name: &str) {
        let Some(file) = self.file.clone() else { return };
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

    // user rules live in modslut_rules.txt next to the profile's modlist.txt.
    // create a commented template on first use, then open it in the os editor.
    fn open_user_rules(&mut self) {
        let Some(file) = &self.file else { return };
        let Some(path) = user_rule_files(Some(file)).into_iter().next() else {
            self.status = "couldn't figure out where to put user rules".into();
            return;
        };
        if !path.exists() {
            if let Err(e) = std::fs::write(&path, USER_RULES_TEMPLATE) {
                self.status = format!("couldn't create {}: {e}", path.display());
                return;
            }
        }
        // windows: hand it to notepad; elsewhere: xdg-open
        let opened = if cfg!(target_os = "windows") {
            std::process::Command::new("notepad.exe").arg(&path).spawn().is_ok()
        } else {
            std::process::Command::new("xdg-open").arg(&path).spawn().is_ok()
        };
        self.status = if opened {
            format!("editing {} - save, then hit reload", path.display())
        } else {
            format!("couldn't open an editor - file is at {}", path.display())
        };
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
            user_rules_only: self.user_rules_only,
        };
        save_ui_prefs(&prefs);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // track the window rect every frame so on_exit can persist it
        ctx.input(|i| {
            if let Some(r) = i.viewport().outer_rect {
                self.win_rect = Some(r);
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
                    self.load_and_preview(p);
                }
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
        }

        let fs = self.font_size;
        ctx.style_mut(|s| {
            s.visuals = egui::Visuals::dark();
            s.visuals.window_fill = BG;
            s.visuals.panel_fill = BG;
            s.visuals.extreme_bg_color = TOOLBAR;
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
            .frame(egui::Frame::NONE.fill(TOOLBAR).inner_margin(egui::Margin::symmetric(12, 10)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("ModSlut")
                            .strong()
                            .size(self.font_size * 1.3)
                            .color(egui::Color32::WHITE),
                    );
                    ui.separator();

                    // per-user font scaling
                    if ui.button("A−").on_hover_text("smaller text").clicked() {
                        self.font_size = (self.font_size - 1.0).max(10.0);
                        save_font_size(self.font_size);
                    }
                    if ui.button("A+").on_hover_text("bigger text").clicked() {
                        self.font_size = (self.font_size + 1.0).min(28.0);
                        save_font_size(self.font_size);
                    }
                    ui.separator();

                    let btn = |label: &str| {
                        egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE))
                            .fill(BTN)
                    };
                    if ui.add(btn("open modlist.txt…")).clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_title("open modlist.txt")
                            .add_filter("modlist", &["txt"])
                            .pick_file()
                        {
                            self.load_and_preview(path);
                        }
                    }
                    if ui.add(btn("reload")).clicked() && self.file.is_some() {
                        let p = self.file.clone().unwrap();
                        self.load_and_preview(p);
                    }
                    if self.cats_report.is_some()
                        && ui
                            .add(btn(if self.show_cats { "changes" } else { "categories" }))
                            .clicked()
                    {
                        self.show_cats = !self.show_cats;
                    }
                    if self.file.is_some()
                        && ui
                            .add(btn("user rules"))
                            .on_hover_text(
                                "open modslut_rules.txt - your own rules, checked before the built-ins",
                            )
                            .clicked()
                    {
                        self.open_user_rules();
                    }
                    // "apply user rules only": built-in cascade AND guess
                    // tiers off - only your pins + proven data move mods
                    let mut uro = self.user_rules_only;
                    if ui
                        .checkbox(&mut uro, "user rules only")
                        .on_hover_text(
                            "built-in rules and category/keyword guesses stay out.\n\
                             only your modslut_rules.txt pins plus proven data\n\
                             (loot masterlist, conflict index, plugin masters,\n\
                             patch-follows-master) are allowed to move mods.",
                        )
                        .changed()
                    {
                        self.user_rules_only = uro;
                        if let Some(p) = self.file.clone() {
                            self.load_and_preview(p);
                        }
                    }
                    if self.debug_note.is_some()
                        && self.file.is_some()
                        && ui
                            .add(btn("open log"))
                            .on_hover_text("debug_sort.log - written automatically on every run, next to ModSlut.exe")
                            .clicked()
                    {
                        let p = crate::debug_log_path();
                        let opened = if cfg!(target_os = "windows") {
                            std::process::Command::new("notepad.exe").arg(&p).spawn().is_ok()
                        } else {
                            std::process::Command::new("xdg-open").arg(&p).spawn().is_ok()
                        };
                        if !opened {
                            self.status = format!("log is at {}", p.display());
                        }
                    }

                    let ready = !self.plan.is_empty() && self.sorted.is_some() && !self.written;
                    if ui
                        .add_enabled(
                            ready,
                            egui::Button::new(
                                egui::RichText::new("apply changes")
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(APPLY),
                        )
                        .clicked()
                    {
                        self.apply();
                    }
                    // mo2 only re-reads the profile when this process exits,
                    // so the one-click path is apply + close
                    if ui
                        .add_enabled(
                            ready,
                            egui::Button::new(
                                egui::RichText::new("apply & quit")
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(APPLY),
                        )
                        .clicked()
                    {
                        self.apply();
                        if self.written {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                    // save ONLY what your own rules say - the rest of the
                    // computed plan stays unapplied
                    if ui
                        .add_enabled(
                            self.file.is_some() && !self.written,
                            egui::Button::new(
                                egui::RichText::new("apply user rules only")
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(BTN),
                        )
                        .on_hover_text(
                            "writes modlist.txt with ONLY your modslut_rules.txt\n\
                             rules applied - no built-in rules, no guesses, no\n\
                             loot/conflict/family moves. .bak backup is made first.",
                        )
                        .clicked()
                    {
                        self.apply_user_rules_only();
                    }
                    if self.written && ui.add(btn("exit")).clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
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

        // ---- rule editor (right-click "edit rules…") - loot's metadata
        // window, modslut flavor: everything you click writes one line to
        // modslut_rules.txt and refreshes the preview
        if let Some((mod_name, cur_sec)) = self.picker_for.clone() {
            let mut open = true;
            let mut rule: Option<String> = None;
            egui::Window::new(format!("rules for \"{mod_name}\""))
                .collapsible(false)
                .resizable(true)
                .default_width(400.0)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(
                        egui::RichText::new(format!("currently in: {cur_sec}"))
                            .color(DIM)
                            .italics(),
                    );
                    ui.add_space(4.0);

                    ui.label(egui::RichText::new("quick rules").strong());
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("never move").on_hover_text(
                            "pin it to its current section - modslut never suggests moving it again",
                        ).clicked() {
                            rule = Some(format!("!{mod_name} = {cur_sec}"));
                        }
                        if ui.button("float (wins in-section)").on_hover_text(
                            "bottom of the section in mo2 - wins every in-section conflict",
                        ).clicked() {
                            rule = Some(format!("^{mod_name} = {cur_sec}"));
                        }
                        if ui.button("sink (loses in-section)").on_hover_text(
                            "top of the section in mo2 - loses every in-section conflict",
                        ).clicked() {
                            rule = Some(format!("<{mod_name} = {cur_sec}"));
                        }
                    });
                    ui.add_space(6.0);

                    ui.label(egui::RichText::new("beats another mod (promote)").strong());
                    ui.horizontal(|ui| {
                        ui.label("this mod wins over:");
                        ui.text_edit_singleline(&mut self.promote_target);
                        if ui.button("add").clicked()
                            && !self.promote_target.trim().is_empty()
                        {
                            rule = Some(format!(
                                ">{mod_name} = {}",
                                self.promote_target.trim()
                            ));
                            self.promote_target.clear();
                        }
                    });
                    ui.label(
                        egui::RichText::new("exact mod name, as shown in the changes list")
                            .color(DIM)
                            .italics()
                            .small(),
                    );
                    ui.add_space(6.0);

                    ui.label(egui::RichText::new("send to section").strong());
                    ui.horizontal(|ui| {
                        ui.label("filter:");
                        ui.text_edit_singleline(&mut self.picker_filter);
                    });
                    ui.separator();
                    let needle = self.picker_filter.to_lowercase();
                    egui::ScrollArea::vertical()
                        .max_height(380.0)
                        .show(ui, |ui| {
                            for sec in &self.sections {
                                if !needle.is_empty() && !sec.to_lowercase().contains(&needle) {
                                    continue;
                                }
                                if ui.button(sec).clicked() {
                                    rule = Some(format!("!{mod_name} = {sec}"));
                                }
                            }
                        });
                });
            if let Some(r) = rule {
                self.append_user_rule(r);
                self.picker_for = None;
            } else if !open {
                self.picker_for = None;
            }
        }

        // ---- bottom status bar ----
        egui::TopBottomPanel::bottom("status")
            .frame(egui::Frame::NONE.fill(TOOLBAR).inner_margin(egui::Margin::symmetric(14, 8)))
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

                ui.painter().rect_filled(left_rect, 0.0, PANEL);
                ui.painter().rect_filled(right_rect, 0.0, BG);

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
                let change_map: std::collections::HashMap<String, Vec<usize>> = {
                    let mut m: std::collections::HashMap<String, Vec<usize>> =
                        std::collections::HashMap::new();
                    for (i, c) in self.plan.iter().enumerate() {
                        m.entry(c.name.clone()).or_default().push(i);
                    }
                    m
                };
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        // separator renames aren't mods, so they can't hang
                        // under a section's mod list - they get their own
                        // group at the very top
                        let rename_idxs: Vec<usize> = self
                            .plan
                            .iter()
                            .enumerate()
                            .filter(|(_, c)| c.kind == ChangeKind::Rename)
                            .map(|(i, _)| i)
                            .collect();
                        if !rename_idxs.is_empty() {
                            egui::CollapsingHeader::new(
                                egui::RichText::new(format!(
                                    "separator renames  ·  {}",
                                    rename_idxs.len()
                                ))
                                .strong()
                                .color(RENAME_CLR),
                            )
                            .id_salt("sec::renames")
                            .default_open(true)
                            .show(ui, |ui| {
                                for i in rename_idxs {
                                    let sec = self.plan.get(i).map(|c| c.detail.clone()).unwrap_or_default();
                                    self.change_row(ui, i, &sec);
                                }
                            });
                        }
                        let mut groups: Vec<(String, Vec<String>)> = self.layout.clone();
                        if !self.parking_mods.is_empty() {
                            groups.push(("parking lot".to_string(), self.parking_mods.clone()));
                        }
                        for (sec, mods) in groups {
                            let n_changes: usize = mods
                                .iter()
                                .map(|m| change_map.get(m.as_str()).map_or(0, |v| v.len()))
                                .sum();
                            let head = if n_changes > 0 {
                                egui::RichText::new(format!(
                                    "{sec}  ·  {} mods · {n_changes} change(s)",
                                    mods.len()
                                ))
                                .strong()
                                .color(egui::Color32::WHITE)
                            } else {
                                egui::RichText::new(format!("{sec}  ·  {} mods", mods.len()))
                                    .color(DIM)
                            };
                            egui::CollapsingHeader::new(head)
                                .id_salt(format!("sec::{sec}"))
                                .default_open(n_changes > 0)
                                .show(ui, |ui| {
                                    for m in &mods {
                                        match change_map.get(m.as_str()) {
                                            Some(idxs) => {
                                                for i in idxs.clone() {
                                                    self.change_row(ui, i, &sec);
                                                }
                                            }
                                            None => self.plain_row(ui, m, &sec),
                                        }
                                    }
                                });
                        }
                    });
                })(&mut left_ui);

                // right: summary + detail
                let mut right_ui = ui.new_child(
                    egui::UiBuilder::new().max_rect(right_rect.shrink2(egui::vec2(16.0, 12.0))),
                );
                right_ui.set_clip_rect(right_rect);
                (|ui: &mut egui::Ui| {
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
                                "@ = pinned by a rule in rules.txt · ~ = fuzzy guess · 'no matching separator' needs an @ rule",
                            )
                            .color(DIM)
                            .italics(),
                        );
                        ui.add_space(6.0);
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut rep.as_str())
                                        .font(egui::TextStyle::Monospace)
                                        .desired_width(f32::INFINITY)
                                        .interactive(true),
                                );
                            });
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

                egui::Grid::new("summary").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                    ui.label(egui::RichText::new("section moves").color(MOVE_CLR));
                    ui.label(format!("{moves}"));
                    ui.end_row();
                    ui.label(egui::RichText::new("in-section reorders").color(REOR_CLR));
                    ui.label(format!("{reors}"));
                    ui.end_row();
                    ui.label(egui::RichText::new("vr/variant promotions").color(PROM_CLR));
                    ui.label(format!("{proms}"));
                    ui.end_row();
                    ui.label(egui::RichText::new("base-replacer sinks").color(SINK_CLR));
                    ui.label(format!("{sinks}"));
                    ui.end_row();
                    let renames = self
                        .plan
                        .iter()
                        .filter(|c| c.kind == ChangeKind::Rename)
                        .count();
                    ui.label(egui::RichText::new("separator renames").color(RENAME_CLR));
                    ui.label(format!("{renames}"));
                    ui.end_row();
                    ui.label(egui::RichText::new("total").strong());
                    ui.label(format!("{}", self.plan.len()));
                    ui.end_row();
                });

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
                    ui.add_space(8.0);
                    let blurb = match c.kind {
                        ChangeKind::Move => "this mod's name matched a rule for a different section.",
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
                    ui.label(
                        egui::RichText::new(&name)
                            .strong()
                            .size(self.font_size * 1.1)
                            .color(egui::Color32::WHITE),
                    );
                    ui.label(egui::RichText::new(format!("in [{sec}]")).color(DIM));
                    ui.add_space(4.0);
                    ui.label("no pending change - modslut is leaving this one where it is.");
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(
                            "right-click it in the list to pin, float, sink, or send it to another section.",
                        )
                        .color(DIM)
                        .italics(),
                    );
                } else {
                    ui.label(egui::RichText::new("click a mod on the left for details.").color(DIM));
                }
                })(&mut right_ui);
            });
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
}

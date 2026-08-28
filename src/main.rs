// modslut - MO2 left-pane modlist organizer
// reads modlist.txt, shoves mods into the right separator sections by keyword rules,
// keeps parents above their patches, promotes VR/NG variants over SE siblings.
//
// usage:
//   modslut                                             opens the gui
//   modslut check  <modlist.txt> [-r rules.txt]         dry run, just reports
//   modslut sort   <modlist.txt> [-r rules.txt] [-o out] shows plan, asks, then writes
//   modslut rules  > rules.txt                          dump the built-in default rules

// no console window popping up next to the gui on windows
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

pub(crate) mod conflicts;
pub(crate) mod plugins;
mod gui;

use conflicts::ConflictIndex;
use std::collections::HashMap;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

// a mod entry: the raw line (keeps its +/-/* flag) + lowercase name for matching
#[derive(Clone)]
struct ModEntry {
    raw: String,
    name: String,   // flag-stripped, as-is case
    lower: String,  // lowercase, for keyword matching
    norm: String,   // alnum-only, for parent/child containment checks
}

struct Section {
    sep_line: String,
    label: String, // separator name without flag and _separator suffix
    mods: Vec<ModEntry>,
}

struct Modlist {
    header: String,
    parking: Vec<String>,  // everything before the first separator
    sections: Vec<Section>,
    trailing: Vec<String>, // anything after the last separator (shouldn't happen, but stay lossless)
}

fn strip_flag(line: &str) -> &str {
    line.trim_start_matches(['+', '-', '*'])
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

fn parse(text: &str) -> Modlist {
    let mut lines = text.lines();
    let header = lines.next().unwrap_or("").to_string();
    let mut sections: Vec<Section> = Vec::new();
    let mut pending: Vec<ModEntry> = Vec::new();

    // mo2 format: a separator TERMINATES its block - the mods above it
    // (in the file) are its section. if a file somehow has no separators,
    // everything lands in the parking lot, verbatim.
    for line in lines {
        let t = line.trim_end();
        if t.is_empty() {
            continue;
        }
        if t.contains("_separator") {
            let label = strip_flag(t).trim_end_matches("_separator").trim().to_string();
            sections.push(Section {
                sep_line: t.to_string(),
                label,
                mods: std::mem::take(&mut pending),
            });
        } else {
            let name = strip_flag(t).trim().to_string();
            pending.push(ModEntry {
                raw: t.to_string(),
                lower: name.to_lowercase(),
                norm: norm(&name),
                name,
            });
        }
    }
    let (parking, trailing): (Vec<String>, Vec<String>) = if sections.is_empty() {
        (pending.drain(..).map(|m| m.raw).collect(), Vec::new())
    } else {
        // mods after the final separator - keep them at the end, lossless
        (Vec::new(), pending.drain(..).map(|m| m.raw).collect())
    };
    Modlist { header, parking, sections, trailing }
}

fn serialize(ml: &Modlist) -> String {
    let mut out = String::new();
    out.push_str(&ml.header);
    out.push('\n');
    for p in &ml.parking {
        out.push_str(p);
        out.push('\n');
    }
    for s in &ml.sections {
        for m in &s.mods {
            out.push_str(&m.raw);
            out.push('\n');
        }
        out.push_str(&s.sep_line);
        out.push('\n');
    }
    for t in &ml.trailing {
        out.push_str(t);
        out.push('\n');
    }
    out
}

// ---- rules ----
// rules.txt format:
//   # comment
//   keyword = Section Label          (lowercase substring match, first hit wins)
//   keyword !excl !excl = Section    (exclusions: skip if any of these appear)
//   !exact Mod Name = Section Label  (exact name match, checked before keywords)
//   @Category Name = Section Label   (mo2 category match, from meta.ini/categories.dat)
//   >Winner Name = Loser Name        (promote rule: winner sorts above loser in-file)
//   <Mod Name = Section Label        (sink rule: pin mod to the TOP of that section
//                                     in mo2 - lowest priority in the section, loses
//                                     to everything below it. for base replacers)
//   ^Mod Name = Section Label        (float rule: pin mod to the BOTTOM of that
//                                     section in mo2 - highest priority there,
//                                     wins everything in-section)
//
// precedence: !exact > @category > category name matching a separator name > keyword
//
// USER RULES: "modslut_rules.txt" next to modlist.txt (per-profile) and/or next
// to the exe (global) is loaded ON TOP of the built-ins - same syntax, checked
// first, so user rules can override any built-in decision.

struct Rules {
    exact: Vec<(String, String)>,
    keyword: Vec<(String, Vec<String>, String)>, // (phrase, exclusions, section)
    category: Vec<(String, String)>,
    promote: Vec<(String, String)>,
    sink: Vec<(String, String)>,
    float: Vec<(String, String)>,
}

impl Rules {
    fn empty() -> Rules {
        Rules { exact: vec![], keyword: vec![], category: vec![], promote: vec![], sink: vec![], float: vec![] }
    }
    // user rules get checked first (first hit wins), so they go in front
    fn prepend(&mut self, user: Rules) {
        self.exact.splice(0..0, user.exact);
        self.keyword.splice(0..0, user.keyword);
        self.category.splice(0..0, user.category);
        self.promote.splice(0..0, user.promote);
        self.sink.splice(0..0, user.sink);
        self.float.splice(0..0, user.float);
    }
}

const DEFAULT_RULES: &str = include_str!("../rules.txt");

// bump every build - shown in the gui title and cli so we always know
// which build a bug report came from
pub const VERSION: &str = "0.12.2";

// patch-flavored names: these live and die with their master and must win
// file conflicts against it. runtime patchers (skypatcher) don't care about
// file order, so they're exempt.
fn patch_flavored(lower: &str) -> bool {
    (lower.contains("patch")
        || lower.contains("add-on")
        || lower.contains("addon")
        || lower.contains("fix")
        || lower.contains("compendium")
        || lower.ends_with(" vr")
        || lower.contains(" vr "))
        && !lower.contains("skypatcher")
}

// all active mods in priority order (file order: first = highest = wins)
pub(crate) fn active_mods(ml: &Modlist) -> Vec<String> {
    let mut v: Vec<String> = ml
        .parking
        .iter()
        .map(|l| {
            l.trim_start_matches(['+', '-', '*'])
                .trim_end_matches("_separator")
                .to_string()
        })
        .filter(|n| !n.is_empty() && !n.starts_with('#'))
        .collect();
    for s in &ml.sections {
        v.extend(s.mods.iter().map(|m| m.name.clone()));
    }
    v
}

fn parse_rules_into(text: &str, r: &mut Rules) {
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if let Some(rest) = l.strip_prefix('!') {
            if let Some((a, b)) = rest.split_once('=') {
                r.exact.push((a.trim().to_string(), b.trim().to_string()));
            }
        } else if let Some(rest) = l.strip_prefix('>') {
            if let Some((a, b)) = rest.split_once('=') {
                r.promote.push((a.trim().to_string(), b.trim().to_string()));
            }
        } else if let Some(rest) = l.strip_prefix('<') {
            if let Some((a, b)) = rest.split_once('=') {
                r.sink.push((a.trim().to_string(), b.trim().to_string()));
            }
        } else if let Some(rest) = l.strip_prefix('^') {
            if let Some((a, b)) = rest.split_once('=') {
                r.float.push((a.trim().to_string(), b.trim().to_string()));
            }
        } else if let Some(rest) = l.strip_prefix('@') {
            if let Some((a, b)) = rest.split_once('=') {
                r.category.push((a.trim().to_lowercase(), b.trim().to_string()));
            }
        } else if let Some((a, b)) = l.split_once('=') {
            // exclusions: "enb !patch !fix = ENB" - split on ' !' so the
            // phrase itself can contain spaces
            let mut parts = a.split(" !");
            let phrase = parts.next().unwrap().trim().to_lowercase();
            let excl: Vec<String> = parts.map(|e| e.trim().to_lowercase()).collect();
            r.keyword.push((phrase, excl, b.trim().to_string()));
        }
    }
}

// user rule file locations, most specific first: the profile folder holding
// this modlist, then the folder the exe lives in
pub(crate) fn user_rule_files(modlist: Option<&Path>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = modlist.and_then(|m| m.parent()) {
        v.push(d.join("modslut_rules.txt"));
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(d) = exe.parent() {
            v.push(d.join("modslut_rules.txt"));
        }
    }
    v
}

pub(crate) fn load_rules_for(path: Option<&str>, modlist: Option<&Path>) -> (Rules, Vec<PathBuf>) {
    let text = match path {
        Some(p) => fs::read_to_string(p).unwrap_or_else(|e| {
            eprintln!("couldn't read rules file {p}: {e}");
            std::process::exit(2);
        }),
        None => DEFAULT_RULES.to_string(),
    };
    let mut r = Rules::empty();
    parse_rules_into(&text, &mut r);

    let mut loaded = Vec::new();
    let mut user = Rules::empty();
    for f in user_rule_files(modlist) {
        if f.is_file() {
            match fs::read_to_string(&f) {
                Ok(t) => {
                    parse_rules_into(&t, &mut user);
                    loaded.push(f);
                }
                Err(e) => eprintln!("couldn't read user rules {}: {e}", f.display()),
            }
        }
    }
    if !loaded.is_empty() {
        r.prepend(user);
    }
    (r, loaded)
}

// ---- mo2 categories ----
// <instance>/categories.dat: "ID|Name|NexusIDs(comma)|ParentID" per line.
// <instance>/mods/<mod name>/meta.ini: [General] category=ID,ID,...
// (primary category first). the mod folder name matches the modlist line.
struct Categories {
    id_to_name: HashMap<u32, String>,
    // nexuscatmap.dat: NexusID -> local category ID (MO2 2.5 stores the
    // nexus category on each mod and resolves it through this map)
    nexus_to_local: HashMap<u32, u32>,
    mods_dir: std::path::PathBuf,
}

impl Categories {
    // modlist.txt lives at <instance>/profiles/<profile>/modlist.txt
    fn discover(modlist_path: &Path) -> Option<Categories> {
        let root = modlist_path.parent()?.parent()?.parent()?;
        let cats_file = root.join("categories.dat");
        let mods_dir = root.join("mods");
        if !cats_file.is_file() || !mods_dir.is_dir() {
            return None;
        }
        let text = fs::read_to_string(cats_file).ok()?;
        let mut id_to_name = HashMap::new();
        for line in text.lines() {
            // "ID|Name|NexusIDs|ParentID" - mo2 2.5 may append columns, so
            // accept anything with at least the id+name pair
            let cells: Vec<&str> = line.trim_end().split('|').collect();
            if cells.len() >= 2 {
                if let Ok(id) = cells[0].trim().parse::<u32>() {
                    id_to_name.insert(id, cells[1].trim().to_string());
                }
            }
        }
        if id_to_name.is_empty() {
            return None;
        }
        // nexuscatmap.dat lines are "LocalID|Name|NexusID"
        let mut nexus_to_local = HashMap::new();
        if let Ok(map_text) = fs::read_to_string(root.join("nexuscatmap.dat")) {
            for line in map_text.lines() {
                let cells: Vec<&str> = line.trim_end().split('|').collect();
                if cells.len() >= 3 {
                    if let (Ok(local), Ok(nexus)) = (
                        cells[0].trim().parse::<u32>(),
                        cells[2].trim().parse::<u32>(),
                    ) {
                        nexus_to_local.insert(nexus, local);
                    }
                }
            }
        }
        Some(Categories { id_to_name, nexus_to_local, mods_dir })
    }

    // primary category name for a mod, if mo2 has one assigned
    fn category_of(&self, mod_name: &str) -> Option<String> {
        self.category_detail_of(mod_name).map(|(_, n)| n)
    }

    // raw meta.ini category id + resolved categories.dat name, for tracing.
    // MO2 2.5: explicit assignments live in category=ID,ID,...; otherwise the
    // mod carries nexusCategory=<nexus id> and the shown category is resolved
    // at runtime through nexuscatmap.dat - so we do the same here.
    fn category_detail_of(&self, mod_name: &str) -> Option<(u32, String)> {
        let meta = fs::read_to_string(self.mods_dir.join(mod_name).join("meta.ini")).ok()?;
        let mut nexus_cat: Option<u32> = None;
        for line in meta.lines() {
            let l = line.trim();
            if let Some(v) = l.strip_prefix("category") {
                let v = v.trim_start_matches(['=', ' ']).trim();
                let first = v.split(',').next()?.trim();
                if let Ok(id) = first.parse::<u32>() {
                    if id != 0 {
                        let name = self
                            .id_to_name
                            .get(&id)
                            .cloned()
                            .unwrap_or_else(|| format!("<unknown id {id}>"));
                        return Some((id, name));
                    }
                }
            } else if let Some(v) = l.strip_prefix("nexusCategory") {
                let v = v.trim_start_matches(['=', ' ']).trim();
                if let Ok(id) = v.parse::<u32>() {
                    if id != 0 {
                        nexus_cat = Some(id);
                    }
                }
            }
        }
        let nid = nexus_cat?;
        let local = *self.nexus_to_local.get(&nid)?;
        let name = self
            .id_to_name
            .get(&local)
            .cloned()
            .unwrap_or_else(|| format!("<unknown id {local}>"));
        Some((local, name))
    }
}

// nexus mega-categories whose names are meaningless for separator matching.
// "miscellaneous" fuzzy-matching "miscellaneous compatibility patches" is
// how junk drawers happen - deny the fuzzy tier for these entirely.
const FUZZY_DENY: &[&str] = &[
    "miscellaneous",
    "models and textures",
    "visuals and graphics",
    "vr",
];

// words that carry no meaning for matching category names to separator
// labels - "Landscape and Environment" and "Environment" must collide
const STOPWORDS: &[&str] = &[
    "and", "the", "for", "with", "of", "a", "an", "mod", "mods", "se", "vr", "to", "in", "on",
];

// meaningful lowercase tokens of a name (alnum words, no stopwords, no junk)
fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 4 && !STOPWORDS.contains(w))
        .map(|w| w.to_string())
        .collect()
}

// fuzzy category -> separator bridge: exact match first, then shared-token
// match (e.g. nexus category "Landscape" finds separator "Landscape and
// Environment"). best score wins; ties go to the earliest separator.
// returns the separator label plus the shared tokens (the "score") for tracing
fn fuzzy_match_scored<'a>(cat: &str, sections: &'a [Section]) -> Option<(&'a str, Vec<String>)> {
    let cn = norm(cat);
    if let Some(s) = sections.iter().find(|s| norm(&s.label) == cn) {
        return Some((s.label.as_str(), vec![]));
    }
    let ct = tokens(cat);
    if ct.is_empty() {
        return None;
    }
    let mut best: Option<(Vec<String>, &Section)> = None;
    for s in sections {
        let st = tokens(&s.label);
        let shared: Vec<String> = ct.iter().filter(|t| st.contains(t)).cloned().collect();
        if !shared.is_empty()
            && best
                .as_ref()
                .map(|(b, _)| shared.len() > b.len())
                .unwrap_or(true)
        {
            best = Some((shared, s));
        }
    }
    best.map(|(shared, s)| (s.label.as_str(), shared))
}

fn fuzzy_match_section<'a>(cat: &str, sections: &'a [Section]) -> Option<&'a str> {
    fuzzy_match_scored(cat, sections).map(|(l, _)| l)
}

// which tier of the cascade made the call - written into the debug trace
#[derive(Clone)]
enum Why {
    OutputGuard,                    // generated output, never moves
    ExactRule,                      // !exact name rule
    CategoryRule(String),           // @category rule (category name)
    CategoryExactMatch(String),     // category name == separator label
    CategoryFuzzy(String, Vec<String>), // category, shared tokens
    Keyword(String),                // keyword rule (the phrase that hit)
    NoMatch,                        // nothing claimed this mod
    FollowParent(String),           // family integrity: follows its master
}

impl std::fmt::Display for Why {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Why::OutputGuard => write!(f, "guard: generated output, never moves"),
            Why::ExactRule => write!(f, "T1 exact name rule"),
            Why::CategoryRule(c) => write!(f, "T2 @category rule [{c}]"),
            Why::CategoryExactMatch(c) => write!(f, "T2 category==separator [{c}]"),
            Why::CategoryFuzzy(c, t) => write!(f, "T2 category~separator fuzzy [{c}] shared={}", t.join("+")),
            Why::Keyword(k) => write!(f, "T3 keyword [{k}]"),
            Why::NoMatch => write!(f, "T4 no rule, category, or keyword matched"),
            Why::FollowParent(p) => write!(f, "family: follows master [{p}]"),
        }
    }
}

// what a mod's mo2 category suggests: explicit @rule first, then a category
// whose name matches a separator label (exact, then fuzzy shared-token)
#[allow(dead_code)]
fn suggest_by_category<'a>(
    cat: &str,
    rules: &'a Rules,
    sections: &'a [Section],
) -> Option<&'a str> {
    let cl = cat.to_lowercase();
    for (name, sec) in &rules.category {
        if *name == cl {
            return Some(sec);
        }
    }
    fuzzy_match_section(cat, sections)
}

// like suggest_section, but reports which tier decided (for the debug trace)
fn suggest_explained<'a>(
    m: &ModEntry,
    rules: &'a Rules,
    cat: Option<&str>,
    sections: &'a [Section],
) -> (Option<&'a str>, Why) {
    // generated outputs never move, no matter what the keywords say
    if m.lower.starts_with("output") {
        return (None, Why::OutputGuard);
    }
    for (name, sec) in &rules.exact {
        if m.name == *name {
            return (Some(sec), Why::ExactRule);
        }
    }
    if let Some(c) = cat {
        let cl = c.to_lowercase();
        for (name, sec) in &rules.category {
            if *name == cl {
                return (Some(sec), Why::CategoryRule(c.to_string()));
            }
        }
        let cn = norm(c);
        if let Some(s) = sections.iter().find(|s| norm(&s.label) == cn) {
            return (Some(s.label.as_str()), Why::CategoryExactMatch(c.to_string()));
        }
        // junk mega-categories never fuzzy-match: their only shared tokens
        // are words like "miscellaneous", which turned "Miscellaneous
        // Compatibility Patches" into a 25-mod junk drawer through the
        // back door. these fall through to keyword rules / stay put.
        if !FUZZY_DENY.contains(&cl.as_str()) {
            if let Some((label, shared)) = fuzzy_match_scored(c, sections) {
                return (Some(label), Why::CategoryFuzzy(c.to_string(), shared));
            }
        }
    }
    let hay = format!(" {} ", m.lower.replace('_', " "));
    for (kw, excl, sec) in &rules.keyword {
        if hay.contains(kw.as_str()) && !excl.iter().any(|e| hay.contains(e.as_str())) {
            return (Some(sec), Why::Keyword(kw.clone()));
        }
    }
    (None, Why::NoMatch)
}

#[allow(dead_code)]
fn suggest_section<'a>(
    m: &ModEntry,
    rules: &'a Rules,
    cat: Option<&str>,
    sections: &'a [Section],
) -> Option<&'a str> {
    suggest_explained(m, rules, cat, sections).0
}

// is this mod name patch-flavored? kept for future use by the gui
#[allow(dead_code)]
fn parent_depth(m: &ModEntry, others: &[&ModEntry]) -> usize {
    let mut d = 0;
    for o in others {
        if o.norm != m.norm && o.norm.len() >= 10 && m.norm.contains(&o.norm) {
            d += 1;
        }
    }
    let nl = &m.lower;
    let patchy = (nl.contains("patch") || nl.contains("add-on") || nl.contains("addon"))
        && !nl.contains("patch hub")
        && !nl.contains("patch collection")
        && !nl.contains("compendium");
    if patchy {
        d += 1;
    }
    d
}

// a single proposed change, for the gui list
#[derive(Clone)]
struct Change {
    kind: ChangeKind,
    name: String,
    detail: String,  // "from -> to" or section label
    section: String, // section the mod currently sits in
}

#[derive(Clone, Copy, PartialEq)]
enum ChangeKind {
    Move,
    Reorder,
    Promote,
    Sink,
    Float,
    Warn,
}

// ---- platform guard ----
// sniff skse plugin dlls inside enabled mods for the runtime they were
// built against. ae (1.6.x) and oldrim plugins are dead on arrival in a skyrim
// vr profile; se (1.5.97) plugins only work when built with vr support
// (commonlibvr), so those get a "verify" flag instead of a hard fail.
#[derive(Clone, Copy, PartialEq)]
pub enum PlatKind {
    AeOnly,
    Oldrim,
    SeUnclear,
}

impl PlatKind {
    pub fn label(self) -> &'static str {
        match self {
            PlatKind::AeOnly => "AE (1.6.x) plugin - dead on arrival in VR",
            PlatKind::Oldrim => "Oldrim (1.9.32) plugin - wrong game",
            PlatKind::SeUnclear => "SE (1.5.97) plugin, no VR markers - verify",
        }
    }
}

pub struct PlatformWarning {
    pub mod_name: String,
    pub dll: String,
    pub kind: PlatKind,
    pub section: String,
}

// `enabled` is (mod name, current section) for every ticked mod
pub fn platform_scan(enabled: &[(String, String)], mods_dir: &Path) -> Vec<PlatformWarning> {
    let mut out = Vec::new();
    for (name, section) in enabled {
        let dir = mods_dir.join(name);
        if !dir.is_dir() {
            continue;
        }
        for entry in walkdir::WalkDir::new(&dir)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let p = entry.path();
            if !p
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("dll"))
            {
                continue;
            }
            // only skse plugin locations matter - random dlls elsewhere in a
            // mod (tools, launchers) aren't loaded by the game
            let lower_path = p.to_string_lossy().to_lowercase().replace('\\', "/");
            if !lower_path.contains("skse/plugins") {
                continue;
            }
            let fname = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            let Ok(bytes) = fs::read(p) else { continue };
            let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
            // a vr marker anywhere means the build targets vr - leave it alone
            if has(b"sksevr") {
                continue;
            }
            let mut ae = has(b"skse64_1_6_");
            let mut se = has(b"skse64_1_5_");
            let oldrim = has(b"skse_1_9_32");
            // address library payload filenames name their runtime too
            if fname.starts_with("versionlib-1-6") {
                ae = true;
            }
            if fname.starts_with("versionlib-1-5") {
                se = true;
            }
            let kind = if ae {
                PlatKind::AeOnly
            } else if oldrim {
                PlatKind::Oldrim
            } else if se {
                PlatKind::SeUnclear
            } else {
                continue;
            };
            out.push(PlatformWarning {
                mod_name: name.clone(),
                dll: fname,
                kind,
                section: section.clone(),
            });
            break; // one warning per mod is enough noise
        }
    }
    out
}

// runs everything in memory, writes the plan into `log`, fills `out` with
// structured changes, returns change count. `trace` gets the full verbose
// diagnostic (per-mod tier decisions, category resolution, reorder deltas)
// that gets saved as debug_sort.log.
// nothing hits disk here - the caller decides whether to write.
fn run(
    ml: &mut Modlist,
    rules: &Rules,
    log: &mut String,
    out: &mut Vec<Change>,
    cats: Option<&Categories>,
    conflicts: Option<&ConflictIndex>,
    trace: &mut String,
) -> usize {
    let mut changes = 0usize;

    let _ = writeln!(trace, "=== modslut debug trace ===");
    let _ = writeln!(
        trace,
        "categories.dat: {}",
        if cats.is_some() { "found" } else { "NOT FOUND - category tiers skipped" }
    );
    let ci_note = match conflicts {
        Some(ci) => format!("{} related pair(s) loaded", ci.pairs.len()),
        None => "NOT AVAILABLE - conflict auto-fix pass skipped".to_string(),
    };
    let _ = writeln!(trace, "conflict index: {ci_note}");

    // index: mod name -> current section label
    let mut where_is: HashMap<String, String> = HashMap::new();
    for s in &ml.sections {
        for m in &s.mods {
            where_is.insert(m.name.clone(), s.label.clone());
        }
    }

    // pass 1: wrong-section moves
    let _ = writeln!(trace, "\n--- pass 1: section assignment (per mod) ---");

    // decide every mod first, so family overrides can see a master's
    // final destination rather than its starting position
    struct Decision {
        from: String,
        want: Option<String>,
        why: Why,
        cat_str: String,
    }
    let mut decisions: HashMap<String, Decision> = HashMap::new();
    let mut all_mods: Vec<ModEntry> = Vec::new();
    for s in &ml.sections {
        for m in &s.mods {
            let cat = cats.and_then(|c| c.category_detail_of(&m.name));
            let (want, why) = suggest_explained(
                m,
                rules,
                cat.as_ref().map(|(_, n)| n.as_str()),
                &ml.sections,
            );
            let want = want
                .filter(|w| ml.sections.iter().any(|x| x.label == *w))
                .map(|w| w.to_string());
            let cat_str = match &cat {
                Some((id, n)) => format!("#{id} \"{n}\""),
                None => "none".to_string(),
            };
            all_mods.push(m.clone());
            decisions.insert(
                m.name.clone(),
                Decision { from: s.label.clone(), want, why, cat_str },
            );
        }
    }

    // family integrity: a patch/addon lives and dies with its master. if a
    // mod's name contains another mod's name (its master), it stays wherever
    // the master ends up - a patch stranded in a different section can fall
    // BELOW its master in priority and get overwritten by it. explicit
    // !exact and @category rules outrank this; keywords and fuzzy don't.
    // gated to patch-flavored kids (and vr variants): a mod that merely
    // MENTIONS another mod ("Particle Lights - Christmas (ENB and Community
    // Shaders)") is not its child, and runtime patchers (skypatcher) don't
    // care about file order.
    //
    // chains resolve recursively (a follows b follows c) with memoization -
    // containment norms strictly shrink along a chain, so it always ends.
    {
        // parent of each lineage-gated mod = longest contained other norm
        let mut parent_of: HashMap<String, String> = HashMap::new();
        for m in &all_mods {
            let nl = &m.lower;
            if !patch_flavored(nl) {
                continue;
            }
            if let Some(p) = all_mods
                .iter()
                .filter(|o| o.norm != m.norm && o.norm.len() >= 10 && m.norm.contains(&o.norm))
                .max_by_key(|o| o.norm.len())
            {
                parent_of.insert(m.name.clone(), p.name.clone());
            }
        }

        // where does a mod ultimately end up?
        // explicit rules anchor; lineage mods follow their parent's
        // destination; everyone else keeps its own rule decision or stays.
        fn resolve(
            name: &str,
            parent_of: &HashMap<String, String>,
            decisions: &HashMap<String, Decision>,
            memo: &mut HashMap<String, String>,
        ) -> String {
            if let Some(d) = memo.get(name) {
                return d.clone();
            }
            let d = &decisions[name];
            let explicit = matches!(d.why, Why::ExactRule | Why::CategoryRule(_));
            let dest = if explicit {
                d.want.clone().unwrap_or_else(|| d.from.clone())
            } else if let Some(p) = parent_of.get(name) {
                resolve(p, parent_of, decisions, memo)
            } else {
                d.want.clone().unwrap_or_else(|| d.from.clone())
            };
            memo.insert(name.to_string(), dest.clone());
            dest
        }

        let mut memo: HashMap<String, String> = HashMap::new();
        let names: Vec<String> = decisions.keys().cloned().collect();
        for name in &names {
            let dest = resolve(name, &parent_of, &decisions, &mut memo);
            let d = decisions.get_mut(name).unwrap();
            let followed = parent_of
                .get(name)
                .filter(|_| !matches!(d.why, Why::ExactRule | Why::CategoryRule(_)));
            match followed {
                Some(p) if dest != d.from => {
                    d.want = Some(dest);
                    d.why = Why::FollowParent(p.clone());
                }
                Some(p) => {
                    // family says stay put - veto any keyword/fuzzy move that
                    // would drag the patch away from its master
                    if d.want.is_some() {
                        d.want = None;
                        d.why = Why::FollowParent(p.clone());
                    }
                }
                _ => {}
            }
        }
    }

    // trace + collect the surviving moves
    let mut moves: Vec<(String, String, String)> = vec![]; // (mod name, from, to)
    for m in &all_mods {
        let d = &decisions[&m.name];
        let action = match &d.want {
            Some(w) if *w != d.from => format!("MOVE -> [{w}]"),
            _ => "stay".to_string(),
        };
        let _ = writeln!(
            trace,
            "  {} | cat {} | {} | {}",
            m.name, d.cat_str, d.why, action
        );
        if let Some(w) = &d.want {
            if *w != d.from {
                moves.push((m.name.clone(), d.from.clone(), w.clone()));
            }
        }
    }
    for (name, from, to) in &moves {
        changes += 1;
        let _ = writeln!(log, "MOVE  {name}   [{from} -> {to}]");
        out.push(Change {
            kind: ChangeKind::Move,
            name: name.clone(),
            detail: format!("{from} -> {to}"),
            section: from.clone(),
        });
        let src = ml.sections.iter_mut().find(|s| &s.label == from).unwrap();
        let idx = src.mods.iter().position(|m| &m.name == name).unwrap();
        let entry = src.mods.remove(idx);
        let dst = ml.sections.iter_mut().find(|s| &s.label == to).unwrap();
        dst.mods.push(entry);
        where_is.insert(name.clone(), to.clone());
    }

    // pass 1b: parked mods (before the first separator) that match a rule
    // get filed into their section - flags ride along, disabled stays disabled
    if !ml.parking.is_empty() {
        let _ = writeln!(trace, "\n--- pass 1b: parking lot ({} mod(s)) ---", ml.parking.len());
    }
    let mut parked_moves: Vec<(String, String)> = vec![]; // (raw line, to)
    for line in &ml.parking {
        let name = strip_flag(line).trim().to_string();
        let entry = ModEntry {
            raw: line.clone(),
            lower: name.to_lowercase(),
            norm: norm(&name),
            name,
        };
        let cat = cats.and_then(|c| c.category_detail_of(&entry.name));
        let (want, why) = suggest_explained(
            &entry,
            rules,
            cat.as_ref().map(|(_, n)| n.as_str()),
            &ml.sections,
        );
        let cat_str = match &cat {
            Some((id, n)) => format!("#{id} \"{n}\""),
            None => "none".to_string(),
        };
        let _ = writeln!(
            trace,
            "  {} | cat {} | {} | {}",
            entry.name,
            cat_str,
            why,
            match want {
                Some(w) => format!("MOVE -> [{w}]"),
                None => "stay".to_string(),
            }
        );
        if let Some(want) = want {
            if ml.sections.iter().any(|x| x.label == want) {
                parked_moves.push((line.clone(), want.to_string()));
            }
        }
    }
    for (line, to) in &parked_moves {
        changes += 1;
        let name = strip_flag(line).trim().to_string();
        let _ = writeln!(log, "MOVE  {name}   [parking lot -> {to}]");
        out.push(Change {
            kind: ChangeKind::Move,
            name: name.clone(),
            detail: format!("parking lot -> {to}"),
            section: "parking lot".to_string(),
        });
        let entry = ModEntry {
            raw: line.clone(),
            lower: name.to_lowercase(),
            norm: norm(&name),
            name,
        };
        let dst = ml.sections.iter_mut().find(|s| &s.label == to).unwrap();
        dst.mods.push(entry);
    }
    ml.parking
        .retain(|l| !parked_moves.iter().any(|(pl, _)| pl == l));

    // pass 2: intra-section order. mo2's ui is the file reversed:
    // file top = ui bottom = WINS conflicts. patches/addons must win over
    // their parents, so a patch sits directly BEFORE its parent in the file
    // (directly BELOW it in mo2's ui). no more patch clumps at the end of
    // a section: every group is "master, then its patches" in the ui.
    for s in &mut ml.sections {
        let snapshot: Vec<ModEntry> = s.mods.clone();
        let n = snapshot.len();

        // parent of each mod = the in-section mod with the LONGEST norm that
        // this mod's name contains (longest = most specific, handles chains
        // like "mod" -> "mod - dlc addon" -> "mod - dlc addon - fix")
        let mut parent: Vec<Option<usize>> = vec![None; n];
        for i in 0..n {
            let mut best: Option<usize> = None;
            for j in 0..n {
                if i == j {
                    continue;
                }
                let (a, b) = (&snapshot[i], &snapshot[j]);
                if a.norm != b.norm
                    && b.norm.len() >= 10
                    && a.norm.contains(&b.norm)
                    && best.map(|k| snapshot[k].norm.len() < b.norm.len()).unwrap_or(true)
                {
                    best = Some(j);
                }
            }
            parent[i] = best;
        }

        // walk the chain to each mod's root, counting depth (cycle-safe:
        // containment is strict, so chains can't loop)
        let mut key: Vec<(usize, std::cmp::Reverse<usize>, usize)> = Vec::with_capacity(n);
        for i in 0..n {
            let mut depth = 0usize;
            let mut root = i;
            let mut cur = i;
            while let Some(p) = parent[cur] {
                depth += 1;
                root = p;
                cur = p;
                if depth > n {
                    break; // paranoia
                }
            }
            key.push((root, std::cmp::Reverse(depth), i));
        }

        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| key[i]);
        let sorted: Vec<ModEntry> = order.iter().map(|&i| snapshot[i].clone()).collect();

        // report anything that actually changed position, with the parent and
        // index delta so the trace shows exactly why it moved
        let mut traced_header = false;
        for (new_i, m) in sorted.iter().enumerate() {
            let old_i = snapshot.iter().position(|o| o.name == m.name).unwrap();
            if old_i != new_i {
                changes += 1;
                let _ = writeln!(log, "REOR  {}   [{}]", m.name, s.label);
                out.push(Change {
                    kind: ChangeKind::Reorder,
                    name: m.name.clone(),
                    detail: format!("within [{}]", s.label),
                    section: s.label.clone(),
                });
                if !traced_header {
                    let _ = writeln!(trace, "\n--- pass 2: reorder [{}] ---", s.label);
                    traced_header = true;
                }
                let parent_name = parent[old_i].map(|p| snapshot[p].name.as_str());
                let (_, std::cmp::Reverse(depth), _) = key[old_i];
                let _ = writeln!(
                    trace,
                    "  {} | parent {} | depth {} | file index {} -> {} (file-earlier = wins)",
                    m.name,
                    parent_name.unwrap_or("<none>"),
                    depth,
                    old_i,
                    new_i
                );
            }
        }
        s.mods = sorted;
    }

    // pass 2.5: conflict-proven auto-fix. for mod pairs in the same section
    // that share real files (conflict.ini), exactly one of them patch-flavored:
    // the patch MUST win those files, so it sorts file-earlier than the base.
    // this catches relationships name-containment can't ("Ugly Shine Wet
    // Gloss begone R.A.S.S. Fix" vs "R.A.S.S. - Rain Ash And Snow Shaders").
    if let Some(ci) = conflicts {
        let _ = writeln!(trace, "\n--- pass 2.5: conflict-proven auto-fix ---");
        for s in &mut ml.sections {
            // collect enforcements (patch, base, shared)
            let mut enforce: Vec<(String, String, u32)> = Vec::new();
            for i in 0..s.mods.len() {
                for j in (i + 1)..s.mods.len() {
                    let (a, b) = (&s.mods[i], &s.mods[j]);
                    // unticked mods can't conflict - mo2 doesn't load them
                    if !a.raw.trim_start().starts_with('+')
                        || !b.raw.trim_start().starts_with('+')
                    {
                        continue;
                    }
                    let Some((_w, n)) = ci.shared(&a.name, &b.name) else { continue };
                    if n < conflicts::MIN_SHARED {
                        continue;
                    }
                    let (pa, pb) = (patch_flavored(&a.lower), patch_flavored(&b.lower));
                    if pa == pb {
                        continue; // both or neither patchy: no direction to enforce
                    }
                    let (patch, base) = if pa { (a, b) } else { (b, a) };
                    if enforce.iter().any(|(p, _, _)| p == &patch.name) {
                        continue; // one entry per patch is enough for the trace
                    }
                    let _ = writeln!(
                        trace,
                        "  [{label}] {patch} vs {base}: {n} shared files -> patch must win",
                        label = s.label,
                        patch = patch.name,
                        base = base.name
                    );
                    enforce.push((patch.name.clone(), base.name.clone(), n));
                }
            }
            // apply: patch file-earlier than base. a patch can have several
            // bases, so iterate to a fixed point (bounded).
            for _ in 0..6 {
                let mut moved = false;
                for (patch, base, n) in &enforce {
                    let Some(pi) = s.mods.iter().position(|m| &m.name == patch) else { continue };
                    let Some(bi) = s.mods.iter().position(|m| &m.name == base) else { continue };
                    if pi > bi {
                        // base currently wins - wrong. patch jumps in front of it
                        let e = s.mods.remove(pi);
                        s.mods.insert(bi, e);
                        changes += 1;
                        moved = true;
                        let _ = writeln!(
                            log,
                            "REOR  {patch} above {base} (conflict-proven: {n} shared files)"
                        );
                        out.push(Change {
                            kind: ChangeKind::Reorder,
                            name: patch.clone(),
                            detail: format!(
                                "conflict-proven: shares {n} files with {base}, patch must win"
                            ),
                            section: s.label.clone(),
                        });
                    }
                }
                if !moved {
                    break;
                }
            }
        }
    }

    // pass 4: audit - with the final order settled, verify every family pair
    // that shares real files actually has the child winning. violations get
    // reported, never silently fixed (they'd mean two passes disagree).
    if let Some(ci) = conflicts {
        let _ = writeln!(trace, "\n--- pass 4: conflict audit ---");
        let mut fails = 0usize;
        for s in &ml.sections {
            for i in 0..s.mods.len() {
                for j in (i + 1)..s.mods.len() {
                    let (a, b) = (&s.mods[i], &s.mods[j]);
                    // unticked mods can't conflict - mo2 doesn't load them
                    if !a.raw.trim_start().starts_with('+')
                        || !b.raw.trim_start().starts_with('+')
                    {
                        continue;
                    }
                    // family = containment + patch-flavored child
                    let (child, parent) = if a.norm.len() >= 10 && b.norm.contains(&a.norm) {
                        (a, b)
                    } else if b.norm.len() >= 10 && a.norm.contains(&b.norm) {
                        (b, a)
                    } else {
                        continue;
                    };
                    if !patch_flavored(&child.lower) {
                        continue;
                    }
                    let Some((winner, n)) = ci.shared(&child.name, &parent.name) else { continue };
                    if n < conflicts::MIN_SHARED {
                        continue;
                    }
                    if winner != child.name.as_str() {
                        fails += 1;
                        let _ = writeln!(
                            trace,
                            "  AUDIT FAIL [{label}] {parent} still wins {n} shared files over its own patch {child}",
                            label = s.label,
                            parent = parent.name,
                            child = child.name
                        );
                        let _ = writeln!(
                            log,
                            "WARN  {parent} beats its patch {child} on {n} shared files - needs a manual look",
                            parent = parent.name,
                            child = child.name
                        );
                    }
                }
            }
        }
        let _ = writeln!(trace, "  audit complete: {fails} violation(s)");
    }

        // pass 3: promote rules (winner earlier in file = wins conflicts)
        if !rules.promote.is_empty() {
            let _ = writeln!(trace, "\n--- pass 3: promote rules ---");
        }
        for (win, lose) in &rules.promote {
            for s in &mut ml.sections {
                let wi = s.mods.iter().position(|m| m.name == *win);
                let li = s.mods.iter().position(|m| m.name == *lose);
                if let (Some(wi), Some(li)) = (wi, li) {
                    let _ = writeln!(
                        trace,
                        "  [{label}] {win}@{wi} vs {lose}@{li} (file-earlier wins): {verdict}",
                        label = s.label,
                        verdict = if wi > li { "PROMOTE winner" } else { "already correct" }
                    );
                    if wi > li {
                        let e = s.mods.remove(wi);
                        s.mods.insert(li, e);
                        changes += 1;
                        let _ = writeln!(log, "PROM  {win} above {lose}");
                        out.push(Change {
                            kind: ChangeKind::Promote,
                            name: win.clone(),
                            detail: format!("above {lose}"),
                            section: s.label.clone(),
                        });
                    }
                }
            }
        }
        // pass 3b: sink rules - pin a mod to the END of its section's mod
        // list (right above the separator = top of the section in mo2 =
        // lowest priority in-section). for base replacers everything else
        // should overwrite. only fires when the mod is already in the
        // named section - it never moves anything across sections.
        if !rules.sink.is_empty() {
            let _ = writeln!(trace, "\n--- pass 3b: sink rules ---");
        }
        for (name, sec) in &rules.sink {
            for s in &mut ml.sections {
                if s.label != *sec {
                    continue;
                }
                let Some(i) = s.mods.iter().position(|m| m.name == *name) else {
                    continue;
                };
                let last = s.mods.len() - 1;
                let _ = writeln!(
                    trace,
                    "  [{sec}] {name}@{i} of {last}: {verdict}",
                    verdict = if i == last { "already sunk" } else { "SINK to section top (ui)" }
                );
                if i != last {
                    let e = s.mods.remove(i);
                    s.mods.push(e);
                    changes += 1;
                    let _ = writeln!(log, "SINK  {name} to top of [{sec}] (loses in-section)");
                    out.push(Change {
                        kind: ChangeKind::Sink,
                        name: name.clone(),
                        detail: format!("top of [{sec}] - loses to everything below it"),
                        section: sec.clone(),
                    });
                }
            }
        }

        // pass 3c: float rules - mirror of sink. pin a mod to the START of its
        // section's mod list (furthest from the separator = bottom of the
        // section in mo2 = highest priority in-section, wins everything).
        if !rules.float.is_empty() {
            let _ = writeln!(trace, "\n--- pass 3c: float rules ---");
        }
        for (name, sec) in &rules.float {
            for s in &mut ml.sections {
                if s.label != *sec {
                    continue;
                }
                let Some(i) = s.mods.iter().position(|m| m.name == *name) else {
                    continue;
                };
                let _ = writeln!(
                    trace,
                    "  [{sec}] {name}@{i}: {verdict}",
                    verdict = if i == 0 { "already floated" } else { "FLOAT to section bottom (ui)" }
                );
                if i != 0 {
                    let e = s.mods.remove(i);
                    s.mods.insert(0, e);
                    changes += 1;
                    let _ = writeln!(log, "FLOT  {name} to bottom of [{sec}] (wins in-section)");
                    out.push(Change {
                        kind: ChangeKind::Float,
                        name: name.clone(),
                        detail: format!("bottom of [{sec}] - wins everything in-section"),
                        section: sec.clone(),
                    });
                }
            }
        }
    changes
}

// ---- category report ----
// every mo2 category actually in use by this list's mods, with counts and
// whether it auto-matches a separator name. this is the bridge between
// nexus/mo2 categories and a custom separator layout: anything marked
// "no match" needs an @ rule in rules.txt to sort by category.
fn category_report(ml: &Modlist, cats: &Categories, rules: &Rules) -> String {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut uncategorized = 0usize;
    for s in &ml.sections {
        for m in &s.mods {
            match cats.category_of(&m.name) {
                Some(c) => *counts.entry(c).or_default() += 1,
                None => uncategorized += 1,
            }
        }
    }
    let mut rows: Vec<(String, usize)> = counts.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));

    let mut out = String::new();
    let _ = writeln!(out, "{:<40} {:>5}   maps to", "category", "mods");
    for (name, n) in &rows {
        // same bridge the sorter uses: explicit @ rule first (marked @ so
        // you can tell pinned mappings from guesses), then exact norm match,
        // then fuzzy shared-token. fuzzy hits are marked with ~ so you can
        // see which mappings are guesses worth pinning down with an @ rule
        let cn = norm(name);
        // mirror the sorter's @ rule check exactly (exact lowercase match)
        let cl = name.to_lowercase();
        let at_rule = rules
            .category
            .iter()
            .find(|(c, _)| *c == cl)
            .map(|(_, s)| s.clone());
        let target = match at_rule {
            Some(s) => format!("@ {s}"),
            None => match ml.sections.iter().find(|s| norm(&s.label) == cn) {
                Some(s) => s.label.clone(),
                None => match fuzzy_match_section(name, &ml.sections) {
                    Some(label) => format!("~ {label} (fuzzy)"),
                    None => "- no matching separator (add an @ rule)".to_string(),
                },
            },
        };
        let _ = writeln!(out, "{name:<40} {n:>5}   {target}");
    }
    let _ = writeln!(out, "\n{uncategorized} mod(s) have no category assigned");
    out
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();

    // double-clicked (no args) -> open the gui
    let Some(cmd) = args.first() else {
        gui::run_gui();
        return ExitCode::SUCCESS;
    };

    // we're in cli mode on a no-console build - grab the parent terminal back
    #[cfg(target_os = "windows")]
    unsafe {
        extern "system" {
            fn AttachConsole(pid: u32) -> i32;
        }
        AttachConsole(u32::MAX); // ATTACH_PARENT_PROCESS
    }

    eprintln!("modslut v{VERSION}");

    if cmd == "rules" {
        print!("{DEFAULT_RULES}");
        return ExitCode::SUCCESS;
    }

    // bare `modslut modlist.txt` defaults to sort-with-confirm
    let (cmd, file_idx) = if cmd == "check" || cmd == "sort" || cmd == "categories" {
        (cmd.as_str(), 1)
    } else {
        ("sort", 0)
    };
    let Some(file) = args.get(file_idx) else {
        eprintln!("gimme a modlist.txt to work on");
        return ExitCode::from(2);
    };

    let opt = |flag: &str| -> Option<String> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };

    let text = fs::read_to_string(file).unwrap_or_else(|e| {
        eprintln!("couldn't read {file}: {e}");
        std::process::exit(2);
    });
    let mut ml = parse(&text);
    let (rules, user_files) = load_rules_for(opt("-r").as_deref(), Some(Path::new(file)));
    for f in &user_files {
        eprintln!("user rules: {}", f.display());
    }
    let cats = Categories::discover(Path::new(file));

    // conflict index: read the per-profile cache when it's fresh, otherwise
    // scan the mods folder and write it (first scan on a big list takes a bit)
    let mlp = Path::new(file);
    let conflicts = ConflictIndex::mods_dir_of(mlp).and_then(|mods_dir| {
        let ini = ConflictIndex::ini_path(mlp)?;
        if ConflictIndex::is_fresh(mlp) {
            let ci = ConflictIndex::load(&ini)?;
            eprintln!("conflict index: {} pair(s) from cache", ci.pairs.len());
            Some(ci)
        } else {
            eprintln!("scanning mod files for conflicts (cached in conflict.ini after)…");
            let ci = ConflictIndex::build(&mods_dir, &active_mods(&ml));
            eprintln!(
                "indexed {} files, {} related pair(s)",
                ci.files_indexed,
                ci.pairs.len()
            );
            if let Err(e) = ci.save(&ini) {
                eprintln!("couldn't write conflict.ini: {e}");
            }
            Some(ci)
        }
    });

    // category report mode: show the category -> separator bridge, then bail
    if cmd == "categories" {
        match &cats {
            Some(c) => print!("{}", category_report(&ml, c, &rules)),
            None => eprintln!(
                "no categories.dat / mods folder found near {file} - \
                 run me from inside mo2 so i can see the instance"
            ),
        }
        return ExitCode::SUCCESS;
    }

    // everything happens in memory first - the plan gets printed either way
    let mut log = String::new();
    let mut plan = Vec::new();
    let mut trace = String::new();
    let n = run(&mut ml, &rules, &mut log, &mut plan, cats.as_ref(), conflicts.as_ref(), &mut trace);
    print!("{log}");
    println!("\n{n} suggested change(s)");

    // --trace writes the full diagnostic to debug_sort.log next to the modlist
    if args.iter().any(|a| a == "--trace") {
        let tp = Path::new(file)
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("debug_sort.log");
        match fs::write(&tp, &trace) {
            Ok(_) => println!("wrote {}", tp.display()),
            Err(e) => eprintln!("couldn't write trace: {e}"),
        }
    }

    if cmd == "sort" {
        if n == 0 {
            println!("nothing to do - list's already clean");
            return ExitCode::SUCCESS;
        }
        // the button: nothing gets written unless the user says yes
        print!("\nwrite sorted list? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush().ok();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).ok();
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("ok, no files touched");
            return ExitCode::SUCCESS;
        }
        let out = opt("-o").unwrap_or_else(|| {
            let p = Path::new(file);
            let stem = p.file_stem().unwrap().to_string_lossy();
            p.with_file_name(format!("{stem}.sorted.txt")).to_string_lossy().into_owned()
        });
        fs::write(&out, serialize(&ml)).unwrap_or_else(|e| {
            eprintln!("couldn't write {out}: {e}");
            std::process::exit(2);
        });
        println!("wrote {out}");
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(labels: &[&str]) -> Vec<Section> {
        labels
            .iter()
            .map(|l| Section {
                sep_line: format!("-{l}_separator"),
                label: l.to_string(),
                mods: vec![],
            })
            .collect()
    }

    #[test]
    fn fuzzy_exact_norm_wins() {
        let s = secs(&["Landscape and Environment", "Landscape"]);
        assert_eq!(fuzzy_match_section("landscape", &s), Some("Landscape"));
    }

    #[test]
    fn fuzzy_shared_token() {
        let s = secs(&["Landscape and Environment", "Audio"]);
        // nexus category "Environment" should land in the combined separator
        assert_eq!(
            fuzzy_match_section("Environment", &s),
            Some("Landscape and Environment")
        );
    }

    #[test]
    fn fuzzy_ignores_stopwords_and_short_tokens() {
        let s = secs(&["Models and Textures"]);
        assert_eq!(fuzzy_match_section("Mods for the AI", &s), None);
        assert_eq!(
            fuzzy_match_section("Textures", &s),
            Some("Models and Textures")
        );
    }

    #[test]
    fn fuzzy_no_match_returns_none() {
        let s = secs(&["Audio Overhaul"]);
        assert_eq!(fuzzy_match_section("Combat", &s), None);
    }
}

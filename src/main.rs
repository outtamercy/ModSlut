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
pub(crate) mod game;
pub(crate) mod loot;
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
//   <Mod Name                        (sectionless sink: load first in whatever section
//                                     holds the mod - for no-esp frameworks that are
//                                     masters for other mods but census-invisible)
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
    sink_any: Vec<String>, // sectionless sink: load first in WHATEVER section holds the mod
    // directives (switches, not rules): a bare `!name` line with no '='
    rename_separators: bool, // opt-in: pass 0 retitles separators to concepts
    proven_only: bool,       // opt-in: only proven moves (rules/loot/conflict/census), no guesses
    dump: Vec<String>, // dump sections ("!dump = End of List"): a waiting room, not a
                       // real section - rules never resolve INTO it, and nothing gets
                       // moved there. list-specific, so it's opt-in per user rules.
}

impl Rules {
    fn empty() -> Rules {
        Rules { exact: vec![], keyword: vec![], category: vec![], promote: vec![], sink: vec![], float: vec![], sink_any: vec![], rename_separators: false, proven_only: false, dump: vec![] }
    }
    // user rules get checked first (first hit wins), so they go in front
    fn prepend(&mut self, user: Rules) {
        self.exact.splice(0..0, user.exact);
        self.keyword.splice(0..0, user.keyword);
        self.category.splice(0..0, user.category);
        self.promote.splice(0..0, user.promote);
        self.sink.splice(0..0, user.sink);
        self.float.splice(0..0, user.float);
        self.sink_any.splice(0..0, user.sink_any);
        self.rename_separators |= user.rename_separators;
        self.proven_only |= user.proven_only;
        self.dump.splice(0..0, user.dump);
    }
}

const DEFAULT_RULES: &str = include_str!("../rules.txt");

// bump every build - shown in the gui title and cli so we always know
// which build a bug report came from
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

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
                if a.trim().eq_ignore_ascii_case("dump") {
                    // !dump = Section Label - a waiting-room separator:
                    // contents should be filed out, nothing files in
                    r.dump.push(b.trim().to_string());
                } else {
                    r.exact.push((a.trim().to_string(), b.trim().to_string()));
                }
            } else {
                // bare `!directive` (no '='): a switch, not a pin
                match rest.trim().to_lowercase().as_str() {
                    "rename-separators" => r.rename_separators = true,
                    "proven-only" => r.proven_only = true,
                    _ => {}
                }
            }
        } else if let Some(rest) = l.strip_prefix('>') {
            if let Some((a, b)) = rest.split_once('=') {
                r.promote.push((a.trim().to_string(), b.trim().to_string()));
            }
        } else if let Some(rest) = l.strip_prefix('<') {
            if let Some((a, b)) = rest.split_once('=') {
                r.sink.push((a.trim().to_string(), b.trim().to_string()));
            } else {
                // no target: sink in whatever section the mod lives in.
                // for no-esp frameworks (skypatcher, mfg fix) that are
                // masters for other mods but invisible to the plugin
                // census - they must load EARLY and the file can't prove it.
                r.sink_any.push(rest.trim().to_string());
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

// ---- keywords.ini: alias / family / never tables for name matching ----
// flat ini, four sections:
//   [strip]   noise = se, sse, ...      tokens removed before comparison
//   [alias]   Canonical = variant, ...  variant resolves to canonical (HARD)
//   [family]  Canonical = sibling, ...  platform siblings: equal for ORDER only
//   [never]   Name = other, ...         key must NEVER match any listed name
// lives next to modlist.txt (per-profile) and/or next to the exe. optional.
#[derive(Default)]
pub(crate) struct Keywords {
    strip: std::collections::HashSet<String>,
    alias: HashMap<String, String>,  // tnorm(variant) -> tnorm(canonical)
    family: HashMap<String, String>, // tnorm(member)  -> tnorm(canonical)
    never: Vec<(Vec<String>, Vec<Vec<String>>)>, // key tokens -> excluded token sets
}

impl Keywords {
    // token norm: lowercase, alnum tokens, strip-list removed, space-joined
    fn tnorm(&self, s: &str) -> String {
        let mut t = String::new();
        for c in s.chars() {
            t.push(if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { ' ' });
        }
        t.split_whitespace()
            .filter(|w| !self.strip.contains(*w))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn parse_into(text: &str, kw: &mut Keywords) {
        let mut section = String::new();
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                section = line[1..line.len() - 1].trim().to_lowercase();
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            match section.as_str() {
                "strip" => {
                    for w in v.split(',') {
                        let w = w.trim().to_lowercase();
                        if !w.is_empty() {
                            kw.strip.insert(w);
                        }
                    }
                }
                "alias" | "family" => {
                    let canon = kw.tnorm(k.trim());
                    if canon.is_empty() {
                        continue;
                    }
                    let mut vars: Vec<String> = v
                        .split(',')
                        .map(|x| kw.tnorm(x.trim()))
                        .filter(|t| !t.is_empty())
                        .collect();
                    vars.push(canon.clone());
                    let map = if section == "alias" { &mut kw.alias } else { &mut kw.family };
                    for tv in vars {
                        map.insert(tv, canon.clone());
                    }
                }
                "never" => {
                    let key: Vec<String> = kw.tnorm(k.trim()).split_whitespace().map(String::from).collect();
                    if key.is_empty() {
                        continue;
                    }
                    let excl: Vec<Vec<String>> = v
                        .split(',')
                        .map(|e| kw.tnorm(e.trim()).split_whitespace().map(String::from).collect())
                        .filter(|t: &Vec<String>| !t.is_empty())
                        .collect();
                    if !excl.is_empty() {
                        kw.never.push((key, excl));
                    }
                }
                _ => {}
            }
        }
    }

    pub(crate) fn load(modlist: Option<&Path>) -> (Keywords, Vec<PathBuf>) {
        let mut kw = Keywords::default();
        let mut loaded = Vec::new();
        let mut files = Vec::new();
        if let Some(d) = modlist.and_then(|m| m.parent()) {
            files.push(d.join("keywords.ini"));
        }
        if let Ok(exe) = env::current_exe() {
            if let Some(d) = exe.parent() {
                files.push(d.join("keywords.ini"));
            }
        }
        for f in files {
            if f.is_file() {
                match fs::read_to_string(&f) {
                    Ok(t) => {
                        Self::parse_into(&t, &mut kw);
                        loaded.push(f);
                    }
                    Err(e) => eprintln!("couldn't read keywords {}: {e}", f.display()),
                }
            }
        }
        (kw, loaded)
    }

    // canonical identity: exact alias/family hit, else SUBSTITUTE the matched
    // variant's tokens in place (a patch named "USSEP - X Patch" becomes
    // "Unofficial ... Patch - X Patch" so containment still sees the child).
    pub(crate) fn canonical(&self, s: &str) -> String {
        let t = self.tnorm(s);
        if t.is_empty() {
            return t;
        }
        if let Some(c) = self.alias.get(&t).or_else(|| self.family.get(&t)) {
            return c.clone();
        }
        let toks: Vec<&str> = t.split_whitespace().collect();
        for (var, canon) in self.alias.iter().chain(self.family.iter()) {
            let vt: Vec<&str> = var.split_whitespace().collect();
            if vt.is_empty() || vt.len() > toks.len() || vt.iter().any(|w| w.len() < 3) {
                continue;
            }
            if let Some(pos) = toks.windows(vt.len()).position(|w| w == vt.as_slice()) {
                let mut out: Vec<&str> = toks[..pos].to_vec();
                out.extend(canon.split_whitespace());
                out.extend(toks[pos + vt.len()..].iter());
                return out.join(" ");
            }
        }
        t
    }

    // collapsed canonical for containment checks (same shape as norm())
    pub(crate) fn cnorm(&self, s: &str) -> String {
        norm(&self.canonical(s))
    }

    pub(crate) fn is_never(&self, a: &str, b: &str) -> bool {
        let ta: Vec<String> = self.tnorm(a).split_whitespace().map(String::from).collect();
        let tb: Vec<String> = self.tnorm(b).split_whitespace().map(String::from).collect();
        let sup = |big: &[String], small: &[String]| small.iter().all(|t| big.contains(t));
        self.never.iter().any(|(key, excls)| {
            (sup(&ta, key) && excls.iter().any(|e| sup(&tb, e)))
                || (sup(&tb, key) && excls.iter().any(|e| sup(&ta, e)))
        })
    }
}

// user rule file locations, most specific first: the profile folder holding
// this modlist, then the folder the exe lives in
// debug_sort.log lives next to ms.exe (the tool folder), same place as
// keywords.ini and conflict.ini - everything modslut needs in one spot.
pub(crate) fn debug_log_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("debug_sort.log")))
        .unwrap_or_else(|| PathBuf::from("debug_sort.log"))
}

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

pub(crate) fn load_rules_for(
    path: Option<&str>,
    modlist: Option<&Path>,
    skip_builtins: bool,
) -> (Rules, Vec<PathBuf>) {
    // skip_builtins = "user rules only" mode: the built-in cascade stays out
    // of the way entirely - only the user's own pins move mods. an explicit
    // -r file always loads (the user asked for THAT file specifically).
    let text = match path {
        Some(p) => Some(fs::read_to_string(p).unwrap_or_else(|e| {
            eprintln!("couldn't read rules file {p}: {e}");
            std::process::exit(2);
        })),
        None if !skip_builtins => Some(DEFAULT_RULES.to_string()),
        None => None,
    };
    let mut r = Rules::empty();
    if let Some(text) = text {
        parse_rules_into(&text, &mut r);
    }

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
// junk catch-all categories that carry no sorting signal. "visuals and
// graphics" was once here, but it's a real concept (imaginator, flt and
// friends live there) - it can drive fuzzy matches and separator renames.
const FUZZY_DENY: &[&str] = &[
    "miscellaneous",
    "models and textures",
    "vr",
];

// words that carry no meaning for matching category names to separator
// labels - "Landscape and Environment" and "Environment" must collide
const STOPWORDS: &[&str] = &[
    "and", "the", "for", "with", "of", "a", "an", "mod", "mods", "se", "vr", "to", "in", "on",
];

// words that describe SHAPE, not content: "Bug Fixes" and "Weapons, Armour,
// Clothing, and Clutter Fixes" sharing "fixes" is not evidence they belong
// together - it's how 123 bug-fix mods landed in a weapons section. a fuzzy
// match needs a DISTINCTIVE shared token (or two generic ones).
// (stored in tokens()'s singularized form: "fixes" -> "fixe", etc.)
const GENERIC_TOKENS: &[&str] = &[
    "fix", "fixe", "patch", "patche", "overhaul", "collection",
    "compendium", "tweak", "improvement", "resource", "addon",
];

// qualifier tokens that NARROW a section's scope: a category that doesn't
// say "male" must never fuzzy-file into "Male Body Additions"
const QUALIFIER_TOKENS: &[&str] = &["male", "female"];

// meaningful lowercase tokens of a name (alnum words, no stopwords, no junk).
// len >= 3 so "bug" survives - "Essential Bug Fixes" vs "Weapons... Fixes"
// is decided by exactly that token.
fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3 && !STOPWORDS.contains(w))
        // gentle singular: "npcs" and "npc" are the same concept
        .map(|w| {
            if w.len() > 3 && w.ends_with('s') && !w.ends_with("ss") {
                w[..w.len() - 1].to_string()
            } else {
                w.to_string()
            }
        })
        .collect()
}

// fuzzy category -> separator bridge: exact match first, then shared-token
// match (e.g. nexus category "Landscape" finds separator "Landscape and
// Environment"). best score wins; ties go to the earliest separator.
// returns the separator label plus the shared tokens (the "score") for tracing
// strict = per-mod category filing: generic-token-only matches rejected,
// qualifier narrowing honored. lenient = concept resolution / separator
// rename suggestions, where a generic concept like "Fixes" is the point.
fn fuzzy_match_scored<'a>(
    cat: &str,
    sections: &'a [Section],
    strict: bool,
) -> Option<(&'a str, Vec<String>)> {
    let cn = norm(cat);
    if let Some(s) = sections.iter().find(|s| norm(&s.label) == cn) {
        return Some((s.label.as_str(), vec![]));
    }
    let ct = tokens(cat);
    if ct.is_empty() {
        return None;
    }
    // score: distinctive shared tokens first, total shared, then the
    // tie-breaks below. stored raw; compared with Reverse on the last two.
    let mut best: Option<(usize, usize, usize, usize, Vec<String>, &Section)> = None;
    for s in sections {
        let st = tokens(&s.label);
        // a qualifier the category doesn't share narrows the section away
        // ("Body, Face, and Hair" is not "Male Body Additions")
        if strict
            && QUALIFIER_TOKENS
                .iter()
                .any(|q| st.iter().any(|t| t == q) && !ct.iter().any(|t| t == q))
        {
            continue;
        }
        let shared: Vec<String> = ct.iter().filter(|t| st.contains(t)).cloned().collect();
        let distinctive = shared
            .iter()
            .filter(|t| !GENERIC_TOKENS.contains(&t.as_str()))
            .count();
        // no distinctive token AND fewer than two generic ones = no evidence
        if strict && distinctive == 0 && shared.len() < 2 {
            continue;
        }
        if shared.is_empty() {
            continue;
        }
        // tie-break: a section sharing the category's PRIMARY (earliest)
        // token beats one sharing only secondary tokens - "Body, Face, and
        // Hair" is a body category that also does hair, so "Skin & Body"
        // wins over "Hair". final tiebreak: fewer label tokens = broader
        // scope ("Skin & Body" over "Skin and Body - Argonians and Khajiits")
        let min_pos = ct
            .iter()
            .position(|t| st.contains(t))
            .unwrap_or(usize::MAX);
        let cand = (
            distinctive,
            shared.len(),
            std::cmp::Reverse(min_pos),
            std::cmp::Reverse(st.len()),
        );
        let beats = best
            .as_ref()
            .map(|(bd, _, bpos, blen, bshared, _)| {
                cand > (
                    *bd,
                    bshared.len(),
                    std::cmp::Reverse(*bpos),
                    std::cmp::Reverse(*blen),
                )
            })
            .unwrap_or(true);
        if beats {
            best = Some((distinctive, shared.len(), min_pos, st.len(), shared, s));
        }
    }
    best.map(|(_, _, _, _, shared, s)| (s.label.as_str(), shared))
}

fn fuzzy_match_section<'a>(cat: &str, sections: &'a [Section], strict: bool) -> Option<&'a str> {
    fuzzy_match_scored(cat, sections, strict).map(|(l, _)| l)
}

// ---- concept-targeted rules ----
// rule targets are CONCEPTS ("Lighting", "Fixes", "Audio"), not literal
// separator names. at sort time each concept resolves against whatever
// separators the list actually has:
//   1. exact label, 2. normalized equality, 3. containment either way
//   ("Lux (Lighting)" contains concept "Lighting"), 4. shared-token fuzzy.
// no match = the rule quietly no-ops and lower tiers decide. this is what
// makes the built-in rules release-safe: they can never invent a section
// that isn't there, and they never strand a mod when it isn't.
fn resolve_concept<'a>(concept: &str, sections: &'a [Section]) -> Option<(&'a str, &'static str)> {
    if let Some(s) = sections.iter().find(|s| s.label == concept) {
        return Some((s.label.as_str(), "exact"));
    }
    let cn = norm(concept);
    if let Some(s) = sections.iter().find(|s| norm(&s.label) == cn) {
        return Some((s.label.as_str(), "norm"));
    }
    {
        // containment, but SCORED - "first in list order" is how "Lighting"
        // landed on "Special Load After Lighting Mods" instead of
        // "Lux (Lighting)". rank: the concept appearing as whole words in
        // the label beats a raw substring, which beats reverse containment;
        // ties go to the SHORTEST label (closest concept), then earliest.
        let cw: Vec<String> = concept
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(|w| w.to_string())
            .collect();
        let word_hit = |label: &str| -> bool {
            let lw: Vec<String> = label
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| !w.is_empty())
                .map(|w| w.to_string())
                .collect();
            // exact word or same-stem ("animation" ~ "animations") - a bare
            // stem compare is what lets the plural section beat a long
            // compound that happens to contain the word
            let stem_eq = |a: &str, b: &str| {
                // plural/short-suffix variants only ("animation"~"animations").
                // loose prefix matching would make "body" claim "bodyslide".
                let (d, ok) = (a.len().abs_diff(b.len()), a.len() >= 4 && b.len() >= 4);
                a == b || (ok && d <= 2 && (a.starts_with(b) || b.starts_with(a)))
            };
            !cw.is_empty()
                && lw
                    .windows(cw.len())
                    .any(|w| w.iter().zip(cw.iter()).all(|(a, b)| stem_eq(a, b)))
        };
        let mut best: Option<(u8, usize, &Section)> = None;
        for s in sections {
            let sn = norm(&s.label);
            let rank = if !cn.is_empty() && word_hit(&s.label) {
                0
            } else if cn.len() >= 4 && sn.len() >= 4 && sn.contains(&cn) {
                1
            } else if cn.len() >= 4 && sn.len() >= 4 && cn.contains(&sn) {
                2
            } else {
                continue;
            };
            let cand = (rank, sn.len(), s);
            if best.as_ref().map(|b| (cand.0, cand.1) < (b.0, b.1)).unwrap_or(true) {
                best = Some(cand);
            }
        }
        if let Some((_, _, s)) = best {
            return Some((s.label.as_str(), "contains"));
        }
    }
    if let Some((label, _)) = fuzzy_match_scored(concept, sections, false) {
        return Some((label, "tokens"));
    }
    None
}

// ---- separator auto-rename ----
// a separator the rules can't see is a dead section: "Ya Filthy Animal"
// gives resolve_concept nothing to grab, so every nsfw rule no-ops and the
// user hand-files 200 mods. the fix is not to guess harder but to let the
// CONTENTS name the concept: count what lives under the separator, and if
// one concept clearly dominates and the label doesn't already say it,
// append "- Concept" to the label. the user's flavor name stays in front.
// nexus categories are miscat-heavy, so the bar is high: a dominant category
// needs ~a third of the section AND double the runner-up, and the junk
// categories (miscellaneous etc.) can never drive a rename. nsfw detection
// uses framework names only (sexlab/ostim/...) - body mods are NOT nsfw.
const NSFW_FRAMEWORK_TOKENS: &[&str] = &["sexlab", "ostim", "aroused", "arousal", "slal", "nsfw"];

fn label_has_concept(label: &str, concept: &str) -> bool {
    let lw: Vec<String> = label
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect();
    let stem_eq = |a: &str, b: &str| {
        let (d, ok) = (a.len().abs_diff(b.len()), a.len() >= 4 && b.len() >= 4);
        a == b || (ok && d <= 2 && (a.starts_with(b) || b.starts_with(a)))
    };
    concept
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 4)
        .all(|cw| lw.iter().any(|w| stem_eq(w, &cw)))
}

// words (stems, >= min_len chars) shared between two phrases
fn shared_stem(a: &str, b: &str, min_len: usize) -> bool {
    let words = |s: &str| -> Vec<String> {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= min_len)
            .map(|w| w.to_string())
            .collect()
    };
    let (aw, bw) = (words(a), words(b));
    aw.iter().any(|x| {
        bw.iter().any(|y| {
            let d = x.len().abs_diff(y.len());
            x == y || (d <= 2 && (x.starts_with(y.as_str()) || y.starts_with(x.as_str())))
        })
    })
}

fn suggest_sep_renames(
    ml: &Modlist,
    cats: Option<&Categories>,
    trace: &mut String,
) -> Vec<(usize, String)> {
    // structural labels name a PLACE, not a concept - never tag them
    const STRUCTURAL: &[&str] = &["game folder", "parking", "end of list", "nexus"];
    let mut out = Vec::new();
    for (idx, s) in ml.sections.iter().enumerate() {
        let enabled: Vec<&ModEntry> = s
            .mods
            .iter()
            .filter(|m| m.raw.trim_start().starts_with('+'))
            .collect();
        // nsfw: framework names are near-perfect precision (nobody names a
        // house mod "sexlab"), so two hits is enough and there's no size
        // gate - a 3-mod section still deserves to be found by nsfw rules
        let nsfw_hits = enabled
            .iter()
            .filter(|m| {
                let l = m.name.to_lowercase();
                NSFW_FRAMEWORK_TOKENS.iter().any(|t| l.contains(t))
            })
            .count();
        if nsfw_hits >= 2 && !label_has_concept(&s.label, "nsfw") {
            out.push((idx, format!("{} - NSFW", s.label)));
            let _ = writeln!(
                trace,
                "  [{}] + '- NSFW' ({nsfw_hits} framework mods inside)",
                s.label
            );
            continue; // one suffix per separator
        }
        if enabled.len() < 4 {
            continue; // too little signal to name a section by its contents
        }
        let Some(cats) = cats else { continue };
        if STRUCTURAL.iter().any(|t| s.label.to_lowercase().contains(t)) {
            continue;
        }
        // dominant nexus category
        let mut hist: HashMap<String, usize> = HashMap::new();
        let mut categorized = 0usize;
        for m in &enabled {
            if let Some(c) = cats.category_of(&m.name) {
                let cl = c.to_lowercase();
                // junk categories never name a section - and neither does
                // "utilities": it's technically a real category but as a
                // suffix it's pure noise next to a specific label
                // ("Extension Frameworks - Extended Functionality - Utilities"
                // tells you less than the name already did)
                if FUZZY_DENY.contains(&cl.as_str()) || cl == "utilities" {
                    continue;
                }
                *hist.entry(c).or_default() += 1;
                categorized += 1;
            }
        }
        if categorized < 4 {
            continue;
        }
        let mut ranked: Vec<(String, usize)> = hist.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1));
        let (top_cat, top_n) = (&ranked[0].0, ranked[0].1);
        let runner_up = ranked.get(1).map(|(_, n)| *n).unwrap_or(0);
        let dominant = top_n >= 4 && top_n * 10 >= categorized * 3 && top_n >= runner_up * 2;
        // rename only when the label is SILENT about the concept: if label
        // and category share any word at all ("Skin & Body" ~ "Body, Face,
        // and Hair"), the label already says what it is and a suffix is noise
        if dominant
            && !label_has_concept(&s.label, top_cat)
            && !shared_stem(&s.label, top_cat, 3)
        {
            out.push((idx, format!("{} - {}", s.label, top_cat)));
            let _ = writeln!(
                trace,
                "  [{}] + '- {top_cat}' ({top_n}/{categorized} categorized mods, runner-up {runner_up})",
                s.label
            );
        }
    }
    out
}

// rewrite every rule's target through resolve_concept, dropping rules whose
// concept doesn't exist in this list (with a trace note). promote rules are
// exempt: their right-hand side is a mod NAME, not a section.
fn resolve_targets(rules: &Rules, sections: &[Section], trace: &mut String) -> Rules {
    let _ = writeln!(trace, "\n--- concept resolution (rule targets -> actual separators) ---");
    let mut out = Rules::empty();
    out.rename_separators = rules.rename_separators;
    out.proven_only = rules.proven_only;
    out.dump = rules.dump.clone();
    let mut resolve = |target: &str, kind: &str| -> Option<String> {
        match resolve_concept(target, sections) {
            Some((label, how)) => {
                // a dump section is a waiting room, never a destination
                if rules.dump.iter().any(|d| d.eq_ignore_ascii_case(label)) {
                    let _ = writeln!(
                        trace,
                        "  [{target}] ({kind}): resolved to dump section [{label}] - rule skipped"
                    );
                    return None;
                }
                if label != target {
                    let _ = writeln!(trace, "  [{target}] -> [{label}] ({how})");
                }
                Some(label.to_string())
            }
            None => {
                let _ = writeln!(
                    trace,
                    "  [{target}] ({kind}): no matching separator - rule skipped"
                );
                None
            }
        }
    };
    for (a, b) in &rules.exact {
        if let Some(t) = resolve(b, "exact") {
            out.exact.push((a.clone(), t));
        }
    }
    for (a, excl, b) in &rules.keyword {
        if let Some(t) = resolve(b, "keyword") {
            out.keyword.push((a.clone(), excl.clone(), t));
        }
    }
    for (a, b) in &rules.category {
        if let Some(t) = resolve(b, "category") {
            out.category.push((a.clone(), t));
        }
    }
    for (a, b) in &rules.sink {
        if let Some(t) = resolve(b, "sink") {
            out.sink.push((a.clone(), t));
        }
    }
    for (a, b) in &rules.float {
        if let Some(t) = resolve(b, "float") {
            out.float.push((a.clone(), t));
        }
    }
    out.promote = rules.promote.clone();
    out.sink_any = rules.sink_any.clone();
    out
}

fn is_esm_entry(
    mod_name: &str,
    census: Option<&[(String, String, crate::plugins::PluginInfo)]>,
) -> bool {
    if mod_name.to_lowercase().ends_with(".esm") {
        return true;
    }
    if let Some(census) = census {
        census.iter().any(|(m, _, info)| {
            m == mod_name && (info.is_esm || info.plugin.to_lowercase().ends_with(".esm"))
        })
    } else {
        false
    }
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
    fuzzy_match_section(cat, sections, true)
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
            if let Some((label, shared)) = fuzzy_match_scored(c, sections, true) {
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

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum ChangeKind {
    Move,
    Reorder,
    Promote,
    Sink,
    Float,
    Warn,
    Rename,
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
    census: Option<&[(String, String, crate::plugins::PluginInfo)]>,
    kw: &Keywords,
    // solo = "apply user rules only": every proven pass (loot, family,
    // census masters, conflict) stays out - the caller hands in ONLY the
    // user's rules, and category-name guessing is suppressed too, so the
    // resulting modlist differs from the input only where a user rule
    // actually speaks. used by the gui's "apply user rules only" button.
    solo: bool,
    trace: &mut String,
) -> usize {
    let mut changes = 0usize;
    if solo {
        let _ = writeln!(
            trace,
            "user-rules-only apply: solo run - only modslut_rules.txt rules act (loot/family/census/conflict passes skipped)"
        );
    }

    // canonical name identities from keywords.ini: alias hits are HARD
    // (confidence 1.0 - the variant IS the canonical mod), family siblings
    // share order identity, [never] pairs veto containment matching.
    let cnorm_of: HashMap<String, String> = ml
        .sections
        .iter()
        .flat_map(|s| s.mods.iter())
        .map(|m| (m.name.clone(), kw.cnorm(&m.name)))
        .collect();
    let cn = |m: &ModEntry| -> String { cnorm_of.get(&m.name).cloned().unwrap_or_else(|| m.norm.clone()) };
    if !kw.alias.is_empty() || !kw.family.is_empty() || !kw.never.is_empty() {
        let _ = writeln!(
            trace,
            "keywords.ini: {} aliases, {} family members, {} never-rules",
            kw.alias.len(),
            kw.family.len(),
            kw.never.len()
        );
    }

    let _ = writeln!(trace, "=== modslut v{VERSION} debug trace ===");
    let _ = writeln!(
        trace,
        "built: {} {}",
        env!("CARGO_PKG_VERSION"),
        option_env!("MODSLUT_BUILD_SHA").unwrap_or("local")
    );
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

    // concept-targeted rules: rewrite targets to this list's actual
    // separators (or drop the rule if the concept doesn't exist here)
    // pass 0: separator auto-rename. runs BEFORE target resolution so rules
    // resolve against the renamed labels - and since a rename only APPENDS
    // "- Concept", an old rule target still containment-matches the new
    // label, so user pins written against the old name keep working.
    // off by default: most users have established separator names already.
    // opt in with a bare `!rename-separators` line in modslut_rules.txt.
    if rules.rename_separators {
        let renames = suggest_sep_renames(ml, cats, trace);
        if !renames.is_empty() {
            let _ = writeln!(
                trace,
                "\n--- pass 0: separator auto-rename ({} separator(s)) ---",
                renames.len()
            );
        }
        for (idx, new_label) in renames {
            let old = ml.sections[idx].label.clone();
            let _ = writeln!(trace, "  REN   [{old}] -> [{new_label}]");
            let _ = writeln!(log, "REN   [{old}] -> [{new_label}]");
            ml.sections[idx].label = new_label.clone();
            changes += 1;
            out.push(Change {
                kind: ChangeKind::Rename,
                name: old.clone(),
                detail: format!("renamed to [{new_label}]"),
                section: old,
            });
        }
    } else {
        let _ = writeln!(
            trace,
            "\n--- pass 0: separator auto-rename OFF (add `!rename-separators` to modslut_rules.txt to enable) ---"
        );
    }
    if rules.proven_only {
        let _ = writeln!(
            trace,
            "proven-only mode: ON - only explicit rules, LOOT, conflict, and census-proven constraints move mods (category/keyword guesses suppressed)"
        );
    }

    let resolved_rules = resolve_targets(rules, &ml.sections, trace);
    let rules = &resolved_rules;

    // "never move" means NEVER move: explicit user pins lock section / in-section
    // position. passes consult this set and leave pinned mods where they are.
    let pinned: std::collections::HashSet<&str> = rules
        .exact
        .iter()
        .map(|(n, _)| n.as_str())
        .chain(rules.sink.iter().map(|(n, _)| n.as_str()))
        .chain(rules.sink_any.iter().map(|n| n.as_str()))
        .chain(rules.float.iter().map(|(n, _)| n.as_str()))
        .collect();

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
    let mut guess_suppressed = 0usize;
    for s in &ml.sections {
        for m in &s.mods {
            let cat = cats.and_then(|c| c.category_detail_of(&m.name));
            let (want, why) = suggest_explained(
                m,
                rules,
                cat.as_ref().map(|(_, n)| n.as_str()),
                &ml.sections,
            );
            // proven-only: guesses (category≈separator fuzzy, keyword hits)
            // never move a mod - only what the user or the data can PROVE.
            // solo keeps rule-driven tiers (they're all user rules there) but
            // drops the category-name guesses, which need no rule at all.
            let guess = matches!(
                why,
                Why::CategoryExactMatch(_) | Why::CategoryFuzzy(..) | Why::Keyword(_)
            );
            let (want, why) = if (rules.proven_only && guess)
                || (solo && matches!(why, Why::CategoryExactMatch(_) | Why::CategoryFuzzy(..)))
            {
                guess_suppressed += 1;
                (None, Why::NoMatch)
            } else {
                (want, why)
            };
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
    if rules.proven_only && guess_suppressed > 0 {
        let _ = writeln!(
            trace,
            "proven-only: {guess_suppressed} guess-tier move(s) suppressed (category/keyword tiers held)"
        );
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
    // solo mode skips this: family-following is a proven pass, and "apply
    // user rules only" must not move anything a user rule didn't name.
    if !solo {
        // parent of each lineage-gated mod = longest contained other norm
        let mut parent_of: HashMap<String, String> = HashMap::new();
        for m in &all_mods {
            let nl = &m.lower;
            if !patch_flavored(nl) {
                continue;
            }
            if let Some(p) = all_mods
                .iter()
                .filter(|o| {
                    let (co, cm) = (cn(o), cn(m));
                    co != cm && co.len() >= 10 && cm.contains(&co) && !kw.is_never(&o.name, &m.name)
                })
                .max_by_key(|o| cn(o).len())
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

    // content parents (plugin-master relationships from the census) live
    // outside the pass 1c scope so pass 2's in-section ordering can also
    // see them - pulling a patch into its master's section is only half
    // the job, it must also sort AFTER the master inside that section.
    let mut content_parents: HashMap<String, Vec<String>> = HashMap::new();
    // which game? the census root esm answers it without mo2 metadata;
    // no census -> skyrim (pre-detection behavior). base masters, loot
    // folder, everything game-shaped keys off this one struct.
    let game = game::detect_or_skyrim(&{
        let mut v: Vec<String> = Vec::new();
        if let Some(census) = census {
            v.extend(census.iter().map(|(_, _, i)| i.plugin.clone()));
        }
        v
    });
    let _ = writeln!(trace, "game: {} (base masters: {})", game.name, game.base_masters.len());

    // loot masterlist (local file, no download): plugin-level "after" edges
    // become mod-level hard constraints via the census provider map.
    let loot_data = if solo { None } else { loot::load(&game) };
    let mut loot_after: HashMap<String, Vec<String>> = HashMap::new();

    // pass 1c: plugin-master family (content-based). modlist file order is
    // priority: EARLIER = wins = loads later. if mod A's plugin declares
    // mod B's plugin as a master in its TES4 header, A must not sort
    // file-LATER than B - that would load the addon before its master.
    // name-containment can't see short masters like "MLO2"; the plugin
    // header doesn't lie. base-game and
    // creation-club masters are exempt: their provider mods (a "Clean
    // Masters" container, say) sit wherever the user put them and must
    // not drag the entire list after themselves.
    if let Some(census) = census {
        let base_game: &[&str] = game.base_masters;
        // plugin filename -> providing mod (first provider wins)
        let mut provider: HashMap<&str, &str> = HashMap::new();
        for (mod_name, _, info) in census {
            provider.entry(info.plugin.as_str()).or_insert(mod_name);
        }
        // mod -> content parents: other mods whose plugins it depends on.
        // a mod can ship SEVERAL plugins with different masters (one census
        // row per plugin) - the parents are the UNION across rows, or the
        // last row silently drops the others.
        let mut cparents: HashMap<String, Vec<String>> = HashMap::new();
        for (mod_name, _, info) in census {
            let ps = cparents.entry(mod_name.clone()).or_default();
            for mp in info.masters.iter() {
                if base_game.contains(&mp.as_str()) || mp.starts_with("cc") {
                    continue;
                }
                if let Some(m) = provider.get(mp.as_str()) {
                    if *m != mod_name {
                        ps.push(m.to_string());
                    }
                }
            }
        }
        cparents.retain(|_, ps| {
            ps.sort();
            ps.dedup();
            !ps.is_empty()
        });
        // loot "after" edges: plugin A after plugin B => A's mod loads after
        // B's mod. every edge is human-verified in the masterlist, so no
        // patch-flavored gate and no hub exemption - loot afters are
        // deliberate, not inferred.
        if let Some(ld) = &loot_data {
            for (mod_name, _, info) in census {
                let plugin = info.plugin.to_lowercase();
                let Some(targets) = ld.after.get(&plugin) else { continue };
                let ps = loot_after.entry(mod_name.clone()).or_default();
                for t in targets {
                    if base_game.contains(&t.as_str()) || t.starts_with("cc") {
                        continue;
                    }
                    if let Some(m) = provider.get(t.as_str()) {
                        if *m != mod_name {
                            ps.push(m.to_string());
                        }
                    }
                }
            }
            loot_after.retain(|_, ps| {
                ps.sort();
                ps.dedup();
                !ps.is_empty()
            });
            let _ = writeln!(
                trace,
                "loot masterlist: {} ({} after-rule(s) apply to {} mod(s))",
                ld.path.display(),
                ld.after.len(),
                loot_after.len()
            );
        } else {
            let _ = writeln!(
                trace,
                "loot masterlist: not found - update LOOT's masterlist for Skyrim VR to enable"
            );
        }
        content_parents = cparents.clone();

        let esm_mods: std::collections::HashSet<String> = census
            .iter()
            .filter_map(|(m, _, info)| {
                if info.is_esm
                    || info.plugin.to_lowercase().ends_with(".esm")
                    || m.to_lowercase().ends_with(".esm")
                {
                    Some(m.clone())
                } else {
                    None
                }
            })
            .collect();

        if !cparents.is_empty() || !esm_mods.is_empty() {
            // CANONICAL LOAD ORDER: section file index 0 = loads first (top of file),
            // section file index n_secs-1 = loads last (bottom of file).
            let lsec_of: HashMap<&str, usize> = ml
                .sections
                .iter()
                .enumerate()
                .map(|(i, s)| (s.label.as_str(), i))
                .collect();

            let max_esm_sec = esm_mods
                .iter()
                .filter_map(|m| decisions.get(m))
                .map(|d| {
                    d.want
                        .as_deref()
                        .and_then(|w| lsec_of.get(w).copied())
                        .unwrap_or_else(|| lsec_of.get(d.from.as_str()).copied().unwrap_or(0))
                })
                .max()
                .unwrap_or(0);

            // final destination load-index per mod, recursively
            fn cresolve(
                name: &str,
                cparents: &HashMap<String, Vec<String>>,
                decisions: &HashMap<String, Decision>,
                lsec_of: &HashMap<&str, usize>,
                esm_mods: &std::collections::HashSet<String>,
                max_esm_sec: usize,
                memo: &mut HashMap<String, usize>,
                depth: usize,
            ) -> usize {
                if let Some(i) = memo.get(name) {
                    return *i;
                }
                if depth > 64 {
                    return 0;
                }
                let d = &decisions[name];
                let mut dest = d
                    .want
                    .as_ref()
                    .and_then(|w| lsec_of.get(w.as_str()).copied())
                    .unwrap_or_else(|| lsec_of.get(d.from.as_str()).copied().unwrap_or(0));
                if let Some(ps) = cparents.get(name) {
                    for p in ps {
                        if decisions.contains_key(p) {
                            let pi = cresolve(p, cparents, decisions, lsec_of, esm_mods, max_esm_sec, memo, depth + 1);
                            if pi > dest {
                                dest = pi;
                            }
                        }
                    }
                }
                if !esm_mods.contains(name) {
                    if max_esm_sec > dest {
                        dest = max_esm_sec;
                    }
                }
                memo.insert(name.to_string(), dest);
                dest
            }

            let mut memo: HashMap<String, usize> = HashMap::new();
            let names: Vec<String> = decisions.keys().cloned().collect();
            let mut pulled = 0usize;
            for name in &names {
                let anchored = matches!(decisions[name].why, Why::ExactRule | Why::OutputGuard)
                    || pinned.contains(name.as_str());
                if anchored {
                    continue;
                }
                let di = cresolve(name, &cparents, &decisions, &lsec_of, &esm_mods, max_esm_sec, &mut memo, 0);
                let cur = {
                    let d = &decisions[name];
                    d.want
                        .as_ref()
                        .and_then(|w| lsec_of.get(w.as_str()).copied())
                        .unwrap_or_else(|| lsec_of.get(d.from.as_str()).copied().unwrap_or(0))
                };
                if di > cur {
                    let target = ml.sections[di].label.clone();
                    if rules.dump.iter().any(|l| l.eq_ignore_ascii_case(&target)) {
                        continue;
                    }
                    let strongest = cparents
                        .get(name)
                        .and_then(|ps| {
                            ps.iter()
                                .filter(|p| decisions.contains_key(*p))
                                .max_by_key(|p| memo.get(*p).copied().unwrap_or(0))
                                .cloned()
                        })
                        .unwrap_or_else(|| "ESM master".to_string());
                    let _ = writeln!(
                        trace,
                        "  {name} | master constraint [{strongest}] loads later | dest -> [{target}]"
                    );
                    let d = decisions.get_mut(name).unwrap();
                    d.want = Some(target);
                    d.why = Why::FollowParent(strongest);
                    pulled += 1;
                }
            }
            let _ = writeln!(
                trace,
                "  plugin family: {pulled} mod(s) moved to load after their plugin masters"
            );
        }
    }

    // trace + collect the surviving moves
    let mut moves: Vec<(String, String, String)> = vec![]; // (mod name, from, to)
    let mut dump_blocked = 0usize;
    for m in &all_mods {
        let d = &decisions[&m.name];
        // a dump section is a waiting room: nothing ever moves INTO it
        if let Some(w) = &d.want {
            if *w != d.from && rules.dump.iter().any(|l| l.eq_ignore_ascii_case(w)) {
                let _ = writeln!(
                    trace,
                    "  {} | BLOCKED: [{}] is a dump section - staying in [{}]",
                    m.name, w, d.from
                );
                dump_blocked += 1;
                continue;
            }
        }
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
    // dump-section summary: how many waiting-room mods got filed out, and
    // how many nothing claimed (those need a rule, a category, or a manual
    // decision - the dump is not a home)
    for dl in &rules.dump {
        let Some(sec) = ml.sections.iter().find(|s| s.label.eq_ignore_ascii_case(dl)) else {
            continue;
        };
        let staying = sec.mods.iter().filter(|m| {
            let d = &decisions[&m.name];
            d.want.is_none() || d.want.as_deref() == Some(sec.label.as_str())
        }).count();
        let filed = sec.mods.len() - staying;
        let _ = writeln!(
            trace,
            "dump section [{}]: {filed} filed out, {staying} unclaimed{}",
            sec.label,
            if staying > 0 { " (no rule/category/keyword matched them)" } else { "" }
        );
        let _ = writeln!(
            log,
            "DUMP  [{}]: {filed} filed out, {staying} unclaimed",
            sec.label
        );
    }
    if dump_blocked > 0 {
        let _ = writeln!(trace, "dump sections blocked {dump_blocked} inbound move(s)");
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
        let (want, why) = if (rules.proven_only
            && matches!(
                why,
                Why::CategoryExactMatch(_) | Why::CategoryFuzzy(..) | Why::Keyword(_)
            ))
            || (solo && matches!(why, Why::CategoryExactMatch(_) | Why::CategoryFuzzy(..)))
        {
            (None, Why::NoMatch)
        } else {
            (want, why)
        };
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

    // =====================================================================
    // constraint engine: one canonical placement for every in-section rule.
    //
    // CANONICAL LOAD ORDER inside each section: index 0 = loads FIRST,
    // last index = loads last (wins in-section). every constraint reads
    // "X loads after Y" = X index > Y index. pins are absolute positions.
    // no code below this point may think in mo2 file order - the only
    // direction conversions are building `work` (reverse of file) and the
    // write-back at the end.
    //
    // application order: family grouping -> conflict-proven enforcement ->
    // promote -> sink/float -> PIN RESTORATION. pins are restored LAST so
    // that nothing - including another constraint's insertion - can shift
    // a pinned mod off its slot. "never move" means never.
    // =====================================================================
    // dedup for change rows: the fixed-point loop can re-fire a pass on
    // an oscillating pair; one row per (kind, name, detail) ever
    let mut seen_change: std::collections::HashSet<(ChangeKind, String, String)> =
        std::collections::HashSet::new();
    macro_rules! push_change {
        ($out:expr, $c:expr) => {{
            let c = $c;
            if seen_change.insert((c.kind, c.name.clone(), c.detail.clone())) {
                $out.push(c);
            }
        }};
    }
    for s in &mut ml.sections {
        let snapshot: Vec<ModEntry> = s.mods.clone(); // mo2 file order
        let n = snapshot.len();
        if n == 0 {
            continue;
        }

        // canonical working copy: load order = file order reversed.
        // orig_load[name] = original load-order index (for pins + reporting)
        let mut work: Vec<ModEntry> = snapshot.iter().rev().cloned().collect();
        let orig_load: HashMap<String, usize> =
            work.iter().enumerate().map(|(i, m)| (m.name.clone(), i)).collect();

        // parent relation (direction-free): census content parents beat
        // name guessing - "MCM Helper VR" contains nothing of "SkyUI - VR",
        // but its plugin header says who its master is. otherwise the
        // parent is the in-section mod with the LONGEST norm this mod's
        // name contains (longest = most specific, handles chains like
        // "mod" -> "mod - dlc addon" -> "mod - dlc addon - fix").
        let idx_of: HashMap<&str, usize> =
            work.iter().enumerate().map(|(i, m)| (m.name.as_str(), i)).collect();
        let mut parent: Vec<Option<usize>> = vec![None; n];
        for i in 0..n {
            if let Some(ps) = content_parents.get(&work[i].name) {
                if let Some(p) = ps.iter().find_map(|p| idx_of.get(p.as_str())) {
                    parent[i] = Some(*p);
                    continue;
                }
            }
            let mut best: Option<usize> = None;
            for j in 0..n {
                if i == j {
                    continue;
                }
                let (a, b) = (&work[i], &work[j]);
                let (ca, cb) = (cn(a), cn(b));
                if ca != cb
                    && cb.len() >= 10
                    && ca.contains(&cb)
                    && !kw.is_never(&a.name, &b.name)
                    && best.map(|k| cn(&work[k]).len() < cb.len()).unwrap_or(true)
                {
                    best = Some(j);
                }
            }
            parent[i] = best;
        }

        // family grouping: every family loads root FIRST, then its children
        // by increasing depth (a patch loads after its master, a patch's
        // patch after the patch). groups keep their original relative
        // placement. chain-walking is cycle-safe: containment strictly
        // shrinks, so chains can't loop.
        let mut depth_of: Vec<usize> = vec![0; n];
        let mut root_of: Vec<usize> = (0..n).collect();
        for i in 0..n {
            let mut depth = 0usize;
            let mut cur = i;
            while let Some(p) = parent[cur] {
                depth += 1;
                cur = p;
                if depth > n {
                    break; // paranoia
                }
            }
            depth_of[i] = depth;
            root_of[i] = cur;
        }
        // sort key, expressed in load terms: group by the root's original
        // load position (families with later-loading roots stay later),
        // parents before children, original position breaks ties.
        // solo mode skips this: family clustering is a proven pass, and an
        // "apply user rules only" write must leave every mod a user rule
        // didn't name exactly where it sits.
        if !solo {
            let mut order: Vec<usize> = (0..n).collect();
            order.sort_by_key(|&i| {
                (
                    std::cmp::Reverse(n - 1 - root_of[i]),
                    depth_of[i],
                    std::cmp::Reverse(orig_load[&work[i].name]),
                )
            });
            work = order.iter().map(|&i| work[i].clone()).collect();
        }


        // -----------------------------------------------------------------
        // SLOT SPACE. pins are absolute: a pinned mod occupies its original
        // in-section slot forever, and every other constraint works on the
        // unpinned subsequence `u`. the mapping: free[r] = absolute slot of
        // rank r in u; the first free slot past absolute slot s is
        // free[partition_point(free <= s)]. this matters because a child
        // constrained to load after a PINNED master can't just be inserted
        // after it in the full list - the merge would slide it into a gap
        // below the master's slot and break the constraint all over again.
        // all constraint passes run on u to a fixed point; pins never move.
        // -----------------------------------------------------------------
        let mut slot_owner: Vec<Option<ModEntry>> = vec![None; n];
        let mut u: Vec<ModEntry> = Vec::with_capacity(n);
        for m in work {
            if pinned.contains(m.name.as_str()) {
                let slot = orig_load[&m.name].min(n - 1);
                slot_owner[slot] = Some(m);
            } else {
                u.push(m);
            }
        }
        let free: Vec<usize> = (0..n).filter(|s| slot_owner[*s].is_none()).collect();
        let pinned_slot: HashMap<&str, usize> = slot_owner
            .iter()
            .enumerate()
            .filter_map(|(s, o)| o.as_ref().map(|m| (m.name.as_str(), s)))
            .collect();
        // absolute slot of a mod by name: pinned -> its fixed slot,
        // unpinned -> free[its rank in u]
        macro_rules! abs_pos {
            ($u:expr, $name:expr) => {
                if let Some(s) = pinned_slot.get($name) {
                    Some(*s)
                } else {
                    $u.iter().position(|m| m.name == $name).map(|r| free[r])
                }
            };
        }

        for _round in 0..6 {
            let round_start: Vec<String> = u.iter().map(|m| m.name.clone()).collect();

            // constraint: promote rules - winner loads after loser
            for (win, lose) in &rules.promote {
                let wi = u.iter().position(|m| m.name == *win);
                let li = u.iter().position(|m| m.name == *lose);
                if let (Some(wi), Some(li)) = (wi, li) {
                    let _ = writeln!(
                        trace,
                        "  [{label}] {win}@{wi} vs {lose}@{li} (load order, later wins): {verdict}",
                        label = s.label,
                        verdict = if wi < li { "PROMOTE winner" } else { "already correct" }
                    );
                    if wi < li {
                        let e = u.remove(wi);
                        let nl = u.iter().position(|m| m.name == *lose).unwrap();
                        u.insert(nl + 1, e);
                        changes += 1;
                        let _ = writeln!(log, "PROM  {win} loads after {lose}");
                        push_change!(out, Change {
                            kind: ChangeKind::Promote,
                            name: win.clone(),
                            detail: format!("loads after {lose}"),
                            section: s.label.clone(),
                        });
                    }
                }
            }

            // constraint: sink rules - mod loads FIRST among the unpinned
            // (top of the section in mo2's ui, loses to everything below)
            for (name, sec) in &rules.sink {
                if s.label != *sec {
                    continue;
                }
                let Some(i) = u.iter().position(|m| m.name == *name) else {
                    continue;
                };
                let _ = writeln!(
                    trace,
                    "  [{sec}] {name}@rank {i}: {verdict}",
                    verdict = if i == 0 { "already sunk" } else { "SINK to section top (ui)" }
                );
                if i != 0 {
                    let e = u.remove(i);
                    u.insert(0, e);
                    changes += 1;
                    let _ = writeln!(log, "SINK  {name} to top of [{sec}] (loses in-section)");
                    push_change!(out, Change {
                        kind: ChangeKind::Sink,
                        name: name.clone(),
                        detail: format!("top of [{sec}] - loses to everything below it"),
                        section: sec.clone(),
                    });
                }
            }

            // constraint: sectionless sinks - no-esp frameworks (runtime
            // patchers like skypatcher, mfg fix) are masters for other
            // mods but have no plugin for the census to prove it. they
            // load FIRST in whatever section holds them, no target needed.
            for name in &rules.sink_any {
                let Some(i) = u.iter().position(|m| m.name == *name) else {
                    continue;
                };
                let _ = writeln!(
                    trace,
                    "  [{label}] {name}@rank {i}: {verdict}",
                    label = s.label,
                    verdict = if i == 0 { "already sunk" } else { "SINK (framework, loads early)" }
                );
                if i != 0 {
                    let e = u.remove(i);
                    u.insert(0, e);
                    changes += 1;
                    let _ = writeln!(log, "SINK  {name} loads first in [{}] (framework)", s.label);
                    push_change!(out, Change {
                        kind: ChangeKind::Sink,
                        name: name.clone(),
                        detail: format!("top of [{}] - framework, loads early", s.label),
                        section: s.label.clone(),
                    });
                }
            }

            // constraint: float rules - mirror of sink: loads LAST
            for (name, sec) in &rules.float {
                if s.label != *sec {
                    continue;
                }
                let Some(i) = u.iter().position(|m| m.name == *name) else {
                    continue;
                };
                let _ = writeln!(
                    trace,
                    "  [{sec}] {name}@rank {i}: {verdict}",
                    verdict = if i == u.len() - 1 { "already floated" } else { "FLOAT to section bottom (ui)" }
                );
                if i != u.len() - 1 {
                    let e = u.remove(i);
                    u.push(e);
                    changes += 1;
                    let _ = writeln!(log, "FLOT  {name} to bottom of [{sec}] (wins in-section)");
                    push_change!(out, Change {
                        kind: ChangeKind::Float,
                        name: name.clone(),
                        detail: format!("bottom of [{sec}] - wins everything in-section"),
                        section: sec.clone(),
                    });
                }
            }

            // constraint: content-parent DAG. a child must load after ALL
            // of its plugin-master parents in this section - including
            // PINNED parents, whose slots are fixed: the child goes to the
            // first free slot past the latest parent's slot.
            if !content_parents.is_empty() || !loot_after.is_empty() {
                for _ in 0..6 {
                    let mut moved = false;
                    for ci in 0..u.len() {
                        let name = u[ci].name.clone();
                        let cps = content_parents.get(&name);
                        let lps2 = loot_after.get(&name);
                        if cps.is_none() && lps2.is_none() {
                            continue;
                        }
                        let latest_parent_slot = cps
                            .into_iter()
                            .chain(lps2)
                            .flat_map(|v| v.iter())
                            .filter_map(|p| abs_pos!(&u, p.as_str()))
                            .max();
                        let Some(lps) = latest_parent_slot else { continue };
                        let child_slot = free[ci.min(free.len().saturating_sub(1))];
                        if child_slot < lps {
                            let e = u.remove(ci);
                            let r = free.partition_point(|f| *f <= lps);
                            u.insert(r.min(u.len()), e);
                            moved = true;
                            let src = if lps2.is_some() && cps.is_none() { "loot" } else { "content DAG" };
                            let _ = writeln!(
                                trace,
                                "  [{label}] {name} | {src}: loads after all parents (slot {child_slot} -> past {lps})",
                                label = s.label
                            );
                        }
                    }
                    if !moved {
                        break;
                    }
                }
            }

            // constraint: ESM before non-ESM in-section
            if census.is_some() {
                for _ in 0..6 {
                    let mut moved = false;
                    let latest_esm_slot = (0..n)
                        .filter(|slot| {
                            let m_name = if let Some(p) = &slot_owner[*slot] {
                                Some(&p.name)
                            } else {
                                let r = free.iter().position(|f| *f == *slot);
                                r.and_then(|idx| u.get(idx)).map(|m| &m.name)
                            };
                            m_name.map_or(false, |nm| is_esm_entry(nm, census))
                        })
                        .max();
                    if let Some(les) = latest_esm_slot {
                        for ci in 0..u.len() {
                            if !is_esm_entry(&u[ci].name, census) {
                                let child_slot = free[ci.min(free.len().saturating_sub(1))];
                                if child_slot < les {
                                    let e = u.remove(ci);
                                    let name = e.name.clone();
                                    let r = free.partition_point(|f| *f <= les);
                                    u.insert(r.min(u.len()), e);
                                    moved = true;
                                    let _ = writeln!(
                                        trace,
                                        "  [{label}] {name} | ESM DAG: non-ESM loads after ESM (slot {child_slot} -> past {les})",
                                        label = s.label
                                    );
                                    break;
                                }
                            }
                        }
                    }
                    if !moved {
                        break;
                    }
                }
            }

            // constraint: conflict-proven enforcement. for mod pairs that
            // share real files (conflict.ini), exactly one patch-flavored:
            // the patch MUST win those files, so it loads after the base.
            if let Some(ci) = conflicts {
                let mut enforce: Vec<(String, String, u32)> = Vec::new();
                for i in 0..u.len() {
                    for j in (i + 1)..u.len() {
                        let (a, b) = (&u[i], &u[j]);
                        // unticked mods can't conflict - mo2 doesn't load them
                        if !a.raw.trim_start().starts_with('+')
                            || !b.raw.trim_start().starts_with('+')
                        {
                            continue;
                        }
                        let Some((_w, shared)) = ci.shared(&a.name, &b.name) else { continue };
                        if shared < conflicts::MIN_SHARED {
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
                            "  [{label}] {patch} vs {base}: {shared} shared files -> patch must load after",
                            label = s.label,
                            patch = patch.name,
                            base = base.name
                        );
                        enforce.push((patch.name.clone(), base.name.clone(), shared));
                    }
                }
                for _ in 0..6 {
                    let mut moved = false;
                    for (patch, base, shared) in &enforce {
                        let (Some(pslot), Some(bslot)) =
                            (abs_pos!(&u, patch.as_str()), abs_pos!(&u, base.as_str()))
                        else {
                            continue;
                        };
                        if pslot < bslot {
                            // patch loads BEFORE the base - it would lose
                            // the very files it exists to win. move it to
                            // the first free slot past the base.
                            let pi = u.iter().position(|m| &m.name == patch).unwrap();
                            let e = u.remove(pi);
                            let r = free.partition_point(|f| *f <= bslot);
                            u.insert(r.min(u.len()), e);
                            changes += 1;
                            moved = true;
                            let _ = writeln!(
                                log,
                                "REOR  {patch} loads after {base} (conflict-proven: {shared} shared files)"
                            );
                            push_change!(out, Change {
                                kind: ChangeKind::Reorder,
                                name: patch.clone(),
                                detail: format!(
                                    "conflict-proven: shares {shared} files with {base}, patch must win"
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

            if u.iter().map(|m| m.name.as_str()).eq(round_start.iter().map(|n| n.as_str())) {
                break; // fixed point: every constraint satisfied simultaneously
            }
        }

        // reassemble: pinned mods at their fixed slots, u fills the gaps
        let mut work: Vec<ModEntry> = Vec::with_capacity(n);
        {
            let mut it = u.into_iter();
            for slot in 0..n {
                if let Some(p) = slot_owner[slot].take() {
                    work.push(p);
                } else if let Some(m) = it.next() {
                    work.push(m);
                }
            }
            work.extend(it); // paranoia: never lose a mod
        }

        // write back: load order -> mo2 file order (the second and final
        // direction conversion in this function)
        s.mods = work.into_iter().rev().collect();

        // report every mod whose in-section position changed, with the
        // parent and depth so the trace shows exactly why it moved
        let mut traced_header = false;
        for (new_fi, m) in s.mods.iter().enumerate() {
            let old_fi = snapshot.iter().position(|o| o.name == m.name).unwrap();
            if old_fi != new_fi {
                changes += 1;
                let _ = writeln!(log, "REOR  {}   [{}]", m.name, s.label);
                out.push(Change {
                    kind: ChangeKind::Reorder,
                    name: m.name.clone(),
                    detail: format!("within [{}]", s.label),
                    section: s.label.clone(),
                });
                if !traced_header {
                    let _ = writeln!(trace, "\n--- constraint placement [{}] ---", s.label);
                    traced_header = true;
                }
                let li = n - 1 - new_fi; // final load index
                let old_li = n - 1 - old_fi;
                let parent_name = parent[old_li].map(|p| snapshot[n - 1 - p].name.as_str());
                let _ = writeln!(
                    trace,
                    "  {} | parent {} | depth {} | load index {} -> {} (loads later = wins)",
                    m.name,
                    parent_name.unwrap_or("<none>"),
                    depth_of[old_li],
                    old_li,
                    li
                );
            }
        }
    }

    // =====================================================================
    // audit gate: the order is FINAL now - user pins, family pulls,
    // constraints, everything. these passes verify the output against the
    // invariants and report violations; they never move anything. if the
    // engine ever violates its own rules, that is a sorter bug and these
    // rows are how it surfaces.
    // =====================================================================

    // gate 1: conflict audit. every family pair (containment + patch-flavored
    // child) that shares real files must have the child LOADING AFTER the
    // parent - verified against the final positions, not the stale scan.
    if let Some(ci) = conflicts {
        let _ = writeln!(trace, "\n--- audit gate: conflict winners ---");
        let mut fails = 0usize;
        for s in &ml.sections {
            let n = s.mods.len();
            for i in 0..n {
                for j in (i + 1)..n {
                    let (a, b) = (&s.mods[i], &s.mods[j]);
                    // unticked mods can't conflict - mo2 doesn't load them
                    if !a.raw.trim_start().starts_with('+')
                        || !b.raw.trim_start().starts_with('+')
                    {
                        continue;
                    }
                    // family = containment + patch-flavored child
                    // (canonical names via keywords.ini; [never] pairs veto)
                    if kw.is_never(&a.name, &b.name) {
                        continue;
                    }
                    let (ca, cb) = (cn(a), cn(b));
                    let (child, parent_m) = if ca.len() >= 10 && cb.contains(&ca) {
                        (a, b)
                    } else if cb.len() >= 10 && ca.contains(&cb) {
                        (b, a)
                    } else {
                        continue;
                    };
                    if !patch_flavored(&child.lower) {
                        continue;
                    }
                    let Some((_w, shared)) = ci.shared(&child.name, &parent_m.name) else { continue };
                    if shared < conflicts::MIN_SHARED {
                        continue;
                    }
                    // final positions, load terms: child file index must be
                    // SMALLER (file-earlier = loads later = wins)
                    let ci_fi = i.min(j);
                    let child_fi = if child.name == a.name { i } else { j };
                    if child_fi != ci_fi {
                        fails += 1;
                        let _ = writeln!(
                            trace,
                            "  AUDIT FAIL [{label}] {parent} loads after its own patch {child} ({shared} shared files at stake)",
                            label = s.label,
                            parent = parent_m.name,
                            child = child.name
                        );
                        let _ = writeln!(
                            log,
                            "WARN  {parent} loads after its patch {child} on {shared} shared files - needs a manual look",
                            parent = parent_m.name,
                            child = child.name
                        );
                    }
                }
            }
        }
        let _ = writeln!(trace, "  conflict audit complete: {fails} violation(s)");
    }

    // gate 2: master-order audit. if the final layout loads a mod before
    // the mod providing its plugin's master, that's a broken relationship
    // even when a pin caused it - a pin is a hard veto, but it is no
    // longer SILENT: the user gets a WARN row, not our blessing.
    if let Some(census) = census {
        let base_game: &[&str] = game.base_masters;
        let mut provider: HashMap<&str, &str> = HashMap::new();
        for (mod_name, _, info) in census {
            provider.entry(info.plugin.as_str()).or_insert(mod_name);
        }
        // final position in CANONICAL LOAD ORDER: name -> (load section
        // index, load index in section).
        let mut pos: HashMap<&str, (usize, usize)> = HashMap::new();
        for (si, s) in ml.sections.iter().enumerate() {
            let sn = s.mods.len();
            for (mi, m) in s.mods.iter().enumerate() {
                if m.raw.trim_start().starts_with('+') {
                    pos.insert(m.name.as_str(), (si, sn - 1 - mi));
                }
            }
        }
        let _ = writeln!(trace, "\n--- audit gate: master order ---");
        let mut fails = 0usize;
        let mut forced = 0usize;
        let mut reported: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for (mod_name, _, info) in census {
            let Some(&p1) = pos.get(mod_name.as_str()) else { continue };
            // strongest master = latest-loading provider mod
            let strongest = info
                .masters
                .iter()
                .filter(|mp| !base_game.contains(&mp.as_str()) && !mp.starts_with("cc"))
                .filter_map(|mp| provider.get(mp.as_str()))
                .filter(|pm| **pm != mod_name.as_str())
                .filter_map(|pm| pos.get(*pm).map(|p2| (*pm, *p2)))
                .max_by_key(|(_, p2)| *p2);
            let Some((master_mod, p2)) = strongest else { continue };
            let inverted = p1 < p2;
            if inverted && reported.insert(mod_name.as_str()) {
                fails += 1;
                let by_pin = pinned.contains(mod_name.as_str()) || pinned.contains(master_mod);
                if by_pin {
                    forced += 1;
                }
                let _ = writeln!(
                    trace,
                    "  AUDIT FAIL {mod_name} loads before its master {master_mod} (load {:?} < {:?}){}",
                    p1, p2,
                    if by_pin { " [forced by user pin]" } else { "" }
                );
                let _ = writeln!(
                    log,
                    "WARN  {mod_name} loads before its master {master_mod}{}",
                    if by_pin { " (forced by user pin)" } else { "" }
                );
                out.push(Change {
                    kind: ChangeKind::Warn,
                    name: mod_name.clone(),
                    detail: format!(
                        "[master order] loads before its master {master_mod}{}",
                        if by_pin { " (forced by user pin)" } else { "" }
                    ),
                    section: ml.sections[p1.0].label.clone(),
                });
            }
        }

        // ESM vs non-ESM flag audit
        for (mod_name, _, _) in census {
            if is_esm_entry(mod_name, Some(census)) {
                continue;
            }
            let Some(&p1) = pos.get(mod_name.as_str()) else { continue };
            let latest_esm = census
                .iter()
                .map(|(m, _, _)| m.as_str())
                .filter(|m| is_esm_entry(m, Some(census)) && *m != mod_name.as_str())
                .filter_map(|m| pos.get(m).map(|p2| (m, *p2)))
                .max_by_key(|(_, p2)| *p2);
            if let Some((esm_mod, p2)) = latest_esm {
                if p1 < p2 && reported.insert(mod_name.as_str()) {
                    fails += 1;
                    let by_pin = pinned.contains(mod_name.as_str()) || pinned.contains(esm_mod);
                    if by_pin {
                        forced += 1;
                    }
                    let _ = writeln!(
                        trace,
                        "  AUDIT FAIL non-ESM {mod_name} loads before ESM {esm_mod} (load {:?} < {:?}){}",
                        p1, p2,
                        if by_pin { " [forced by user pin]" } else { "" }
                    );
                    let _ = writeln!(
                        log,
                        "WARN  non-ESM {mod_name} loads before ESM {esm_mod}{}",
                        if by_pin { " (forced by user pin)" } else { "" }
                    );
                    out.push(Change {
                        kind: ChangeKind::Warn,
                        name: mod_name.clone(),
                        detail: format!(
                            "[ESM order] non-ESM loads before ESM {esm_mod}{}",
                            if by_pin { " (forced by user pin)" } else { "" }
                        ),
                    section: ml.sections[p1.0].label.clone(),
                    });
                }
            }
        }
        let _ = writeln!(
            trace,
            "  master-order audit complete: {fails} violation(s) ({forced} forced by user pins)"
        );
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
                None => match fuzzy_match_section(name, &ml.sections, false) {
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
    // crash forensics: any panic anywhere gets dumped to a log next to the exe,
    // so a silent window-vanish still leaves us a trail.
    std::panic::set_hook(Box::new(|info| {
        let loc = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown".to_string());
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic".to_string());
        let bt = std::backtrace::Backtrace::force_capture();
        let body = format!(
            "=== modslut v{VERSION} panic ===\n{msg}\nlocation: {loc}\n\nbacktrace:\n{bt}\n"
        );
        // next to the exe; fall back to temp dir; last resort: stderr
        let mut path = env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("modslut_crash.log")));
        if path.is_none() {
            path = Some(env::temp_dir().join("modslut_crash.log"));
        }
        if let Some(p) = &path {
            use std::io::Write as _;
            let _ = fs::write(p, &body);
            // append a marker line in case of repeated crashes
            if let Ok(mut f) = fs::OpenOptions::new().append(true).open(p) {
                let _ = writeln!(f, "--- end panic (modslut v{VERSION}) ---\n");
            }
        }
        eprintln!("{body}");
    }));

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
    // --user-rules-only: built-in cascade off, guess tiers off - only the
    // user's own rules plus proven data (loot/conflict/census/family) act
    let user_rules_only = args.iter().any(|a| a == "--user-rules-only");
    let (mut rules, user_files) =
        load_rules_for(opt("-r").as_deref(), Some(Path::new(file)), user_rules_only);
    if user_rules_only {
        rules.proven_only = true;
        eprintln!("user-rules-only mode: built-in rules skipped, guess tiers suppressed");
    }
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
            if let Some(ci) = ConflictIndex::load_checked(&ini, &mods_dir) {
                eprintln!("conflict index: {} pair(s) from {}", ci.pairs.len(), ini.display());
                return Some(ci);
            }
            eprintln!("conflict cache belongs to a different instance - rescanning");
        }
        {
            eprintln!("scanning mod files for conflicts (cached in conflict.ini after)…");
            let ci = ConflictIndex::build(&mods_dir, &active_mods(&ml));
            eprintln!(
                "indexed {} files, {} related pair(s)",
                ci.files_indexed,
                ci.pairs.len()
            );
            if let Err(e) = ci.save(&ini, &mods_dir) {
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

    // hidden debugging aid: --census-log <debug_sort.log> replays a user's
    // plugin census from a posted log, so a reported sort can be reproduced
    // exactly without their mods folder. format per line:
    //   "  plugin.esp (Mod Name) esm esl | masters: a.esp, b.esp | GRUP:n"
    let census: Option<Vec<(String, String, crate::plugins::PluginInfo)>> =
        opt("--census-log").and_then(|p| {
            let text = fs::read_to_string(p).ok()?;
            let mut out = Vec::new();
            for line in text.lines() {
                let l = line.trim_start();
                let Some(paren) = l.find(" (") else { continue };
                let Some(close) = l[paren..].find(") ") else { continue };
                let plugin = l[..paren].to_string();
                let mod_name = l[paren + 2..paren + close].to_string();
                let rest = &l[paren + close + 2..];
                let Some(mpos) = rest.find("masters: ") else { continue };
                let after = &rest[mpos + 9..];
                let masters_str = after.split('|').next().unwrap_or("").trim();
                let masters = if masters_str.is_empty() || masters_str == "-" {
                    vec![]
                } else {
                    masters_str.split(',').map(|s| s.trim().to_string()).collect()
                };
                let flags = &rest[..mpos];
                out.push((
                    mod_name,
                    String::new(),
                    crate::plugins::PluginInfo {
                        plugin,
                        masters,
                        is_esm: flags.contains("esm"),
                        is_esl: flags.contains("esl"),
                        record_count: 0,
                        groups: vec![],
                    },
                ));
            }
            eprintln!("census replay: {} plugin(s) from log", out.len());
            (!out.is_empty()).then_some(out)
        });

    // everything happens in memory first - the plan gets printed either way
    let mut log = String::new();
    let mut plan = Vec::new();
    let mut trace = String::new();
    let (kw, kw_files) = Keywords::load(Some(Path::new(file)));
    for f in &kw_files {
        eprintln!("keywords: {}", f.display());
    }
    let n = run(
        &mut ml,
        &rules,
        &mut log,
        &mut plan,
        cats.as_ref(),
        conflicts.as_ref(),
        census.as_deref(),
        &kw,
        false,
        &mut trace,
    );
    print!("{log}");
    println!("\n{n} suggested change(s)");

    // --user-rules-apply: solo run - print what ONLY the user's rules would
    // do (no write; this is the cli view of the gui's "apply user rules
    // only" button)
    if args.iter().any(|a| a == "--user-rules-apply") {
        let mut ml2 = parse(&text);
        let (urules, ufiles) = load_rules_for(None, Some(Path::new(file)), true);
        if ufiles.is_empty() {
            println!("\nno modslut_rules.txt found - nothing of yours to apply");
            return ExitCode::SUCCESS;
        }
        let mut log2 = String::new();
        let mut plan2 = Vec::new();
        let mut trace2 = String::new();
        let n2 = run(
            &mut ml2,
            &urules,
            &mut log2,
            &mut plan2,
            cats.as_ref(),
            None,
            None,
            &kw,
            true,
            &mut trace2,
        );
        print!("{log2}");
        println!("\n{n2} user-rule change(s) (solo run - nothing written)");
        return ExitCode::SUCCESS;
    }

    // --trace writes the full diagnostic to debug_sort.log next to the exe
    if args.iter().any(|a| a == "--trace") {
        let tp = debug_log_path();
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

    fn kw_fixture() -> Keywords {
        let mut kw = Keywords::default();
        Keywords::parse_into(
            "[strip]\nnoise = se, sse, ae, vr, edition, special\n\
             [alias]\nUnofficial Skyrim Special Edition Patch = ussep\n\
             SSE Engine Fixes = sse engine fixes skse64 plugin, engine fixes part 1\n\
             [family]\nPapyrusUtil = PapyrusUtil VR, PapyrusUtil SE\n\
             [never]\nCBBE = 3BA, BHUNP\nETHEREAL COSMOS = ETHEREAL CLOUDS\n",
            &mut kw,
        );
        kw
    }

    #[test]
    fn kw_alias_exact_hit() {
        let kw = kw_fixture();
        assert_eq!(kw.canonical("USSEP"), "unofficial skyrim patch");
    }

    #[test]
    fn kw_alias_substitutes_inside_longer_name() {
        let kw = kw_fixture();
        let c = kw.canonical("USSEP - Whatever Patch");
        // variant substituted in place, child keeps its own identity
        assert!(c.contains("unofficial skyrim patch"), "got {c}");
        assert!(c.contains("whatever patch"), "got {c}");
    }

    #[test]
    fn kw_alias_enables_containment() {
        let kw = kw_fixture();
        let child = norm(&kw.canonical("USSEP - Whatever Patch"));
        let parent = norm(&kw.canonical("Unofficial Skyrim Special Edition Patch"));
        assert!(child != parent && parent.len() >= 10 && child.contains(&parent));
    }

    #[test]
    fn kw_family_siblings_share_order_identity() {
        let kw = kw_fixture();
        assert_eq!(kw.canonical("PapyrusUtil VR"), kw.canonical("PapyrusUtil SE"));
    }

    #[test]
    fn kw_never_vetoes_both_directions() {
        let kw = kw_fixture();
        assert!(kw.is_never("CBBE Body Slide", "3BA Amazing Body"));
        assert!(kw.is_never("3BA Amazing Body", "CBBE Body Slide"));
        assert!(kw.is_never("ETHEREAL COSMOS - Special Edition", "ETHEREAL CLOUDS - Special Edition"));
        assert!(!kw.is_never("CBBE Body Slide", "CBBE Outfits"));
    }

    #[test]
    fn kw_empty_is_passthrough() {
        let kw = Keywords::default();
        assert_eq!(kw.canonical("Some Mod SE"), "some mod se"); // no strip entries loaded
        assert!(!kw.is_never("CBBE", "3BA"));
    }

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
        assert_eq!(fuzzy_match_section("landscape", &s, true), Some("Landscape"));
    }

    #[test]
    fn fuzzy_shared_token() {
        let s = secs(&["Landscape and Environment", "Audio"]);
        // nexus category "Environment" should land in the combined separator
        assert_eq!(
            fuzzy_match_section("Environment", &s, true),
            Some("Landscape and Environment")
        );
    }

    #[test]
    fn fuzzy_ignores_stopwords_and_short_tokens() {
        let s = secs(&["Models and Textures"]);
        assert_eq!(fuzzy_match_section("Mods for the AI", &s, true), None);
        assert_eq!(
            fuzzy_match_section("Textures", &s, true),
            Some("Models and Textures")
        );
    }

    #[test]
    fn fuzzy_no_match_returns_none() {
        let s = secs(&["Audio Overhaul"]);
        assert_eq!(fuzzy_match_section("Combat", &s, true), None);
    }

    #[test]
    fn fuzzy_generic_token_alone_is_not_evidence() {
        // "Bug Fixes" sharing only "fixes" with a weapons section = junk drawer
        let s = secs(&["Weapons, Armour, Clothing, and Clutter Fixes"]);
        assert_eq!(fuzzy_match_section("Bug Fixes", &s, true), None);
        let s = secs(&["Soul Trap Management Overhauls"]);
        assert_eq!(fuzzy_match_section("Overhauls", &s, true), None);
    }

    #[test]
    fn fuzzy_distinctive_token_beats_generic() {
        // "bug" is distinctive: the bug-fix section beats the weapons section
        let s = secs(&["Weapons, Armour, Clothing, and Clutter Fixes", "Essential Bug Fixes"]);
        assert_eq!(
            fuzzy_match_section("Bug Fixes", &s, true),
            Some("Essential Bug Fixes")
        );
    }

    #[test]
    fn fuzzy_qualifier_narrows_section_away() {
        // the category doesn't say "male", so the male-only section is out
        let s = secs(&["Male Body Additions", "Skin & Body"]);
        assert_eq!(
            fuzzy_match_section("Body, Face, and Hair", &s, true),
            Some("Skin & Body")
        );
        // but a category that DOES say male can land there
        assert_eq!(
            fuzzy_match_section("Male Body", &s, true),
            Some("Male Body Additions")
        );
    }

    #[test]
    fn concept_exact_label_wins() {
        let s = secs(&["Lighting", "Lux (Lighting)"]);
        assert_eq!(resolve_concept("Lighting", &s).unwrap().0, "Lighting");
    }

    #[test]
    fn concept_containment_finds_family_sep() {
        let s = secs(&["Lux (Lighting)", "Gameplay"]);
        assert_eq!(resolve_concept("Lighting", &s).unwrap().0, "Lux (Lighting)");
    }

    #[test]
    fn containment_prefers_closest_label_not_first() {
        // both contain the concept; the shorter (closest) one must win,
        // regardless of list order
        let s = secs(&["Special Load After Lighting Mods", "Lux (Lighting)"]);
        assert_eq!(resolve_concept("Lighting", &s).unwrap().0, "Lux (Lighting)");
        let s = secs(&["Male Body Additions", "Skin & Body", "OBody and Bodyslide Presets"]);
        assert_eq!(resolve_concept("Body", &s).unwrap().0, "Skin & Body");
        let s = secs(&["Interface - Controller Bindings", "Interface - Menus", "Interface - VR Specific"]);
        assert_eq!(resolve_concept("Interface", &s).unwrap().0, "Interface - Menus");
    }

    #[test]
    fn concept_multiword_hits_word_sequence() {
        let s = secs(&["Interface - VR Specific", "VR Controller Bindings"]);
        assert_eq!(resolve_concept("VR Specific", &s).unwrap().0, "Interface - VR Specific");
    }

    #[test]
    fn plural_sep_beats_long_compound() {
        let s = secs(&["Extension Frameworks - Animation & Behavior Engine", "Animations"]);
        assert_eq!(resolve_concept("Animation", &s).unwrap().0, "Animations");
    }

    #[test]
    fn rename_gates_label_silence() {
        // a label that already says the concept (any shared word) is not
        // silent - renaming it would be noise
        assert!(shared_stem("Skin & Body", "Body, Face, and Hair", 3));
        assert!(shared_stem("Expanded Cities, Towns, and Villages", "Cities, Towns, Villages, and Hamlets", 3));
        assert!(shared_stem("Unofficial Skyrim Modders Patch Emporium", "Modders Resources", 3));
        assert!(!shared_stem("Ya Filthy Animal", "nsfw", 3));
        assert!(!shared_stem("Animals", "Creatures and Mounts", 3));
        // concept coverage: every content word must appear in the label
        assert!(label_has_concept("Lux (Lighting)", "lighting"));
        assert!(!label_has_concept("Skin & Body", "Body, Face, and Hair"));
        assert!(label_has_concept("Animations", "animation")); // plural stem
    }

    #[test]
    fn concept_reverse_containment() {
        // concept is more specific than the sep: "Interface - VR Specific"
        // should land in a plain "Interface" separator
        let s = secs(&["Interface"]);
        assert_eq!(
            resolve_concept("Interface - VR Specific", &s).unwrap().0,
            "Interface"
        );
    }

    #[test]
    fn concept_unmatched_noops() {
        let s = secs(&["Gameplay", "Textures"]);
        assert!(resolve_concept("NSFW", &s).is_none());
        assert!(resolve_concept("Unofficial Patches", &s).is_none());
    }

    #[test]
    fn concept_short_targets_dont_containment_match() {
        // "VR" (2 chars) must not containment-match random separators
        let s = secs(&["VR Controller Bindings"]);
        assert_eq!(
            resolve_concept("VR", &s).map(|(l, _)| l),
            Some("VR Controller Bindings") // tokens tier is fine
        );
        let s2 = secs(&["Overhauls"]);
        assert!(resolve_concept("VR", &s2).is_none());
    }

    #[test]
    fn resolve_targets_drops_dead_rules_keeps_promote() {
        let s = secs(&["Gameplay"]);
        let mut r = Rules::empty();
        r.exact.push(("Some Mod".into(), "Nonexistent Section".into()));
        r.exact.push(("Other Mod".into(), "gameplay".into())); // norm match
        r.promote.push(("A VR".into(), "A".into()));
        let mut trace = String::new();
        let out = resolve_targets(&r, &s, &mut trace);
        assert_eq!(out.exact.len(), 1);
        assert_eq!(out.exact[0].0, "Other Mod");
        assert_eq!(out.exact[0].1, "Gameplay");
        assert_eq!(out.promote.len(), 1); // promote rhs is a mod name - untouched
        assert!(trace.contains("no matching separator"));
    }

    #[test]
    fn master_dependency_enforced_non_patch_flavored() {
        let text = "# modlist\n+DependentMod\n-Gameplay_separator\n+MasterMod\n-Essentials_separator\n";
        let mut ml = parse(text);
        let rules = Rules::empty();
        let mut log = String::new();
        let mut out = Vec::new();
        let mut trace = String::new();
        let kw = Keywords::default();
        let census = vec![
            (
                "DependentMod".to_string(),
                "Gameplay".to_string(),
                crate::plugins::PluginInfo {
                    plugin: "standalone_plugin.esp".to_string(), // non-patch-flavored name!
                    masters: vec!["master.esm".to_string()],
                    is_esm: false,
                    is_esl: false,
                    record_count: 5,
                    groups: vec![],
                },
            ),
            (
                "MasterMod".to_string(),
                "Essentials".to_string(),
                crate::plugins::PluginInfo {
                    plugin: "master.esm".to_string(),
                    masters: vec![],
                    is_esm: true,
                    is_esl: false,
                    record_count: 10,
                    groups: vec![],
                },
            ),
        ];

        run(
            &mut ml,
            &rules,
            &mut log,
            &mut out,
            None,
            None,
            Some(&census),
            &kw,
            false,
            &mut trace,
        );

        // DependentMod must be shifted down into or after Essentials_separator so MasterMod loads first!
        let dep_sec = ml.sections.iter().find(|s| s.mods.iter().any(|m| m.name == "DependentMod")).unwrap();
        assert_eq!(dep_sec.label, "Essentials");
    }

    #[test]
    fn esm_loads_before_non_esm() {
        let text = "# modlist\n+NonEsmMod\n+EsmMod\n-Section_separator\n";
        let mut ml = parse(text);
        let rules = Rules::empty();
        let mut log = String::new();
        let mut out = Vec::new();
        let mut trace = String::new();
        let kw = Keywords::default();
        let census = vec![
            (
                "NonEsmMod".to_string(),
                "Section".to_string(),
                crate::plugins::PluginInfo {
                    plugin: "plugin.esp".to_string(),
                    masters: vec![],
                    is_esm: false,
                    is_esl: false,
                    record_count: 5,
                    groups: vec![],
                },
            ),
            (
                "EsmMod".to_string(),
                "Section".to_string(),
                crate::plugins::PluginInfo {
                    plugin: "master.esm".to_string(),
                    masters: vec![],
                    is_esm: true,
                    is_esl: false,
                    record_count: 10,
                    groups: vec![],
                },
            ),
        ];

        run(
            &mut ml,
            &rules,
            &mut log,
            &mut out,
            None,
            None,
            Some(&census),
            &kw,
            false,
            &mut trace,
        );

        let sec = &ml.sections[0];
        // In MO2 file order: bottom of section (sec.mods[1]) loads FIRST.
        // So EsmMod (loads first) is at sec.mods[1], NonEsmMod (loads later) is at sec.mods[0].
        assert_eq!(sec.mods[1].name, "EsmMod");
        assert_eq!(sec.mods[0].name, "NonEsmMod");
    }

    #[test]
    fn user_pin_overrides_master_order_and_warns() {
        let text = "# modlist\n+DependentMod\n-Gameplay_separator\n+MasterMod\n-Essentials_separator\n";
        let mut ml = parse(text);
        let mut rules = Rules::empty();
        // User explicitly pins DependentMod to Gameplay
        rules.exact.push(("DependentMod".to_string(), "Gameplay".to_string()));

        let mut log = String::new();
        let mut out = Vec::new();
        let mut trace = String::new();
        let kw = Keywords::default();
        let census = vec![
            (
                "DependentMod".to_string(),
                "Gameplay".to_string(),
                crate::plugins::PluginInfo {
                    plugin: "standalone_plugin.esp".to_string(),
                    masters: vec!["master.esm".to_string()],
                    is_esm: false,
                    is_esl: false,
                    record_count: 5,
                    groups: vec![],
                },
            ),
            (
                "MasterMod".to_string(),
                "Essentials".to_string(),
                crate::plugins::PluginInfo {
                    plugin: "master.esm".to_string(),
                    masters: vec![],
                    is_esm: true,
                    is_esl: false,
                    record_count: 10,
                    groups: vec![],
                },
            ),
        ];

        run(
            &mut ml,
            &rules,
            &mut log,
            &mut out,
            None,
            None,
            Some(&census),
            &kw,
            false,
            &mut trace,
        );

        // DependentMod stayed in Gameplay due to user pin
        let dep_sec = ml.sections.iter().find(|s| s.mods.iter().any(|m| m.name == "DependentMod")).unwrap();
        assert_eq!(dep_sec.label, "Gameplay");

        // A warning change must be output
        let has_warn = out.iter().any(|c| c.kind == ChangeKind::Warn && c.detail.contains("loads before its master"));
        assert!(has_warn, "Expected warning for master order violation caused by user pin");
    }
}

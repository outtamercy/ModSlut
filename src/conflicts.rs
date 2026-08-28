// conflict index - computes what mo2's conflicts tab shows, without mo2.
// walks every active mod's folder, indexes relative file paths, and records
// for each pair of mods how many files they share and who wins them (the
// winner is always the higher-priority mod, so pairs store winner>loser).
//
// the result is cached per-profile in conflict.ini next to modlist.txt:
//   - regenerated when missing, when modlist.txt is newer, or when the mods/
//     root folder is newer (mod installed/removed). delete it to force a rescan.
//   - format: one "Winner > Loser = <shared file count>" line per pair.

use rayon::prelude::*;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use walkdir::WalkDir;

// a file provided by more mods than this is something weird (empty bsa
// placeholder, folder marker) - not decision-grade conflict data
const MAX_PROVIDERS: usize = 12;
// below this many shared files a pair is noise, not a relationship
pub const MIN_SHARED: u32 = 3;

pub struct ConflictIndex {
    // (winner, loser) -> shared file count. winner = higher priority at scan time.
    pub pairs: HashMap<(String, String), u32>,
    pub files_indexed: usize,
    pub mods_scanned: usize,
}

impl ConflictIndex {
    pub fn ini_path(modlist: &Path) -> Option<PathBuf> {
        Some(modlist.parent()?.join("conflict.ini"))
    }

    pub fn mods_dir_of(modlist: &Path) -> Option<PathBuf> {
        let root = modlist.parent()?.parent()?.parent()?;
        let d = root.join("mods");
        d.is_dir().then_some(d)
    }

    fn mtime(p: &Path) -> Option<SystemTime> {
        fs::metadata(p).and_then(|m| m.modified()).ok()
    }

    // fresh = exists and newer than both modlist.txt and the mods/ root
    pub fn is_fresh(modlist: &Path) -> bool {
        let (Some(ini), Some(mods)) = (Self::ini_path(modlist), Self::mods_dir_of(modlist)) else {
            return false;
        };
        let Some(t) = Self::mtime(&ini) else { return false };
        let newer = |p: &Path| Self::mtime(p).is_some_and(|m| m > t);
        !newer(modlist) && !newer(&mods)
    }

    // build from disk. active = mod folder names in priority order
    // (modlist file order: index 0 = highest priority = wins conflicts).
    pub fn build(mods_dir: &Path, active: &[String]) -> ConflictIndex {
        let rank: HashMap<&str, usize> = active
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), i))
            .collect();

        // walk every active mod in parallel, collect its relative file paths
        let per_mod: Vec<(usize, Vec<String>)> = active
            .par_iter()
            .enumerate()
            .filter_map(|(idx, name)| {
                let dir = mods_dir.join(name);
                if !dir.is_dir() {
                    return None;
                }
                let mut paths = Vec::new();
                for e in WalkDir::new(&dir).into_iter().filter_map(|e| e.ok()) {
                    if !e.file_type().is_file() {
                        continue;
                    }
                    let Ok(rel) = e.path().strip_prefix(&dir) else { continue };
                    let rel = rel.to_string_lossy().replace('\\', "/").to_lowercase();
                    if rel == "meta.ini" {
                        continue;
                    }
                    paths.push(rel);
                }
                Some((idx, paths))
            })
            .collect();

        // invert: path -> providers (mod indices)
        let mut by_path: HashMap<String, Vec<u32>> = HashMap::new();
        let mut files_indexed = 0usize;
        for (idx, paths) in &per_mod {
            files_indexed += paths.len();
            for p in paths {
                by_path.entry(p.clone()).or_default().push(*idx as u32);
            }
        }
        drop(per_mod);

        // pair each provider with the winner (lowest index = highest priority)
        let mut pairs: HashMap<(String, String), u32> = HashMap::new();
        for providers in by_path.values() {
            if providers.len() < 2 || providers.len() > MAX_PROVIDERS {
                continue;
            }
            let winner = *providers.iter().min().unwrap();
            for &p in providers {
                if p == winner {
                    continue;
                }
                *pairs
                    .entry((active[winner as usize].clone(), active[p as usize].clone()))
                    .or_insert(0) += 1;
            }
        }

        ConflictIndex { pairs, files_indexed, mods_scanned: rank.len() }
    }

    // shared files between two mods: Some((winner, count)) if they're related
    pub fn shared<'a>(&'a self, a: &'a str, b: &'a str) -> Option<(&'a str, u32)> {
        if let Some(&n) = self.pairs.get(&(a.to_string(), b.to_string())) {
            return Some((a, n));
        }
        if let Some(&n) = self.pairs.get(&(b.to_string(), a.to_string())) {
            return Some((b, n));
        }
        None
    }

    pub fn save(&self, ini: &Path) -> std::io::Result<()> {
        let mut out = String::new();
        let _ = writeln!(out, "# modslut conflict index v1 - delete this file to force a rescan");
        let _ = writeln!(
            out,
            "# {} files indexed across {} mods; Winner > Loser = shared files (winner has higher priority)",
            self.files_indexed, self.mods_scanned
        );
        let mut rows: Vec<_> = self.pairs.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1));
        for ((w, l), n) in rows {
            if *n >= MIN_SHARED {
                let _ = writeln!(out, "{w} > {l} = {n}");
            }
        }
        fs::write(ini, out)
    }

    pub fn load(ini: &Path) -> Option<ConflictIndex> {
        let text = fs::read_to_string(ini).ok()?;
        let mut pairs = HashMap::new();
        for line in text.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            let Some((names, n)) = l.split_once('=') else { continue };
            let Some((w, lose)) = names.split_once('>') else { continue };
            let Ok(n) = n.trim().parse::<u32>() else { continue };
            pairs.insert((w.trim().to_string(), lose.trim().to_string()), n);
        }
        Some(ConflictIndex { pairs, files_indexed: 0, mods_scanned: 0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winner_is_higher_priority() {
        let dir = std::env::temp_dir().join("ms_ci_test");
        let mods = dir.join("mods");
        fs::create_dir_all(mods.join("BaseMod/textures")).unwrap();
        fs::create_dir_all(mods.join("PatchMod/textures")).unwrap();
        fs::write(mods.join("BaseMod/textures/a.dds"), b"x").unwrap();
        fs::write(mods.join("BaseMod/textures/b.dds"), b"x").unwrap();
        fs::write(mods.join("BaseMod/textures/c.dds"), b"x").unwrap();
        fs::write(mods.join("PatchMod/textures/a.dds"), b"y").unwrap();
        fs::write(mods.join("PatchMod/textures/b.dds"), b"y").unwrap();
        fs::write(mods.join("PatchMod/textures/c.dds"), b"y").unwrap();
        fs::write(mods.join("PatchMod/meta.ini"), b"").unwrap();

        // PatchMod listed first = higher priority = wins
        let active = vec!["PatchMod".to_string(), "BaseMod".to_string()];
        let ci = ConflictIndex::build(&mods, &active);
        assert_eq!(ci.shared("BaseMod", "PatchMod"), Some(("PatchMod", 3)));
        assert_eq!(ci.shared("PatchMod", "Nope"), None);

        let ini = dir.join("conflict.ini");
        ci.save(&ini).unwrap();
        let re = ConflictIndex::load(&ini).unwrap();
        assert_eq!(re.shared("BaseMod", "PatchMod"), Some(("PatchMod", 3)));
        fs::remove_dir_all(&dir).ok();
    }
}

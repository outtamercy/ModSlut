// plugins.rs - native rust port of skygen's plugin extractor (phase 1:
// structural layer). reads tes4 plugin binaries directly: header masters,
// esm/esl flags, and a top-level GRUP census (which record types a plugin
// carries and how many). no xedit, no nexus api, no mo2 metadata - the
// plugin tells us what it is and what it needs.
//
// phase 2 (shared with skygen): full sub-record parsing (loom's WEAP dnam
// offsets, ARMO bodt slots, KWDA material formids) for true functional
// categorization.

use flate2::read::ZlibDecoder;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct PluginInfo {
    pub plugin: String,              // file name, e.g. "USSEP.esp"
    pub masters: Vec<String>,        // MAST entries, in order
    pub is_esm: bool,                // header flag 0x1
    pub is_esl: bool,                // header flag 0x200
    pub record_count: u32,           // HEDR numRecords
    pub groups: Vec<(String, u32)>,  // top-level GRUP label -> count
    pub records: Vec<(String, u32)>, // actual record signature -> count
    // Resolved KWDA form IDs from semantic record types. Example:
    // "skyrim.esm|0006bbd2" (ArmorHeavy). Kept as counts so a mod with one
    // accidental override cannot outweigh a real equipment collection.
    pub keywords: Vec<(String, u32)>,
}

// The normal ModSlut sort does not need to interpret plugin contents.  It
// only needs to know which MO2 folder owns a plugin filename so libloot's
// solved order can be projected back into the left pane.  Keep that cheap
// inventory separate from the optional header/record census.
#[derive(Clone, Debug)]
pub struct PluginOwner {
    pub mod_name: String,
    pub section: String,
    pub plugin: String,
    pub path: std::path::PathBuf,
}

impl PluginInfo {
    // one-line record census for the debug trace: "WEAP:5 NPC_:12 CELL:3"
    pub fn census_line(&self) -> String {
        let source = if self.records.is_empty() {
            &self.groups
        } else {
            &self.records
        };
        source
            .iter()
            .map(|(g, n)| format!("{g}:{n}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

const REC_HEADER: usize = 24;

fn u32le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn bump(counts: &mut Vec<(String, u32)>, signature: &[u8]) {
    let signature = String::from_utf8_lossy(signature).to_string();
    match counts.iter_mut().find(|(sig, _)| *sig == signature) {
        Some((_, count)) => *count += 1,
        None => counts.push((signature, 1)),
    }
}

fn semantic_keyword_record(signature: &[u8]) -> bool {
    matches!(
        signature,
        b"ARMO" | b"WEAP" | b"AMMO" | b"NPC_" | b"RACE" | b"SPEL" | b"MGEF"
    )
}

fn keyword_key(form_id: u32, masters: &[String]) -> String {
    let master_index = (form_id >> 24) as usize;
    let source = masters
        .get(master_index)
        .map(String::as_str)
        .unwrap_or("self");
    format!("{source}|{:08x}", form_id & 0x00ff_ffff)
}

fn record_keyword_ids(
    data: &[u8],
    flags: u32,
    masters: &[String],
    keywords: &mut Vec<(String, u32)>,
) {
    // Compressed records start with their uncompressed byte count, followed
    // by a zlib payload. If a malformed record won't inflate, skip that one
    // record - categorizing a mod is never worth crashing the sort preview.
    let inflated;
    let data = if flags & 0x0004_0000 != 0 {
        if data.len() < 4 {
            return;
        }
        let mut decoder = ZlibDecoder::new(&data[4..]);
        let mut out = Vec::with_capacity(u32le(data, 0) as usize);
        if decoder.read_to_end(&mut out).is_err() {
            return;
        }
        inflated = out;
        &inflated[..]
    } else {
        data
    };
    let mut off = 0usize;
    while off + 6 <= data.len() {
        let kind = &data[off..off + 4];
        let size = u16::from_le_bytes([data[off + 4], data[off + 5]]) as usize;
        let body = off + 6;
        if body + size > data.len() {
            return;
        }
        if kind == b"KWDA" {
            for chunk in data[body..body + size].chunks_exact(4) {
                let id = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                bump(keywords, keyword_key(id, masters).as_bytes());
            }
        }
        off = body + size;
    }
}

// Walk a GRUP's nested headers, seeking over record payloads. We count the
// actual record signatures, not merely the presence of a top-level GRUP.
// Malformed input stops its own branch; a weird plugin never gets to nuke a
// sort preview.
fn scan_group_records(
    file: &mut fs::File,
    end: u64,
    depth: u8,
    records: &mut Vec<(String, u32)>,
    masters: &[String],
    keywords: &mut Vec<(String, u32)>,
) {
    if depth > 64 {
        return;
    }
    loop {
        let Ok(pos) = file.stream_position() else {
            return;
        };
        if pos.saturating_add(REC_HEADER as u64) > end {
            return;
        }
        let mut header = [0u8; REC_HEADER];
        if file.read_exact(&mut header).is_err() {
            return;
        }
        let size = u32le(&header, 4) as u64;
        if &header[0..4] == b"GRUP" {
            if size < REC_HEADER as u64 || pos.saturating_add(size) > end {
                return;
            }
            let child_end = pos + size;
            scan_group_records(file, child_end, depth + 1, records, masters, keywords);
            if file.seek(SeekFrom::Start(child_end)).is_err() {
                return;
            }
        } else {
            let record_end = pos.saturating_add(REC_HEADER as u64).saturating_add(size);
            if record_end > end {
                return;
            }
            bump(records, &header[0..4]);
            if semantic_keyword_record(&header[0..4]) {
                let mut data = vec![0u8; size as usize];
                if file.read_exact(&mut data).is_err() {
                    return;
                }
                record_keyword_ids(&data, u32le(&header, 8), masters, keywords);
            } else if file.seek(SeekFrom::Start(record_end)).is_err() {
                return;
            }
        }
    }
}

fn scan_bytes_records(
    bytes: &[u8],
    pos: &mut usize,
    end: usize,
    depth: u8,
    records: &mut Vec<(String, u32)>,
    masters: &[String],
    keywords: &mut Vec<(String, u32)>,
) {
    if depth > 64 {
        return;
    }
    while pos.saturating_add(REC_HEADER) <= end && pos.saturating_add(REC_HEADER) <= bytes.len() {
        let start = *pos;
        let header = &bytes[start..start + REC_HEADER];
        let size = u32le(header, 4) as usize;
        if &header[0..4] == b"GRUP" {
            if size < REC_HEADER || start.saturating_add(size) > end {
                return;
            }
            let mut child = start + REC_HEADER;
            let child_end = start + size;
            scan_bytes_records(
                bytes,
                &mut child,
                child_end,
                depth + 1,
                records,
                masters,
                keywords,
            );
            *pos = child_end;
        } else {
            let record_end = start.saturating_add(REC_HEADER).saturating_add(size);
            if record_end > end {
                return;
            }
            bump(records, &header[0..4]);
            if semantic_keyword_record(&header[0..4]) {
                record_keyword_ids(
                    &bytes[start + REC_HEADER..record_end],
                    u32le(header, 8),
                    masters,
                    keywords,
                );
            }
            *pos = record_end;
        }
    }
}

// parse a .esp/.esm/.esl. returns None on anything malformed - a weird file
// should never take the sorter down with it.
//
// streams instead of slurping: the census only needs the TES4 header block
// and each top-level GRUP's 24-byte header, so we read a few KB and seek
// past the payload. big plugins (dyndolod's esp can be hundreds of MB) used
// to be read in full on EVERY preview reload - that was the veto-click lag.
pub fn parse_plugin(path: &Path) -> Option<PluginInfo> {
    let file_name = path.file_name()?.to_string_lossy().to_lowercase();
    let mut f = fs::File::open(path).ok()?;

    let mut hdr = [0u8; REC_HEADER];
    f.read_exact(&mut hdr).ok()?;
    if &hdr[0..4] != b"TES4" {
        return None;
    }
    let flags = u32le(&hdr, 8);
    let data_size = u32le(&hdr, 4) as usize;

    // TES4 data is a run of subrecords: [4 type][u16 size][data]
    let mut data = vec![0u8; data_size];
    f.read_exact(&mut data).ok()?;
    let mut masters = Vec::new();
    let mut record_count = 0u32;
    let mut off = 0usize;
    while off + 6 <= data.len() {
        let stype = &data[off..off + 4];
        let ssize = u16::from_le_bytes([data[off + 4], data[off + 5]]) as usize;
        let body = off + 6;
        if body + ssize > data.len() {
            break;
        }
        match stype {
            b"MAST" => {
                let raw = &data[body..body + ssize];
                let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
                masters.push(String::from_utf8_lossy(&raw[..end]).to_lowercase());
            }
            b"HEDR" if ssize >= 8 => {
                record_count = u32le(&data, body + 4);
            }
            _ => {}
        }
        off = body + ssize;
    }

    // top-level GRUPs: preserve the lightweight old summary, then recurse
    // through their headers to count actual records by signature.
    let mut groups: Vec<(String, u32)> = Vec::new();
    let mut records: Vec<(String, u32)> = Vec::new();
    let mut keywords: Vec<(String, u32)> = Vec::new();
    loop {
        let group_start = f.stream_position().ok()?;
        let mut gh = [0u8; REC_HEADER];
        if f.read_exact(&mut gh).is_err() {
            break; // clean EOF or truncation - either way, census over
        }
        let size = u32le(&gh, 4) as u64;
        if size < REC_HEADER as u64 {
            break;
        }
        if &gh[0..4] == b"GRUP" {
            let label = String::from_utf8_lossy(&gh[8..12]).to_string();
            match groups.iter_mut().find(|(g, _)| *g == label) {
                Some((_, n)) => *n += 1,
                None => groups.push((label, 1)),
            }
            let group_end = group_start.saturating_add(size);
            scan_group_records(&mut f, group_end, 1, &mut records, &masters, &mut keywords);
            if f.seek(SeekFrom::Start(group_end)).is_err() {
                break;
            }
            continue;
        }
        if f.seek(SeekFrom::Current(size as i64 - REC_HEADER as i64))
            .is_err()
        {
            break;
        }
    }

    Some(PluginInfo {
        plugin: file_name,
        masters,
        is_esm: flags & 0x1 != 0,
        is_esl: flags & 0x200 != 0,
        record_count,
        groups,
        records,
        keywords,
    })
}

pub fn parse_plugin_bytes(bytes: &[u8], file_name: &str) -> Option<PluginInfo> {
    if bytes.len() < REC_HEADER || &bytes[0..4] != b"TES4" {
        return None;
    }
    let flags = u32le(bytes, 8);
    let data_size = u32le(bytes, 4) as usize;
    let data_end = (REC_HEADER + data_size).min(bytes.len());

    // TES4 data is a run of subrecords: [4 type][u16 size][data]
    let mut masters = Vec::new();
    let mut record_count = 0u32;
    let mut off = REC_HEADER;
    while off + 6 <= data_end {
        let stype = &bytes[off..off + 4];
        let ssize = u16::from_le_bytes([bytes[off + 4], bytes[off + 5]]) as usize;
        let body = off + 6;
        if body + ssize > data_end {
            break;
        }
        match stype {
            b"MAST" => {
                let raw = &bytes[body..body + ssize];
                let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
                masters.push(String::from_utf8_lossy(&raw[..end]).to_lowercase());
            }
            b"HEDR" if ssize >= 8 => {
                record_count = u32le(bytes, body + 4);
            }
            _ => {}
        }
        off = body + ssize;
    }

    // after the TES4 record: a run of top-level GRUPs. each header is
    // [4 "GRUP"][u32 total size][4 label][i32 group type]... - for the
    // census we only need the label and the skip distance.
    let mut groups: Vec<(String, u32)> = Vec::new();
    let mut records: Vec<(String, u32)> = Vec::new();
    let mut keywords: Vec<(String, u32)> = Vec::new();
    let mut pos = data_end;
    while pos + REC_HEADER <= bytes.len() {
        let sig = &bytes[pos..pos + 4];
        let size = u32le(bytes, pos + 4) as usize;
        if size < REC_HEADER || pos + size > bytes.len() {
            break;
        }
        if sig == b"GRUP" {
            let label = String::from_utf8_lossy(&bytes[pos + 8..pos + 12]).to_string();
            match groups.iter_mut().find(|(g, _)| *g == label) {
                Some((_, n)) => *n += 1,
                None => groups.push((label, 1)),
            }
            let mut child = pos + REC_HEADER;
            let end = pos + size;
            scan_bytes_records(
                bytes,
                &mut child,
                end,
                1,
                &mut records,
                &masters,
                &mut keywords,
            );
        }
        pos += size;
    }

    Some(PluginInfo {
        plugin: file_name.to_string(),
        masters,
        is_esm: flags & 0x1 != 0,
        is_esl: flags & 0x200 != 0,
        record_count,
        groups,
        records,
        keywords,
    })
}

// find every plugin file inside a mod folder (virtual tree root = the mod dir)
pub fn plugins_in_mod(mod_dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(mod_dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let p = entry.path();
        if p.extension().is_some_and(|e| {
            e.eq_ignore_ascii_case("esp")
                || e.eq_ignore_ascii_case("esm")
                || e.eq_ignore_ascii_case("esl")
        }) {
            out.push(p.to_path_buf());
        }
    }
    out
}

pub fn plugin_owners(mods: &[(String, String)], mods_dir: &Path) -> Vec<PluginOwner> {
    let mut owners = Vec::new();
    for (mod_name, section) in mods {
        let root = mods_dir.join(mod_name);
        if !root.is_dir() {
            continue;
        }
        for path in plugins_in_mod(&root) {
            let Some(plugin) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            owners.push(PluginOwner {
                mod_name: mod_name.clone(),
                section: section.clone(),
                plugin: plugin.to_ascii_lowercase(),
                path,
            });
        }
    }
    owners
}

// a master-order violation: plugin loads before something it requires
pub struct MasterViolation {
    pub mod_name: String,
    pub plugin: String,
    pub master: String,
    pub section: String,
}

// ---- census cache ("manifest") ----
// the streaming parser is fast per-file, but a 2000-mod list still walks
// a LOT of directories. cache the per-mod plugin info keyed by a
// fingerprint of the enabled mod list: adding, removing, or unticking a
// mod changes the fingerprint and forces a rescan; anything else (rule
// edits, resorting the same mods) loads instantly.
// sections are NOT cached - they're re-derived from the current modlist,
// so a sort never makes the cache stale.

pub fn census_cache_path(modlist: &Path) -> std::path::PathBuf {
    crate::migrate_profile_file(
        modlist,
        "plugin_census.cache",
        modlist.with_file_name("plugin_census.cache"),
    )
}

// fnv-1a over the sorted enabled mod names (std's DefaultHasher isn't
// guaranteed stable across processes)
pub fn census_fingerprint(enabled: &[(String, String)]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut names: Vec<&str> = enabled.iter().map(|(n, _)| n.as_str()).collect();
    names.sort();
    for n in names {
        for b in n.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn save_census(path: &Path, fp: u64, census: &[(String, String, PluginInfo)]) {
    let mut out = format!("#modslut-census-v3 {fp:016x}\n");
    for (mod_name, _sec, info) in census {
        let flags = format!(
            "{}{}",
            if info.is_esm { "e" } else { "" },
            if info.is_esl { "l" } else { "" }
        );
        let masters = info.masters.join(",");
        let groups = info
            .groups
            .iter()
            .map(|(g, n)| format!("{g}:{n}"))
            .collect::<Vec<_>>()
            .join(",");
        let records = info
            .records
            .iter()
            .map(|(g, n)| format!("{g}:{n}"))
            .collect::<Vec<_>>()
            .join(",");
        let keywords = info
            .keywords
            .iter()
            .map(|(key, n)| format!("{key}:{n}"))
            .collect::<Vec<_>>()
            .join(",");
        // tabs are safe: mo2 mod names can't contain them
        out.push_str(&format!(
            "{mod_name}\t{}\t{flags}\t{}\t{masters}\t{groups}\t{records}\t{keywords}\n",
            info.plugin, info.record_count
        ));
    }
    crate::ensure_profile_data_parent(path).ok();
    std::fs::write(path, out).ok();
}

// returns (mod name, plugin info) pairs if the cache matches the
// fingerprint; sections get re-attached by the caller from the modlist
pub fn load_census(path: &Path, fp: u64) -> Option<Vec<(String, PluginInfo)>> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let head = lines.next()?;
    let stored = head.strip_prefix("#modslut-census-v3 ")?;
    if stored != format!("{fp:016x}") {
        return None;
    }
    let mut out = Vec::new();
    for l in lines {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() != 8 {
            return None; // corrupt line - rescan everything
        }
        let masters = if f[4].is_empty() {
            vec![]
        } else {
            f[4].split(',').map(|s| s.to_string()).collect()
        };
        let groups = if f[5].is_empty() {
            vec![]
        } else {
            f[5].split(',')
                .filter_map(|p| {
                    p.rsplit_once(':')
                        .map(|(g, n)| (g.to_string(), n.parse().unwrap_or(0)))
                })
                .collect()
        };
        let records = if f[6].is_empty() {
            vec![]
        } else {
            f[6].split(',')
                .filter_map(|p| {
                    p.rsplit_once(':')
                        .map(|(g, n)| (g.to_string(), n.parse().unwrap_or(0)))
                })
                .collect()
        };
        let keywords = if f[7].is_empty() {
            vec![]
        } else {
            f[7].split(',')
                .filter_map(|p| {
                    p.rsplit_once(':')
                        .map(|(key, n)| (key.to_string(), n.parse().unwrap_or(0)))
                })
                .collect()
        };
        out.push((
            f[0].to_string(),
            PluginInfo {
                plugin: f[1].to_string(),
                masters,
                is_esm: f[2].contains('e'),
                is_esl: f[2].contains('l'),
                record_count: f[3].parse().unwrap_or(0),
                groups,
                records,
                keywords,
            },
        ));
    }
    Some(out)
}

// master-order check, split from parsing so a cached census can be
// checked without touching disk
pub fn violations_from_census(
    census: &[(String, String, PluginInfo)],
    load_order: &[String],
) -> Vec<MasterViolation> {
    let pos_of = |name: &str| load_order.iter().position(|p| p == name);
    let mut violations = Vec::new();
    for (mod_name, section, info) in census {
        let Some(my_pos) = pos_of(&info.plugin) else {
            continue;
        };
        for master in &info.masters {
            // masters we don't even have enabled are mo2's red-text
            // problem, not ours - only ordering is checked here
            if let Some(m_pos) = pos_of(master) {
                if m_pos > my_pos {
                    violations.push(MasterViolation {
                        mod_name: mod_name.clone(),
                        plugin: info.plugin.clone(),
                        master: master.clone(),
                        section: section.clone(),
                    });
                    break; // one per plugin is enough
                }
            }
        }
    }
    violations
}

// `enabled` is (mod name, current section); `load_order` is the enabled
// plugin list from plugins.txt (lines, '*' stripped, lowercased, in order).
// base game + creation club masters are exempt - mo2 keeps those pinned.
pub fn master_violations(
    enabled: &[(String, String)],
    mods_dir: &Path,
    load_order: &[String],
) -> (Vec<MasterViolation>, Vec<(String, String, PluginInfo)>) {
    let mut census = Vec::new();
    for (mod_name, section) in enabled {
        let dir = mods_dir.join(mod_name);
        if !dir.is_dir() {
            continue;
        }
        for p in plugins_in_mod(&dir) {
            let Some(info) = parse_plugin(&p) else {
                continue;
            };
            census.push((mod_name.clone(), section.clone(), info));
        }
    }
    let violations = violations_from_census(&census, load_order);
    (violations, census)
}

#[cfg(test)]
mod tests {
    use super::*;

    // minimal synthetic plugin: TES4 header with HEDR + 2 MASTs, then two GRUPs
    fn fake_plugin() -> Vec<u8> {
        let mut data = Vec::new();
        // HEDR: version f32 + numRecords u32 + nextObjID u32
        data.extend_from_slice(b"HEDR");
        data.extend_from_slice(&12u16.to_le_bytes());
        data.extend_from_slice(&1.7f32.to_le_bytes());
        data.extend_from_slice(&42u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        for m in ["skyrim.esm\0", "update.esm\0"] {
            data.extend_from_slice(b"MAST");
            data.extend_from_slice(&(m.len() as u16).to_le_bytes());
            data.extend_from_slice(m.as_bytes());
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"TES4");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x1u32.to_le_bytes()); // esm flag
        out.extend_from_slice(&[0u8; 12]); // formid/timestamp/version
        out.extend_from_slice(&data);
        for label in [b"WEAP", b"NPC_", b"WEAP"] {
            out.extend_from_slice(b"GRUP");
            out.extend_from_slice(&24u32.to_le_bytes()); // empty group
            out.extend_from_slice(label);
            out.extend_from_slice(&0i32.to_le_bytes());
            out.extend_from_slice(&[0u8; 8]);
        }
        out
    }

    #[test]
    fn parses_masters_flags_and_census() {
        let bytes = fake_plugin();
        let info = parse_plugin_bytes(&bytes, "test.esp").unwrap();
        assert_eq!(info.masters, vec!["skyrim.esm", "update.esm"]);
        assert!(info.is_esm);
        assert!(!info.is_esl);
        assert_eq!(info.record_count, 42);
        assert_eq!(
            info.groups,
            vec![("WEAP".to_string(), 2), ("NPC_".to_string(), 1)]
        );
        assert!(info.records.is_empty());
    }

    #[test]
    fn parses_the_light_flag_on_an_esl_plugin() {
        let mut bytes = fake_plugin();
        bytes[8..12].copy_from_slice(&(0x1u32 | 0x200).to_le_bytes());
        let info = parse_plugin_bytes(&bytes, "test.esl").unwrap();
        assert!(info.is_esm);
        assert!(info.is_esl);
    }

    #[test]
    fn counts_actual_record_headers_inside_groups() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"WEAP");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 16]);
        let mut pos = 0;
        let mut records = Vec::new();
        let mut keywords = Vec::new();
        scan_bytes_records(
            &bytes,
            &mut pos,
            bytes.len(),
            0,
            &mut records,
            &[],
            &mut keywords,
        );
        assert_eq!(records, vec![("WEAP".to_string(), 1)]);
    }

    #[test]
    fn resolves_kwda_ids_against_plugin_masters() {
        let mut data = Vec::new();
        data.extend_from_slice(b"KWDA");
        data.extend_from_slice(&8u16.to_le_bytes());
        // Skyrim.esm master index 0: ArmorHeavy and ArmorCuirass.
        data.extend_from_slice(&0x0006_BBD2u32.to_le_bytes());
        data.extend_from_slice(&0x0006_C0ECu32.to_le_bytes());
        let mut keywords = Vec::new();
        record_keyword_ids(&data, 0, &["skyrim.esm".into()], &mut keywords);
        assert_eq!(
            keywords,
            vec![
                ("skyrim.esm|0006bbd2".into(), 1),
                ("skyrim.esm|0006c0ec".into(), 1),
            ]
        );
    }
}

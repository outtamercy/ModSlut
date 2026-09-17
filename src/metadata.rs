// Human-authored metadata that is useful even when it cannot safely become
// an automatic sort rule. Kept beside ModSlut, profile-specific, and plain
// TSV so an optional knowledge pack can be reviewed before anyone imports it.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

pub const FILE_PREFIX: &str = "modslut_human_metadata_";

#[derive(Clone, Copy)]
pub enum Kind {
    Requirement,
    Incompatibility,
    Message,
}

impl Kind {
    fn text(self) -> &'static str {
        match self {
            Self::Requirement => "requirement",
            Self::Incompatibility => "incompatibility",
            Self::Message => "message",
        }
    }
}

#[derive(Clone, Default)]
struct Entry {
    requirements: Vec<String>,
    incompatibilities: Vec<String>,
    messages: Vec<String>,
}

#[derive(Clone, Default)]
pub struct HumanMetadata {
    entries: HashMap<String, Entry>,
    pub path: PathBuf,
}

fn key(text: &str) -> String {
    text.to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

pub fn path_for(modlist: &Path) -> PathBuf {
    let profile = modlist
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("default");
    let safe: String = profile
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let legacy_name = format!("{FILE_PREFIX}{safe}.tsv");
    let legacy = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|dir| dir.join(&legacy_name)))
        .unwrap_or_else(|| modlist.with_file_name(legacy_name));
    crate::migrate_profile_file(modlist, "human_metadata.tsv", legacy)
}

impl HumanMetadata {
    pub fn load(modlist: &Path) -> Self {
        let path = path_for(modlist);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self {
                entries: HashMap::new(),
                path,
            };
        };
        let mut entries: HashMap<String, Entry> = HashMap::new();
        for line in text.lines() {
            if line.starts_with('#') {
                continue;
            }
            let mut fields = line.splitn(3, '\t');
            let (Some(kind), Some(name), Some(value)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let bucket = entries.entry(key(name)).or_default();
            match kind {
                "requirement" => bucket.requirements.push(value.to_string()),
                "incompatibility" => bucket.incompatibilities.push(value.to_string()),
                "message" => bucket.messages.push(value.to_string()),
                _ => {}
            }
        }
        Self { entries, path }
    }

    pub fn values(&self, name: &str, kind: Kind) -> Vec<String> {
        let Some(entry) = self.entries.get(&key(name)) else {
            return Vec::new();
        };
        match kind {
            Kind::Requirement => entry.requirements.clone(),
            Kind::Incompatibility => entry.incompatibilities.clone(),
            Kind::Message => entry.messages.clone(),
        }
    }
}

pub fn append(modlist: &Path, kind: Kind, name: &str, value: &str) -> std::io::Result<()> {
    let path = path_for(modlist);
    let clean = |s: &str| s.replace(['\t', '\r', '\n'], " ").trim().to_string();
    let name = clean(name);
    let value = clean(value);
    if name.is_empty() || value.is_empty() {
        return Ok(());
    }
    let mut text = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        "# ModSlut human metadata. Profile-local, portable, and safe to review before sharing.\n# kind\tmod\tvalue\n".into()
    });
    let candidate = format!("{}\t{name}\t{value}", kind.text());
    if !text
        .lines()
        .any(|line| line.eq_ignore_ascii_case(&candidate))
    {
        crate::ensure_profile_data_parent(&path)?;
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&candidate);
        text.push('\n');
        std::fs::write(path, text)?;
    }
    Ok(())
}

/// Remove every human-authored metadata row for one mod. This is deliberately
/// all-or-nothing: the GUI's context menu says "Clear Metadata", not "maybe
/// clear one mystery field".
pub fn clear_for(modlist: &Path, name: &str) -> std::io::Result<bool> {
    let path = path_for(modlist);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };
    let wanted = key(name);
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| {
            let mut fields = line.splitn(3, '\t');
            let _kind = fields.next();
            let Some(row_name) = fields.next() else {
                return true;
            };
            key(row_name) != wanted
        })
        .collect();
    let updated = format!("{}\n", kept.join("\n"));
    if updated == text {
        return Ok(false);
    }
    std::fs::write(path, updated)?;
    Ok(true)
}

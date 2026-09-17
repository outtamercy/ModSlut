// Profile-local LOOT-style group graph for the MO2 left pane.
//
// A group is deliberately a separator label, not a second taxonomy.  The
// roadmap says what shelf a mod belongs on; this tiny graph says which shelf
// must come later.  It is owned by ModSlut and never edits LOOT's masterlist.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

pub fn path_for(modlist: &Path) -> PathBuf {
    crate::profile_data_path(modlist, "groups.ini")
}

fn key(text: &str) -> String {
    text.trim().to_ascii_lowercase()
}

pub fn load_or_create(
    modlist: &Path,
    labels: &[String],
) -> std::io::Result<(PathBuf, Vec<(String, String)>)> {
    let path = path_for(modlist);
    if !path.exists() {
        crate::ensure_profile_data_parent(&path)?;
        let mut text = String::from(
            "# ModSlut separator group graph. Profile-local and safe to share.\n#\n# Each group is one of this profile's separator labels.\n# In [after], `later = earlier` means the later group loads after the earlier group.\n\n[after]\n",
        );
        for label in labels {
            text.push_str("# ");
            text.push_str(label);
            text.push_str(" = <earlier separator>\n");
        }
        fs::write(&path, text)?;
    }
    Ok((path.clone(), parse(&path, labels)?))
}

pub fn parse(path: &Path, labels: &[String]) -> std::io::Result<Vec<(String, String)>> {
    let text = fs::read_to_string(path)?;
    let allowed: HashSet<String> = labels.iter().map(|s| key(s)).collect();
    let mut section = "";
    let mut edges: Vec<(String, String)> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = &line[1..line.len() - 1];
            continue;
        }
        if !section.eq_ignore_ascii_case("after") {
            continue;
        }
        let Some((later, earlier)) = line.split_once('=') else {
            continue;
        };
        let later = later.trim();
        let earlier = earlier.trim();
        if later.is_empty() || earlier.is_empty() || later.eq_ignore_ascii_case(earlier) {
            continue;
        }
        if allowed.contains(&key(later)) && allowed.contains(&key(earlier)) {
            let later = labels
                .iter()
                .find(|s| s.eq_ignore_ascii_case(later))
                .unwrap()
                .clone();
            let earlier = labels
                .iter()
                .find(|s| s.eq_ignore_ascii_case(earlier))
                .unwrap()
                .clone();
            if !edges
                .iter()
                .any(|(a, b)| a.eq_ignore_ascii_case(&later) && b.eq_ignore_ascii_case(&earlier))
            {
                edges.push((later, earlier));
            }
        }
    }
    Ok(edges)
}

pub fn save(path: &Path, edges: &[(String, String)]) -> std::io::Result<()> {
    crate::ensure_profile_data_parent(path)?;
    let mut text = String::from(
        "# ModSlut separator group graph. Profile-local and safe to share.\n# `later = earlier` means the later separator loads after the earlier one.\n\n[after]\n",
    );
    for (later, earlier) in edges {
        text.push_str(later);
        text.push_str(" = ");
        text.push_str(earlier);
        text.push('\n');
    }
    fs::write(path, text)
}

/// Discard only the profile's extra graph links. The always-present baseline
/// is the real separator order, so clearing restores that simple spine rather
/// than leaving the sorter with no structure at all. Keep a recoverable copy
/// beside the profile data in case the click was enthusiastic rather than
/// intentional.
pub fn clear(path: &Path) -> std::io::Result<()> {
    if path.exists() {
        fs::copy(path, path.with_extension("before-clear.ini"))?;
    }
    save(path, &[])
}

pub fn import_for_profile(
    modlist: &Path,
    source: &Path,
    labels: &[String],
) -> std::io::Result<usize> {
    let edges = parse(source, labels)?;
    let path = path_for(modlist);
    if path.exists() {
        fs::copy(&path, path.with_extension("before-import.ini"))?;
    }
    save(&path, &edges)?;
    Ok(edges.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_unknown_and_self_edges() {
        let dir = std::env::temp_dir().join("modslut-groups-test.ini");
        fs::write(
            &dir,
            "[after]\nLate = Early\nGhost = Early\nEarly = Early\n",
        )
        .unwrap();
        let got = parse(&dir, &["Early".into(), "Late".into()]).unwrap();
        assert_eq!(got, vec![("Late".into(), "Early".into())]);
        let _ = fs::remove_file(dir);
    }
}

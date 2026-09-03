// game.rs - which creation-engine game is this modlist for?
// detected from the plugin census: the root .esm headers don't lie and
// need no mo2 metadata. everything game-specific (base masters exempt
// from parentage, loot's games/ folder names) keys off this.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Game {
    SkyrimSeVr, // SE, AE and VR share the master set
    Fallout4,
    Starfield,
    Enderal,
    Oblivion,
    Fallout3Nv, // fallout3.esm / falloutnv.esm
    Unknown,
}

pub struct GameInfo {
    pub game: Game,
    pub name: &'static str,
    // root dlcs: exempt from parentage so a "Clean Masters" container mod
    // can't drag the whole list after itself. lowercase.
    pub base_masters: &'static [&'static str],
    // loot's games/ folder names, most preferred first
    pub loot_folders: &'static [&'static str],
    // loot settings.yaml game "type" strings - the games/ folder name is a
    // SETTING in loot (it autodetects installs, and users can rename the
    // entry), so the authoritative mapping lives there
    pub loot_types: &'static [&'static str],
    // substring for the renamed-folder scan
    pub loot_scan_key: &'static str,
}

const SKYRIM_MASTERS: &[&str] = &[
    "skyrim.esm",
    "skyrimvr.esm",
    "update.esm",
    "dawnguard.esm",
    "hearthfires.esm",
    "dragonborn.esm",
];
const FALLOUT4_MASTERS: &[&str] = &[
    "fallout4.esm",
    "fallout4vr.esm",
    "dlcrobot.esm",
    "dlcworkshop01.esm",
    "dlccoast.esm",
    "dlcworkshop02.esm",
    "dlcworkshop03.esm",
    "dlcnukaworld.esm",
];
const STARFIELD_MASTERS: &[&str] = &["starfield.esm", "blueprintships-starfield.esm", "sfbgs003.esm", "sfbgs004.esm"];
const ENDERAL_MASTERS: &[&str] = &[
    "skyrim.esm",
    "update.esm",
    "enderal - forgotten stories.esm",
    "enderal - forgotten stories (special edition).esm",
];
const OBLIVION_MASTERS: &[&str] = &["oblivion.esm", "knights.esp"];
const FALLOUT3NV_MASTERS: &[&str] = &["fallout3.esm", "falloutnv.esm"];

const INFO: &[(Game, &str, &[&str], &[&str], &[&str], &str)] = &[
    (Game::SkyrimSeVr, "Skyrim SE/AE/VR", SKYRIM_MASTERS,
        &["Skyrim VR", "Skyrim Special Edition", "Skyrim"],
        &["SkyrimVR", "SkyrimSE", "Skyrim"], "skyrim"),
    (Game::Fallout4, "Fallout 4", FALLOUT4_MASTERS,
        &["Fallout4VR", "Fallout4"],
        &["Fallout4VR", "Fallout4"], "fallout4"),
    (Game::Starfield, "Starfield", STARFIELD_MASTERS,
        &["Starfield"],
        &["Starfield"], "starfield"),
    (Game::Enderal, "Enderal", ENDERAL_MASTERS,
        &["Enderal Special Edition", "Enderal"],
        &["Enderal", "EnderalSE"], "enderal"),
    (Game::Oblivion, "Oblivion", OBLIVION_MASTERS,
        &["Oblivion"],
        &["Oblivion"], "oblivion"),
    (Game::Fallout3Nv, "Fallout 3 / New Vegas", FALLOUT3NV_MASTERS,
        &["FalloutNV", "Fallout3"],
        &["FalloutNV", "Fallout3"], "fallout"),
];

pub fn info(game: Game) -> GameInfo {
    let (g, name, masters, folders, types, key) = INFO.iter().find(|(g, ..)| *g == game).copied().unwrap();
    GameInfo { game: g, name, base_masters: masters, loot_folders: folders, loot_types: types, loot_scan_key: key }
}

// root esm -> game. census plugin names arrive in any case; compare lowered.
pub fn detect_from_plugins(plugins: &[String]) -> Game {
    let has = |needle: &str| plugins.iter().any(|p| p.to_lowercase() == needle);
    if has("skyrim.esm") || has("skyrimvr.esm") {
        // enderal runs on the skyrim engine with its own esm alongside
        if plugins.iter().any(|p| p.to_lowercase().starts_with("enderal")) {
            return Game::Enderal;
        }
        return Game::SkyrimSeVr;
    }
    if has("fallout4.esm") || has("fallout4vr.esm") {
        return Game::Fallout4;
    }
    if has("starfield.esm") {
        return Game::Starfield;
    }
    if has("oblivion.esm") {
        return Game::Oblivion;
    }
    if has("fallout3.esm") || has("falloutnv.esm") {
        return Game::Fallout3Nv;
    }
    Game::Unknown
}

// unknown list (no census): default to skyrim - preserves pre-detection behavior
pub fn detect_or_skyrim(plugins: &[String]) -> GameInfo {
    let g = detect_from_plugins(plugins);
    info(if g == Game::Unknown { Game::SkyrimSeVr } else { g })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_by_root_esm() {
        let p = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(detect_from_plugins(&p(&["Skyrim.esm", "USSEP.esp"])), Game::SkyrimSeVr);
        assert_eq!(detect_from_plugins(&p(&["SkyrimVR.esm"])), Game::SkyrimSeVr);
        assert_eq!(detect_from_plugins(&p(&["Fallout4.esm"])), Game::Fallout4);
        assert_eq!(detect_from_plugins(&p(&["Starfield.esm"])), Game::Starfield);
        assert_eq!(detect_from_plugins(&p(&["Oblivion.esm"])), Game::Oblivion);
        assert_eq!(detect_from_plugins(&p(&["FalloutNV.esm"])), Game::Fallout3Nv);
        assert_eq!(
            detect_from_plugins(&p(&["Skyrim.esm", "Enderal - Forgotten Stories.esm"])),
            Game::Enderal
        );
        assert_eq!(detect_from_plugins(&p(&["RandomMod.esp"])), Game::Unknown);
    }

    #[test]
    fn unknown_defaults_to_skyrim() {
        assert_eq!(detect_or_skyrim(&[]).game, Game::SkyrimSeVr);
    }
}

// game.rs - ModSlut's LOOT/libloot game catalog.
//
// Keep this independent from the GUI: libloot, the master census, the
// roadmap sorter, and Settings all use this one catalog. That prevents a
// pretty game picker from advertising a game the sorter cannot understand.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Game {
    Oblivion,
    OblivionRemastered,
    Skyrim,
    SkyrimSe,
    SkyrimVr,
    Enderal,
    Fallout3,
    FalloutNv,
    Fallout4,
    Fallout4Vr,
    Morrowind,
    OpenMw,
    Starfield,
    Unknown,
}

#[derive(Clone, Copy, Debug)]
pub struct GameInfo {
    pub game: Game,
    pub name: &'static str,
    // Root DLCs are exempt from parentage so a "Clean Masters" folder can't
    // drag the entire list after itself. Names are lowercase.
    pub base_masters: &'static [&'static str],
    // LOOT's configured game folders, most preferred first.
    pub loot_folders: &'static [&'static str],
    // LOOT settings.toml `type` values.
    pub loot_types: &'static [&'static str],
    pub loot_scan_key: &'static str,
}

const SKYRIM_DLC: &[&str] = &[
    "skyrim.esm",
    "update.esm",
    "dawnguard.esm",
    "hearthfires.esm",
    "dragonborn.esm",
];
const SKYRIM_VR_DLC: &[&str] = &[
    "skyrimvr.esm",
    "update.esm",
    "dawnguard.esm",
    "hearthfires.esm",
    "dragonborn.esm",
];
const FALLOUT4_DLC: &[&str] = &[
    "fallout4.esm",
    "dlcrobot.esm",
    "dlcworkshop01.esm",
    "dlccoast.esm",
    "dlcworkshop02.esm",
    "dlcworkshop03.esm",
    "dlcnukaworld.esm",
];
const FALLOUT4VR_DLC: &[&str] = &[
    "fallout4vr.esm",
    "dlcrobot.esm",
    "dlcworkshop01.esm",
    "dlccoast.esm",
    "dlcworkshop02.esm",
    "dlcworkshop03.esm",
    "dlcnukaworld.esm",
];
const STARFIELD_MASTERS: &[&str] = &[
    "starfield.esm",
    "blueprintships-starfield.esm",
    "sfbgs003.esm",
    "sfbgs004.esm",
];
const ENDERAL_MASTERS: &[&str] = &[
    "skyrim.esm",
    "update.esm",
    "enderal - forgotten stories.esm",
    "enderal - forgotten stories (special edition).esm",
];
const OBLIVION_MASTERS: &[&str] = &["oblivion.esm", "knights.esp"];
const FALLOUT3_MASTERS: &[&str] = &["fallout3.esm"];
const FALLOUTNV_MASTERS: &[&str] = &[
    "falloutnv.esm",
    "deadmoney.esm",
    "honesthearts.esm",
    "oldworldblues.esm",
    "lonesomeroad.esm",
    "gunrunnersarsenal.esm",
];
const MORROWIND_MASTERS: &[&str] = &["morrowind.esm", "tribunal.esm", "bloodmoon.esm"];

const INFO: &[(Game, &str, &[&str], &[&str], &[&str], &str)] = &[
    (
        Game::Oblivion,
        "The Elder Scrolls IV: Oblivion",
        OBLIVION_MASTERS,
        &["Oblivion"],
        &["Oblivion"],
        "oblivion",
    ),
    (
        Game::OblivionRemastered,
        "The Elder Scrolls IV: Oblivion Remastered",
        OBLIVION_MASTERS,
        &["Oblivion Remastered"],
        &["OblivionRemastered"],
        "oblivionremastered",
    ),
    (
        Game::Skyrim,
        "The Elder Scrolls V: Skyrim",
        SKYRIM_DLC,
        &["Skyrim"],
        &["Skyrim"],
        "skyrim",
    ),
    (
        Game::SkyrimSe,
        "The Elder Scrolls V: Skyrim Special Edition",
        SKYRIM_DLC,
        &["Skyrim Special Edition"],
        &["SkyrimSE"],
        "skyrimse",
    ),
    (
        Game::SkyrimVr,
        "The Elder Scrolls V: Skyrim VR",
        SKYRIM_VR_DLC,
        &["Skyrim VR"],
        &["SkyrimVR"],
        "skyrimvr",
    ),
    // Enderal consumes Skyrim SE-format plugins but has a separate LOOT game
    // folder and masterlist identity.
    (
        Game::Enderal,
        "Enderal",
        ENDERAL_MASTERS,
        &["Enderal Special Edition", "Enderal"],
        &["Enderal", "EnderalSE"],
        "enderal",
    ),
    (
        Game::Fallout3,
        "Fallout 3",
        FALLOUT3_MASTERS,
        &["Fallout3"],
        &["Fallout3"],
        "fallout3",
    ),
    (
        Game::FalloutNv,
        "Fallout: New Vegas",
        FALLOUTNV_MASTERS,
        &["FalloutNV"],
        &["FalloutNV"],
        "falloutnv",
    ),
    (
        Game::Fallout4,
        "Fallout 4",
        FALLOUT4_DLC,
        &["Fallout4"],
        &["Fallout4"],
        "fallout4",
    ),
    (
        Game::Fallout4Vr,
        "Fallout 4 VR",
        FALLOUT4VR_DLC,
        &["Fallout4VR"],
        &["Fallout4VR"],
        "fallout4vr",
    ),
    (
        Game::Morrowind,
        "The Elder Scrolls III: Morrowind",
        MORROWIND_MASTERS,
        &["Morrowind"],
        &["Morrowind"],
        "morrowind",
    ),
    (
        Game::OpenMw,
        "OpenMW",
        MORROWIND_MASTERS,
        &["OpenMW"],
        &["OpenMW"],
        "openmw",
    ),
    (
        Game::Starfield,
        "Starfield",
        STARFIELD_MASTERS,
        &["Starfield"],
        &["Starfield"],
        "starfield",
    ),
];

pub const CATALOG: &[Game] = &[
    Game::Oblivion,
    Game::OblivionRemastered,
    Game::Skyrim,
    Game::SkyrimSe,
    Game::SkyrimVr,
    Game::Enderal,
    Game::Fallout3,
    Game::FalloutNv,
    Game::Fallout4,
    Game::Fallout4Vr,
    Game::Morrowind,
    Game::OpenMw,
    Game::Starfield,
];

pub fn info(game: Game) -> GameInfo {
    let (g, name, masters, folders, types, key) = INFO
        .iter()
        .find(|(g, ..)| *g == game)
        .copied()
        .unwrap_or_else(|| {
            INFO.iter()
                .find(|(g, ..)| *g == Game::SkyrimSe)
                .copied()
                .expect("Skyrim SE catalog")
        });
    GameInfo {
        game: g,
        name,
        base_masters: masters,
        loot_folders: folders,
        loot_types: types,
        loot_scan_key: key,
    }
}

pub fn key(game: Game) -> &'static str {
    match game {
        Game::Unknown => "auto",
        _ => info(game).loot_types[0],
    }
}

pub fn from_key(value: &str) -> Option<Game> {
    if value.eq_ignore_ascii_case("auto") {
        return None;
    }
    CATALOG
        .iter()
        .copied()
        .find(|&game| key(game).eq_ignore_ascii_case(value))
}

// Root masters distinguish every LOOT game except classic Skyrim vs SE and
// Oblivion vs its remaster. Those pairs deliberately use the user's default
// only when needed; guessing a newer game from a shared master would be worse
// than keeping the safe classic identification.
pub fn detect_from_plugins(plugins: &[String]) -> Game {
    let has = |needle: &str| plugins.iter().any(|p| p.eq_ignore_ascii_case(needle));
    if has("skyrimvr.esm") {
        return Game::SkyrimVr;
    }
    if has("skyrim.esm") {
        if plugins
            .iter()
            .any(|p| p.to_ascii_lowercase().starts_with("enderal"))
        {
            return Game::Enderal;
        }
        return Game::SkyrimSe;
    }
    if has("fallout4vr.esm") {
        return Game::Fallout4Vr;
    }
    if has("fallout4.esm") {
        return Game::Fallout4;
    }
    if has("starfield.esm") {
        return Game::Starfield;
    }
    if has("morrowind.esm") {
        return Game::Morrowind;
    }
    if has("oblivion.esm") {
        return Game::Oblivion;
    }
    if has("falloutnv.esm") {
        return Game::FalloutNv;
    }
    if has("fallout3.esm") {
        return Game::Fallout3;
    }
    Game::Unknown
}

pub fn detect_with_default(plugins: &[String], default_game: Option<Game>) -> GameInfo {
    let detected = detect_from_plugins(plugins);
    // `Skyrim.esm` and `Oblivion.esm` alone cannot distinguish these two
    // supported pairs. A deliberately chosen default is a valid tiebreaker
    // there, but never a license to force a recognisable Fallout/VR/etc.
    let selected = match (detected, default_game) {
        (Game::Unknown, Some(default)) => default,
        (Game::Unknown, None) => Game::SkyrimSe,
        (Game::SkyrimSe, Some(Game::Skyrim)) => Game::Skyrim,
        (Game::Oblivion, Some(Game::OblivionRemastered)) => Game::OblivionRemastered,
        (known, _) => known,
    };
    info(selected)
}

// Retained for callers that do not have a user preference yet.
pub fn detect_or_skyrim(plugins: &[String]) -> GameInfo {
    detect_with_default(plugins, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn p(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn detects_every_distinct_root_master() {
        assert_eq!(detect_from_plugins(&p(&["SkyrimVR.esm"])), Game::SkyrimVr);
        assert_eq!(detect_from_plugins(&p(&["Skyrim.esm"])), Game::SkyrimSe);
        assert_eq!(
            detect_from_plugins(&p(&["Fallout4VR.esm"])),
            Game::Fallout4Vr
        );
        assert_eq!(detect_from_plugins(&p(&["Fallout4.esm"])), Game::Fallout4);
        assert_eq!(detect_from_plugins(&p(&["FalloutNV.esm"])), Game::FalloutNv);
        assert_eq!(detect_from_plugins(&p(&["Fallout3.esm"])), Game::Fallout3);
        assert_eq!(detect_from_plugins(&p(&["Morrowind.esm"])), Game::Morrowind);
        assert_eq!(detect_from_plugins(&p(&["Starfield.esm"])), Game::Starfield);
    }

    #[test]
    fn preference_only_fills_an_unknown_census() {
        assert_eq!(
            detect_with_default(&p(&["random.esp"]), Some(Game::OpenMw)).game,
            Game::OpenMw
        );
        assert_eq!(
            detect_with_default(&p(&["Fallout4.esm"]), Some(Game::OpenMw)).game,
            Game::Fallout4
        );
        assert_eq!(
            detect_with_default(&p(&["Skyrim.esm"]), Some(Game::Skyrim)).game,
            Game::Skyrim
        );
        assert_eq!(
            detect_with_default(&p(&["Oblivion.esm"]), Some(Game::OblivionRemastered)).game,
            Game::OblivionRemastered
        );
    }
}

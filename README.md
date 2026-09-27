# ModSlut

MO2 modlist organizer. Reads the active profile's modlist.txt, proposes
section moves / in-section reorders / conflict-proven fixes, edits nothing
until you hit apply. Platform-guard for VR profiles (flags AE/LE-only SKSE
plugins), plugin master-order validation, LOOT-style whole-list rule editing.

Built-in rules (rules.txt, compiled in) are universal defaults; personal
layout rules go in modslut_rules.txt next to your modlist.txt (created by
the app on first use - user rules win).

## Build (Windows, native)
    cargo build --release

## Cross-compile from Linux (what releases use)
llvm-mingw + rust target x86_64-pc-windows-gnu; see .cargo/config.toml.
debug_sort.log is written next to modlist.txt on every run.

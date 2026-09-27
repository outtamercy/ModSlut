ModSlut is an MO2 sorting tool for both sides of your load order: 
it sorts plugins using LOOT’s libloot engine, and sorts your actual MO2 modlist—separators, pluginless mods, patches, outputs, the whole cursed left pane,  and the general “why is this sitting down here?” mess.
It reads your MO2 profile, builds a preview first, and lets you apply mod sorting, plugin sorting, or both. Nothing gets silently yeeted around.
ModSlut isn’t Skyrim-only, either. It uses LOOT’s supported-game data and can work with MO2 profiles for supported Bethesda games—Skyrim SE/AE/VR, Fallout 4 and Fallout 4 VR, New Vegas, Enderal, Starfield, and more as support is filled out. Skyrim VR is where it’s had the most real-world abuse so far, because apparently I enjoy suffering.
What it does:

    Sorts MO2 mods into your separator roadmap.
    Uses LOOT/libloot for plugin sorting.
    Keeps pluginless mods in the same workflow instead of pretending they don’t exist.
    Understands patches, outputs, variants, VR/SE sibling mods, and common modlist structure.
    Learns corrections for the active profile: an applied drag/drop or exact home is saved as a durable learned home, so you can remove the temporary rule later without MS immediately forgetting why it belongs there.
    Lets you forget a learned home from Clear Metadata when you change your mind. It is your list, not a hostage situation.
    Shows a preview before applying changes.
    Separate apply buttons for mods, plugins, or both.
    Refreshes MO2 after applying so you’re not mashing F5 wondering if it worked.
    Includes inline metadata editing for custom group, load-after/before, requirements, incompatibilities, messages, tags, and more.
    Has a compatibility guard for suspicious SKSE DLLs in Skyrim VR profiles.
    Supports self-contained themes, custom backgrounds, and read-only MO2 theme imports.

Credit where credit's due:
Plugin sorting is powered by libloot, the sorting engine from the LOOT project. Huge credit to the LOOT team for the masterlist data, sorting work, and years of keeping modders from setting their load orders on fire.
ModSlut builds on that foundation for MO2: plugin sorting on the right, mod/separator sorting on the left, and a workflow that keeps both in one place.

## What it’s not

It is not magic. If your roadmap is chaos, ModSlut can organize the chaos, but it can’t read your soul or know that “Random Test Mod FINAL v3 actually final” belongs in a secret home only you understand ,  you’ll need to teach it that one.

## Before you use it

Back up your profile. Seriously. The app previews changes first for a reason, but backups are cheap and regret is annoying.
Start with Sort Mods, look through the changes, then apply when it looks right. Teach it the exceptions as you go and it gets more useful for that specific list.
Built for people with too many mods and not enough patience.

It’s a LOOT replacement for plugin sorting, plus the MO2 left-pane brain LOOT never had. 

## Themes, backgrounds, and stylesheets

ModSlut can use self-contained themes, custom backgrounds, and read-only MO2 theme imports. Your theme can make the app look like a clean utility, a purple space garden, or whatever other questionable aesthetic decision got you through a 2,000-mod setup.

MO2 stylesheet/theme imports are read-only. MS can borrow the look; it does not rewrite MO2's theme files, stomp your setup, or quietly turn your UI into somebody else's bad idea.

## Learning, minus the fake AI nonsense

ModSlut does not upload your list or pretend it has psychic powers. It learns from corrections you explicitly make in the active profile.

- Drag a mod where it belongs, then Apply Mods: MS keeps that home for this profile.
- Save an exact placement rule: MS learns the home immediately, not only after another Sort.
- Remove the temporary rule later if you want. The learned home remains until you explicitly clear that mod's metadata/learned home.
- A correction in one MO2 profile does not leak into every other list on your drive. That would be deranged.

## Shareable modlist imports and download-link manifests (0.15.2)

Authors can export a portable roadmap and an `.mslinks` address book for their modlist. Someone else can import both: MS rebuilds the separator roadmap and shows the source pages for the mods they still need. It is a modlist sharing helper, not a Wabbajack clone wearing a fake moustache.

**Approved sites are not a free malware blessing.**

ModSlut only opens known mod-hosting pages. It does not download, scan, install, host, endorse, or verify somebody’s mystery ZIP just because it lives on Google Drive, Discord, GitHub, or whatever.

Check the author, check the file, use your antivirus, use your brain a little. If you choose to bypass a blocked/unapproved source, that is very much a you decision.

Found a legit mod site that should be approved? Bug Mayhem in the ModSlut Discord with the site and mod page. Don’t just send “add this random link pls” and make me go spelunking through the internet.

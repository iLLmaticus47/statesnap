//! Figures out which game a running program belongs to, and guesses where
//! that game keeps its saves.

use std::path::{Path, PathBuf};
use std::{env, fs};

#[derive(Clone, Debug, Default)]
pub struct Detected {
    pub name: String,
    pub app_id: Option<u32>,
    /// Install folder of the game (or the exe's folder for non-Steam programs).
    pub dir: PathBuf,
    /// Steam's folder name for the game, a useful second name to search by.
    pub install_name: Option<String>,
}

pub fn steam_root() -> Option<PathBuf> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;
    let from = |hive, key: &str, value: &str| -> Option<PathBuf> {
        let path: String = RegKey::predef(hive).open_subkey(key).ok()?.get_value(value).ok()?;
        let path = PathBuf::from(path.replace('/', "\\"));
        path.is_dir().then_some(path)
    };
    from(HKEY_CURRENT_USER, "Software\\Valve\\Steam", "SteamPath")
        .or_else(|| from(HKEY_LOCAL_MACHINE, "SOFTWARE\\WOW6432Node\\Valve\\Steam", "InstallPath"))
}

/// Reads `"key"  "value"` pairs from Valve's text format (flat, no nesting needed here).
fn quoted_tokens(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut token = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    if let Some(next) = chars.next() {
                        token.push(next);
                    }
                }
                '"' => break,
                c => token.push(c),
            }
        }
        tokens.push(token);
    }
    tokens
}

fn vdf_pairs(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut tokens = quoted_tokens(line);
        if tokens.len() == 2 {
            let value = tokens.pop().unwrap();
            out.push((tokens.pop().unwrap().to_ascii_lowercase(), value));
        }
    }
    out
}

fn vdf_get(pairs: &[(String, String)], key: &str) -> Option<String> {
    pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

/// Identifies the game an executable belongs to. Steam games are recognised
/// by living in `…\steamapps\common\<folder>\`; anything else is named after
/// its exe.
pub fn identify(exe: &Path) -> Detected {
    for dir in exe.ancestors().skip(1) {
        let Some(common) = dir.parent() else { break };
        let Some(steamapps) = common.parent() else { break };
        let is = |p: &Path, name: &str| p.file_name().map(|n| n.to_string_lossy().eq_ignore_ascii_case(name)).unwrap_or(false);
        if !is(common, "common") || !is(steamapps, "steamapps") {
            continue;
        }
        let install_name = dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let mut found = Detected {
            name: install_name.clone(),
            app_id: None,
            dir: dir.to_path_buf(),
            install_name: Some(install_name.clone()),
        };
        if let Ok(entries) = fs::read_dir(steamapps) {
            for entry in entries.flatten() {
                let file = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if !file.starts_with("appmanifest_") || !file.ends_with(".acf") {
                    continue;
                }
                let Ok(text) = fs::read_to_string(entry.path()) else { continue };
                let pairs = vdf_pairs(&text);
                if vdf_get(&pairs, "installdir").map(|d| d.eq_ignore_ascii_case(&install_name)) == Some(true) {
                    found.app_id = vdf_get(&pairs, "appid").and_then(|id| id.parse().ok());
                    if let Some(name) = vdf_get(&pairs, "name") {
                        found.name = name;
                    }
                    break;
                }
            }
        }
        return found;
    }
    Detected {
        name: exe.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
        app_id: None,
        dir: exe.parent().unwrap_or(Path::new("")).to_path_buf(),
        install_name: None,
    }
}

fn simplify(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|entries| entries.flatten().filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false)).map(|e| e.path()).collect())
        .unwrap_or_default()
}

/// True when the folder holds at least one file, at any depth.
pub fn has_files(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else { return false };
    entries.flatten().any(|e| match e.file_type() {
        Ok(t) if t.is_file() => true,
        Ok(t) if t.is_dir() => has_files(&e.path()),
        _ => false,
    })
}

fn env_dir(name: &str) -> Option<PathBuf> {
    env::var_os(name).map(PathBuf::from)
}

fn documents_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = env_dir("USERPROFILE") {
        out.push(home.join("Documents"));
    }
    if let Some(onedrive) = env_dir("OneDrive") {
        out.push(onedrive.join("Documents"));
    }
    out
}

/// Reads Steam's `remotecache.vdf`: the list of files Steam Cloud syncs for
/// one game, each with a numeric "root" saying which base folder it lives in.
fn parse_remotecache(text: &str) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut pending: Option<String> = None;
    let mut current: Option<String> = None;
    for line in text.lines() {
        match line.trim() {
            "{" => {
                depth += 1;
                if depth == 2 {
                    current = pending.take();
                }
                continue;
            }
            "}" => {
                depth = depth.saturating_sub(1);
                current = None;
                continue;
            }
            _ => {}
        }
        let mut tokens = quoted_tokens(line);
        match (depth, tokens.len()) {
            (1, 1) => pending = tokens.pop(),
            (2, 2) if tokens[0].eq_ignore_ascii_case("root") => {
                if let (Some(file), Ok(root)) = (&current, tokens[1].parse()) {
                    out.push((file.clone(), root));
                }
            }
            _ => {}
        }
    }
    out
}

/// Deepest folder shared by all the given `a/b/file` paths ("" if none).
fn common_dir(files: &[&str]) -> Vec<String> {
    let mut common: Option<Vec<&str>> = None;
    for file in files {
        let mut parts: Vec<&str> = file.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
        parts.pop(); // the file name
        common = Some(match common {
            None => parts,
            Some(prev) => prev.iter().zip(&parts).take_while(|(a, b)| a.eq_ignore_ascii_case(b)).map(|(a, _)| *a).collect(),
        });
    }
    common.unwrap_or_default().into_iter().map(String::from).collect()
}

/// Turns Steam Cloud's file list into the folder that holds the saves.
/// `bases_for(root)` lists candidate base folders for a root code, best guess
/// first; the one where the listed files really exist wins, so a wrong guess
/// about what a root code means cannot produce a wrong folder.
fn resolve_cloud_dir(entries: &[(String, u32)], bases_for: &dyn Fn(u32) -> Vec<PathBuf>) -> Option<PathBuf> {
    // Use the root most files live in (settings sometimes sit elsewhere).
    let mut roots: Vec<u32> = entries.iter().map(|(_, r)| *r).collect();
    roots.sort_unstable();
    roots.dedup();
    let count = |root: u32| entries.iter().filter(|(_, r)| *r == root).count();
    roots.sort_by_key(|r| std::cmp::Reverse(count(*r)));

    for root in roots {
        let files: Vec<&str> = entries.iter().filter(|(_, r)| *r == root).map(|(f, _)| f.as_str()).collect();
        let shared = common_dir(&files);
        // Root 0 is the game's own Steam Cloud folder. Any other root is a
        // general place (Documents, AppData, the install folder): without a
        // shared subfolder the "save folder" would be that whole place.
        if root != 0 && shared.is_empty() {
            continue;
        }
        let rel = |file: &str| file.split(['/', '\\']).filter(|p| !p.is_empty()).collect::<PathBuf>();
        let best = bases_for(root)
            .into_iter()
            .map(|base| (files.iter().filter(|f| base.join(rel(f)).is_file()).count(), base))
            .filter(|(hits, _)| *hits > 0)
            .max_by_key(|(hits, _)| *hits);
        if let Some((_, base)) = best {
            return Some(shared.iter().fold(base, |dir, part| dir.join(part)));
        }
    }
    None
}

/// Save folder according to Steam Cloud's own records for this game.
fn cloud_save_dir(game: &Detected) -> Option<PathBuf> {
    let app_id = game.app_id?.to_string();
    let steam = steam_root()?;
    // Most recently synced Steam account first.
    let mut caches: Vec<(std::time::SystemTime, PathBuf)> = subdirs(&steam.join("userdata"))
        .into_iter()
        .map(|user| user.join(&app_id))
        .filter_map(|dir| Some((fs::metadata(dir.join("remotecache.vdf")).ok()?.modified().ok()?, dir)))
        .collect();
    caches.sort_by(|a, b| b.0.cmp(&a.0));

    let home = env_dir("USERPROFILE");
    for (_, app_dir) in caches {
        let Ok(text) = fs::read_to_string(app_dir.join("remotecache.vdf")) else { continue };
        let entries = parse_remotecache(&text);
        let bases_for = |root: u32| -> Vec<PathBuf> {
            let known = |code: u32| -> Vec<PathBuf> {
                match code {
                    0 => vec![app_dir.join("remote")],
                    1 => vec![game.dir.clone()],
                    2 => documents_dirs(),
                    3 => env_dir("LOCALAPPDATA").into_iter().collect(),
                    4 => env_dir("APPDATA").into_iter().collect(),
                    5 => vec![app_dir.clone()],
                    9 => home.iter().map(|h| h.join("Saved Games")).collect(),
                    10 => env_dir("ProgramData").into_iter().collect(),
                    12 => home.iter().map(|h| h.join("AppData").join("LocalLow")).collect(),
                    _ => Vec::new(),
                }
            };
            // The expected base first, then every other one as a fallback.
            let mut bases = known(root);
            for code in [0, 1, 2, 3, 4, 5, 9, 10, 12] {
                if code != root {
                    bases.extend(known(code));
                }
            }
            bases
        };
        if let Some(dir) = resolve_cloud_dir(&entries, &bases_for) {
            return Some(dir);
        }
    }
    None
}

/// Unreal Engine games save to `%LOCALAPPDATA%\<Project>\Saved\SaveGames`,
/// where <Project> is the folder next to `Engine` in the install folder
/// (often an internal code name, not the game's title).
fn unreal_save_dir(install: &Path) -> Option<PathBuf> {
    let local = env_dir("LOCALAPPDATA")?;
    for project in subdirs(install) {
        let name = project.file_name()?.to_owned();
        if name.eq_ignore_ascii_case("Engine") || !(project.join("Binaries").is_dir() || project.join("Content").is_dir()) {
            continue;
        }
        let candidates = [local.join(&name).join("Saved").join("SaveGames"), project.join("Saved").join("SaveGames")];
        if let Some(found) = candidates.into_iter().find(|c| has_files(c)) {
            return Some(found);
        }
    }
    None
}

/// Unity games save to `LocalLow\<Company>\<Product>`; both names are in
/// `<Game>_Data\app.info` in the install folder.
fn unity_save_dir(install: &Path) -> Option<PathBuf> {
    let low = env_dir("USERPROFILE")?.join("AppData").join("LocalLow");
    for data in subdirs(install) {
        if !data.file_name()?.to_string_lossy().ends_with("_Data") {
            continue;
        }
        let Ok(info) = fs::read_to_string(data.join("app.info")) else { continue };
        let mut lines = info.lines().map(str::trim).filter(|l| !l.is_empty());
        if let (Some(company), Some(product)) = (lines.next(), lines.next()) {
            let dir = low.join(company).join(product);
            if has_files(&dir) {
                return Some(dir);
            }
        }
    }
    None
}

/// Best guess for a game's save folder, most reliable source first:
/// Steam Cloud's file list, the engine's standard location, then folders
/// named after the game. None when nothing plausible exists; the user then
/// picks the folder by hand.
pub fn suggest_save_dir(game: &Detected) -> Option<PathBuf> {
    if let Some(dir) = cloud_save_dir(game).or_else(|| unreal_save_dir(&game.dir)).or_else(|| unity_save_dir(&game.dir)) {
        return Some(dir);
    }

    // A folder named like the game in the common save locations.
    let mut wanted: Vec<String> = vec![simplify(&game.name)];
    if let Some(install) = &game.install_name {
        wanted.push(simplify(install));
    }
    wanted.retain(|w| w.chars().count() >= 3);
    wanted.dedup();
    if wanted.is_empty() {
        return None;
    }
    let matches = |dir: &Path| {
        let name = simplify(&dir.file_name().unwrap_or_default().to_string_lossy());
        wanted.iter().any(|w| *w == name)
    };

    let var = |name: &str| env::var_os(name).map(PathBuf::from);
    let home = var("USERPROFILE")?;
    let mut documents = vec![home.join("Documents")];
    if let Some(onedrive) = var("OneDrive") {
        documents.push(onedrive.join("Documents"));
    }

    // (root, also look one level deeper for Publisher\Game layouts)
    let mut roots: Vec<(PathBuf, bool)> = vec![(home.join("Saved Games"), false)];
    for docs in &documents {
        roots.push((docs.join("My Games"), true));
        roots.push((docs.clone(), false));
    }
    roots.push((home.join("AppData").join("LocalLow"), true));
    if let Some(roaming) = var("APPDATA") {
        roots.push((roaming, true));
    }
    if let Some(local) = var("LOCALAPPDATA") {
        roots.push((local, true));
    }

    for (root, deep) in roots {
        let level1 = subdirs(&root);
        let hit = level1.iter().find(|d| matches(d)).cloned().or_else(|| {
            if !deep {
                return None;
            }
            level1.iter().flat_map(|d| subdirs(d)).find(|d| matches(d))
        });
        if let Some(dir) = hit {
            // Unreal Engine games keep saves in <Game>\Saved\SaveGames.
            let unreal = dir.join("Saved").join("SaveGames");
            return Some(if unreal.is_dir() { unreal } else { dir });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_acf() {
        let text = "\"AppState\"\n{\n\t\"appid\"\t\t\"1145360\"\n\t\"name\"\t\t\"Hades\"\n\t\"installdir\"\t\t\"Hades\"\n\t\"path\"\t\t\"D:\\\\Steam \\\"Lib\\\"\"\n}\n";
        let pairs = vdf_pairs(text);
        assert_eq!(vdf_get(&pairs, "appid").as_deref(), Some("1145360"));
        assert_eq!(vdf_get(&pairs, "name").as_deref(), Some("Hades"));
        assert_eq!(vdf_get(&pairs, "path").as_deref(), Some("D:\\Steam \"Lib\""));
        assert_eq!(vdf_get(&pairs, "appstate"), None);
    }

    #[test]
    fn identifies_steam_game_from_manifest() {
        let root = std::env::temp_dir().join(format!("statesnap-steam-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let steamapps = root.join("SteamApps");
        let game = steamapps.join("common").join("Disco Elysium");
        fs::create_dir_all(game.join("bin")).unwrap();
        fs::write(steamapps.join("appmanifest_1.acf"), "\"AppState\"\n{\n\t\"appid\"\t\"1\"\n\t\"name\"\t\"Other\"\n\t\"installdir\"\t\"Other\"\n}").unwrap();
        fs::write(
            steamapps.join("appmanifest_632470.acf"),
            "\"AppState\"\n{\n\t\"appid\"\t\"632470\"\n\t\"name\"\t\"Disco Elysium - The Final Cut\"\n\t\"installdir\"\t\"Disco Elysium\"\n}",
        )
        .unwrap();

        let found = identify(&game.join("bin").join("disco.exe"));
        assert_eq!(found.app_id, Some(632470));
        assert_eq!(found.name, "Disco Elysium - The Final Cut");
        assert_eq!(found.dir, game);
        assert_eq!(found.install_name.as_deref(), Some("Disco Elysium"));
        fs::remove_dir_all(&root).unwrap();
    }

    const REMOTECACHE: &str = "\"1790930\"\n{\n\t\"ChangeNumber\"\t\t\"1\"\n\t\"OSType\"\t\t\"0\"\n\t\"Proj/Saved/SaveGames/Slot.sav\"\n\t{\n\t\t\"root\"\t\t\"3\"\n\t\t\"size\"\t\t\"1870\"\n\t}\n\t\"Proj/Saved/SaveGames/Backup/Auto.sav\"\n\t{\n\t\t\"root\"\t\t\"3\"\n\t\t\"size\"\t\t\"5\"\n\t}\n\t\"settings.ini\"\n\t{\n\t\t\"root\"\t\t\"0\"\n\t}\n}\n";

    #[test]
    fn parses_steam_cloud_file_list() {
        let entries = parse_remotecache(REMOTECACHE);
        assert_eq!(
            entries,
            vec![("Proj/Saved/SaveGames/Slot.sav".to_string(), 3), ("Proj/Saved/SaveGames/Backup/Auto.sav".to_string(), 3), ("settings.ini".to_string(), 0)]
        );
    }

    #[test]
    fn finds_shared_folder() {
        assert_eq!(common_dir(&["A/Saved/SaveGames/x.sav", "a/Saved/SaveGames/Backup/y.sav"]), ["A", "Saved", "SaveGames"]);
        assert_eq!(common_dir(&["x.sav", "sub/y.sav"]), Vec::<String>::new());
        assert_eq!(common_dir(&["Game/saves/1.sav"]), ["Game", "saves"]);
    }

    #[test]
    fn cloud_list_resolves_to_the_folder_where_the_files_really_are() {
        let root = std::env::temp_dir().join(format!("statesnap-cloud-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (wrong, right) = (root.join("wrong"), root.join("right"));
        fs::create_dir_all(&wrong).unwrap();
        fs::create_dir_all(right.join("Proj/Saved/SaveGames/Backup")).unwrap();
        fs::write(right.join("Proj/Saved/SaveGames/Slot.sav"), b"s").unwrap();
        let entries = parse_remotecache(REMOTECACHE);

        // The expected base has no such files; the fallback base does.
        let bases = |_root: u32| vec![wrong.clone(), right.clone()];
        assert_eq!(resolve_cloud_dir(&entries, &bases), Some(right.join("Proj").join("Saved").join("SaveGames")));

        // Files listed but present nowhere: no suggestion rather than a wrong one.
        let nowhere = |_root: u32| vec![wrong.clone()];
        assert_eq!(resolve_cloud_dir(&entries, &nowhere), None);

        // Loose files directly in Documents/AppData must never make that whole
        // folder the "save folder".
        fs::write(right.join("loose.sav"), b"s").unwrap();
        let loose = vec![("loose.sav".to_string(), 2)];
        assert_eq!(resolve_cloud_dir(&loose, &bases), None);
        // ...but directly inside Steam's own per-game folder (root 0) is fine.
        let own = vec![("loose.sav".to_string(), 0)];
        assert_eq!(resolve_cloud_dir(&own, &bases), Some(right.clone()));
        fs::remove_dir_all(&root).unwrap();
    }

    /// Diagnostic, not a real test: prints what would be suggested for every
    /// Steam game installed on this machine.
    /// `cargo test real_machine -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_machine_suggestions() {
        let Some(steam) = steam_root() else { return println!("Steam not found") };
        let mut libraries = vec![steam.clone()];
        if let Ok(text) = fs::read_to_string(steam.join("steamapps").join("libraryfolders.vdf")) {
            libraries.extend(vdf_pairs(&text).into_iter().filter(|(k, _)| k == "path").map(|(_, v)| PathBuf::from(v)));
        }
        for library in libraries {
            for game in subdirs(&library.join("steamapps").join("common")) {
                let found = identify(&game.join("game.exe"));
                println!("{} [{:?}]\n    -> {:?}", found.name, found.app_id, suggest_save_dir(&found));
            }
        }
    }

    #[test]
    fn non_steam_program_is_named_after_its_exe() {
        let found = identify(Path::new("C:\\Games\\Cool Game\\cool.exe"));
        assert_eq!(found.name, "cool");
        assert_eq!(found.app_id, None);
        assert_eq!(found.dir, Path::new("C:\\Games\\Cool Game"));
    }

    #[test]
    fn simplifies_names() {
        assert_eq!(simplify("Baldur's Gate 3"), "baldursgate3");
        assert_eq!(simplify("DARK SOULS™ III"), "darksoulsiii");
    }
}

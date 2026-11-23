//! Snapshots are plain folders: `<snapshot_dir>\<game>\<snapshot name>\…`,
//! an exact copy of the game's save folder. No metadata files, so they can be
//! browsed, zipped or copied by hand.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use std::{fs, io, thread};

pub const AUTO_PREFIX: &str = "_auto-backup ";
const PARTIAL_SUFFIX: &str = ".partial";

#[derive(Clone, Debug)]
pub struct SnapshotInfo {
    pub name: String,
    pub path: PathBuf,
    pub created: SystemTime,
    pub files: u64,
    pub bytes: u64,
}

/// Makes a string safe to use as a Windows folder name.
pub fn sanitize(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .take(80)
        .collect();
    s = s.trim().trim_end_matches(['.', ' ']).to_string();
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        s.insert(0, '_');
    }
    s
}

pub fn timestamp_name() -> String {
    chrono::Local::now().format("%Y-%m-%d %H.%M.%S").to_string()
}

pub fn format_time(t: SystemTime) -> String {
    chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M").to_string()
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{} B", bytes);
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{:.1} {}", value, UNITS[unit])
}

/// Folder holding all snapshots of one game.
pub fn game_dir(snapshot_root: &Path, game_name: &str) -> PathBuf {
    let name = sanitize(game_name);
    snapshot_root.join(if name.is_empty() { "_unnamed".to_string() } else { name })
}

/// `wanted` if free, otherwise "wanted (2)", "wanted (3)", …
pub fn unique_path(dir: &Path, wanted: &str) -> PathBuf {
    let first = dir.join(wanted);
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{} ({})", wanted, n)))
        .find(|p| !p.exists())
        .unwrap()
}

fn with_path(e: io::Error, path: &Path) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {}", path.display(), e))
}

/// Copies one file, retrying briefly: games often hold a save file open for a
/// moment while writing it.
fn copy_file(src: &Path, dst: &Path) -> io::Result<u64> {
    let mut attempt = 0;
    loop {
        match fs::copy(src, dst) {
            Ok(n) => return Ok(n),
            Err(e) if attempt < 4 && e.kind() != io::ErrorKind::NotFound => {
                attempt += 1;
                // Windows refuses to overwrite read-only or hidden files, so
                // drop the old destination file before trying again.
                if dst.is_file() {
                    clear_readonly(dst);
                    let _ = fs::remove_file(dst);
                }
                thread::sleep(Duration::from_millis(150));
            }
            Err(e) => return Err(with_path(e, src)),
        }
    }
}

/// Recursively copies `src` into `dst` (overwriting). Returns (files, bytes).
/// Symlinks and junctions are skipped.
pub fn copy_dir(src: &Path, dst: &Path) -> io::Result<(u64, u64)> {
    fs::create_dir_all(dst).map_err(|e| with_path(e, dst))?;
    let (mut files, mut bytes) = (0u64, 0u64);
    for entry in fs::read_dir(src).map_err(|e| with_path(e, src))? {
        let entry = entry.map_err(|e| with_path(e, src))?;
        let kind = entry.file_type().map_err(|e| with_path(e, &entry.path()))?;
        let target = dst.join(entry.file_name());
        if kind.is_symlink() {
            continue;
        } else if kind.is_dir() {
            let (f, b) = copy_dir(&entry.path(), &target)?;
            files += f;
            bytes += b;
        } else {
            bytes += copy_file(&entry.path(), &target)?;
            files += 1;
        }
    }
    Ok((files, bytes))
}

fn dir_stats(dir: &Path) -> (u64, u64) {
    let (mut files, mut bytes) = (0u64, 0u64);
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(t) if t.is_dir() => {
                    let (f, b) = dir_stats(&entry.path());
                    files += f;
                    bytes += b;
                }
                Ok(t) if t.is_file() => {
                    files += 1;
                    bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
                }
                _ => {}
            }
        }
    }
    (files, bytes)
}

/// Removes a folder even if it contains read-only files.
pub fn remove_dir(dir: &Path) -> io::Result<()> {
    clear_readonly(dir);
    fs::remove_dir_all(dir).map_err(|e| with_path(e, dir))
}

fn clear_readonly(path: &Path) {
    let Ok(meta) = fs::symlink_metadata(path) else { return };
    if meta.permissions().readonly() {
        let mut perm = meta.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        let _ = fs::set_permissions(path, perm);
    }
    if meta.is_dir() && !meta.file_type().is_symlink() {
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                clear_readonly(&entry.path());
            }
        }
    }
}

/// Lists a game's snapshots, newest first.
pub fn list(game_dir: &Path) -> Vec<SnapshotInfo> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(game_dir) else { return out };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_dir() || name.ends_with(PARTIAL_SUFFIX) {
            continue;
        }
        let path = entry.path();
        let (files, bytes) = dir_stats(&path);
        let created = meta.created().or_else(|_| meta.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
        out.push(SnapshotInfo { name, path, created, files, bytes });
    }
    out.sort_by(|a, b| b.created.cmp(&a.created));
    out
}

/// Copies the save folder into a new snapshot. The copy goes to a `.partial`
/// folder first and is renamed at the end, so a failed copy never looks like
/// a valid snapshot.
pub fn create(save_path: &Path, game_dir: &Path, wanted_name: &str) -> io::Result<SnapshotInfo> {
    if !save_path.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("{}: save folder not found", save_path.display())));
    }
    fs::create_dir_all(game_dir).map_err(|e| with_path(e, game_dir))?;

    let mut name = sanitize(wanted_name);
    if name.is_empty() {
        name = timestamp_name();
    }
    let final_path = unique_path(game_dir, &name);
    let name = final_path.file_name().unwrap().to_string_lossy().into_owned();
    let partial = game_dir.join(format!("{}{}", name, PARTIAL_SUFFIX));
    if partial.exists() {
        remove_dir(&partial)?;
    }

    let (files, bytes) = match copy_dir(save_path, &partial) {
        Ok(stats) => stats,
        Err(e) => {
            let _ = remove_dir(&partial);
            return Err(e);
        }
    };
    if files == 0 {
        let _ = remove_dir(&partial);
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("{}: no files to copy", save_path.display())));
    }
    fs::rename(&partial, &final_path).map_err(|e| with_path(e, &final_path))?;
    Ok(SnapshotInfo { name, path: final_path, created: SystemTime::now(), files, bytes })
}

/// Makes `save_path` an exact copy of `snapshot`: snapshot files are copied
/// over, then anything in the save folder that the snapshot doesn't have is
/// removed (a leftover newer save could otherwise still be picked by the game).
pub fn restore(snapshot: &Path, save_path: &Path) -> io::Result<(u64, u64)> {
    if !snapshot.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("{}: snapshot not found", snapshot.display())));
    }
    let stats = copy_dir(snapshot, save_path)?;
    remove_extras(save_path, snapshot)?;
    Ok(stats)
}

fn remove_extras(dest: &Path, reference: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dest).map_err(|e| with_path(e, dest))? {
        let entry = entry.map_err(|e| with_path(e, dest))?;
        let path = entry.path();
        let twin = reference.join(entry.file_name());
        let kind = entry.file_type().map_err(|e| with_path(e, &path))?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            if twin.is_dir() {
                remove_extras(&path, &twin)?;
            } else {
                remove_dir(&path)?;
            }
        } else if !twin.is_file() {
            clear_readonly(&path);
            fs::remove_file(&path).map_err(|e| with_path(e, &path))?;
        }
    }
    Ok(())
}

/// Backs up the current saves before a restore. Returns None when there is
/// nothing to back up.
pub fn auto_backup(save_path: &Path, game_dir: &Path) -> io::Result<Option<SnapshotInfo>> {
    if !save_path.is_dir() || dir_stats(save_path).0 == 0 {
        return Ok(None);
    }
    create(save_path, game_dir, &format!("{}{}", AUTO_PREFIX, timestamp_name())).map(Some)
}

/// Keeps only the newest `keep` automatic backups. Run this after the restore
/// has finished: the snapshot being restored may itself be an old auto backup.
pub fn prune_auto_backups(game_dir: &Path, keep: usize) {
    let autos: Vec<_> = list(game_dir).into_iter().filter(|s| s.name.starts_with(AUTO_PREFIX)).collect();
    for old in autos.iter().skip(keep.max(1)) {
        let _ = remove_dir(&old.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("statesnap-test-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sanitizes() {
        assert_eq!(sanitize("  before: boss?  "), "before_ boss_");
        assert_eq!(sanitize("ending B..."), "ending B");
        assert_eq!(sanitize("con"), "_con");
        assert_eq!(sanitize("COM1.txt"), "_COM1.txt");
        assert_eq!(sanitize("   "), "");
        assert_eq!(sanitize("café ending č.2"), "café ending č.2"); // non-ASCII is kept
    }

    #[test]
    fn sizes() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn snapshot_and_restore_roundtrip() {
        let root = temp("roundtrip");
        let saves = root.join("saves");
        let snaps = root.join("snaps");
        fs::create_dir_all(saves.join("slot1")).unwrap();
        fs::write(saves.join("profile.sav"), b"v1").unwrap();
        fs::write(saves.join("slot1").join("a.sav"), b"aaaa").unwrap();

        let first = create(&saves, &snaps, "before boss").unwrap();
        assert_eq!((first.files, first.bytes), (2, 6));
        assert_eq!(first.name, "before boss");
        // Same name again gets a suffix instead of overwriting.
        assert_eq!(create(&saves, &snaps, "before boss").unwrap().name, "before boss (2)");
        // Empty name falls back to a timestamp.
        assert!(!create(&saves, &snaps, "").unwrap().name.is_empty());

        // The game moves on: one file changes, one is added, one folder disappears.
        fs::write(saves.join("profile.sav"), b"v2-longer").unwrap();
        fs::write(saves.join("new.sav"), b"new").unwrap();
        fs::create_dir_all(saves.join("slot2")).unwrap();
        fs::write(saves.join("slot2").join("b.sav"), b"b").unwrap();
        fs::remove_dir_all(saves.join("slot1")).unwrap();

        let backup = auto_backup(&saves, &snaps).unwrap().unwrap();
        assert!(backup.name.starts_with(AUTO_PREFIX));

        restore(&first.path, &saves).unwrap();
        assert_eq!(fs::read(saves.join("profile.sav")).unwrap(), b"v1");
        assert_eq!(fs::read(saves.join("slot1").join("a.sav")).unwrap(), b"aaaa");
        assert!(!saves.join("new.sav").exists());
        assert!(!saves.join("slot2").exists());

        // And the pre-restore state can be brought back from the auto backup.
        restore(&backup.path, &saves).unwrap();
        assert_eq!(fs::read(saves.join("profile.sav")).unwrap(), b"v2-longer");
        assert!(saves.join("slot2").join("b.sav").exists());
        assert!(!saves.join("slot1").exists());

        assert_eq!(list(&snaps).len(), 4);
        assert!(list(&snaps).iter().all(|s| !s.name.ends_with(PARTIAL_SUFFIX)));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn empty_or_missing_save_folder_is_an_error() {
        let root = temp("empty");
        let saves = root.join("saves");
        let snaps = root.join("snaps");
        assert!(create(&saves, &snaps, "x").is_err());
        fs::create_dir_all(&saves).unwrap();
        assert!(create(&saves, &snaps, "x").is_err());
        assert!(list(&snaps).is_empty());
        assert!(!snaps.join("x.partial").exists());
        assert!(auto_backup(&saves, &snaps).unwrap().is_none());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn auto_backups_are_pruned() {
        let root = temp("prune");
        let saves = root.join("saves");
        let snaps = root.join("snaps");
        fs::create_dir_all(&saves).unwrap();
        fs::write(saves.join("a"), b"1").unwrap();
        create(&saves, &snaps, "keep me").unwrap();
        for _ in 0..4 {
            auto_backup(&saves, &snaps).unwrap();
            prune_auto_backups(&snaps, 2);
        }
        let all = list(&snaps);
        assert_eq!(all.iter().filter(|s| s.name.starts_with(AUTO_PREFIX)).count(), 2);
        assert!(all.iter().any(|s| s.name == "keep me"));
        fs::remove_dir_all(&root).unwrap();
    }
}

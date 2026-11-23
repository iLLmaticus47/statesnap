//! Config file: `%APPDATA%\StateSnap\config.toml`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::{env, fs, io};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    /// "auto", "en" or "sr".
    pub language: String,
    /// Root folder for all snapshots: `<snapshot_dir>\<game>\<snapshot>\…`.
    pub snapshot_dir: PathBuf,
    /// Beep when a snapshot finishes (useful in fullscreen, where no popup is visible).
    pub sound: bool,
    /// How many automatic pre-restore backups to keep per game.
    pub auto_backups_kept: usize,
    /// Freeze the game while the snapshot-name popup is open.
    pub pause_game: bool,
    /// Game used when the foreground window is not a known game.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_game: Option<String>,
    pub hotkeys: Hotkeys,
    pub games: Vec<Game>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Hotkeys {
    /// Asks for a name, then snapshots.
    pub named: String,
    /// Snapshots immediately, named by date and time.
    pub quick: String,
    /// Opens the manager window.
    pub manager: String,
}

impl Hotkeys {
    pub fn as_array(&self) -> [String; 3] {
        [self.named.clone(), self.quick.clone(), self.manager.clone()]
    }

    pub fn from_array([named, quick, manager]: [String; 3]) -> Hotkeys {
        Hotkeys { named, quick, manager }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Game {
    pub name: String,
    pub save_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<u32>,
    /// Install folder; a foreground program running from inside it is this game.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_dir: Option<PathBuf>,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Hotkeys {
            named: "Ctrl+Alt+F5".into(),
            quick: "Ctrl+Alt+F6".into(),
            manager: "Ctrl+Alt+F9".into(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: "en".into(),
            snapshot_dir: default_snapshot_dir(),
            sound: true,
            auto_backups_kept: 5,
            pause_game: true,
            last_game: None,
            hotkeys: Hotkeys::default(),
            games: Vec::new(),
        }
    }
}

fn home() -> PathBuf {
    env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\"))
}

fn default_snapshot_dir() -> PathBuf {
    let docs = home().join("Documents");
    if docs.is_dir() {
        docs.join("StateSnap")
    } else {
        home().join("StateSnap")
    }
}

pub fn app_dir() -> PathBuf {
    env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(home)
        .join("StateSnap")
}

pub fn config_path() -> PathBuf {
    app_dir().join("config.toml")
}

impl Config {
    /// Loads the config. The bool is true when an unreadable file had to be
    /// set aside (kept as `config.toml.bad`) and defaults were used instead.
    pub fn load() -> (Config, bool) {
        let path = config_path();
        match fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => (cfg, false),
                Err(_) => {
                    let _ = fs::rename(&path, path.with_extension("toml.bad"));
                    let cfg = Config::default();
                    let _ = cfg.save();
                    (cfg, true)
                }
            },
            Err(_) => {
                let cfg = Config::default();
                let _ = cfg.save(); // write a starter file the user can edit
                (cfg, false)
            }
        }
    }

    pub fn save(&self) -> io::Result<()> {
        let path = config_path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        // Write to a temp file and swap, so a crash never leaves half a config.
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &path)
    }

    pub fn game_index(&self, name: &str) -> Option<usize> {
        self.games.iter().position(|g| g.name.eq_ignore_ascii_case(name))
    }

    /// Finds the game whose install folder contains `exe`.
    pub fn game_by_exe(&self, exe: &Path) -> Option<usize> {
        self.games.iter().position(|g| match &g.match_dir {
            Some(dir) => is_inside(exe, dir),
            None => false,
        })
    }

    pub fn game_by_app_id(&self, app_id: u32) -> Option<usize> {
        self.games.iter().position(|g| g.app_id == Some(app_id))
    }
}

/// Lowercased path with backslashes and no trailing separator, for comparisons.
pub fn norm(path: &Path) -> String {
    let s = path.to_string_lossy().replace('/', "\\").to_lowercase();
    let s = s.strip_prefix("\\\\?\\").unwrap_or(&s);
    s.trim_end_matches('\\').to_string()
}

/// True when `path` is `dir` itself or somewhere below it (case-insensitive).
pub fn is_inside(path: &Path, dir: &Path) -> bool {
    let (p, d) = (norm(path), norm(dir));
    if d.is_empty() {
        return false;
    }
    p == d || p.starts_with(&(d + "\\"))
}

/// Refuses save folders where a restore (which mirrors the folder) would be dangerous.
pub fn save_path_is_safe(save_path: &Path, snapshot_dir: &Path) -> bool {
    let p = norm(save_path);
    // Drive root ("c:") or UNC share root.
    if p.len() <= 2 || p.matches('\\').count() == 0 {
        return false;
    }
    if p == norm(&home()) {
        return false;
    }
    !is_inside(save_path, snapshot_dir) && !is_inside(snapshot_dir, save_path)
}

pub const MOD_ALT: u32 = 0x0001;
pub const MOD_CONTROL: u32 = 0x0002;
pub const MOD_SHIFT: u32 = 0x0004;
pub const MOD_WIN: u32 = 0x0008;

/// Parses "Ctrl+Shift+F5" into (modifiers, virtual key code).
pub fn parse_hotkey(text: &str) -> Option<(u32, u32)> {
    let mut mods = 0u32;
    let mut key = None;
    for part in text.split('+').map(|p| p.trim().to_ascii_uppercase()) {
        match part.as_str() {
            "CTRL" | "CONTROL" => mods |= MOD_CONTROL,
            "ALT" => mods |= MOD_ALT,
            "SHIFT" => mods |= MOD_SHIFT,
            "WIN" | "SUPER" => mods |= MOD_WIN,
            "" => return None,
            other => {
                if key.is_some() {
                    return None;
                }
                key = Some(parse_key(other)?);
            }
        }
    }
    key.map(|k| (mods, k))
}

/// Keys that may be used without Ctrl/Alt/Shift/Win, because games and other
/// programs practically never use them on their own.
pub fn key_ok_alone(vk: u32) -> bool {
    (0x7C..=0x87).contains(&vk) // F13–F24
        || vk == 0x13 // Pause
        || vk == 0x91 // Scroll Lock
}

/// Turns (modifiers, virtual key) into text like "Ctrl+Alt+F5", in the same
/// format `parse_hotkey` reads. None for keys that can't be named.
pub fn format_hotkey(mods: u32, vk: u32) -> Option<String> {
    let mut out = format_modifiers(mods);
    out.push_str(&key_name(vk)?);
    Some(out)
}

/// "Ctrl+Alt+" etc. (with trailing plus), or "" with no modifiers.
pub fn format_modifiers(mods: u32) -> String {
    let mut out = String::new();
    for (bit, name) in [(MOD_CONTROL, "Ctrl"), (MOD_ALT, "Alt"), (MOD_SHIFT, "Shift"), (MOD_WIN, "Win")] {
        if mods & bit != 0 {
            out.push_str(name);
            out.push('+');
        }
    }
    out
}

fn key_name(vk: u32) -> Option<String> {
    Some(match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from_u32(vk)?.to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x70 + 1),
        0x60..=0x69 => format!("Num{}", vk - 0x60),
        0x2D => "Insert".into(),
        0x2E => "Delete".into(),
        0x24 => "Home".into(),
        0x23 => "End".into(),
        0x21 => "PageUp".into(),
        0x22 => "PageDown".into(),
        0x13 => "Pause".into(),
        0x91 => "ScrollLock".into(),
        0x2C => "PrintScreen".into(),
        0x26 => "Up".into(),
        0x28 => "Down".into(),
        0x25 => "Left".into(),
        0x27 => "Right".into(),
        0x20 => "Space".into(),
        0x08 => "Backspace".into(),
        _ => return None,
    })
}

fn parse_key(name: &str) -> Option<u32> {
    let bytes = name.as_bytes();
    if bytes.len() == 1 && (bytes[0].is_ascii_uppercase() || bytes[0].is_ascii_digit()) {
        return Some(bytes[0] as u32);
    }
    if let Some(n) = name.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
        if (1..=24).contains(&n) {
            return Some(0x70 + n - 1);
        }
    }
    if let Some(n) = name.strip_prefix("NUM").and_then(|n| n.parse::<u32>().ok()) {
        if n <= 9 {
            return Some(0x60 + n);
        }
    }
    Some(match name {
        "INSERT" | "INS" => 0x2D,
        "DELETE" | "DEL" => 0x2E,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" | "PGUP" => 0x21,
        "PAGEDOWN" | "PGDN" => 0x22,
        "PAUSE" => 0x13,
        "SCROLLLOCK" => 0x91,
        "PRINTSCREEN" | "PRTSC" => 0x2C,
        "UP" => 0x26,
        "DOWN" => 0x28,
        "LEFT" => 0x25,
        "RIGHT" => 0x27,
        "SPACE" => 0x20,
        "TAB" => 0x09,
        "BACKSPACE" => 0x08,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkeys() {
        assert_eq!(parse_hotkey("Ctrl+Shift+F5"), Some((MOD_CONTROL | MOD_SHIFT, 0x74)));
        assert_eq!(parse_hotkey("alt + s"), Some((MOD_ALT, 'S' as u32)));
        assert_eq!(parse_hotkey("F24"), Some((0, 0x87)));
        assert_eq!(parse_hotkey("Ctrl+Num3"), Some((MOD_CONTROL, 0x63)));
        assert_eq!(parse_hotkey("Ctrl+"), None);
        assert_eq!(parse_hotkey("Ctrl+A+B"), None);
        assert_eq!(parse_hotkey("Ctrl+Banana"), None);
    }

    #[test]
    fn formatted_hotkeys_parse_back() {
        assert_eq!(format_hotkey(MOD_CONTROL | MOD_ALT, 0x74).as_deref(), Some("Ctrl+Alt+F5"));
        assert_eq!(format_hotkey(MOD_SHIFT | MOD_WIN, 'S' as u32).as_deref(), Some("Shift+Win+S"));
        assert_eq!(format_hotkey(MOD_CONTROL, 0xBA), None); // ';' has no name
        let keys = (0x08..=0xFF).filter(|vk| key_name(*vk).is_some());
        for vk in keys {
            for mods in [0, MOD_CONTROL, MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_WIN] {
                let text = format_hotkey(mods, vk).unwrap();
                assert_eq!(parse_hotkey(&text), Some((mods, vk)), "{}", text);
            }
        }
    }

    #[test]
    fn inside() {
        assert!(is_inside(Path::new("D:\\Steam\\steamapps\\common\\Hades\\x64\\Hades.exe"), Path::new("d:/steam/steamapps/common/hades/")));
        assert!(!is_inside(Path::new("D:\\Games\\Hades2\\a.exe"), Path::new("D:\\Games\\Hades")));
        assert!(is_inside(Path::new("C:\\a"), Path::new("C:\\a")));
    }

    #[test]
    fn unsafe_paths() {
        let snaps = Path::new("C:\\Users\\x\\Documents\\StateSnap");
        assert!(!save_path_is_safe(Path::new("C:\\"), snaps));
        assert!(!save_path_is_safe(Path::new("C:\\Users\\x\\Documents"), snaps));
        assert!(!save_path_is_safe(Path::new("C:\\Users\\x\\Documents\\StateSnap\\Hades"), snaps));
        assert!(save_path_is_safe(Path::new("C:\\Users\\x\\Saved Games\\Hades"), snaps));
    }

    #[test]
    fn roundtrip() {
        let mut cfg = Config::default();
        cfg.games.push(Game { name: "Hades".into(), save_path: "C:\\saves".into(), app_id: Some(1145360), match_dir: None });
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.games[0].app_id, Some(1145360));
        assert_eq!(back.hotkeys.quick, "Ctrl+Alt+F6");
        // A partial file still loads, with defaults filled in.
        let partial: Config = toml::from_str("sound = false").unwrap();
        assert!(!partial.sound);
        assert_eq!(partial.auto_backups_kept, 5);
    }
}

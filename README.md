# StateSnap

A small Windows tray tool that copies a game's save files into a named snapshot with one shortcut, and puts any snapshot back into the game with one click. Useful for keeping saves from before a big choice, so you can see every ending without replaying the game.

It uses native Windows controls only. While idle it uses no CPU and a few MB of memory.

## Download

Get `statesnap.exe` from the [latest release](https://github.com/iLLmaticus47/statesnap/releases/latest)
and run it. There is nothing to install: the exe is self-contained (about 650 KB) and can be kept
anywhere. Only one copy runs at a time; starting it again opens the existing window.

The exe is not code-signed, so Windows SmartScreen may warn the first time you run it. Click
**More info → Run anyway**.

## Build it yourself

You need [Rust](https://rustup.rs). Then:

```
cargo build --release
target\release\statesnap.exe
```

## Shortcuts

Defaults (change them in **Settings**):

| Shortcut | What it does |
|---|---|
| `Ctrl+Alt+F5` | Asks for a name, then takes a snapshot (Enter saves, Esc cancels) |
| `Ctrl+Alt+F6` | Takes a snapshot right away, named by date and time. Best for exclusive-fullscreen games, where a popup would minimize the game |
| `Ctrl+Alt+F9` | Opens the StateSnap window |

To change a shortcut: open **Settings**, click the shortcut's box and press the new key combination. Backspace turns a shortcut off. If another program already uses a combination, StateSnap says so and lets you pick another. Changes apply immediately, with no restart.

## The window

- **Game**: the game whose snapshots are listed. **Add…** adds a game by hand, **Edit…** changes its name or save folder, **Remove** takes it off the list (snapshots stay on disk).
- **New snapshot…**: same as the named-snapshot shortcut.
- **Restore into game**: puts the selected snapshot back (double-clicking a snapshot does the same).
- **Rename… / Delete**: for the selected snapshot.
- **Show in Explorer**: opens File Explorer so you can look at or copy the snapshot files. Nothing needs to be chosen there.
- **Settings…**: shortcuts, snapshot folder, language, sound, start with Windows.

Hover over any button for a short explanation. The tray icon opens the window on left-click and shows a menu on right-click.

## How it works

- **Finding the game.** When you press a shortcut, StateSnap looks at the window in front. Steam games are recognised from Steam's install manifests. The first time, it shows the game's name and save folder for you to confirm; after that it remembers the game.
- **Finding the save folder.** In order: Steam Cloud's own list of the game's save files (`userdata\<account>\<appid>\remotecache.vdf`), the standard Unreal Engine and Unity locations, then a folder named after the game in Documents, Saved Games or AppData. If none of these finds it, you choose the folder yourself; PCGamingWiki lists save locations. An empty folder is refused, and if a game's folder turns out to be empty later, StateSnap asks again instead of failing.
- **Pausing.** While the snapshot-name popup is open, the game process is frozen and continues when the popup closes. This only happens when the window in front was recognised as the game. Turn it off in Settings for online games, where a frozen client can be disconnected.
- **Errors** (for example nothing to copy) appear in a message box on top of the game.
- **Non-Steam games** are added with **Add…**. The shortcuts then use the last selected game.
- **Snapshots are plain folders**: `Documents\StateSnap\<game>\<snapshot name>\`.
- **Restoring** makes the save folder an exact copy of the snapshot, including removing newer files that aren't in it. The current saves are always backed up first as `_auto-backup …` (the last 5 are kept), so a restore can be undone.
- Restore from the game's main menu or with the game closed. If the game uses Steam Cloud and Steam asks which version to keep, pick the local one.

## Settings file

Settings are stored in `%APPDATA%\StateSnap\config.toml` and are normally changed from the Settings window.

## Code map

- `src/main.rs` — startup, single-instance check, crash log
- `src/ui.rs` — tray icon, global shortcuts and the manager, snapshot and settings windows
- `src/snapshot.rs` — taking, restoring, renaming and deleting snapshots, automatic backups
- `src/steam.rs` — recognising the game in front and guessing its save folder
- `src/config.rs` — the settings file
- `src/lang.rs` — interface text in English and Serbian
- `src/winutil.rs` — small wrappers over the Win32 calls the app needs

## License

MIT — see [LICENSE](LICENSE).

StateSnap is not affiliated with Valve or Steam. It only reads files Steam already keeps on your
computer.

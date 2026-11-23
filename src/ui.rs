//! Tray icon, global hotkeys and the three small windows (manager, snapshot
//! name, game setup). Native Win32 controls only: the app idles at 0% CPU and
//! a few MB of RAM while a game is running.
//!
//! Rule used throughout: never hold a `state` borrow across a call that can
//! pump window messages (message boxes, dialogs, showing windows), because
//! those can re-enter the event handler.

use crate::config::{self, Config, Game};
use crate::lang::{self, fill, Strings};
use crate::snapshot::{self, SnapshotInfo};
use crate::steam::{self, Detected};
use crate::winutil;
use native_windows_gui as nwg;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::{fs, thread};

pub const MAIN_TITLE: &str = "StateSnap";
/// Posted by a second instance to make the running one show its window (WM_APP + 1).
pub const WM_SHOW_MANAGER: u32 = 0x8001;
const WM_HOTKEY: u32 = 0x0312;

const HK_NAMED: i32 = 1;
const HK_QUICK: i32 = 2;
const HK_MANAGER: i32 = 3;

static ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

#[derive(Default)]
struct Ui {
    // Manager window
    main: nwg::Window,
    lbl_game: nwg::Label,
    cb_game: nwg::ComboBox<String>,
    btn_add: nwg::Button,
    btn_edit: nwg::Button,
    btn_remove: nwg::Button,
    list: nwg::ListView,
    btn_snapshot: nwg::Button,
    btn_restore: nwg::Button,
    btn_rename: nwg::Button,
    btn_delete: nwg::Button,
    btn_open: nwg::Button,
    btn_settings: nwg::Button,
    lbl_status: nwg::Label,
    lbl_hint: nwg::Label,
    tooltip: nwg::Tooltip,

    // Settings dialog
    set_dlg: nwg::Window,
    set_hint: nwg::Label,
    set_labels: [nwg::Label; 6],
    /// Shortcut capture boxes: named, quick, manager.
    set_keys: [nwg::TextInput; 3],
    set_folder: nwg::TextInput,
    set_browse: nwg::Button,
    set_backups: nwg::TextInput,
    set_lang: nwg::ComboBox<&'static str>,
    set_sound: nwg::CheckBox,
    set_autostart: nwg::CheckBox,
    set_pause: nwg::CheckBox,
    set_note: nwg::Label,
    set_ok: nwg::Button,
    set_cancel: nwg::Button,

    // Snapshot name popup
    name_dlg: nwg::Window,
    name_label: nwg::Label,
    name_input: nwg::TextInput,
    name_hint: nwg::Label,
    name_ok: nwg::Button,
    name_cancel: nwg::Button,

    // Game setup dialog
    game_dlg: nwg::Window,
    game_hint: nwg::Label,
    game_name_label: nwg::Label,
    game_name: nwg::TextInput,
    game_path_label: nwg::Label,
    game_path: nwg::TextInput,
    game_browse: nwg::Button,
    game_ok: nwg::Button,
    game_cancel: nwg::Button,

    // Tray
    tray: nwg::TrayNotification,
    tray_menu: nwg::Menu,
    mi_open: nwg::MenuItem,
    mi_quick: nwg::MenuItem,
    mi_sep1: nwg::MenuSeparator,
    mi_folder: nwg::MenuItem,
    mi_settings: nwg::MenuItem,
    mi_sep2: nwg::MenuSeparator,
    mi_exit: nwg::MenuItem,

    /// Wakes the UI thread when a background copy finishes.
    notice: nwg::Notice,
    icon_small: nwg::Icon,
    icon_big: nwg::Icon,
    font: nwg::Font,
}

#[derive(Clone, Copy)]
enum Then {
    Nothing,
    NamedSnapshot,
    QuickSnapshot,
}

enum NameMode {
    Idle,
    New { game: usize },
    Rename { path: PathBuf },
}

enum GameMode {
    Idle,
    Add { detected: Option<Detected>, then: Then },
    Edit { index: usize, then: Then },
}

struct State {
    cfg: Config,
    /// Snapshots currently shown in the list, same order as the rows.
    snapshots: Vec<SnapshotInfo>,
    name_mode: NameMode,
    game_mode: GameMode,
    /// Window (normally the game) to hand focus back to when a popup closes.
    prev_fg: isize,
    /// Game process to pause while the name popup is open (0 = none).
    game_pid: u32,
    /// A background copy is running.
    busy: bool,
    /// Shortcuts as currently entered in the Settings dialog (not yet saved).
    captured: [String; 3],
}

/// Language codes in the order they appear in the Settings dropdown.
const LANG_CODES: [&str; 3] = ["en", "sr", "auto"];
const RAW_ID_HOTKEYS: usize = 0x1_0001;
const RAW_ID_CAPTURE: usize = 0x1_0010;
const TIMER_PAUSE: usize = 1;
const WM_TIMER: u32 = 0x0113;

enum Job {
    Snapshot { game: String, result: Result<SnapshotInfo, String> },
    Restore { name: String, result: Result<(), String> },
}

enum Target {
    Game(usize),
    Setup(Option<Detected>),
}

pub struct App {
    ui: Ui,
    s: &'static Strings,
    state: RefCell<State>,
    tx: Sender<Job>,
    rx: Receiver<Job>,
}

fn load_icon(size: i32) -> nwg::Icon {
    let handle = winutil::icon_from_png(ICON_PNG, size);
    if handle.is_null() {
        return nwg::Icon::from_system(nwg::OemIcon::Information);
    }
    let mut icon = nwg::Icon::default();
    icon.handle = handle as _;
    icon
}

fn hotkey_hint(s: &Strings, hotkeys: &config::Hotkeys) -> String {
    let show = |k: &String| if k.trim().is_empty() { s.hotkey_off.to_string() } else { k.clone() };
    fill(s.hint_hotkeys, &[&show(&hotkeys.named), &show(&hotkeys.quick), &show(&hotkeys.manager)])
}

fn build_ui(s: &Strings, hotkeys: &config::Hotkeys) -> Result<Ui, nwg::NwgError> {
    use nwg::{ButtonFlags as BF, WindowFlags as WF};
    let mut ui = Ui::default();
    ui.icon_small = load_icon(winutil::small_icon_size());
    ui.icon_big = load_icon(winutil::small_icon_size() * 2);
    nwg::Font::builder().family("Segoe UI").size(16).build(&mut ui.font)?;
    let font = Some(&ui.font);
    let btn = BF::VISIBLE | BF::TAB_STOP;

    // ---- Manager window (starts hidden; lives in the tray) ----
    nwg::Window::builder()
        .flags(WF::WINDOW | WF::MINIMIZE_BOX)
        .size((680, 446))
        .center(true)
        .title(MAIN_TITLE)
        .icon(Some(&ui.icon_big))
        .build(&mut ui.main)?;
    nwg::Label::builder().parent(&ui.main).text(s.game).font(font).position((12, 15)).size((50, 22)).build(&mut ui.lbl_game)?;
    nwg::ComboBox::builder().parent(&ui.main).font(font).position((64, 12)).size((298, 26)).build(&mut ui.cb_game)?;
    nwg::Button::builder().parent(&ui.main).text(s.add).font(font).flags(btn).position((372, 10)).size((94, 28)).build(&mut ui.btn_add)?;
    nwg::Button::builder().parent(&ui.main).text(s.edit).font(font).flags(btn).position((472, 10)).size((94, 28)).build(&mut ui.btn_edit)?;
    nwg::Button::builder().parent(&ui.main).text(s.remove).font(font).flags(btn).position((572, 10)).size((96, 28)).build(&mut ui.btn_remove)?;

    nwg::ListView::builder()
        .parent(&ui.main)
        .list_style(nwg::ListViewStyle::Detailed)
        .flags(nwg::ListViewFlags::VISIBLE | nwg::ListViewFlags::SINGLE_SELECTION | nwg::ListViewFlags::ALWAYS_SHOW_SELECTION | nwg::ListViewFlags::TAB_STOP)
        .ex_flags(nwg::ListViewExFlags::FULL_ROW_SELECT | nwg::ListViewExFlags::GRID)
        .position((12, 50))
        .size((496, 330))
        .build(&mut ui.list)?;
    // Widths are logical pixels; the toolkit applies the DPI scale itself.
    for (i, (text, width)) in [(s.col_name, 210), (s.col_date, 124), (s.col_size, 76), (s.col_files, 56)].into_iter().enumerate() {
        ui.list.insert_column(nwg::InsertListViewColumn {
            index: Some(i as i32),
            fmt: None,
            width: Some(width),
            text: Some(text.to_string()),
        });
    }
    ui.list.set_headers_enabled(true);

    let side = |text: &'static str, y: i32, out: &mut nwg::Button, ui_main: &nwg::Window, font: Option<&nwg::Font>| {
        nwg::Button::builder().parent(ui_main).text(text).font(font).flags(btn).position((518, y)).size((150, 30)).build(out)
    };
    side(s.btn_snapshot, 50, &mut ui.btn_snapshot, &ui.main, font)?;
    side(s.btn_restore, 86, &mut ui.btn_restore, &ui.main, font)?;
    side(s.btn_rename, 134, &mut ui.btn_rename, &ui.main, font)?;
    side(s.btn_delete, 170, &mut ui.btn_delete, &ui.main, font)?;
    side(s.btn_open, 218, &mut ui.btn_open, &ui.main, font)?;
    side(s.btn_settings, 350, &mut ui.btn_settings, &ui.main, font)?;

    nwg::Label::builder().parent(&ui.main).text(s.ready).font(font).position((12, 388)).size((656, 22)).build(&mut ui.lbl_status)?;
    nwg::Label::builder().parent(&ui.main).text(&hotkey_hint(s, hotkeys)).font(font).position((12, 414)).size((656, 22)).build(&mut ui.lbl_hint)?;

    // ---- Snapshot name popup (topmost so it shows over the game) ----
    nwg::Window::builder()
        .flags(WF::WINDOW)
        .size((420, 142))
        .center(true)
        .topmost(true)
        .title(s.name_title_new)
        .icon(Some(&ui.icon_big))
        .build(&mut ui.name_dlg)?;
    nwg::Label::builder().parent(&ui.name_dlg).text("").font(font).position((14, 10)).size((392, 22)).build(&mut ui.name_label)?;
    nwg::TextInput::builder()
        .parent(&ui.name_dlg)
        .font(font)
        .limit(80)
        .placeholder_text(Some(s.name_placeholder))
        .position((14, 36))
        .size((392, 26))
        .build(&mut ui.name_input)?;
    nwg::Label::builder().parent(&ui.name_dlg).text(s.name_hint).font(font).position((14, 68)).size((392, 22)).build(&mut ui.name_hint)?;
    nwg::Button::builder().parent(&ui.name_dlg).text(s.ok).font(font).flags(btn).position((218, 102)).size((90, 28)).build(&mut ui.name_ok)?;
    nwg::Button::builder().parent(&ui.name_dlg).text(s.cancel).font(font).flags(btn).position((316, 102)).size((90, 28)).build(&mut ui.name_cancel)?;

    // ---- Game setup dialog ----
    nwg::Window::builder()
        .flags(WF::WINDOW)
        .size((640, 172))
        .center(true)
        .topmost(true)
        .title(s.game_title_add)
        .icon(Some(&ui.icon_big))
        .build(&mut ui.game_dlg)?;
    nwg::Label::builder().parent(&ui.game_dlg).text("").font(font).position((14, 10)).size((612, 22)).build(&mut ui.game_hint)?;
    nwg::Label::builder().parent(&ui.game_dlg).text(s.game_name).font(font).position((14, 47)).size((92, 22)).build(&mut ui.game_name_label)?;
    nwg::TextInput::builder().parent(&ui.game_dlg).font(font).limit(80).position((110, 44)).size((516, 26)).build(&mut ui.game_name)?;
    nwg::Label::builder().parent(&ui.game_dlg).text(s.game_path).font(font).position((14, 83)).size((92, 22)).build(&mut ui.game_path_label)?;
    nwg::TextInput::builder().parent(&ui.game_dlg).font(font).position((110, 80)).size((416, 26)).build(&mut ui.game_path)?;
    nwg::Button::builder().parent(&ui.game_dlg).text(s.browse).font(font).flags(btn).position((534, 79)).size((92, 28)).build(&mut ui.game_browse)?;
    nwg::Button::builder().parent(&ui.game_dlg).text(s.ok).font(font).flags(btn).position((438, 130)).size((90, 28)).build(&mut ui.game_ok)?;
    nwg::Button::builder().parent(&ui.game_dlg).text(s.cancel).font(font).flags(btn).position((536, 130)).size((90, 28)).build(&mut ui.game_cancel)?;

    // ---- Settings dialog ----
    nwg::Window::builder()
        .flags(WF::WINDOW)
        .size((560, 458))
        .center(true)
        .title(s.set_title)
        .icon(Some(&ui.icon_big))
        .build(&mut ui.set_dlg)?;
    nwg::Label::builder().parent(&ui.set_dlg).text(s.set_hint).font(font).position((14, 10)).size((532, 42)).build(&mut ui.set_hint)?;
    let label_texts = [s.set_named, s.set_quick, s.set_manager, s.set_folder, s.set_backups, s.set_language];
    let label_ys = [65, 99, 133, 183, 217, 251];
    for i in 0..6 {
        nwg::Label::builder().parent(&ui.set_dlg).text(label_texts[i]).font(font).position((14, label_ys[i])).size((166, 22)).build(&mut ui.set_labels[i])?;
    }
    for i in 0..3 {
        nwg::TextInput::builder()
            .parent(&ui.set_dlg)
            .font(font)
            .placeholder_text(Some(s.set_press))
            .position((184, label_ys[i] - 3))
            .size((362, 26))
            .build(&mut ui.set_keys[i])?;
    }
    nwg::TextInput::builder().parent(&ui.set_dlg).font(font).position((184, 180)).size((262, 26)).build(&mut ui.set_folder)?;
    nwg::Button::builder().parent(&ui.set_dlg).text(s.browse).font(font).flags(btn).position((454, 179)).size((92, 28)).build(&mut ui.set_browse)?;
    nwg::TextInput::builder()
        .parent(&ui.set_dlg)
        .font(font)
        .flags(nwg::TextInputFlags::VISIBLE | nwg::TextInputFlags::TAB_STOP | nwg::TextInputFlags::NUMBER)
        .limit(3)
        .position((184, 214))
        .size((60, 26))
        .build(&mut ui.set_backups)?;
    nwg::ComboBox::builder()
        .parent(&ui.set_dlg)
        .font(font)
        .collection(vec![s.set_lang_en, s.set_lang_sr, s.set_lang_auto])
        .position((184, 248))
        .size((200, 26))
        .build(&mut ui.set_lang)?;
    let check = nwg::CheckBoxFlags::VISIBLE | nwg::CheckBoxFlags::TAB_STOP;
    nwg::CheckBox::builder().parent(&ui.set_dlg).text(s.set_sound).font(font).flags(check).position((14, 292)).size((532, 24)).build(&mut ui.set_sound)?;
    nwg::CheckBox::builder().parent(&ui.set_dlg).text(s.set_autostart).font(font).flags(check).position((14, 320)).size((532, 24)).build(&mut ui.set_autostart)?;
    nwg::CheckBox::builder().parent(&ui.set_dlg).text(s.set_pause).font(font).flags(check).position((14, 348)).size((532, 24)).build(&mut ui.set_pause)?;
    nwg::Label::builder().parent(&ui.set_dlg).text(s.set_note).font(font).position((14, 380)).size((532, 22)).build(&mut ui.set_note)?;
    nwg::Button::builder().parent(&ui.set_dlg).text(s.save).font(font).flags(btn).position((358, 416)).size((90, 28)).build(&mut ui.set_ok)?;
    nwg::Button::builder().parent(&ui.set_dlg).text(s.cancel).font(font).flags(btn).position((456, 416)).size((90, 28)).build(&mut ui.set_cancel)?;

    // ---- Tooltips: say what each button does when hovered ----
    nwg::Tooltip::builder()
        .register(&ui.btn_add, s.tip_add)
        .register(&ui.btn_edit, s.tip_edit)
        .register(&ui.btn_remove, s.tip_remove)
        .register(&ui.btn_snapshot, s.tip_snapshot)
        .register(&ui.btn_restore, s.tip_restore)
        .register(&ui.btn_rename, s.tip_rename)
        .register(&ui.btn_delete, s.tip_delete)
        .register(&ui.btn_open, s.tip_open)
        .register(&ui.btn_settings, s.tip_settings)
        .register(&ui.set_folder, s.set_folder_tip)
        .build(&mut ui.tooltip)?;
    ui.tooltip.set_delay_time(Some(400));

    // ---- Tray ----
    nwg::TrayNotification::builder().parent(&ui.main).icon(Some(&ui.icon_small)).tip(Some(s.tray_tip)).build(&mut ui.tray)?;
    nwg::Menu::builder().popup(true).parent(&ui.main).build(&mut ui.tray_menu)?;
    nwg::MenuItem::builder().parent(&ui.tray_menu).text(s.tray_open).build(&mut ui.mi_open)?;
    nwg::MenuItem::builder().parent(&ui.tray_menu).text(s.tray_quick).build(&mut ui.mi_quick)?;
    nwg::MenuSeparator::builder().parent(&ui.tray_menu).build(&mut ui.mi_sep1)?;
    nwg::MenuItem::builder().parent(&ui.tray_menu).text(s.tray_folder).build(&mut ui.mi_folder)?;
    nwg::MenuItem::builder().parent(&ui.tray_menu).text(s.tray_settings).build(&mut ui.mi_settings)?;
    nwg::MenuSeparator::builder().parent(&ui.tray_menu).build(&mut ui.mi_sep2)?;
    nwg::MenuItem::builder().parent(&ui.tray_menu).text(s.tray_exit).build(&mut ui.mi_exit)?;

    nwg::Notice::builder().parent(&ui.main).build(&mut ui.notice)?;
    Ok(ui)
}

impl App {
    // ------------------------------------------------------------------
    // Small helpers
    // ------------------------------------------------------------------

    fn hwnd(window: &nwg::Window) -> winapi::shared::windef::HWND {
        window.handle.hwnd().expect("window not built")
    }

    fn set_status(&self, text: &str) {
        self.ui.lbl_status.set_text(text);
    }

    fn error_box(&self, parent: &nwg::Window, text: &str) {
        nwg::modal_error_message(parent.handle, MAIN_TITLE, text);
    }

    fn confirm(&self, text: &str) -> bool {
        let params = nwg::MessageParams {
            title: MAIN_TITLE,
            content: text,
            buttons: nwg::MessageButtons::YesNo,
            icons: nwg::MessageIcons::Question,
        };
        nwg::modal_message(self.ui.main.handle, &params) == nwg::MessageChoice::Yes
    }

    fn save_config(&self) {
        let result = self.state.borrow().cfg.save();
        if let Err(e) = result {
            self.set_status(&fill(self.s.failed, &[&e.to_string()]));
        }
    }

    fn selected_game(&self) -> Option<usize> {
        let index = self.ui.cb_game.selection()?;
        (index < self.state.borrow().cfg.games.len()).then_some(index)
    }

    fn selected_snapshot(&self) -> Option<SnapshotInfo> {
        let row = self.ui.list.selected_item()?;
        self.state.borrow().snapshots.get(row).cloned()
    }

    /// Folder holding the selected game's snapshots.
    fn selected_game_dir(&self) -> Option<PathBuf> {
        let index = self.selected_game()?;
        let st = self.state.borrow();
        Some(snapshot::game_dir(&st.cfg.snapshot_dir, &st.cfg.games[index].name))
    }

    // ------------------------------------------------------------------
    // Manager window
    // ------------------------------------------------------------------

    /// Refills the game dropdown. `select` wins, then the remembered game.
    fn refresh_games(&self, select: Option<usize>) {
        let (names, index) = {
            let st = self.state.borrow();
            let names: Vec<String> = st.cfg.games.iter().map(|g| g.name.clone()).collect();
            let remembered = st.cfg.last_game.as_deref().and_then(|n| st.cfg.game_index(n));
            let index = select.or(remembered).or(if names.is_empty() { None } else { Some(0) });
            let index = index.filter(|i| *i < names.len());
            (names, index)
        };
        let empty = names.is_empty();
        self.ui.cb_game.set_collection(names);
        self.ui.cb_game.set_selection(index);
        self.refresh_list();
        if empty {
            self.set_status(self.s.no_games);
        }
    }

    fn refresh_list(&self) {
        let snapshots = self.selected_game_dir().map(|dir| snapshot::list(&dir)).unwrap_or_default();
        self.ui.list.set_redraw(false);
        self.ui.list.clear();
        for (row, snap) in snapshots.iter().enumerate() {
            self.ui.list.insert_items_row(
                Some(row as i32),
                &[snap.name.clone(), snapshot::format_time(snap.created), snapshot::format_size(snap.bytes), snap.files.to_string()],
            );
        }
        self.ui.list.set_redraw(true);
        self.state.borrow_mut().snapshots = snapshots;
    }

    fn show_manager(&self) {
        if !self.ui.main.visible() {
            self.refresh_games(None);
        }
        winutil::show_and_activate(Self::hwnd(&self.ui.main));
    }

    fn on_game_selected(&self) {
        if let Some(index) = self.selected_game() {
            {
                let mut st = self.state.borrow_mut();
                st.cfg.last_game = Some(st.cfg.games[index].name.clone());
            }
            self.save_config();
        }
        self.refresh_list();
        self.set_status(self.s.ready);
    }

    fn remove_game(&self) {
        let Some(index) = self.selected_game() else { return };
        let name = self.state.borrow().cfg.games[index].name.clone();
        if !self.confirm(&fill(self.s.confirm_remove_game, &[&name])) {
            return;
        }
        {
            let mut st = self.state.borrow_mut();
            st.cfg.games.remove(index);
            st.cfg.last_game = st.cfg.games.first().map(|g| g.name.clone());
        }
        self.save_config();
        self.refresh_games(None);
    }

    fn delete_selected(&self) {
        let Some(snap) = self.selected_snapshot() else { return self.set_status(self.s.select_snapshot) };
        if !self.confirm(&fill(self.s.confirm_delete, &[&snap.name])) {
            return;
        }
        match snapshot::remove_dir(&snap.path) {
            Ok(()) => self.set_status(self.s.ready),
            Err(e) => self.set_status(&fill(self.s.failed, &[&e.to_string()])),
        }
        self.refresh_list();
    }

    fn open_folder(&self) {
        let path = match (self.selected_snapshot(), self.selected_game_dir()) {
            (Some(snap), _) => snap.path,
            (None, Some(dir)) => dir,
            (None, None) => self.state.borrow().cfg.snapshot_dir.clone(),
        };
        let _ = fs::create_dir_all(&path);
        winutil::open_in_explorer(&path);
    }

    // ------------------------------------------------------------------
    // Taking snapshots
    // ------------------------------------------------------------------

    /// Entry point for both snapshot hotkeys: works out which game is meant,
    /// then either asks for a name or snapshots right away.
    fn begin_snapshot(&self, named: bool) {
        // A popup is already waiting for input: just bring it back.
        for dialog in [&self.ui.name_dlg, &self.ui.game_dlg] {
            if dialog.visible() {
                return winutil::show_and_activate(Self::hwnd(dialog));
            }
        }

        let foreground = winutil::foreground_program();
        let target = {
            let mut st = self.state.borrow_mut();
            st.prev_fg = foreground.as_ref().map(|f| f.hwnd).unwrap_or(0);
            st.game_pid = 0;
            let cfg = &mut st.cfg;
            let mut target = None;
            if let Some(exe) = foreground.as_ref().map(|f| &f.exe) {
                if let Some(index) = cfg.game_by_exe(exe) {
                    target = Some(Target::Game(index));
                } else {
                    let detected = steam::identify(exe);
                    if detected.install_name.is_some() {
                        // A Steam game. Known under its app id or name? Then
                        // remember its install folder for next time.
                        let known = detected.app_id.and_then(|id| cfg.game_by_app_id(id)).or_else(|| cfg.game_index(&detected.name));
                        target = Some(match known {
                            Some(index) => {
                                cfg.games[index].match_dir = Some(detected.dir.clone());
                                cfg.games[index].app_id = cfg.games[index].app_id.or(detected.app_id);
                                Target::Game(index)
                            }
                            None => Target::Setup(Some(detected)),
                        });
                    }
                }
            }
            let in_front_is_the_game = target.is_some();
            // Not a recognisable game in front: use the last used game.
            let target = target.unwrap_or_else(|| {
                let remembered = cfg.last_game.as_deref().and_then(|n| cfg.game_index(n));
                match remembered.or(if cfg.games.is_empty() { None } else { Some(0) }) {
                    Some(index) => Target::Game(index),
                    None => Target::Setup(None),
                }
            });
            // Only ever pause a process we positively identified as the game;
            // whatever else is in front (Explorer, a browser) is left alone.
            if in_front_is_the_game && st.cfg.pause_game {
                st.game_pid = foreground.as_ref().map(|f| f.pid).unwrap_or(0);
            }
            target
        };

        let then = if named { Then::NamedSnapshot } else { Then::QuickSnapshot };
        match target {
            Target::Game(index) => {
                let save_folder_is_empty = {
                    let mut st = self.state.borrow_mut();
                    st.cfg.last_game = Some(st.cfg.games[index].name.clone());
                    !steam::has_files(&st.cfg.games[index].save_path)
                };
                self.save_config();
                if save_folder_is_empty {
                    // Nothing to copy: the configured folder is wrong (or the
                    // game has not saved yet). Fix it now instead of failing.
                    self.open_game_dialog(GameMode::Edit { index, then });
                } else if named {
                    self.open_name_dialog(NameMode::New { game: index });
                } else {
                    self.start_snapshot(index, String::new());
                }
            }
            Target::Setup(detected) => self.open_game_dialog(GameMode::Add { detected, then }),
        }
    }

    /// Copies the save folder on a background thread; `on_jobs_done` reports the result.
    fn start_snapshot(&self, game_index: usize, name: String) {
        let (game, root) = {
            let mut st = self.state.borrow_mut();
            let Some(game) = st.cfg.games.get(game_index).cloned() else { return };
            if st.busy {
                drop(st);
                winutil::beep(false);
                return self.set_status(self.s.busy);
            }
            st.busy = true;
            (game, st.cfg.snapshot_dir.clone())
        };
        self.set_status(self.s.working);
        let tx = self.tx.clone();
        let wake = self.ui.notice.sender();
        thread::spawn(move || {
            let dir = snapshot::game_dir(&root, &game.name);
            let result = snapshot::create(&game.save_path, &dir, &name).map_err(|e| e.to_string());
            let _ = tx.send(Job::Snapshot { game: game.name, result });
            wake.notice();
        });
    }

    fn restore_selected(&self) {
        let Some(snap) = self.selected_snapshot() else { return self.set_status(self.s.select_snapshot) };
        let Some(index) = self.selected_game() else { return };
        let (game, root, keep) = {
            let st = self.state.borrow();
            if st.busy {
                drop(st);
                return self.set_status(self.s.busy);
            }
            (st.cfg.games[index].clone(), st.cfg.snapshot_dir.clone(), st.cfg.auto_backups_kept)
        };
        if !self.confirm(&fill(self.s.confirm_restore, &[&snap.name, &game.name])) {
            return;
        }
        self.state.borrow_mut().busy = true;
        self.set_status(self.s.working);
        let tx = self.tx.clone();
        let wake = self.ui.notice.sender();
        thread::spawn(move || {
            let dir = snapshot::game_dir(&root, &game.name);
            // Safety net first; if it fails nothing is touched.
            let result = snapshot::auto_backup(&game.save_path, &dir)
                .and_then(|_| snapshot::restore(&snap.path, &game.save_path))
                .map(|_| snapshot::prune_auto_backups(&dir, keep))
                .map_err(|e| e.to_string());
            let _ = tx.send(Job::Restore { name: snap.name, result });
            wake.notice();
        });
    }

    fn on_jobs_done(&self) {
        use nwg::TrayNotificationFlags as TF;
        while let Ok(job) = self.rx.try_recv() {
            let sound = {
                let mut st = self.state.borrow_mut();
                st.busy = false;
                st.cfg.sound
            };
            let (ok, title, text) = match job {
                Job::Snapshot { game, result: Ok(info) } => {
                    (true, game, fill(self.s.saved, &[&info.name, &info.files.to_string(), &snapshot::format_size(info.bytes)]))
                }
                Job::Snapshot { game, result: Err(e) } => (false, game, fill(self.s.failed, &[&e])),
                Job::Restore { name, result: Ok(()) } => (true, MAIN_TITLE.to_string(), fill(self.s.restored, &[&name])),
                Job::Restore { result: Err(e), .. } => (false, MAIN_TITLE.to_string(), fill(self.s.failed, &[&e])),
            };
            if sound {
                winutil::beep(ok);
            }
            self.set_status(&text);
            if self.ui.main.visible() {
                self.refresh_list();
            }
            let manager_in_front = self.ui.main.visible() && winutil::foreground_window() == 0;
            if !ok {
                // A failure must never go unnoticed: a tray balloon is easy to
                // miss in a game, so this box shows on top of everything.
                winutil::error_box_on_top(&title, &text);
            } else if !manager_in_front {
                self.ui.tray.show(&text, Some(&title), Some(TF::INFO_ICON | TF::SILENT), None);
            }
        }
    }

    // ------------------------------------------------------------------
    // Pausing the game while the name popup is open
    // ------------------------------------------------------------------

    /// Schedules the pause a moment after the popup appears, so the game
    /// first gets to react to losing focus (an exclusive-fullscreen game that
    /// is frozen before that would keep covering the popup).
    fn schedule_game_pause(&self) {
        if self.state.borrow().game_pid != 0 {
            winutil::set_timer(Self::hwnd(&self.ui.main), TIMER_PAUSE, 350);
        }
    }

    fn on_pause_timer(&self) {
        winutil::kill_timer(Self::hwnd(&self.ui.main), TIMER_PAUSE);
        let pid = self.state.borrow().game_pid;
        if pid != 0 && self.ui.name_dlg.visible() {
            winutil::suspend_process(pid);
        }
    }

    /// Lets the game run again. Always called before focus goes back to it.
    fn resume_game(&self) {
        winutil::kill_timer(Self::hwnd(&self.ui.main), TIMER_PAUSE);
        winutil::resume_suspended();
        self.state.borrow_mut().game_pid = 0;
    }

    /// For popups opened from our own window: there is no game to return to or pause.
    fn forget_game_window(&self) {
        let mut st = self.state.borrow_mut();
        st.prev_fg = 0;
        st.game_pid = 0;
    }

    // ------------------------------------------------------------------
    // Snapshot name popup
    // ------------------------------------------------------------------

    fn open_name_dialog(&self, mode: NameMode) {
        let (title, label, text) = match &mode {
            NameMode::New { game } => {
                let name = self.state.borrow().cfg.games.get(*game).map(|g| g.name.clone()).unwrap_or_default();
                (self.s.name_title_new, fill(self.s.name_for, &[&name]), String::new())
            }
            NameMode::Rename { path } => {
                let current = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                (self.s.name_title_rename, fill(self.s.name_for, &[&current]), current)
            }
            NameMode::Idle => return,
        };
        let pause = matches!(mode, NameMode::New { .. });
        self.state.borrow_mut().name_mode = mode;
        self.ui.name_dlg.set_text(title);
        self.ui.name_label.set_text(&label);
        self.ui.name_input.set_text(&text);
        winutil::show_and_activate(Self::hwnd(&self.ui.name_dlg));
        self.ui.name_input.set_focus();
        self.ui.name_input.set_selection(0..text.chars().count() as u32);
        if pause {
            self.schedule_game_pause();
        }
    }

    /// Hides a popup, un-pauses the game and gives focus back to it.
    fn close_popup(&self, window: &nwg::Window) {
        self.resume_game();
        window.set_visible(false);
        let previous = std::mem::take(&mut self.state.borrow_mut().prev_fg);
        winutil::activate(previous);
    }

    fn name_confirmed(&self) {
        let text = self.ui.name_input.text();
        let mode = std::mem::replace(&mut self.state.borrow_mut().name_mode, NameMode::Idle);
        self.close_popup(&self.ui.name_dlg);
        match mode {
            NameMode::New { game } => self.start_snapshot(game, text),
            NameMode::Rename { path } => {
                let new_name = snapshot::sanitize(&text);
                let old_name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                if new_name.is_empty() || new_name == old_name {
                    return;
                }
                let Some(dir) = path.parent() else { return };
                // A case-only rename targets the same folder, so don't "uniquify" it.
                let target = if new_name.eq_ignore_ascii_case(&old_name) { dir.join(&new_name) } else { snapshot::unique_path(dir, &new_name) };
                if let Err(e) = fs::rename(&path, &target) {
                    self.error_box(&self.ui.main, &fill(self.s.err_rename, &[&e.to_string()]));
                }
                self.refresh_list();
            }
            NameMode::Idle => {}
        }
    }

    fn name_cancelled(&self) {
        self.state.borrow_mut().name_mode = NameMode::Idle;
        self.close_popup(&self.ui.name_dlg);
    }

    // ------------------------------------------------------------------
    // Game setup dialog
    // ------------------------------------------------------------------

    fn open_game_dialog(&self, mode: GameMode) {
        let (title, hint, name, path) = match &mode {
            GameMode::Add { detected: Some(game), .. } => match steam::suggest_save_dir(game) {
                Some(guess) => (self.s.game_title_add, self.s.game_hint_found, game.name.clone(), guess.display().to_string()),
                None => (self.s.game_title_add, self.s.game_hint_not_found, game.name.clone(), String::new()),
            },
            GameMode::Add { detected: None, .. } => (self.s.game_title_add, self.s.game_hint_manual, String::new(), String::new()),
            GameMode::Edit { index, .. } => {
                let Some(game) = self.state.borrow().cfg.games.get(*index).cloned() else { return };
                let mut hint = self.s.game_hint_manual;
                let mut path = game.save_path.clone();
                if !steam::has_files(&game.save_path) {
                    // The folder on record is useless; look for the real one.
                    hint = self.s.game_hint_empty;
                    let found = game.match_dir.as_ref().and_then(|dir| {
                        steam::suggest_save_dir(&Detected {
                            name: game.name.clone(),
                            app_id: game.app_id,
                            dir: dir.clone(),
                            install_name: dir.file_name().map(|n| n.to_string_lossy().into_owned()),
                        })
                    });
                    if let Some(found) = found {
                        hint = self.s.game_hint_empty_found;
                        path = found;
                    }
                }
                (self.s.game_title_edit, hint, game.name.clone(), path.display().to_string())
            }
            GameMode::Idle => return,
        };
        self.state.borrow_mut().game_mode = mode;
        self.ui.game_dlg.set_text(title);
        self.ui.game_hint.set_text(hint);
        self.ui.game_name.set_text(&name);
        self.ui.game_path.set_text(&path);
        winutil::show_and_activate(Self::hwnd(&self.ui.game_dlg));
        if name.is_empty() {
            self.ui.game_name.set_focus();
        } else {
            self.ui.game_path.set_focus();
        }
    }

    fn browse_save_folder(&self) {
        let current = PathBuf::from(self.ui.game_path.text().trim());
        let build = |start: Option<String>| {
            let mut dialog = nwg::FileDialog::default();
            let mut builder = nwg::FileDialog::builder().title(self.s.pick_folder).action(nwg::FileDialogAction::OpenDirectory);
            if let Some(start) = start {
                builder = builder.default_folder(start);
            }
            builder.build(&mut dialog).map(|_| dialog)
        };
        let start = current.is_dir().then(|| current.display().to_string());
        let Ok(dialog) = build(start).or_else(|_| build(None)) else { return };
        if dialog.run(Some(&self.ui.game_dlg)) {
            if let Ok(picked) = dialog.get_selected_item() {
                self.ui.game_path.set_text(&picked.to_string_lossy());
            }
        }
    }

    fn game_confirmed(&self) {
        let name = self.ui.game_name.text().trim().to_string();
        let path = PathBuf::from(self.ui.game_path.text().trim().trim_matches('"'));

        // Validate without holding the borrow while an error box is open.
        let problem = {
            let st = self.state.borrow();
            let editing = match &st.game_mode {
                GameMode::Edit { index, .. } => Some(*index),
                GameMode::Add { .. } => None,
                GameMode::Idle => return,
            };
            if name.is_empty() {
                Some(self.s.err_name_empty)
            } else if st.cfg.game_index(&name).is_some_and(|i| Some(i) != editing) {
                Some(self.s.err_name_taken)
            } else if !path.is_dir() {
                Some(self.s.err_path_missing)
            } else if !config::save_path_is_safe(&path, &st.cfg.snapshot_dir) {
                Some(self.s.err_path_unsafe)
            } else if !steam::has_files(&path) {
                Some(self.s.err_path_empty)
            } else {
                None
            }
        };
        if let Some(problem) = problem {
            return self.error_box(&self.ui.game_dlg, problem);
        }

        let mode = std::mem::replace(&mut self.state.borrow_mut().game_mode, GameMode::Idle);
        let (index, then) = {
            let mut st = self.state.borrow_mut();
            match mode {
                GameMode::Add { detected, then } => {
                    st.cfg.games.push(Game {
                        name: name.clone(),
                        save_path: path,
                        app_id: detected.as_ref().and_then(|d| d.app_id),
                        match_dir: detected.map(|d| d.dir),
                    });
                    (st.cfg.games.len() - 1, then)
                }
                GameMode::Edit { index, then } => {
                    let old_name = st.cfg.games[index].name.clone();
                    if old_name != name {
                        // Snapshots live in a folder named after the game: move it along.
                        let from = snapshot::game_dir(&st.cfg.snapshot_dir, &old_name);
                        let to = snapshot::game_dir(&st.cfg.snapshot_dir, &name);
                        if from.is_dir() && (!to.exists() || config::norm(&from) == config::norm(&to)) {
                            let _ = fs::rename(&from, &to);
                        }
                    }
                    st.cfg.games[index].name = name.clone();
                    st.cfg.games[index].save_path = path;
                    (index, then)
                }
                GameMode::Idle => return,
            }
        };
        self.state.borrow_mut().cfg.last_game = Some(name);
        self.save_config();
        self.ui.game_dlg.set_visible(false);
        self.refresh_games(Some(index));
        self.set_status(self.s.ready);
        match then {
            // Focus stays with us for the name popup; it returns to the game afterwards.
            Then::NamedSnapshot => self.open_name_dialog(NameMode::New { game: index }),
            Then::QuickSnapshot => {
                self.close_popup(&self.ui.game_dlg);
                self.start_snapshot(index, String::new());
            }
            Then::Nothing => self.close_popup(&self.ui.game_dlg),
        }
    }

    fn game_cancelled(&self) {
        self.state.borrow_mut().game_mode = GameMode::Idle;
        self.close_popup(&self.ui.game_dlg);
    }

    // ------------------------------------------------------------------
    // Event routing
    // ------------------------------------------------------------------

    fn on_hotkey(&self, id: i32) {
        match id {
            HK_NAMED => self.begin_snapshot(true),
            HK_QUICK => self.begin_snapshot(false),
            HK_MANAGER => self.show_manager(),
            _ => {}
        }
    }

    fn on_button(&self, handle: nwg::ControlHandle) {
        let ui = &self.ui;
        if handle == ui.btn_snapshot.handle {
            match self.selected_game() {
                Some(game) => {
                    self.forget_game_window();
                    self.open_name_dialog(NameMode::New { game });
                }
                None => self.open_game_dialog(GameMode::Add { detected: None, then: Then::Nothing }),
            }
        } else if handle == ui.btn_restore.handle {
            self.restore_selected();
        } else if handle == ui.btn_rename.handle {
            match self.selected_snapshot() {
                Some(snap) => {
                    self.forget_game_window();
                    self.open_name_dialog(NameMode::Rename { path: snap.path });
                }
                None => self.set_status(self.s.select_snapshot),
            }
        } else if handle == ui.btn_delete.handle {
            self.delete_selected();
        } else if handle == ui.btn_open.handle {
            self.open_folder();
        } else if handle == ui.btn_settings.handle {
            self.open_settings();
        } else if handle == ui.set_ok.handle {
            self.settings_saved();
        } else if handle == ui.set_cancel.handle {
            self.settings_cancelled();
        } else if handle == ui.set_browse.handle {
            self.browse_snapshot_folder();
        } else if handle == ui.btn_add.handle {
            self.forget_game_window();
            self.open_game_dialog(GameMode::Add { detected: None, then: Then::Nothing });
        } else if handle == ui.btn_edit.handle {
            if let Some(index) = self.selected_game() {
                self.forget_game_window();
                self.open_game_dialog(GameMode::Edit { index, then: Then::Nothing });
            }
        } else if handle == ui.btn_remove.handle {
            self.remove_game();
        } else if handle == ui.name_ok.handle {
            self.name_confirmed();
        } else if handle == ui.name_cancel.handle {
            self.name_cancelled();
        } else if handle == ui.game_browse.handle {
            self.browse_save_folder();
        } else if handle == ui.game_ok.handle {
            self.game_confirmed();
        } else if handle == ui.game_cancel.handle {
            self.game_cancelled();
        }
    }

    fn on_menu(&self, handle: nwg::ControlHandle) {
        let ui = &self.ui;
        if handle == ui.mi_open.handle {
            self.show_manager();
        } else if handle == ui.mi_quick.handle {
            self.begin_snapshot(false);
        } else if handle == ui.mi_folder.handle {
            let root = self.state.borrow().cfg.snapshot_dir.clone();
            let _ = fs::create_dir_all(&root);
            winutil::open_in_explorer(&root);
        } else if handle == ui.mi_settings.handle {
            self.open_settings();
        } else if handle == ui.mi_exit.handle {
            nwg::stop_thread_dispatch();
        }
    }

    fn on_event(&self, event: nwg::Event, _data: nwg::EventData, handle: nwg::ControlHandle) {
        use nwg::Event as E;
        let ui = &self.ui;
        match event {
            E::OnButtonClick => self.on_button(handle),
            E::OnMenuItemSelected => self.on_menu(handle),
            E::OnNotice => self.on_jobs_done(),
            E::OnComboxBoxSelection if handle == ui.cb_game.handle => self.on_game_selected(),
            E::OnListViewDoubleClick if handle == ui.list.handle => self.restore_selected(),
            E::OnKeyEnter if handle == ui.name_dlg.handle => self.name_confirmed(),
            E::OnKeyEnter if handle == ui.game_dlg.handle => self.game_confirmed(),
            E::OnKeyEsc | E::OnWindowClose if handle == ui.name_dlg.handle => self.name_cancelled(),
            E::OnKeyEsc | E::OnWindowClose if handle == ui.game_dlg.handle => self.game_cancelled(),
            E::OnKeyEnter if handle == ui.set_dlg.handle => self.settings_saved(),
            E::OnKeyEsc | E::OnWindowClose if handle == ui.set_dlg.handle => self.settings_cancelled(),
            E::OnKeyEsc if handle == ui.main.handle => ui.main.set_visible(false),
            E::OnContextMenu if handle == ui.tray.handle => {
                let (x, y) = nwg::GlobalCursor::position();
                ui.tray_menu.popup(x, y);
            }
            E::OnMousePress(nwg::MousePressEvent::MousePressLeftUp) if handle == ui.tray.handle => self.show_manager(),
            _ => {}
        }
    }

    // ------------------------------------------------------------------
    // Global hotkeys
    // ------------------------------------------------------------------

    /// Registers the given hotkeys (empty ones are "off"); returns the ones
    /// another program already owns.
    fn register_hotkeys(&self, hotkeys: &config::Hotkeys) -> Vec<String> {
        let hwnd = Self::hwnd(&self.ui.main);
        let mut failed = Vec::new();
        for (id, text) in [HK_NAMED, HK_QUICK, HK_MANAGER].into_iter().zip(hotkeys.as_array()) {
            winutil::unregister_hotkey(hwnd, id);
            if text.trim().is_empty() {
                continue;
            }
            let ok = config::parse_hotkey(&text).is_some_and(|(mods, key)| winutil::register_hotkey(hwnd, id, mods, key));
            if !ok {
                failed.push(text);
            }
        }
        failed
    }

    fn unregister_hotkeys(&self) {
        let hwnd = Self::hwnd(&self.ui.main);
        for id in [HK_NAMED, HK_QUICK, HK_MANAGER] {
            winutil::unregister_hotkey(hwnd, id);
        }
    }

    // ------------------------------------------------------------------
    // Settings dialog
    // ------------------------------------------------------------------

    fn open_settings(&self) {
        if self.ui.set_dlg.visible() {
            return winutil::show_and_activate(Self::hwnd(&self.ui.set_dlg));
        }
        // Off while the dialog is open, so pressing the current combination
        // in a shortcut box is seen by the box instead of firing the action.
        self.unregister_hotkeys();
        let cfg = {
            let mut st = self.state.borrow_mut();
            st.captured = st.cfg.hotkeys.as_array();
            st.cfg.clone()
        };
        for (input, text) in self.ui.set_keys.iter().zip(cfg.hotkeys.as_array()) {
            input.set_text(&text);
        }
        self.ui.set_folder.set_text(&cfg.snapshot_dir.display().to_string());
        self.ui.set_backups.set_text(&cfg.auto_backups_kept.to_string());
        let lang = LANG_CODES.iter().position(|c| c.eq_ignore_ascii_case(&cfg.language)).unwrap_or(0);
        self.ui.set_lang.set_selection(Some(lang));
        let checked = |on: bool| if on { nwg::CheckBoxState::Checked } else { nwg::CheckBoxState::Unchecked };
        self.ui.set_sound.set_check_state(checked(cfg.sound));
        self.ui.set_autostart.set_check_state(checked(winutil::autostart_enabled()));
        self.ui.set_pause.set_check_state(checked(cfg.pause_game));
        winutil::show_and_activate(Self::hwnd(&self.ui.set_dlg));
        self.ui.set_ok.set_focus();
    }

    /// Key handling for the three shortcut boxes. Returns Some to swallow the
    /// message, so typing never puts characters in the box.
    fn on_capture_message(&self, field: usize, msg: u32, vk: u32) -> Option<isize> {
        const WM_KEYDOWN: u32 = 0x0100;
        const WM_KEYUP: u32 = 0x0101;
        const WM_CHAR: u32 = 0x0102;
        const WM_DEADCHAR: u32 = 0x0103;
        const WM_SYSKEYDOWN: u32 = 0x0104;
        const WM_SYSKEYUP: u32 = 0x0105;
        const WM_SYSCHAR: u32 = 0x0106;
        const WM_SYSDEADCHAR: u32 = 0x0107;
        const WM_CUT: u32 = 0x0300;
        const WM_PASTE: u32 = 0x0302;
        const WM_CLEAR: u32 = 0x0303;
        const WM_CONTEXTMENU: u32 = 0x007B;
        const VK_BACK: u32 = 0x08;
        const VK_DELETE: u32 = 0x2E;

        let input = &self.ui.set_keys[field];
        let committed = || self.state.borrow().captured[field].clone();
        match msg {
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let mods = winutil::current_modifiers();
                if winutil::is_modifier_key(vk) {
                    // Live preview while modifiers are held: "Ctrl+Alt+…"
                    input.set_text(&format!("{}…", config::format_modifiers(mods)));
                } else if mods == 0 && (vk == VK_BACK || vk == VK_DELETE) {
                    self.state.borrow_mut().captured[field].clear();
                    input.set_text("");
                } else {
                    match config::format_hotkey(mods, vk).filter(|_| mods != 0 || config::key_ok_alone(vk)) {
                        Some(text) => {
                            input.set_text(&text);
                            self.state.borrow_mut().captured[field] = text;
                        }
                        None => {
                            // A key on its own (like F5) would fire inside games too.
                            winutil::beep(false);
                            input.set_text(&committed());
                        }
                    }
                }
                Some(0)
            }
            WM_KEYUP | WM_SYSKEYUP => {
                if winutil::current_modifiers() == 0 {
                    input.set_text(&committed());
                }
                Some(0)
            }
            WM_CHAR | WM_DEADCHAR | WM_SYSCHAR | WM_SYSDEADCHAR | WM_CUT | WM_PASTE | WM_CLEAR | WM_CONTEXTMENU => Some(0),
            _ => None,
        }
    }

    fn browse_snapshot_folder(&self) {
        let current = PathBuf::from(self.ui.set_folder.text().trim());
        let mut dialog = nwg::FileDialog::default();
        let mut builder = nwg::FileDialog::builder().title(self.s.set_pick_folder).action(nwg::FileDialogAction::OpenDirectory);
        if current.is_dir() {
            builder = builder.default_folder(current.display().to_string());
        }
        if builder.build(&mut dialog).is_ok() && dialog.run(Some(&self.ui.set_dlg)) {
            if let Ok(picked) = dialog.get_selected_item() {
                self.ui.set_folder.set_text(&picked.to_string_lossy());
            }
        }
    }

    fn settings_saved(&self) {
        let captured = self.state.borrow().captured.clone();
        let folder = PathBuf::from(self.ui.set_folder.text().trim().trim_matches('"'));
        let backups = self.ui.set_backups.text().trim().parse::<usize>().ok().filter(|n| (1..=100).contains(n));

        let problem = {
            let st = self.state.borrow();
            let used: Vec<String> = captured.iter().filter(|k| !k.is_empty()).map(|k| k.to_ascii_lowercase()).collect();
            let has_duplicates = used.iter().enumerate().any(|(i, k)| used[..i].contains(k));
            let folder_ok = folder.is_absolute()
                && fs::create_dir_all(&folder).is_ok()
                && st.cfg.games.iter().all(|g| config::save_path_is_safe(&g.save_path, &folder));
            if has_duplicates {
                Some(self.s.err_hotkey_dup.to_string())
            } else if !folder_ok {
                Some(self.s.err_folder.to_string())
            } else if backups.is_none() {
                Some(self.s.err_backups.to_string())
            } else {
                None
            }
        };
        if let Some(problem) = problem {
            return self.error_box(&self.ui.set_dlg, &problem);
        }

        // Only accept shortcuts Windows actually lets us have.
        let hotkeys = config::Hotkeys::from_array(captured);
        let failed = self.register_hotkeys(&hotkeys);
        if !failed.is_empty() {
            self.unregister_hotkeys();
            return self.error_box(&self.ui.set_dlg, &fill(self.s.err_hotkey_taken, &[&failed.join(", ")]));
        }

        let language = LANG_CODES[self.ui.set_lang.selection().unwrap_or(0).min(LANG_CODES.len() - 1)];
        let autostart = self.ui.set_autostart.check_state() == nwg::CheckBoxState::Checked;
        {
            let mut st = self.state.borrow_mut();
            st.cfg.hotkeys = hotkeys.clone();
            st.cfg.snapshot_dir = folder;
            st.cfg.auto_backups_kept = backups.unwrap_or(5);
            st.cfg.language = language.to_string();
            st.cfg.sound = self.ui.set_sound.check_state() == nwg::CheckBoxState::Checked;
            st.cfg.pause_game = self.ui.set_pause.check_state() == nwg::CheckBoxState::Checked;
        }
        self.save_config();
        if autostart != winutil::autostart_enabled() {
            let _ = winutil::set_autostart(autostart);
        }
        self.ui.lbl_hint.set_text(&hotkey_hint(self.s, &hotkeys));
        self.ui.set_dlg.set_visible(false);
        self.refresh_list();
        self.set_status(self.s.ready);
    }

    fn settings_cancelled(&self) {
        self.ui.set_dlg.set_visible(false);
        let hotkeys = self.state.borrow().cfg.hotkeys.clone();
        let failed = self.register_hotkeys(&hotkeys);
        if !failed.is_empty() {
            self.set_status(&fill(self.s.err_hotkey_taken, &[&failed.join(", ")]).replace('\n', " "));
        }
    }
}

pub fn run() -> Result<(), nwg::NwgError> {
    nwg::init()?;
    let (cfg, config_was_reset) = Config::load();
    let s = lang::pick(&cfg.language, winutil::system_language_is_serbian());
    let ui = build_ui(s, &cfg.hotkeys)?;
    let (tx, rx) = channel();
    let first_run = cfg.games.is_empty();
    let app = Rc::new(App {
        ui,
        s,
        state: RefCell::new(State {
            cfg,
            snapshots: Vec::new(),
            name_mode: NameMode::Idle,
            game_mode: GameMode::Idle,
            prev_fg: 0,
            game_pid: 0,
            busy: false,
            captured: Default::default(),
        }),
        tx,
        rx,
    });

    let mut handlers = Vec::new();
    for window in [&app.ui.main, &app.ui.name_dlg, &app.ui.game_dlg, &app.ui.set_dlg] {
        let target = app.clone();
        handlers.push(nwg::full_bind_event_handler(&window.handle, move |event, data, handle| target.on_event(event, data, handle)));
    }
    let mut raw_handlers = Vec::new();
    for field in 0..3 {
        let target = app.clone();
        raw_handlers.push(nwg::bind_raw_event_handler(&app.ui.set_keys[field].handle, RAW_ID_CAPTURE + field, move |_hwnd, msg, wparam, _lparam| {
            target.on_capture_message(field, msg, wparam as u32)
        })?);
    }
    let target = app.clone();
    let raw = nwg::bind_raw_event_handler(&app.ui.main.handle, RAW_ID_HOTKEYS, move |_hwnd, msg, wparam, _lparam| match msg {
        WM_HOTKEY => {
            target.on_hotkey(wparam as i32);
            Some(0)
        }
        WM_TIMER if wparam == TIMER_PAUSE => {
            target.on_pause_timer();
            Some(0)
        }
        WM_SHOW_MANAGER => {
            target.show_manager();
            Some(0)
        }
        _ => None,
    })?;

    let hotkeys = app.state.borrow().cfg.hotkeys.clone();
    let failed = app.register_hotkeys(&hotkeys);
    app.refresh_games(None);
    if first_run {
        app.show_manager();
    }
    if config_was_reset {
        nwg::modal_info_message(app.ui.main.handle, MAIN_TITLE, s.config_reset);
    }
    if !failed.is_empty() {
        app.error_box(&app.ui.main, &fill(s.hotkey_failed, &[&failed.join(", ")]));
        app.open_settings();
    }

    nwg::dispatch_thread_events();

    // Tear down so the tray icon disappears immediately instead of lingering.
    winutil::resume_suspended();
    app.unregister_hotkeys();
    let _ = nwg::unbind_raw_event_handler(&raw);
    for handler in &raw_handlers {
        let _ = nwg::unbind_raw_event_handler(handler);
    }
    for handler in &handlers {
        nwg::unbind_event_handler(handler);
    }
    app.ui.tray.set_visibility(false);
    Ok(())
}

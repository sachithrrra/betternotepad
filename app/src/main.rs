mod encoding;
mod updater;

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Result, bail};
use encoding::{Encoding, Eol, decode, encode};
use gpui_kit::component::{
    ActiveTheme as _, IconName, IndexPath, Root, Selectable as _, Sizable as _, Theme, ThemeConfig,
    ThemeMode, WindowExt as _,
    button::{Button, ButtonGroup, ButtonVariants as _},
    dialog::{DialogAction, DialogClose, DialogFooter},
    h_flex,
    input::{
        self, Input, InputEvent, InputState, Position, Rope, RopeExt as _, Textarea, TextareaState,
    },
    menu::{DropdownMenu as _, PopupMenu},
    resizable::{h_resizable, resizable_panel},
    searchable_list::SearchableVec,
    select::{Select, SelectEvent, SelectState},
    switch::Switch,
    tab::{Tab, TabBar},
    text::{TextView, TextViewStyle},
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

actions!(
    notepad,
    [
        NewTab,
        NewWindow,
        Open,
        Save,
        SaveAs,
        CloseTab,
        CloseWindow,
        NextTab,
        PreviousTab,
        Quit,
        FindNext,
        FindPrevious,
        GoTo,
        TimeDate,
        ToggleWordWrap,
        OpenSettings,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        ToggleStatusBar,
        About,
        ClearRecent,
        CheckForUpdates,
    ]
);

/// Opens a file from File > Open Recent.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = notepad, no_json)]
struct OpenRecent(PathBuf);

const MAX_RECENT: usize = 10;

const DEFAULT_FONT: &str = "Lilex";
/// Line height as a multiple of the editor font size.
const LINE_SPACING: f32 = 1.45;
const FONT_SIZES: [u32; 16] = [8, 9, 10, 11, 12, 14, 16, 18, 20, 22, 24, 26, 28, 36, 48, 72];

/// App-wide preferences, shared by every window.
#[derive(Clone)]
struct Settings {
    appearance: Appearance,
    word_wrap: bool,
    status_bar: bool,
    font_family: SharedString,
    /// Font size in points.
    font_size: f32,
    /// Most recently opened or saved files, newest first.
    recent: Vec<PathBuf>,
}
impl Global for Settings {}

struct Updater(updater::Controller);
impl Global for Updater {}

#[derive(Clone, Copy, PartialEq)]
enum Appearance {
    System,
    Light,
    Dark,
}

impl Appearance {
    const ALL: [Appearance; 3] = [Appearance::System, Appearance::Light, Appearance::Dark];

    fn label(self) -> &'static str {
        match self {
            Appearance::System => "System",
            Appearance::Light => "Light",
            Appearance::Dark => "Dark",
        }
    }
}

/// Where app data lives; none in tests so they can't touch the real one.
fn support_dir() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    Some(std::env::home_dir()?.join("Library/Application Support/Better Notepad"))
}

fn recent_file() -> Option<PathBuf> {
    Some(support_dir()?.join("recent.txt"))
}

/// On first launch (no support dir yet), writes the welcome notes there and
/// returns their paths to open; empty on every later launch.
fn first_run_welcome() -> Vec<PathBuf> {
    let Some(dir) = support_dir().filter(|d| !d.exists()) else {
        return Vec::new();
    };
    let _ = std::fs::create_dir_all(&dir);
    [
        ("Welcome.txt", include_str!("../welcome/welcome.txt")),
        ("Welcome.md", include_str!("../welcome/welcome.md")),
    ]
    .into_iter()
    .map(|(name, text)| (dir.join(name), text))
    .filter(|(path, text)| std::fs::write(path, text).is_ok())
    .map(|(path, _)| path)
    .collect()
}

/// Saved recent files that still exist, one path per line.
fn load_recent() -> Vec<PathBuf> {
    let text = recent_file()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .unwrap_or_default();
    text.lines()
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .take(MAX_RECENT)
        .collect()
}

fn set_recent(cx: &mut App, update: impl FnOnce(&mut Vec<PathBuf>)) {
    cx.update_global::<Settings, _>(|s, _| update(&mut s.recent));
    if let Some(file) = recent_file() {
        let text: Vec<_> = cx
            .global::<Settings>()
            .recent
            .iter()
            .map(|p| p.to_string_lossy())
            .collect();
        let _ = std::fs::create_dir_all(file.parent().unwrap());
        let _ = std::fs::write(file, text.join("\n"));
    }
    set_menus(cx);
}

fn remember_recent(path: &Path, cx: &mut App) {
    set_recent(cx, |recent| {
        recent.retain(|p| p != path);
        recent.insert(0, path.to_path_buf());
        recent.truncate(MAX_RECENT);
    });
}

/// Applies the chosen theme to every window.
fn apply_appearance(cx: &mut App) {
    let dark = match cx.global::<Settings>().appearance {
        Appearance::Light => false,
        Appearance::Dark => true,
        Appearance::System => matches!(
            cx.window_appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        ),
    };
    Theme::change(
        if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        None,
        cx,
    );
    cx.refresh_windows();
}

/// One tab.
struct Doc {
    editor: Entity<TextareaState>,
    path: Option<PathBuf>,
    dirty: bool,
    /// Text as last opened or saved; `dirty` is "differs from this", so undoing
    /// back to it clears the dot. Cloning a rope is O(1).
    saved: Rope,
    eol: Eol,
    encoding: Encoding,
    /// Markdown preview shown beside the editor.
    preview: bool,
    /// Bumped to reset the preview split: it keys the divider state.
    split_resets: usize,
    _subscriptions: Vec<Subscription>,
}

impl Doc {
    fn id(&self) -> EntityId {
        self.editor.entity_id()
    }

    fn name(&self) -> String {
        self.path
            .as_deref()
            .and_then(Path::file_name)
            .map_or("Untitled".into(), |name| {
                name.to_string_lossy().into_owned()
            })
    }

    fn is_markdown(&self) -> bool {
        self.path
            .as_deref()
            .and_then(Path::extension)
            .is_some_and(|ext| {
                ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown")
            })
    }

    fn is_blank(&self, cx: &App) -> bool {
        self.path.is_none() && !self.dirty && self.editor.read(cx).text().len() == 0
    }
}

struct Notepad {
    focus_handle: FocusHandle,
    docs: Vec<Doc>,
    active: usize,
    zoom: u32,
    /// A native prompt is showing; GPUI panics on a second one.
    prompting: bool,
    _subscriptions: Vec<Subscription>,
}

impl Notepad {
    fn new(paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.observe_global_in::<Settings>(window, |this, window, cx| {
                let wrap = cx.global::<Settings>().word_wrap;
                for doc in &this.docs {
                    doc.editor
                        .update(cx, |editor, cx| editor.set_soft_wrap(wrap, window, cx));
                }
                cx.notify();
            }),
            // Menu commands dispatch from the focused element, so never leave the
            // window without focus (e.g. after a dialog closes).
            cx.on_focus_lost(window, |this, window, cx| {
                let editor = this.editor();
                editor.update(cx, |editor, cx| editor.focus(window, cx));
            }),
            // Only matters while the theme is "Use system setting".
            cx.observe_window_appearance(window, |_, _, cx| apply_appearance(cx)),
        ];

        let this = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |this, cx| {
                let dirty = this.docs.iter().any(|doc| doc.dirty);
                if dirty {
                    this.request_close_window(window, cx);
                }
                !dirty
            })
            .unwrap_or(true)
        });

        let mut notepad = Self {
            focus_handle: cx.focus_handle(),
            docs: Vec::new(),
            active: 0,
            zoom: 100,
            prompting: false,
            _subscriptions: subscriptions,
        };
        notepad.add_tab(window, cx);
        notepad.open_paths(paths, window, cx);
        notepad
    }

    fn doc(&self) -> &Doc {
        &self.docs[self.active]
    }

    fn editor(&self) -> Entity<TextareaState> {
        self.doc().editor.clone()
    }

    fn doc_ix(&self, id: EntityId) -> Option<usize> {
        self.docs.iter().position(|doc| doc.id() == id)
    }

    fn add_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> usize {
        let word_wrap = cx.global::<Settings>().word_wrap;
        let editor = cx.new(|cx| {
            TextareaState::new(window, cx)
                .searchable(true)
                .soft_wrap(word_wrap)
        });
        let subscriptions = vec![
            cx.subscribe_in(&editor, window, |this, editor, event, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.refresh_dirty(editor.entity_id(), window, cx);
                }
            }),
            // Cursor moves re-render the status bar.
            cx.observe(&editor, |_, _, cx| cx.notify()),
        ];
        self.docs.push(Doc {
            editor,
            path: None,
            dirty: false,
            saved: Rope::new(),
            eol: Eol::DEFAULT,
            encoding: Encoding::Utf8,
            preview: false,
            split_resets: 0,
            _subscriptions: subscriptions,
        });
        self.activate(self.docs.len() - 1, window, cx);
        self.active
    }

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = ix.min(self.docs.len() - 1);
        let editor = self.editor();
        editor.update(cx, |editor, cx| editor.focus(window, cx));
        self.update_title(window);
        cx.notify();
    }

    fn update_title(&self, window: &mut Window) {
        let doc = self.doc();
        let star = if doc.dirty { "*" } else { "" };
        window.set_window_title(&format!("{star}{} - Better Notepad", doc.name()));
        window.set_window_edited(self.docs.iter().any(|doc| doc.dirty));
    }

    fn refresh_dirty(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.doc_ix(id) else { return };
        let doc = &mut self.docs[ix];
        // ponytail: compares the whole text per edit (length first); track the undo
        // position instead if huge files lag.
        let dirty = doc.editor.read(cx).text() != &doc.saved;
        if doc.dirty != dirty {
            doc.dirty = dirty;
            self.update_title(window);
            cx.notify();
        }
    }

    /// Opens each file in its own tab, reusing a blank active tab and
    /// switching to a tab that already has the file.
    fn open_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        for path in paths {
            if let Some(ix) = self
                .docs
                .iter()
                .position(|doc| doc.path.as_ref() == Some(&path))
            {
                self.activate(ix, window, cx);
                continue;
            }
            let ix = if self.doc().is_blank(cx) {
                self.active
            } else {
                self.add_tab(window, cx)
            };
            self.load(ix, path, window, cx);
        }
    }

    fn load(&mut self, ix: usize, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let (text, eol, encoding) = match std::fs::read(&path) {
            Ok(bytes) => decode(&bytes),
            // A path from the command line that doesn't exist yet becomes a new file there.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                (String::new(), Eol::DEFAULT, Encoding::Utf8)
            }
            Err(err) => {
                self.alert(
                    &format!("Cannot open {}", path.display()),
                    &err.to_string(),
                    window,
                    cx,
                );
                return;
            }
        };
        let doc = &mut self.docs[ix];
        doc.editor
            .update(cx, |editor, cx| editor.set_value(text, window, cx));
        remember_recent(&path, cx);
        doc.saved = doc.editor.read(cx).text().clone();
        (doc.path, doc.eol, doc.encoding, doc.dirty) = (Some(path), eol, encoding, false);
        doc.preview = doc.is_markdown();
        self.activate(ix, window, cx);
    }

    fn write(
        &mut self,
        ix: usize,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let doc = &self.docs[ix];
        let bytes = encode(&doc.editor.read(cx).value(), doc.eol, doc.encoding);
        if let Err(err) = std::fs::write(&path, bytes) {
            self.alert(
                &format!("Cannot save {}", path.display()),
                &err.to_string(),
                window,
                cx,
            );
            return false;
        }
        remember_recent(&path, cx);
        let doc = &mut self.docs[ix];
        doc.saved = doc.editor.read(cx).text().clone();
        let was_markdown = doc.is_markdown();
        (doc.path, doc.dirty) = (Some(path), false);
        // Saving as .md turns the preview on, like opening one; a re-save keeps the user's toggle.
        if !was_markdown && doc.is_markdown() {
            doc.preview = true;
        }
        self.update_title(window);
        cx.notify();
        true
    }

    fn alert(&mut self, message: &str, detail: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.prompting {
            return window.push_notification(format!("{message}: {detail}"), cx);
        }
        self.prompting = true;
        let answer = window.prompt(PromptLevel::Critical, message, Some(detail), &["OK"], cx);
        cx.spawn(async move |this, cx| {
            answer.await.ok();
            this.update(cx, |this, _| this.prompting = false)
        })
        .detach();
    }

    /// Saves the tab to its path, or asks for one. Resolves to false if cancelled or failed.
    async fn save_flow(
        this: WeakEntity<Self>,
        id: EntityId,
        save_as: bool,
        cx: &mut AsyncWindowContext,
    ) -> Result<bool> {
        let Some(current) = this.read_with(cx, |this, _| {
            this.doc_ix(id).map(|ix| this.docs[ix].path.clone())
        })?
        else {
            return Ok(true);
        };
        let path = match current {
            Some(path) if !save_as => path,
            current => {
                let dir = current
                    .as_deref()
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
                    .or_else(std::env::home_dir)
                    .unwrap_or_default();
                let name = current
                    .as_deref()
                    .and_then(Path::file_name)
                    .map_or("Untitled.txt".into(), |n| n.to_string_lossy().into_owned());
                let picked = cx.update(|_, cx| cx.prompt_for_new_path(&dir, Some(&name)))?;
                match picked.await?? {
                    Some(path) => path,
                    None => return Ok(false),
                }
            }
        };
        this.update_in(cx, |this, window, cx| match this.doc_ix(id) {
            Some(ix) => this.write(ix, path, window, cx),
            None => true,
        })
    }

    /// The "Do you want to save changes?" gate. Resolves to true when the tab may be discarded.
    async fn confirm_discard(
        this: WeakEntity<Self>,
        id: EntityId,
        cx: &mut AsyncWindowContext,
    ) -> Result<bool> {
        let answer = this.update_in(cx, |this, window, cx| {
            let Some(ix) = this.doc_ix(id).filter(|&ix| this.docs[ix].dirty) else {
                return Ok(None);
            };
            if this.prompting {
                bail!("a prompt is already open");
            }
            this.prompting = true;
            this.activate(ix, window, cx);
            let name = this.docs[ix]
                .path
                .as_ref()
                .map_or("Untitled".into(), |p| p.display().to_string());
            Ok(Some(window.prompt(
                PromptLevel::Warning,
                &format!("Do you want to save changes to {name}?"),
                None,
                &["Save", "Don't Save", "Cancel"],
                cx,
            )))
        })??;
        let Some(answer) = answer else {
            return Ok(true);
        };
        let answer = answer.await;
        this.update(cx, |this, _| this.prompting = false)?;
        match answer? {
            0 => Self::save_flow(this, id, false, cx).await,
            1 => Ok(true),
            _ => Ok(false),
        }
    }

    fn close_tab_by_id(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            if !Self::confirm_discard(this.clone(), id, cx).await? {
                return Ok(());
            }
            this.update_in(cx, |this, window, cx| {
                let Some(ix) = this.doc_ix(id) else { return };
                if this.docs.len() == 1 {
                    // Closing the last tab closes the window.
                    return window.remove_window();
                }
                this.docs.remove(ix);
                let active = if ix < this.active {
                    this.active - 1
                } else {
                    this.active
                };
                this.activate(active, window, cx);
            })
        })
        .detach_and_log_err(cx);
    }

    fn request_close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<EntityId> = self.docs.iter().map(Doc::id).collect();
        cx.spawn_in(window, async move |this, cx| {
            for id in ids {
                if !Self::confirm_discard(this.clone(), id, cx).await? {
                    return Ok(());
                }
            }
            cx.update(|window, _| window.remove_window())
        })
        .detach_and_log_err(cx);
    }

    // ---- File menu ----

    fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        self.add_tab(window, cx);
    }

    fn open(&mut self, _: &Open, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Some(paths) = picked.await?? {
                this.update_in(cx, |this, window, cx| this.open_paths(paths, window, cx))?;
            }
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn open_recent(&mut self, action: &OpenRecent, window: &mut Window, cx: &mut Context<Self>) {
        if action.0.exists() {
            self.open_paths(vec![action.0.clone()], window, cx);
        } else {
            let path = action.0.clone();
            set_recent(cx, |recent| recent.retain(|p| *p != path));
            self.alert(
                &format!("Cannot find {}", action.0.display()),
                "It may have been moved or deleted.",
                window,
                cx,
            );
        }
    }

    fn save(&mut self, _: &Save, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.doc().id();
        cx.spawn_in(window, async move |this, cx| {
            Self::save_flow(this, id, false, cx).await
        })
        .detach_and_log_err(cx);
    }

    fn save_as(&mut self, _: &SaveAs, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.doc().id();
        cx.spawn_in(window, async move |this, cx| {
            Self::save_flow(this, id, true, cx).await
        })
        .detach_and_log_err(cx);
    }

    fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab_by_id(self.doc().id(), window, cx);
    }

    fn close_window(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
        self.request_close_window(window, cx);
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.activate((self.active + 1) % self.docs.len(), window, cx);
    }

    fn previous_tab(&mut self, _: &PreviousTab, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.docs.len();
        self.activate((self.active + len - 1) % len, window, cx);
    }

    // ---- Edit menu ----

    fn find(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.editor().update(cx, |editor, cx| {
            if editor.search_session().query.is_empty() {
                editor.open_search(false, cx);
                return;
            }
            let found = if forward {
                editor.next_search_match(cx)
            } else {
                editor.previous_search_match(cx)
            };
            if let Some(range) = found {
                editor.set_selected_range(range, cx);
            }
        });
    }

    fn go_to(&mut self, _: &GoTo, window: &mut Window, cx: &mut Context<Self>) {
        let editor = self.editor();
        let line = editor.read(cx).cursor_position().line + 1;
        let input = cx.new(|cx| InputState::new(window, cx).default_value(line.to_string()));
        window.open_dialog(cx, {
            let input = input.clone();
            move |dialog, _, _| {
                let (input, editor) = (input.clone(), editor.clone());
                dialog
                    .title("Go To Line")
                    .w(px(320.))
                    .child(
                        v_flex()
                            .gap_2()
                            .child("Line number:")
                            .child(Input::new(&input)),
                    )
                    .footer(
                        DialogFooter::new()
                            .gap_2()
                            .child(
                                DialogClose::new()
                                    .child(Button::new("cancel").label("Cancel").outline()),
                            )
                            .child(
                                DialogAction::new()
                                    .child(Button::new("ok").label("Go To").primary()),
                            ),
                    )
                    .on_ok(move |_, window, cx| {
                        let total = editor.read(cx).text().lines_len() as u32;
                        match input.read(cx).value().trim().parse::<u32>() {
                            Ok(n @ 1..) if n <= total => {
                                editor.update(cx, |editor, cx| {
                                    editor.set_cursor_position(Position::new(n - 1, 0), window, cx)
                                });
                                true
                            }
                            _ => {
                                window.push_notification(
                                    "The line number is beyond the total number of lines",
                                    cx,
                                );
                                false
                            }
                        }
                    })
            }
        });
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
    }

    fn time_date(&mut self, _: &TimeDate, window: &mut Window, cx: &mut Context<Self>) {
        let now = chrono::Local::now()
            .format("%-I:%M %p %-m/%-d/%Y")
            .to_string();
        let editor = self.editor();
        editor.update(cx, |editor, cx| editor.replace(now, window, cx));
        self.refresh_dirty(editor.entity_id(), window, cx);
    }

    // ---- Format / View / Help ----

    fn zoom_by(&mut self, step: i32, cx: &mut Context<Self>) {
        self.zoom = match step {
            0 => 100,
            _ => self.zoom.saturating_add_signed(step).clamp(10, 500),
        };
        cx.notify();
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        TabBar::new("tabs")
            // Long file names ellipsize instead of pushing the other tabs away.
            .max_width(px(220.))
            .selected_index(self.active)
            .on_click(cx.listener(|this, ix: &usize, window, cx| this.activate(*ix, window, cx)))
            .children(self.docs.iter().map(|doc| {
                let id = doc.id();
                Tab::new()
                    .label(doc.name())
                    .pl_3()
                    // Middle-click closes, like a browser tab.
                    .on_mouse_up(
                        MouseButton::Middle,
                        cx.listener(move |this, _, window, cx| {
                            this.close_tab_by_id(id, window, cx)
                        }),
                    )
                    .suffix(
                        // Inset so the hover background stays inside the tab's edge.
                        // The unsaved dot lives here so an ellipsized name can't hide it.
                        h_flex()
                            .gap_1()
                            .pl_2()
                            .pr_1p5()
                            .when(doc.dirty, |this| this.child("•"))
                            .child(
                                Button::new(("close-tab", id.as_u64() as usize))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Close)
                                    .tooltip("Close tab")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.close_tab_by_id(id, window, cx);
                                    })),
                            ),
                    )
            }))
            // Like a browser, "+" sits right after the last tab. The tab bar only
            // renders `last_empty_space` when it has a suffix, hence the empty one.
            .suffix(div())
            .last_empty_space(
                h_flex().h_full().px_1p5().child(
                    Button::new("new-tab")
                        .ghost()
                        .small()
                        .icon(IconName::Plus)
                        .tooltip("New tab")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.add_tab(window, cx);
                        })),
                ),
            )
    }

    /// The in-window menu bar: a soft gradient with the same 1px divider below as
    /// every other bar (the tab bar above draws its own).
    fn render_menu_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let settings = cx.global::<Settings>().clone();
        // Edit commands (Undo, Copy…) must reach the text area, not the menu.
        let action_context = self.editor().read(cx).focus_handle(cx);
        let doc = self.doc();
        let preview = doc.is_markdown().then_some(doc.preview);
        let theme = cx.theme();
        h_flex()
            .flex_none()
            .h(px(38.))
            .px_2()
            .gap_1()
            .bg(linear_gradient(
                180.,
                linear_color_stop(theme.popover, 0.),
                linear_color_stop(theme.secondary, 1.),
            ))
            .border_b_1()
            .border_color(theme.border)
            .children(
                menus(&settings, false)
                    .into_iter()
                    .enumerate()
                    .map(|(ix, menu)| {
                        let menu = menu.owned();
                        let action_context = action_context.clone();
                        Button::new(("menu", ix))
                            .ghost()
                            // `small` gives the label `text_sm`, the dropdown items' size; the
                            // button sets label size itself, so a plain text size is ignored.
                            .small()
                            .px_3()
                            .label(menu.name.clone())
                            .dropdown_menu(move |popup, window, cx| {
                                fill_menu(
                                    popup.action_context(action_context.clone()),
                                    &menu.items,
                                    window,
                                    cx,
                                )
                            })
                    }),
            )
            .when_some(preview, |this, on| {
                this.child(div().flex_1()).child(
                    Button::new("preview")
                        .ghost()
                        .small()
                        .icon(if on { IconName::EyeOff } else { IconName::Eye })
                        .selected(on)
                        .tooltip(if on { "Hide preview" } else { "Show preview" })
                        .on_click(cx.listener(|this, _, _, cx| {
                            let active = this.active;
                            this.docs[active].preview ^= true;
                            cx.notify();
                        })),
                )
            })
    }
}

impl Render for Notepad {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = cx.global::<Settings>().clone();
        let font_px = settings.font_size * 4. / 3. * self.zoom as f32 / 100.;
        let doc = self.doc();
        let pos = doc.editor.read(cx).cursor_position();
        let status = [
            format!("{}%", self.zoom),
            doc.eol.label().into(),
            doc.encoding.label().into(),
        ];
        let editor = doc.editor.clone();
        let doc_id = doc.id().as_u64() as usize;
        let split_id = SharedString::from(format!("split-{doc_id}-{}", doc.split_resets));
        let active = self.active;
        let this = cx.entity().downgrade();
        // The kit's divider has no click hook, so paint our own line that fills its hit area;
        // double-clicking it re-keys the split, which restores the default halves.
        let divider: base::ResizeHandleRenderer = Rc::new(move |handle, _, cx| {
            let this = this.clone();
            let color = if handle.is_active() {
                cx.theme().ring
            } else {
                cx.theme().border
            };
            Some(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .justify_center()
                    .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                        if event.click_count == 2 {
                            this.update(cx, |this, cx| {
                                if let Some(doc) = this.docs.get_mut(active) {
                                    doc.split_resets += 1;
                                    cx.notify();
                                }
                            })
                            .ok();
                        }
                    })
                    .child(div().h_full().w(px(1.)).bg(color))
                    .into_any_element(),
            )
        });
        let preview = (doc.preview && doc.is_markdown()).then(|| {
            let theme = cx.theme();
            TextView::markdown(
                ("preview", doc.id().as_u64() as usize),
                editor.read(cx).value(),
            )
            .font_family(settings.font_family.clone())
            .text_size(px(font_px))
            // Inline code defaults to the blue `accent` (it tints links in tables), and
            // header text to `muted_foreground`, which reads as disabled.
            .style(
                TextViewStyle::default()
                    .inline_code(HighlightStyle {
                        background_color: Some(theme.muted),
                        ..Default::default()
                    })
                    .table_head(
                        StyleRefinement::default()
                            .text_color(theme.foreground)
                            .font_weight(FontWeight::SEMIBOLD),
                    ),
            )
            .scrollable(true)
            .selectable(true)
        });
        let tabs = self.render_tabs(cx);
        let menu_bar = self.render_menu_bar(cx);
        let theme = cx.theme();

        v_flex()
            .id("notepad")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.open_paths(paths.paths().to_vec(), window, cx);
            }))
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::open))
            .on_action(cx.listener(Self::open_recent))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::save_as))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::close_window))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::previous_tab))
            .on_action(cx.listener(|this, _: &FindNext, _, cx| this.find(true, cx)))
            .on_action(cx.listener(|this, _: &FindPrevious, _, cx| this.find(false, cx)))
            .on_action(cx.listener(Self::go_to))
            .on_action(cx.listener(Self::time_date))
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| this.zoom_by(10, cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| this.zoom_by(-10, cx)))
            .on_action(cx.listener(|this, _: &ZoomReset, _, cx| this.zoom_by(0, cx)))
            .child(div().flex_none().child(tabs))
            .child(menu_bar)
            .child(h_flex().flex_1().min_h_0().items_stretch().map(|this| {
                let textarea = Textarea::new(&editor)
                    .appearance(false)
                    .flex_1()
                    .min_h_0()
                    .h_full()
                    .font_family(settings.font_family.clone())
                    // Show exactly what was typed: no `->` → `→` ligatures.
                    .font_features(FontFeatures::disable_ligatures())
                    .text_size(px(font_px))
                    // The kit fixes this at 1.25rem; follow the font (and zoom) instead.
                    .line_height(px((font_px * LINE_SPACING).round()));
                match preview {
                    None => this.child(textarea),
                    // Split keyed per tab, so each tab keeps its own divider position.
                    Some(preview) => this.child(
                        h_resizable(split_id)
                            .with_handle_appearance(divider)
                            .child(resizable_panel().child(textarea))
                            // No `pr_*`: the scrollbar is painted inset to the
                            // panel's own bounds, so right padding here would
                            // pull it away from the window edge.
                            .child(resizable_panel().pl_4().py_2().child(preview.size_full())),
                    ),
                }
            }))
            .when(settings.status_bar, |this| {
                this.child(
                    h_flex()
                        .h(px(28.))
                        .flex_none()
                        .text_size(px(12.))
                        .text_color(theme.muted_foreground)
                        .bg(theme.secondary)
                        .border_t_1()
                        .border_color(theme.border)
                        .child(div().flex_1().px_3().child(format!(
                            "Ln {}, Col {}",
                            pos.line + 1,
                            pos.character + 1
                        )))
                        .children(status.map(|text| {
                            div()
                                .px_3()
                                .border_l_1()
                                .border_color(theme.border)
                                .child(text)
                        })),
                )
            })
            // gpui-kit 0.6 leaves drawing the overlay layers to the app view.
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

// ---- App setup ----

fn menus(settings: &Settings, native: bool) -> Vec<Menu> {
    let mut menus = Vec::new();
    let app_menu = native && cfg!(target_os = "macos");
    if app_menu {
        menus.push(Menu::new("Better Notepad").items([
            MenuItem::action("About Better Notepad", About),
            MenuItem::action("Check for Updates…", CheckForUpdates),
            MenuItem::separator(),
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::action("Quit Better Notepad", Quit),
        ]));
    }
    menus.extend([
        Menu::new("File").items(
            [
                MenuItem::action("New Tab", NewTab),
                MenuItem::action("New Window", NewWindow),
                MenuItem::action("Open…", Open),
                MenuItem::submenu(
                    Menu::new("Open Recent").items(
                        settings
                            .recent
                            .iter()
                            .map(|path| {
                                MenuItem::action(recent_label(path), OpenRecent(path.clone()))
                            })
                            .chain([
                                MenuItem::separator(),
                                MenuItem::action("Clear Menu", ClearRecent),
                            ]),
                    ),
                ),
                MenuItem::action("Save", Save),
                MenuItem::action("Save As…", SaveAs),
                MenuItem::separator(),
                MenuItem::action("Close Tab", CloseTab),
                MenuItem::action("Close Window", CloseWindow),
            ]
            .into_iter()
            // On macOS these live in the app menu instead.
            .chain(
                (!app_menu)
                    .then(|| {
                        [
                            MenuItem::separator(),
                            MenuItem::action("Settings…", OpenSettings),
                            MenuItem::separator(),
                            MenuItem::action("Exit", Quit),
                        ]
                    })
                    .into_iter()
                    .flatten(),
            ),
        ),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", input::Undo, OsAction::Undo),
            MenuItem::os_action("Redo", input::Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
            MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
            MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
            MenuItem::action("Delete", input::Delete),
            MenuItem::separator(),
            MenuItem::action("Find…", input::Search),
            MenuItem::action("Find Next", FindNext),
            MenuItem::action("Find Previous", FindPrevious),
            MenuItem::action("Replace…", input::Replace),
            MenuItem::action("Go To…", GoTo),
            MenuItem::separator(),
            MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
            MenuItem::action("Time/Date", TimeDate),
        ]),
        Menu::new("Format").items([
            MenuItem::action("Word Wrap", ToggleWordWrap).checked(settings.word_wrap),
            MenuItem::action("Font…", OpenSettings),
        ]),
        Menu::new("View").items([
            MenuItem::submenu(Menu::new("Zoom").items([
                MenuItem::action("Zoom In", ZoomIn),
                MenuItem::action("Zoom Out", ZoomOut),
                MenuItem::action("Restore Default Zoom", ZoomReset),
            ])),
            MenuItem::action("Status Bar", ToggleStatusBar).checked(settings.status_bar),
            MenuItem::separator(),
            MenuItem::action("Next Tab", NextTab),
            MenuItem::action("Previous Tab", PreviousTab),
        ]),
        Menu::new("Help").items([
            MenuItem::action("About Better Notepad", About),
            MenuItem::action("Check for Updates…", CheckForUpdates),
        ]),
    ]);
    menus
}

/// A recent file as `name — ~/dir`, the directory cut in the middle so long
/// paths can't stretch the menu.
fn recent_label(path: &Path) -> String {
    const MAX: usize = 60;
    let name = path
        .file_name()
        .map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy());
    let mut dir = path
        .parent()
        .map_or(String::new(), |d| d.to_string_lossy().into_owned());
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && let Ok(rest) = Path::new(&dir).strip_prefix(&home)
    {
        dir = Path::new("~").join(rest).to_string_lossy().into_owned();
    }
    let label = format!("{name} — {dir}");
    let chars: Vec<char> = label.chars().collect();
    if chars.len() <= MAX {
        return label;
    }
    let keep = MAX - 1;
    let head: String = chars[..keep / 2].iter().collect();
    let tail: String = chars[chars.len() - (keep - keep / 2)..].iter().collect();
    format!("{head}…{tail}")
}

/// Rebuilds the native menu bar; the in-window one re-reads `menus` each render.
fn set_menus(cx: &mut App) {
    let settings = cx.global::<Settings>().clone();
    cx.set_menus(menus(&settings, true));
}

/// Turns the shared menu data into a dropdown, keeping check marks and submenus.
fn fill_menu(
    mut popup: PopupMenu,
    items: &[OwnedMenuItem],
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    for item in items {
        popup = match item {
            OwnedMenuItem::Action {
                name,
                action,
                checked,
                ..
            } => popup.menu_with_check(name.clone(), *checked, action.boxed_clone()),
            OwnedMenuItem::Separator => popup.separator(),
            OwnedMenuItem::Submenu(submenu) => {
                let submenu = submenu.clone();
                popup.submenu(
                    submenu.name.clone(),
                    window,
                    cx,
                    move |popup, window, cx| fill_menu(popup, &submenu.items, window, cx),
                )
            }
            OwnedMenuItem::SystemMenu(_) => popup,
        };
    }
    popup
}

fn bind_keys(cx: &mut App) {
    // `secondary` is Cmd on macOS and Ctrl elsewhere. Cut/Copy/Paste/Undo/Redo/
    // Select All/Find come built into the text area.
    cx.bind_keys([
        KeyBinding::new("secondary-n", NewTab, None),
        KeyBinding::new("secondary-t", NewTab, None),
        KeyBinding::new("secondary-shift-n", NewWindow, None),
        KeyBinding::new("secondary-o", Open, None),
        KeyBinding::new("secondary-s", Save, None),
        KeyBinding::new("secondary-shift-s", SaveAs, None),
        KeyBinding::new("secondary-w", CloseTab, None),
        KeyBinding::new("secondary-shift-w", CloseWindow, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, None),
        KeyBinding::new("secondary-q", Quit, None),
        KeyBinding::new("secondary-,", OpenSettings, None),
        KeyBinding::new("f3", FindNext, None),
        KeyBinding::new("shift-f3", FindPrevious, None),
        KeyBinding::new("secondary-g", GoTo, None),
        KeyBinding::new("f5", TimeDate, None),
        KeyBinding::new("secondary-+", ZoomIn, None),
        KeyBinding::new("secondary-=", ZoomIn, None),
        KeyBinding::new("secondary--", ZoomOut, None),
        KeyBinding::new("secondary-0", ZoomReset, None),
        KeyBinding::new("secondary-h", input::Replace, Some("Input")),
    ]);
}

/// Lilex everywhere plus a calmer palette, layered over the kit's default
/// light/dark themes so every unset color keeps its fallback.
fn install_theme(cx: &mut App) {
    macro_rules! style {
        ($base:expr, { $($field:ident: $hex:literal),* $(,)? }) => {{
            let mut config: ThemeConfig = (*$base).clone();
            config.font_family = Some(DEFAULT_FONT.into());
            config.mono_font_family = Some(DEFAULT_FONT.into());
            config.font_size = Some(15.);
            config.mono_font_size = Some(13.);
            config.radius = Some(6);
            config.radius_lg = Some(10);
            $(config.colors.$field = Some($hex.into());)*
            Rc::new(config)
        }};
    }

    let theme = Theme::global_mut(cx);
    theme.light_theme = style!(theme.light_theme, {
        background: "#ffffff",
        foreground: "#1f2328",
        border: "#e4e4e7",
        input: "#d4d4d8",
        muted_foreground: "#6b7280",
        primary: "#2563eb",
        primary_hover: "#1d4ed8",
        primary_foreground: "#ffffff",
        secondary: "#f4f4f5",
        accent: "#e8eefc",
        accent_foreground: "#1e3a8a",
        popover: "#ffffff",
        selection: "#2563eb40",
        caret: "#2563eb",
        ring: "#2563eb",
        tab_bar: "#f4f4f5",
        tab: "#f4f4f5",
        tab_active: "#ffffff",
        tab_foreground: "#71717a",
        tab_active_foreground: "#18181b",
    });
    theme.dark_theme = style!(theme.dark_theme, {
        background: "#1e1f22",
        foreground: "#e4e4e7",
        border: "#2e3035",
        input: "#3a3d44",
        muted_foreground: "#9ca3af",
        primary: "#4c8dff",
        primary_hover: "#3b7bf0",
        primary_foreground: "#ffffff",
        secondary: "#18191c",
        // Open menu-bar button (selected ghost); derived from `secondary` it's near-black.
        secondary_active: "#3a3d44",
        // Menu/list highlight: a clear blue like macOS, not a muddy navy.
        accent: "#3867d6",
        accent_foreground: "#ffffff",
        popover: "#25262a",
        selection: "#4c8dff4d",
        caret: "#4c8dff",
        ring: "#4c8dff",
        tab_bar: "#18191c",
        tab: "#18191c",
        tab_active: "#1e1f22",
        tab_foreground: "#8b8f98",
        tab_active_foreground: "#f4f4f5",
    });
    let mode = theme.mode;
    Theme::change(mode, None, cx);
}

/// Lilex isn't a system font, so it ships inside the binary (OFL, see fonts/OFL.txt).
fn load_fonts(cx: &App) {
    let fonts = [
        include_bytes!("../fonts/Lilex-Regular.ttf").as_slice(),
        include_bytes!("../fonts/Lilex-Medium.ttf"),
        include_bytes!("../fonts/Lilex-SemiBold.ttf"),
        include_bytes!("../fonts/Lilex-Bold.ttf"),
    ];
    cx.text_system()
        .add_fonts(fonts.map(std::borrow::Cow::Borrowed).to_vec())
        .expect("bundled fonts");
}

/// Everything except opening a window; shared with the UI tests.
fn init(cx: &mut App) {
    gpui_kit::init(cx);
    load_fonts(cx);
    install_theme(cx);
    cx.set_global(Settings {
        appearance: Appearance::System,
        word_wrap: true,
        status_bar: true,
        font_family: DEFAULT_FONT.into(),
        font_size: 12.,
        recent: load_recent(),
    });
    bind_keys(cx);
    set_menus(cx);
    cx.set_global(Updater(updater::start()));

    cx.on_action(|_: &NewWindow, cx| open_window(Vec::new(), cx));
    cx.on_action(|_: &OpenSettings, cx| open_settings(cx));
    cx.on_action(|_: &About, cx| open_about(cx));
    cx.on_action(|_: &CheckForUpdates, cx| {
        updater::check_for_updates(cx.global::<Updater>().0);
    });
    cx.on_action(|_: &ClearRecent, cx| set_recent(cx, Vec::clear));
    cx.on_action(|_: &ToggleWordWrap, cx| {
        cx.update_global::<Settings, _>(|s, _| s.word_wrap = !s.word_wrap);
        set_menus(cx);
    });
    cx.on_action(|_: &ToggleStatusBar, cx| {
        cx.update_global::<Settings, _>(|s, _| s.status_bar = !s.status_bar);
        set_menus(cx);
    });
    // Each window runs its own unsaved-changes prompts; the app exits with the last one.
    // Deferred: Quit usually arrives while its window is mid-update, and
    // updating that window from inside itself fails.
    cx.on_action(|_: &Quit, cx| {
        cx.defer(|cx| {
            for window in cx.windows() {
                let _ = window.update(cx, |_, window, cx| {
                    window.dispatch_action(Box::new(CloseWindow), cx)
                });
            }
        })
    });
}

fn open_window(paths: Vec<PathBuf>, cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(900.), px(640.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Better Notepad".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        apply_appearance(cx);
        let view = cx.new(|cx| Notepad::new(paths, window, cx));
        cx.new(|cx| Root::new(view, window, cx))
    })
    .expect("failed to open window");
}

// ---- Settings window ----

/// Settings and About windows by title; each is reused while it's open.
#[derive(Default)]
struct SingleWindows(std::collections::HashMap<&'static str, WindowHandle<Root>>);
impl Global for SingleWindows {}

fn open_single<V: Render>(
    title: &'static str,
    window_size: Size<Pixels>,
    resizable: bool,
    build: impl FnOnce(&mut Window, &mut Context<V>) -> V,
    cx: &mut App,
) {
    let existing = cx.default_global::<SingleWindows>().0.get(title).copied();
    if let Some(handle) = existing
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let bounds = Bounds::centered(None, window_size, cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..Default::default()
        }),
        is_resizable: resizable,
        is_minimizable: resizable,
        ..Default::default()
    };
    let handle = cx
        .open_window(options, |window, cx| {
            let view = cx.new(|cx| build(window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("failed to open window");
    cx.global_mut::<SingleWindows>().0.insert(title, handle);
}

fn open_settings(cx: &mut App) {
    open_single(
        "Settings",
        size(px(560.), px(520.)),
        true,
        SettingsView::new,
        cx,
    );
}

fn open_about(cx: &mut App) {
    open_single(
        "About Better Notepad",
        size(px(320.), px(240.)),
        false,
        |_, _| AboutView,
        cx,
    );
}

struct AboutView;

impl Render for AboutView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_1()
            .p_6()
            .bg(theme.background)
            .text_color(theme.foreground)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .child(
                div()
                    .text_2xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Better Notepad"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(concat!("Version ", env!("CARGO_PKG_VERSION"))),
            )
            .child(
                div()
                    .pt_3()
                    .text_sm()
                    .text_center()
                    .child("A simple, fast plain-text editor for macOS."),
            )
    }
}

struct SettingsView {
    font_family: Entity<SelectState<SearchableVec<String>>>,
    font_size: Entity<SelectState<Vec<String>>>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = cx.global::<Settings>().clone();
        let mut families = cx.text_system().all_font_names();
        families.sort();
        families.dedup();
        let family_ix = families.iter().position(|f| *f == *settings.font_family);
        let font_family = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(families),
                family_ix.map(IndexPath::new),
                window,
                cx,
            )
            .searchable(true)
        });
        let sizes: Vec<String> = FONT_SIZES.iter().map(u32::to_string).collect();
        let size_ix = FONT_SIZES
            .iter()
            .position(|&s| s as f32 == settings.font_size);
        let font_size =
            cx.new(|cx| SelectState::new(sizes, size_ix.map(IndexPath::new), window, cx));

        // Every setting applies live.
        let subscriptions = vec![
            cx.subscribe(
                &font_family,
                |_, _, event: &SelectEvent<SearchableVec<String>>, cx| {
                    if let SelectEvent::Confirm(Some(family)) = event {
                        let family = SharedString::from(family.clone());
                        cx.update_global::<Settings, _>(|s, _| s.font_family = family);
                    }
                },
            ),
            cx.subscribe(&font_size, |_, _, event: &SelectEvent<Vec<String>>, cx| {
                if let SelectEvent::Confirm(Some(size)) = event
                    && let Ok(size) = size.parse::<f32>()
                {
                    cx.update_global::<Settings, _>(|s, _| s.font_size = size);
                }
            }),
            cx.observe_global::<Settings>(|_, cx| cx.notify()),
        ];
        Self {
            font_family,
            font_size,
            _subscriptions: subscriptions,
        }
    }
}

/// One settings card: title and hint on the left, control on the right.
fn setting_row(
    title: &'static str,
    hint: &'static str,
    control: impl IntoElement,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    h_flex()
        .gap_4()
        .px_4()
        .py_3()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .child(
            v_flex().flex_1().gap_0p5().child(title).child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(hint),
            ),
        )
        .child(control)
}

fn set_toggle(update: fn(&mut Settings, bool)) -> impl Fn(&bool, &mut Window, &mut App) {
    move |on, _, cx| {
        let on = *on;
        cx.update_global::<Settings, _>(|s, _| update(s, on));
        set_menus(cx);
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = cx.global::<Settings>().clone();
        let theme_ix = Appearance::ALL
            .iter()
            .position(|&a| a == settings.appearance);

        v_flex()
            .size_full()
            .p_5()
            .gap_2()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .pb_2()
                    .child("Settings"),
            )
            .child(setting_row(
                "App theme",
                "Select which app theme to display",
                ButtonGroup::new("appearance")
                    .outline()
                    .small()
                    .children(Appearance::ALL.iter().enumerate().map(|(ix, a)| {
                        Button::new(ix)
                            .label(a.label())
                            .selected(theme_ix == Some(ix))
                    }))
                    .on_click(|clicked, _, cx| {
                        let Some(&ix) = clicked.first() else { return };
                        let appearance = Appearance::ALL[ix];
                        cx.update_global::<Settings, _>(|s, _| s.appearance = appearance);
                        apply_appearance(cx);
                    }),
                cx,
            ))
            .child(setting_row(
                "Font",
                "Family and size of the text",
                h_flex()
                    .gap_2()
                    .child(div().w(px(180.)).child(Select::new(&self.font_family)))
                    .child(div().w(px(76.)).child(Select::new(&self.font_size))),
                cx,
            ))
            .child(setting_row(
                "Word wrap",
                "Fit text within the window by default",
                Switch::new("word-wrap")
                    .checked(settings.word_wrap)
                    .on_change(set_toggle(|s, on| s.word_wrap = on)),
                cx,
            ))
            .child(setting_row(
                "Status bar",
                "Show line, column, zoom and encoding",
                Switch::new("status-bar")
                    .checked(settings.status_bar)
                    .on_change(set_toggle(|s, on| s.status_bar = on)),
                cx,
            ))
            .child(
                div()
                    .pt_2()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(concat!("Better Notepad ", env!("CARGO_PKG_VERSION"))),
            )
    }
}

/// Decodes a `file://…` URL, as macOS delivers for "Open With" and drag-onto-dock, into a path.
fn url_to_path(url: &str) -> Option<PathBuf> {
    let path = url.strip_prefix("file://")?;
    let mut bytes = Vec::with_capacity(path.len());
    let mut rest = path.bytes();
    while let Some(b) = rest.next() {
        if b == b'%' {
            let hex: String = [rest.next()?, rest.next()?]
                .map(|b| b as char)
                .into_iter()
                .collect();
            bytes.push(u8::from_str_radix(&hex, 16).ok()?);
        } else {
            bytes.push(b);
        }
    }
    Some(PathBuf::from(String::from_utf8(bytes).ok()?))
}

fn main() {
    let paths: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    #[cfg(feature = "snapshot")]
    if let Ok(out) = std::env::var("NP_SNAPSHOT") {
        return snapshot(&out, paths);
    }
    let (open_tx, open_rx) = async_channel::unbounded::<Vec<PathBuf>>();
    let app = gpui_kit::application().with_assets(gpui_kit::assets::Assets);
    // Finder ("Open With", double-click, drag-onto-dock-icon) delivers files as
    // `file://` URLs here, not as argv — this fires both at launch and while running.
    app.on_open_urls(move |urls| {
        let paths: Vec<PathBuf> = urls.iter().filter_map(|u| url_to_path(u)).collect();
        if !paths.is_empty() {
            open_tx.try_send(paths).ok();
        }
    });
    app.run(move |cx| {
            init(cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);

            let opened = Rc::new(Cell::new(!paths.is_empty()));
            if !paths.is_empty() {
                open_window(paths, cx);
            } else {
                // A Finder-initiated launch delivers its file via `on_open_urls` shortly
                // after this closure runs; wait a beat before defaulting to a blank window.
                let opened = opened.clone();
                cx.spawn(async move |cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    if !opened.replace(true) {
                        cx.update(|cx| open_window(first_run_welcome(), cx));
                    }
                })
                .detach();
            }
            cx.spawn(async move |cx| {
                while let Ok(paths) = open_rx.recv().await {
                    opened.set(true);
                    cx.update(|cx| open_window(paths, cx));
                }
            })
            .detach();
        });
}

/// Dev only: render a window off-screen to a PNG so the UI can be reviewed without a display.
/// `NP_SNAPSHOT=out.png [NP_DARK=1] [NP_ACTION=goto] cargo run --features snapshot -- files…`
#[cfg(feature = "snapshot")]
fn snapshot(out: &str, paths: Vec<PathBuf>) {
    let platform = gpui_kit::platform::current_platform(false);
    let mut cx = VisualTestAppContext::with_asset_source(
        platform,
        std::sync::Arc::new(gpui_kit::assets::Assets),
    );
    cx.update(init);
    let dark = std::env::var("NP_DARK").is_ok();
    let win = cx
        .open_offscreen_window(size(px(900.), px(640.)), move |window, cx| {
            Theme::change(
                if dark {
                    gpui_kit::component::ThemeMode::Dark
                } else {
                    gpui_kit::component::ThemeMode::Light
                },
                Some(window),
                cx,
            );
            let view = cx.new(|cx| Notepad::new(paths, window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("offscreen window");
    cx.run_until_parked();
    // The window follows the system appearance; force the requested one after that.
    cx.update_window(win.into(), |_, window, cx| {
        let mode = if dark {
            gpui_kit::component::ThemeMode::Dark
        } else {
            gpui_kit::component::ThemeMode::Light
        };
        Theme::change(mode, Some(window), cx);
    })
    .unwrap();
    cx.run_until_parked();
    let action: Option<Box<dyn Action>> = match std::env::var("NP_ACTION").as_deref() {
        Ok("goto") => Some(Box::new(GoTo)),
        _ => None,
    };
    if let Some(action) = action {
        cx.update_window(win.into(), |_, window, cx| {
            window.dispatch_action(action, cx)
        })
        .unwrap();
        cx.run_until_parked();
    }
    // Let dialog animations finish.
    std::thread::sleep(std::time::Duration::from_millis(400));
    cx.update_window(win.into(), |_, window, _| window.refresh())
        .unwrap();
    cx.run_until_parked();
    let img = cx
        .update_window(win.into(), |_, window, _| window.render_to_image())
        .unwrap()
        .unwrap();
    img.save(out).unwrap();
}

#[cfg(test)]
mod ui_tests {
    use super::{About, CloseTab, Encoding, GoTo, NewTab, Notepad, Quit, init};
    use gpui_kit::component::{Root, WindowExt as _};
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AnyWindowHandle, AppContext as _, ElementId, Entity, TestAppContext, px, size};

    fn open(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Notepad>) {
        cx.update(init);
        let mut notepad = None;
        let handle = cx.open_window(size(px(900.), px(640.)), |window, cx| {
            let view = cx.new(|cx| Notepad::new(Vec::new(), window, cx));
            notepad = Some(view.clone());
            Root::new(view, window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
            .unwrap();
        (handle.into(), notepad.unwrap())
    }

    #[gpui_kit::test]
    fn editor_fills_the_window(cx: &mut TestAppContext) {
        let (handle, notepad) = open(cx);
        cx.update_window(handle, |_, _, cx| {
            let bounds = notepad.read(cx).editor().read(cx).input_bounds();
            // Below the tab strip and menu bar, above the status bar.
            assert!(bounds.origin.y < px(100.), "{bounds:?}");
            assert!(bounds.size.height > px(450.), "{bounds:?}");
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn menu_commands_work_after_focus_is_lost(cx: &mut TestAppContext) {
        let (handle, _) = open(cx);
        // What the menus do after a dialog closed or focus otherwise went away.
        cx.update_window(handle, |_, window, cx| window.blur(cx))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.focused(cx).is_some(), "focus was not restored");
            window.dispatch_action(Box::new(GoTo), cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            assert!(window.has_active_dialog(cx))
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn quit_closes_clean_windows(cx: &mut TestAppContext) {
        let (handle, _) = open(cx);
        cx.update_window(handle, |_, window, cx| window.dispatch_action(Box::new(Quit), cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(cx.read(|cx| cx.windows().len()), 0);
    }

    #[gpui_kit::test]
    fn help_about_opens_one_window(cx: &mut TestAppContext) {
        let (handle, _) = open(cx);
        cx.update_window(handle, |_, window, cx| {
            // Help is the fifth menu in the in-window menu bar; About is its only item.
            window.click(ElementId::NamedInteger("menu".into(), 4), cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window
                .within("popup-menu")
                .within("items")
                .click(ElementId::Integer(0), cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(cx.read(|cx| cx.windows().len()), 2);
        // Asking again reuses the open About window.
        cx.update_window(handle, |_, window, cx| {
            window.dispatch_action(Box::new(About), cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(cx.read(|cx| cx.windows().len()), 2);
    }

    #[gpui_kit::test]
    fn undoing_back_to_saved_text_clears_dirty(cx: &mut TestAppContext) {
        let (handle, notepad) = open(cx);
        let dirty = |cx: &mut TestAppContext| cx.read(|cx| notepad.read(cx).doc().dirty);
        for (keys, expected) in [
            ("a", true),
            ("secondary-z", false),
            ("secondary-shift-z", true),
        ] {
            cx.update_window(handle, |_, window, cx| window.press(keys, cx))
                .unwrap();
            cx.run_until_parked();
            assert_eq!(dirty(cx), expected, "after {keys}");
        }
    }

    #[gpui_kit::test]
    fn tabs_open_close_and_load_binary_files(cx: &mut TestAppContext) {
        let (handle, notepad) = open(cx);
        let dir = std::env::temp_dir().join(format!("notepad-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p12 = dir.join("cert.p12");
        std::fs::write(&p12, b"\x30\x82\x0a\x00\xff\x00binary").unwrap();

        cx.update_window(handle, |_, window, cx| {
            window.press("secondary-n", cx);
            window.dispatch_action(Box::new(NewTab), cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(cx.read(|cx| notepad.read(cx).docs.len()), 3);

        cx.update_window(handle, |_, window, cx| {
            window.dispatch_action(Box::new(CloseTab), cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(cx.read(|cx| notepad.read(cx).docs.len()), 2);

        cx.update_window(handle, |_, window, cx| {
            notepad.update(cx, |this, cx| {
                this.open_paths(vec![p12.clone()], window, cx)
            });
            window.render_frame(cx);
            let this = notepad.read(cx);
            // The blank active tab is reused for the file.
            assert_eq!(this.docs.len(), 2);
            assert_eq!(this.doc().path.as_ref(), Some(&p12));
            assert_eq!(this.doc().encoding, Encoding::Ansi);
            assert_eq!(this.doc().editor.read(cx).value().chars().count(), 12);
            assert_eq!(cx.global::<super::Settings>().recent.first(), Some(&p12));
        })
        .unwrap();
    }
}

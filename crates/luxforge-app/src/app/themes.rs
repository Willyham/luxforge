//! The interface's theme and the theme library ([design](../../../../docs/design/ui-themes.md#desktop)).
//!
//! The desktop draws the theme the preferences choose. It reads it before the first frame, with the
//! launch preferences ([`Editor::launch_theme`]), so it never draws Luxforge Dark first and then
//! changes. After that every route that changes the choice — a row of the Appearance tab, a palette
//! entry, an evidence step, or another client's `preferences.set` read by the event sync — changes
//! the preferences the desktop's one writer shows, and [`after_message`] draws what they now name:
//! Luxforge Dark at once, any other theme from one `theme.read`, off the update loop. A theme that
//! cannot be shown leaves Luxforge Dark on screen and the status bar names it and the reason; the
//! stored choice is not changed.
//!
//! A theme changes no photograph: nothing here sends `asset.state`, `history.list` or a preview
//! job, reads an asset or uploads a frame. The theme value Iced draws with is [`Editor::theme`];
//! the view model holds only its identity and the library's rows ([`crate::state::themes`]).
//!
//! The library calls — import, export, delete, the report — are host methods any client calls; they
//! go one at a time under the library's `pending` flag, and an import or delete lists the library
//! again before it answers.
use super::{
    Before, Editor,
    message::{Message, theme::ThemeMessage},
    outcome::Outcome,
    tasks::{call, call_detailed, call_own, owner_task, owner_work, request},
};
use crate::state::{
    MenuTarget,
    preferences::PreferenceChange,
    themes::{DrawnTheme, ThemeList, fallback_note, parse_list},
};
use iced::{Color, Task};
use luxforge_core::{
    ErrorKind,
    theme::{LUXFORGE_DARK_ID, LaunchTheme, MAX_THEME_FILE_BYTES, Mode, Palette as Tokens},
};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

/// One theme as `theme.read` answers it: what the desktop draws.
#[derive(Clone, Debug)]
pub(crate) struct ReadTheme {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) mode: Mode,
    pub(crate) tokens: Tokens,
}

/// One library call of this desktop's and the listing read right after it, so the tab shows the
/// library the call left behind.
#[derive(Clone, Debug)]
pub(crate) struct ThemeChange {
    /// What the call itself answered.
    pub(crate) result: Value,
    pub(crate) list: ThemeList,
    /// The event sequence the listing was read at, which orders it against other listings.
    pub(crate) sequence: u64,
    /// The call's own request, whose event the listing already reflects.
    pub(crate) request: String,
}

/// The widget crate's palette for the core's resolved tokens, each by its name. A desktop test
/// holds the two crates' token lists equal, so every token finds its field.
pub(crate) fn widget_palette(tokens: &Tokens) -> luxforge_ui::Palette {
    let mut palette = luxforge_ui::Palette::luxforge_dark();
    for (token, value) in tokens.iter() {
        if let Some(colour) = palette.token_mut(token.name()) {
            *colour = Color::from_rgba8(value.r, value.g, value.b, f32::from(value.a) / 255.0);
        }
    }
    palette
}

/// The theme Iced draws for a resolved theme. Luxforge Dark is the widget crate's own, whose values
/// are the visual language's exactly; any other theme is built from the core's tokens.
pub(crate) fn widget_theme(id: &str, mode: Mode, tokens: &Tokens) -> luxforge_ui::Theme {
    if id == LUXFORGE_DARK_ID {
        return luxforge_ui::Theme::luxforge_dark();
    }
    let mode = match mode {
        Mode::Dark => luxforge_ui::Mode::Dark,
        Mode::Light => luxforge_ui::Mode::Light,
    };
    luxforge_ui::Theme::new(widget_palette(tokens), mode)
}

/// A colour as a frame's state records it: `#rrggbb`, or `#rrggbbaa` when it is not opaque.
fn hex(colour: Color) -> String {
    let [r, g, b, a] = colour.into_rgba8();
    if a == 255 {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }
}

/// The change a row's click sends through the preference writer: `preferences.set {theme}`.
pub(crate) fn choose_change(id: String) -> PreferenceChange {
    PreferenceChange {
        theme: Some(id),
        ..PreferenceChange::default()
    }
}

/// `theme.import` for a Luxforge theme document's text.
pub(crate) fn import_params(content: &str) -> Value {
    json!({"format": "luxforge", "content": content, "mutation": request()})
}

/// `theme.delete` for one stored theme.
pub(crate) fn delete_params(id: &str) -> Value {
    json!({"theme_id": id, "mutation": request()})
}

/// `theme.read` and `theme.export` for one theme.
pub(crate) fn theme_params(id: &str) -> Value {
    json!({"theme_id": id})
}

/// The whole library, as `theme.list` lists it, and the event sequence it was read at.
pub(crate) fn list_now(
    owner: &luxforge_core::OwnerHandle,
    client: luxforge_core::ClientId,
) -> Result<(ThemeList, u64), String> {
    let (listed, sequence) = call(owner, client, "theme.list", json!({}))?;
    Ok((parse_list(listed)?, sequence))
}

/// One theme's resolved tokens, through `theme.read`. A failure is the owner's own reason, which
/// the status bar quotes after the theme it names.
pub(crate) fn read_now(
    owner: &luxforge_core::OwnerHandle,
    client: luxforge_core::ClientId,
    id: &str,
) -> Result<ReadTheme, String> {
    let mut read = call_detailed(owner, client, "theme.read", theme_params(id))
        .map_err(|error| error.message)?;
    let theme = read["theme"].take();
    let field = |name: &str| {
        theme[name]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("theme.read answered no {name}"))
    };
    Ok(ReadTheme {
        id: field("id")?,
        name: field("name")?,
        mode: serde_json::from_value(theme["mode"].clone())
            .map_err(|error| format!("unreadable theme mode: {error}"))?,
        tokens: serde_json::from_value(theme["resolved"].clone())
            .map_err(|error| format!("unreadable theme tokens: {error}"))?,
    })
}

/// Read one chosen theme document as text, on the task's thread: refused from its length before a
/// byte is read when it is larger than `theme.import` takes, bounded in case it grows meanwhile,
/// and refused when it is not UTF-8.
pub(crate) fn read_theme_file(path: &Path) -> Result<String, String> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let too_large = |length: u64| {
        format!(
            "{}: {name} is {length} bytes; a theme file is at most 64 KiB ({MAX_THEME_FILE_BYTES} bytes)",
            ErrorKind::ResourceLimit.code()
        )
    };
    let unreadable = |error: std::io::Error| {
        format!(
            "{}: cannot read {name}: {error}",
            ErrorKind::FileAccess.code()
        )
    };
    let file = std::fs::File::open(path).map_err(unreadable)?;
    let length = file.metadata().map_err(unreadable)?.len();
    if length > MAX_THEME_FILE_BYTES as u64 {
        return Err(too_large(length));
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file.take(MAX_THEME_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if bytes.len() > MAX_THEME_FILE_BYTES {
        return Err(too_large(bytes.len() as u64));
    }
    String::from_utf8(bytes).map_err(|_| {
        format!(
            "{}: {name} is not UTF-8 text",
            ErrorKind::UnsupportedInput.code()
        )
    })
}

/// One library method and the listing after it.
fn change_now(
    owner: &luxforge_core::OwnerHandle,
    client: luxforge_core::ClientId,
    method: &str,
    params: Value,
) -> Result<ThemeChange, String> {
    let (result, request) = call_own(owner, client, method, params)?;
    let (list, sequence) = list_now(owner, client)?;
    Ok(ThemeChange {
        result,
        list,
        sequence,
        request,
    })
}

/// Import one Luxforge theme document: read it here, send its text to `theme.import`, and list the
/// library. The owner thread never touches the file.
pub(crate) fn import_now(
    owner: &luxforge_core::OwnerHandle,
    client: luxforge_core::ClientId,
    path: &Path,
) -> Result<ThemeChange, String> {
    let content = read_theme_file(path)?;
    change_now(owner, client, "theme.import", import_params(&content))
}

/// Delete one stored theme and list the library.
pub(crate) fn delete_now(
    owner: &luxforge_core::OwnerHandle,
    client: luxforge_core::ClientId,
    id: &str,
) -> Result<ThemeChange, String> {
    change_now(owner, client, "theme.delete", delete_params(id))
}

/// One theme's whole report, as the text Copy import report puts on the clipboard.
pub(crate) fn report_now(
    owner: &luxforge_core::OwnerHandle,
    client: luxforge_core::ClientId,
    id: &str,
) -> Result<String, String> {
    let (read, _) = call(owner, client, "theme.read", theme_params(id))?;
    serde_json::to_string_pretty(&read["theme"]["report"]).map_err(|error| error.to_string())
}

/// Export one theme: `theme.export` writes the document, the native save dialog chooses where,
/// suggesting the document's own file name, and the file is written here once the dialog has
/// answered, off the update loop. A cancelled dialog writes nothing.
fn export_task(
    owner: luxforge_core::OwnerHandle,
    client: luxforge_core::ClientId,
    id: String,
) -> Task<Message> {
    owner_work(move || {
        let (exported, _) = call(&owner, client, "theme.export", theme_params(&id))?;
        let text = |name: &str| {
            exported[name]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("theme.export returned no {name}"))
        };
        Ok::<_, String>((text("file_name")?, text("content")?))
    })
    .then(|exported| {
        Task::perform(
            async move {
                let (file_name, content) = exported?;
                let Some(file) = rfd::AsyncFileDialog::new()
                    .set_file_name(&file_name)
                    .add_filter("Luxforge theme", &["lftheme"])
                    .save_file()
                    .await
                else {
                    return Ok(None);
                };
                // Past the dialog's await, so the runtime runs this, not the update loop.
                std::fs::write(file.path(), content).map_err(|error| {
                    format!(
                        "{}: cannot write {}: {error}",
                        ErrorKind::FileAccess.code(),
                        file.file_name()
                    )
                })?;
                Ok(Some(file.file_name()))
            },
            |result| Message::Theme(ThemeMessage::Exported(result)),
        )
    })
}

impl Editor {
    /// Draw the theme the launch read before the first frame, as Iced reads [`Self::theme`] before
    /// it opens the window. A chosen theme that cannot be shown leaves Luxforge Dark, and the status
    /// bar names it and the reason; the stored choice is not changed. `None` is a launch that read
    /// no theme — a unit fixture's — which draws Luxforge Dark until [`after_message`] follows the
    /// stored choice.
    pub(crate) fn launch_theme(&mut self, launch: Option<LaunchTheme>) {
        let Some(launch) = launch else {
            return;
        };
        // What the drawing answers for: the stored choice, which a fallback leaves in place, so it
        // is not read again until another is made.
        let chosen = self
            .preferences
            .applied_theme()
            .map_or_else(|| launch.id.clone(), str::to_owned);
        if launch.id != LUXFORGE_DARK_ID {
            self.theme = widget_theme(&launch.id, launch.resolved.mode, &launch.resolved.tokens);
        }
        let problem = launch.problem.map(|problem| capitalised(&problem.detail));
        if let Some(note) = &problem {
            self.status.text = note.clone();
            self.event(
                "theme_unavailable",
                || json!({"chosen": chosen, "reason": note, "at": "launch"}),
            );
        }
        self.themes.drawn = DrawnTheme {
            chosen,
            id: launch.id,
            name: launch.name,
            mode: launch.resolved.mode,
            problem,
        };
    }

    /// One theme message.
    pub(super) fn theme_update(&mut self, message: ThemeMessage) -> Task<Message> {
        match message {
            ThemeMessage::Listed(result) => {
                self.themes.listing.answered();
                match result {
                    Ok((list, sequence)) => self.adopt_theme_list(list, sequence),
                    Err(error) => self.themes.error = Some(error),
                }
                return self.start_theme_list();
            }
            ThemeMessage::Choose(id) => {
                self.view_state.menu = None;
                if self.preferences.applied_theme() == Some(id.as_str()) {
                    return Task::none();
                }
                return self.store_preferences(choose_change(id));
            }
            ThemeMessage::Read { id, result } => return self.theme_read(id, result),
            ThemeMessage::Import => {
                if self.view_state.picker_open || self.themes.pending || self.evidence.is_some() {
                    return Task::none();
                }
                self.view_state.picker_open = true;
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Import Theme File")
                            .add_filter("Luxforge theme", &["lftheme"])
                            .pick_file()
                            .await
                            .map(|file| file.path().to_path_buf())
                    },
                    |path| Message::Theme(ThemeMessage::ImportPicked(path)),
                );
            }
            ThemeMessage::ImportPicked(path) => {
                self.view_state.picker_open = false;
                if let Some(path) = path {
                    return self.theme_import(path);
                }
            }
            ThemeMessage::Imported(result) => {
                self.themes.pending = false;
                match result {
                    Ok(change) => {
                        let name = change.result["theme"]["name"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned();
                        let report = change.result["report"].clone();
                        self.adopt_theme_change(*change);
                        self.status.text = format!("Imported theme \u{201c}{name}\u{201d}");
                        // Copy in the status bar copies the whole report while this line stands.
                        self.status.copy = Some((
                            self.status.text.clone(),
                            serde_json::to_string_pretty(&report).unwrap_or_default(),
                        ));
                        self.outcome(Outcome::ThemesAnswered { failure: None });
                    }
                    Err(error) => self.theme_refused(error),
                }
            }
            ThemeMessage::Export(id) => {
                self.view_state.menu = None;
                if self.evidence.is_some() {
                    return Task::none();
                }
                return export_task(self.owner.clone(), self.client, id);
            }
            ThemeMessage::Exported(result) => {
                self.status.text = match result {
                    Ok(Some(file)) => format!("Exported {file}"),
                    Ok(None) => "Export cancelled".into(),
                    Err(error) => error,
                };
            }
            ThemeMessage::CopyReport(id) => {
                self.view_state.menu = None;
                let (owner, client) = (self.owner.clone(), self.client);
                return owner_task(
                    move || report_now(&owner, client, &id),
                    |result| Message::Theme(ThemeMessage::ReportRead(result)),
                );
            }
            ThemeMessage::ReportRead(result) => match result {
                Ok(report) => {
                    self.status.text = "Copied the import report".into();
                    return iced::clipboard::write(report);
                }
                Err(error) => self.status.text = error,
            },
            ThemeMessage::Delete(id) => {
                self.view_state.menu = None;
                if self.themes.pending {
                    self.theme_refused("Waiting for the last theme request".into());
                    return Task::none();
                }
                self.themes.pending = true;
                self.themes.refusal = None;
                self.status.text = "Deleting the theme\u{2026}".into();
                let (owner, client) = (self.owner.clone(), self.client);
                return owner_task(
                    move || delete_now(&owner, client, &id),
                    |result| Message::Theme(ThemeMessage::Deleted(result.map(Box::new))),
                );
            }
            ThemeMessage::Deleted(result) => {
                self.themes.pending = false;
                match result {
                    Ok(change) => {
                        self.status.text = if change.result["deleted"] == json!(true) {
                            "Deleted the theme".into()
                        } else {
                            "The theme was already gone".into()
                        };
                        self.adopt_theme_change(*change);
                        self.outcome(Outcome::ThemesAnswered { failure: None });
                    }
                    // The active theme's refusal names it and says to choose another first.
                    Err(error) => self.theme_refused(error),
                }
            }
        }
        Task::none()
    }

    /// Import one file, chosen in the dialog or named by an evidence step: the same task either
    /// way, which reads the file off the update loop and refuses it there when it is too large.
    pub(crate) fn theme_import(&mut self, path: PathBuf) -> Task<Message> {
        if self.themes.pending {
            self.theme_refused("Waiting for the last theme request".into());
            return Task::none();
        }
        self.themes.pending = true;
        self.themes.refusal = None;
        self.status.text = "Importing the theme\u{2026}".into();
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || import_now(&owner, client, &path),
            |result| Message::Theme(ThemeMessage::Imported(result.map(Box::new))),
        )
    }

    /// List the library again: at launch, when the sheet opens and on another client's theme
    /// event. One read is in flight; a wish that arrives meanwhile lists once more after it.
    pub(crate) fn list_themes(&mut self) -> Task<Message> {
        self.themes.listing.offer(());
        self.start_theme_list()
    }

    fn start_theme_list(&mut self) -> Task<Message> {
        if self.themes.listing.start().is_none() {
            return Task::none();
        }
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || list_now(&owner, client),
            |result| Message::Theme(ThemeMessage::Listed(result)),
        )
    }

    /// Adopt a listing read at `sequence`, unless one read later is already held; close a row menu
    /// whose theme it no longer holds.
    fn adopt_theme_list(&mut self, list: ThemeList, sequence: u64) {
        if self.themes.list.is_some() && sequence < self.themes.sequence {
            return;
        }
        self.themes.sequence = sequence;
        if let Some(MenuTarget::Theme(id)) = &self.view_state.menu
            && list.find(id).is_none()
        {
            self.view_state.menu = None;
        }
        self.themes.list = Some(list);
        self.themes.error = None;
    }

    fn adopt_theme_change(&mut self, change: ThemeChange) {
        self.read_back(change.request);
        self.adopt_theme_list(change.list, change.sequence);
    }

    /// An import or delete was refused: the tab and the status bar say why.
    fn theme_refused(&mut self, reason: String) {
        self.outcome(Outcome::ThemesAnswered {
            failure: Some(&reason),
        });
        self.status.text = reason.clone();
        self.themes.refusal = Some(reason);
    }

    /// Draw the theme the preferences now choose, when it is not the one drawn: Luxforge Dark at
    /// once, any other theme from one `theme.read` off the update loop. One read is in flight; a
    /// choice made meanwhile is read once it answers.
    fn follow_theme(&mut self) -> Task<Message> {
        let Some(wanted) = self.preferences.applied_theme() else {
            return Task::none();
        };
        if wanted == self.themes.drawn.chosen || self.themes.reading.is_some() {
            return Task::none();
        }
        let wanted = wanted.to_owned();
        if wanted == LUXFORGE_DARK_ID {
            self.draw_luxforge_dark(wanted, None);
            return Task::none();
        }
        self.themes.reading = Some(wanted.clone());
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            {
                let id = wanted.clone();
                move || read_now(&owner, client, &id).map(Box::new)
            },
            move |result| Message::Theme(ThemeMessage::Read { id: wanted, result }),
        )
    }

    /// `theme.read` answered for `id`. A choice made since it was asked overtakes it; otherwise the
    /// theme is drawn, or Luxforge Dark in its place with the reason in the status bar.
    fn theme_read(&mut self, id: String, result: Result<Box<ReadTheme>, String>) -> Task<Message> {
        if self.themes.reading.as_deref() == Some(id.as_str()) {
            self.themes.reading = None;
        }
        if self.preferences.applied_theme() != Some(id.as_str()) {
            return Task::none();
        }
        match result {
            Ok(theme) => {
                self.theme = widget_theme(&theme.id, theme.mode, &theme.tokens);
                self.event(
                    "theme_applied",
                    || json!({"id": theme.id, "mode": theme.mode, "generation": self.theme.generation()}),
                );
                self.themes.drawn = DrawnTheme {
                    chosen: id,
                    id: theme.id,
                    name: theme.name,
                    mode: theme.mode,
                    problem: None,
                };
                self.outcome(Outcome::ThemeDrawn);
            }
            Err(reason) => {
                let note = fallback_note(&id, &reason);
                self.status.text = note.clone();
                self.event(
                    "theme_unavailable",
                    || json!({"chosen": id, "reason": note, "at": "change"}),
                );
                self.draw_luxforge_dark(id, Some(note));
            }
        }
        Task::none()
    }

    /// Draw Luxforge Dark for the choice `chosen`: chosen itself, or in place of a theme that
    /// cannot be shown. A Luxforge Dark already on screen is kept, so its canvas caches stay.
    fn draw_luxforge_dark(&mut self, chosen: String, problem: Option<String>) {
        if self.themes.drawn.id != LUXFORGE_DARK_ID {
            self.theme = luxforge_ui::Theme::luxforge_dark();
        }
        self.themes.drawn = DrawnTheme {
            chosen,
            problem,
            ..DrawnTheme::default()
        };
        self.outcome(Outcome::ThemeDrawn);
    }

    /// The theme on screen answers for the preferences' choice, with no read and no preference
    /// write outstanding: what an evidence step that chose a theme waits for.
    pub(crate) fn theme_settled(&self) -> bool {
        self.preferences.idle()
            && self.themes.reading.is_none()
            && self
                .preferences
                .applied_theme()
                .is_none_or(|wanted| wanted == self.themes.drawn.chosen)
    }

    /// What a captured frame records of the theme: the one drawn and the choice it answers for,
    /// why a choice is not drawn, its generation and the resolved tokens the scenarios sample
    /// against, and the library as last listed.
    pub(crate) fn theme_summary(&self) -> Value {
        let drawn = &self.themes.drawn;
        let palette = self.theme.palette();
        json!({
            "id": drawn.id,
            "chosen": drawn.chosen,
            "name": drawn.name,
            "mode": drawn.mode,
            "problem": drawn.problem,
            "generation": self.theme.generation(),
            "tokens": {
                "surround": hex(palette.surround),
                "background": hex(palette.background),
                "surface": hex(palette.surface),
                "control": hex(palette.control),
                "text": hex(palette.text),
            },
            "library": {
                "themes": self.themes.list.as_ref().map(|list| {
                    list.themes.iter().map(|theme| theme.id.as_str()).collect::<Vec<_>>()
                }),
                "active": self.themes.list.as_ref().map(|list| list.active.as_str()),
                "unrecognized": self.themes.list.as_ref().map_or(0, |list| list.unrecognized.len()),
                "error": self.themes.error,
            },
            "reading": self.themes.reading,
            "pending": self.themes.pending,
        })
    }
}

/// `text` with its first letter capitalised, for a core message that opens a status line.
fn capitalised(text: &str) -> String {
    let mut characters = text.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// After every message: draw the theme the preferences now choose, whatever route changed them.
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    editor.follow_theme()
}

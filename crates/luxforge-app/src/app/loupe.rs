//! The loupe ([catalog design](../../../../docs/design/catalog.md#browsing-at-speed)): the
//! active frame of Select's view fitted to the screen with the moment's frames under it, the 100%
//! focus check and compare, with a look-ahead in the direction of travel.
//!
//! - **Stepping.** A key that moves the active frame (`←` `→` frames, `↑` `↓` moments, `1`–`9`
//!   and a strip frame a frame of the moment) is the `browse.select` an API client would send, a
//!   synchronous call in its own update, as the grid's arrows are (performance rule 12): the loupe
//!   draws the session's active frame in the frame after the key, from the handle its look-ahead
//!   already holds when it is warm, with no task, worker or allocation hop between the key and
//!   the presented frame. The grid under the loupe scrolls to keep the active frame in its rows
//!   window, so the rows the loupe names are read.
//! - **Frames.** `app/loupe_frames.rs` reads, decodes and holds the frames on screen and the
//!   look-ahead under the loupe's byte budget; `app/loupe_region.rs` cuts the 100% region.
//! - **Identity.** After every message the loupe mirrors what it holds into its state
//!   ([`Holding`](crate::state::loupe::Holding)); the model draws a picture only under its own
//!   frame, and the view borrows a handle by that frame's item and preview key alone
//!   ([`LoupeImages`]).
//! - **Waking.** The loupe's signal carries a decode landing and the owner's previews wake, which
//!   the Select grid registers for this client and passes on here ([`owner_woke`]); the
//!   subscription exists only while the loupe is open. Nothing polls.
//! - **Picking.** `P` is Select's own pick of the active frame ([`Editor::loupe_pick`] through
//!   [`Editor::select_pick`]), and [`Editor::loupe_picked`] is P7, which the pick's answer calls in
//!   the same update to move on from a picked burst frame to the next moment.
use crate::app::{
    Before, Editor,
    loupe_frames::{self, LoupeFrames, LoupeFramesMessage, Want},
    loupe_region::{self, FocusCheck, RegionMessage, RegionRequest},
    message::{Message, loupe::LoupeMessage, select::SelectMessage},
    select::select_call,
    select_previews::SelectPreviews,
    tasks::owner_work,
    waker::Signal,
};
use crate::state::loupe::{
    self as model, Goto, Holding, Picture, Region, Travel, after_pick, focus_request, subject,
    target, unit_of,
};
use crate::state::select::{SelectGesture, select_params};
use iced::{
    Subscription, Task,
    keyboard::{Key, Modifiers, key::Named},
    widget::image::Handle,
};
use luxforge_core::catalog_types::PreviewItem;
use serde_json::{Value, json};
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};

/// The loupe's own work in the editor: its decoded frames and its focus check. Its view state is
/// [`LoupeState`](crate::state::loupe::LoupeState) in Select's state.
#[derive(Debug, Default)]
pub(crate) struct Loupe {
    pub(crate) frames: LoupeFrames,
    pub(crate) focus: FocusCheck,
}

impl Loupe {
    /// What the view borrows: this loupe's frames and region, and the grid's previews for the
    /// strip.
    pub(crate) fn images<'a>(&'a self, grid: &'a SelectPreviews) -> LoupeImages<'a> {
        LoupeImages {
            frames: &self.frames,
            focus: &self.focus,
            grid,
        }
    }
}

/// The owner woke this client for a preview or a region it waits on; taken on the update loop.
static OWNER_WOKE: AtomicBool = AtomicBool::new(false);

/// The loupe's signal: a decode landed, or the owner woke this client.
fn signal() -> &'static Signal<Message> {
    static SIGNAL: OnceLock<Signal<Message>> = OnceLock::new();
    SIGNAL
        .get_or_init(|| Signal::new(|| Message::Select(SelectMessage::Loupe(LoupeMessage::Woken))))
}

/// The owner's previews wake for this client, passed on by the Select grid, which registers the
/// one wake a client has: called on the owner thread, it only raises the flag and posts the
/// loupe's signal.
pub(crate) fn owner_woke() {
    OWNER_WOKE.store(true, Ordering::Release);
    signal().post();
}

/// A decode landed: called on the loupe's worker, it only posts the signal.
pub(crate) fn post() {
    signal().post();
}

fn loupe_message(message: LoupeMessage) -> Message {
    Message::Select(SelectMessage::Loupe(message))
}

fn frames_message(message: LoupeFramesMessage) -> Message {
    loupe_message(LoupeMessage::Frames(message))
}

fn region_message(message: RegionMessage) -> Message {
    loupe_message(LoupeMessage::Region(message))
}

/// The keys the loupe answers while it is open, ahead of the grid's: the arrows (repeating while
/// held), `1`–`9`, `Z`, `C` and `P`. Every other key falls through to Select's own (`Tab`, `Esc`).
pub(crate) fn loupe_keys(key: &Key, modifiers: &Modifiers, repeat: bool) -> Option<Message> {
    if modifiers.alt() || modifiers.control() || modifiers.logo() {
        return None;
    }
    let step = match key {
        Key::Named(Named::ArrowLeft) => Some(LoupeMessage::Frame(Travel::Back)),
        Key::Named(Named::ArrowRight) => Some(LoupeMessage::Frame(Travel::Forward)),
        Key::Named(Named::ArrowUp) => Some(LoupeMessage::Moment(Travel::Back)),
        Key::Named(Named::ArrowDown) => Some(LoupeMessage::Moment(Travel::Forward)),
        _ => None,
    };
    if let Some(step) = step {
        return Some(loupe_message(step));
    }
    if repeat || modifiers.shift() {
        return None;
    }
    let Key::Character(value) = key else {
        return None;
    };
    let message = match value.to_lowercase().as_str() {
        "z" => LoupeMessage::ToggleFocus,
        "c" => LoupeMessage::ToggleCompare,
        "p" => LoupeMessage::Pick,
        digit => match digit.parse::<u32>() {
            Ok(number @ 1..=9) => LoupeMessage::Jump(number - 1),
            _ => return None,
        },
    };
    Some(loupe_message(message))
}

impl Editor {
    /// One loupe message.
    pub(crate) fn loupe_update(&mut self, message: LoupeMessage) -> Task<Message> {
        match message {
            LoupeMessage::Open => return self.loupe_show(),
            LoupeMessage::Close => return self.loupe_close(),
            LoupeMessage::Frame(travel) => self.loupe_goto(Goto::Frame(travel), travel),
            LoupeMessage::Moment(travel) => self.loupe_goto(Goto::Moment(travel), travel),
            LoupeMessage::Jump(index) => {
                let travel = self.select.state.loupe.travel;
                self.loupe_goto(Goto::Frame0(index), travel);
            }
            LoupeMessage::ToggleFocus => {
                let loupe = &mut self.select.state.loupe;
                if loupe.open {
                    loupe.focus = !loupe.focus;
                    loupe.compare = false;
                    if !loupe.focus {
                        self.select.loupe.focus.release();
                    }
                }
            }
            LoupeMessage::ToggleCompare => self.loupe_compare(),
            LoupeMessage::Pick => return self.loupe_pick(),
            LoupeMessage::Pointer(pointer) => {
                self.select.state.loupe.pointer =
                    pointer.filter(|(x, y)| x.is_finite() && y.is_finite());
            }
            LoupeMessage::Frames(LoupeFramesMessage::Read(answers)) => {
                let batch = self.select.loupe.frames.answered(answers);
                return self.frames_task(batch);
            }
            LoupeMessage::Region(message) => {
                let next = self.select.loupe.focus.update(message);
                return loupe_region::task(&self.owner, self.client, next, region_message);
            }
            LoupeMessage::Woken => {
                let owner = OWNER_WOKE.swap(false, Ordering::AcqRel);
                let batch = self.select.loupe.frames.woken(owner);
                let next = self.select.loupe.focus.woken(owner);
                return Task::batch([
                    self.frames_task(batch),
                    loupe_region::task(&self.owner, self.client, next, region_message),
                ]);
            }
        }
        Task::none()
    }

    /// The loupe is open over Select's centre.
    pub(crate) fn loupe_open(&self) -> bool {
        self.select_shown() && self.select.state.loupe.open
    }

    /// Show the active frame in the loupe; with none active, the view's first. An empty view has
    /// nothing to show.
    fn loupe_show(&mut self) -> Task<Message> {
        if !self.select_shown()
            || self
                .select
                .state
                .summary
                .as_ref()
                .is_none_or(|summary| summary.count == 0)
        {
            return Task::none();
        }
        let loupe = &mut self.select.state.loupe;
        loupe.open = true;
        loupe.compare = false;
        loupe.focus = false;
        loupe.travel = Travel::Forward;
        self.select.state.menu = None;
        if subject(self.select.state.summary.as_ref(), &self.session.browse).is_none() {
            self.loupe_select(0);
        }
        Task::none()
    }

    /// Back to the grid, which shows the active frame: the loupe's frames and region are released
    /// and its reads still queued cancelled.
    fn loupe_close(&mut self) -> Task<Message> {
        let loupe = &mut self.select.state.loupe;
        loupe.open = false;
        loupe.compare = false;
        loupe.focus = false;
        loupe.pointer = None;
        loupe.held = Holding::default();
        self.select.loupe.focus.release();
        let batch = self.select.loupe.frames.release();
        self.frames_task(batch)
    }

    /// Release the loupe's frames when Select is left: the loupe stays open for when it is shown
    /// again.
    pub(crate) fn loupe_leave(&mut self) -> Task<Message> {
        self.select.state.loupe.held = Holding::default();
        self.select.loupe.focus.release();
        let batch = self.select.loupe.frames.release();
        self.frames_task(batch)
    }

    /// `C`: compare the active frame's moment side by side, or back to the one frame. A single frame
    /// has nothing to compare, which the status bar says.
    fn loupe_compare(&mut self) {
        let Some(summary) = self.select.state.summary.as_ref() else {
            return;
        };
        let Some(subject) = subject(Some(summary), &self.session.browse) else {
            return;
        };
        let loupe = &mut self.select.state.loupe;
        if !loupe.open {
            return;
        }
        if loupe.compare {
            loupe.compare = false;
        } else if unit_of(summary, subject.position).moment.is_some() {
            loupe.compare = true;
            loupe.focus = false;
            self.select.loupe.focus.release();
        } else {
            self.status.text =
                "Compare shows a burst's or a bracket's frames: this is a single frame".into();
        }
    }

    /// Move the active frame as `goto` says, travelling `travel`.
    fn loupe_goto(&mut self, goto: Goto, travel: Travel) {
        if !self.loupe_open() {
            return;
        }
        let Some(summary) = self.select.state.summary.as_ref() else {
            return;
        };
        let Some(subject) = subject(Some(summary), &self.session.browse) else {
            return;
        };
        self.select.state.loupe.travel = travel;
        if let Some(position) = target(summary, subject.position, goto) {
            self.loupe_select(position);
        }
    }

    /// `browse.select` of `position` alone, synchronously in this update, and the grid under the
    /// loupe scrolled to it. Answers whether the owner took it.
    fn loupe_select(&mut self, position: u32) -> bool {
        let Some(revision) = self.select.state.revision() else {
            return false;
        };
        let params = select_params(
            SelectGesture::Only {
                item: position,
                span: 1,
            },
            Some(revision),
        );
        match select_call(&self.owner, self.client, params) {
            Ok(session) => {
                self.adopt(session);
                self.select.anchor = Some(position);
                self.select.scroll = self.select.layout.reveal(
                    position,
                    self.select.scroll,
                    self.select.viewport.height,
                );
                true
            }
            Err(error) => {
                self.status.text = format!("Selection failed: {error}");
                false
            }
        }
    }

    /// `P`: pick or clear the active frame, through [`Self::select_pick`] as the grid's `P` does;
    /// its answer, in this update, reaches [`Self::loupe_picked`].
    fn loupe_pick(&mut self) -> Task<Message> {
        if !self.loupe_open() {
            return Task::none();
        }
        self.pick_active()
    }

    /// A pick lane D's `select_pick` answered: P7. When it picked the loupe's active frame and that
    /// frame is a burst's, the loupe moves on to the next moment's first frame; a clear, a
    /// bracket's frame, a single, a failed pick or another frame's pick stay where they are.
    /// `positions` are the view positions the pick named.
    pub(crate) fn loupe_picked(&mut self, positions: &[u32], picked: bool, succeeded: bool) {
        if !succeeded || !self.loupe_open() {
            return;
        }
        let Some(summary) = self.select.state.summary.as_ref() else {
            return;
        };
        let Some(active) = subject(Some(summary), &self.session.browse).map(|at| at.position)
        else {
            return;
        };
        if !positions.contains(&active) {
            return;
        }
        if let Some(next) = after_pick(summary, active, picked) {
            self.select.state.loupe.travel = Travel::Forward;
            self.loupe_select(next);
        }
    }

    /// The owner task a read batch is.
    fn frames_task(&self, batch: Option<loupe_frames::ReadBatch>) -> Task<Message> {
        let Some(batch) = batch else {
            return Task::none();
        };
        let (owner, client) = (self.owner.clone(), self.client);
        owner_work(move || loupe_frames::read(&owner, client, batch))
            .map(|answers| frames_message(LoupeFramesMessage::Read(answers)))
    }

    /// What the loupe wants now, as the frames cache takes it.
    fn loupe_wants(&self) -> Vec<Want> {
        model::wanted(&self.select.state, &self.session.browse)
            .into_iter()
            .map(|frame| Want {
                item: frame.item,
                pixels: frame.pixels,
                shown: frame.shown,
            })
            .collect()
    }

    /// The region the focus check asks for now.
    fn loupe_region(&self) -> Option<RegionRequest> {
        focus_request(&self.select.state, &self.session.browse)
            .map(|(item, rect, frame)| RegionRequest { item, rect, frame })
    }

    /// Mirror what the loupe holds into its state, for the model: the pictures of the frames on
    /// screen, each under its own item, the region and the look-ahead's progress.
    fn loupe_mirror(&mut self, wants: &[Want]) {
        let frames = &self.select.loupe.frames;
        let focus = &self.select.loupe.focus;
        let (ahead, ahead_ready) = frames.ahead();
        let active = wants.first().map(|want| &want.item);
        self.select.state.loupe.held = Holding {
            frames: wants
                .iter()
                .filter(|want| want.shown)
                .map(|want| frames.held(&want.item))
                .collect(),
            region: focus.region(),
            region_pending: focus.pending(),
            region_error: active.and_then(|item| focus.error(item)),
            frame_of: focus.frame_of(),
            ahead,
            ahead_ready,
        };
    }

    /// Everything the loupe waits for has arrived: the frames on screen decoded at their size, and
    /// the focus check's region of the rectangle under the pointer. True while it is closed. An
    /// evidence step captures once it is.
    pub(crate) fn loupe_settled(&self) -> bool {
        if !self.loupe_open() {
            return true;
        }
        let state = &self.select.state;
        let Some(subject) = subject(state.summary.as_ref(), &self.session.browse) else {
            return true;
        };
        if state.rows.revision() != subject.revision || state.rows.row(subject.position).is_none() {
            return false;
        }
        let wants = self.loupe_wants();
        self.select.loupe.frames.settled(&wants)
            && self
                .loupe_region()
                .is_none_or(|request| self.select.loupe.focus.settled(&request))
    }

    /// What the loupe shows and holds, for correlated evidence: the active frame and the picture
    /// drawn for it (their identity compared), the bar, the strip, compare, the focus check and the
    /// look-ahead. No path is recorded.
    pub(crate) fn loupe_summary(&self) -> Value {
        let state = &self.select.state.loupe;
        let model = &self.workspace.select.loupe;
        let picture = |picture: Option<&Picture>| {
            picture.map(|picture| {
                json!({
                    "item": picture.item,
                    "key": picture.key,
                    "origin": picture.origin.as_str(),
                    "width": picture.width,
                    "height": picture.height,
                    "stand_in": picture.stand_in,
                    "approximate": picture.approximate,
                })
            })
        };
        let frame = model.frame.as_ref();
        json!({
            "open": state.open,
            "compare": state.compare,
            "focus": state.focus,
            "travel": format!("{:?}", state.travel).to_lowercase(),
            "subject": model.subject.map(|subject| json!({
                "position": subject.position,
                "revision": subject.revision,
                "count": subject.count,
                "moment": subject.moment.map(|moment| json!({
                    "index": moment.index, "start": moment.start, "len": moment.len,
                })),
            })),
            "active": frame.map(|frame| json!({
                "position": frame.position,
                "item": frame.item,
                "name": frame.name,
                "rect": frame.rect.to_array(),
                "note": frame.note,
            })),
            "picture": picture(frame.and_then(|frame| frame.picture.as_ref())),
            "identity": frame.map(|frame| frame.picture.as_ref().is_none_or(|picture| picture.item == frame.item)),
            "info": {
                "moment": model.info.moment,
                "frame": model.info.frame,
                "exposure": model.info.exposure,
                "source": model.info.source,
            },
            "strip": model.strip.as_ref().map(|strip| json!({
                "first": strip.first,
                "active": strip.active,
                "start": strip.start,
                "positions": strip.frames.iter().map(|frame| frame.position).collect::<Vec<_>>(),
                "picked": strip.frames.iter().filter(|frame| frame.picked).count(),
            })),
            "compare_frames": model.compare.iter().map(|cell| json!({
                "position": cell.position,
                "number": cell.number,
                "item": cell.item,
                "active": cell.active,
                "rect": cell.rect.to_array(),
                "picture": picture(cell.picture.as_ref()),
            })).collect::<Vec<_>>(),
            "focus_check": model.focus.as_ref().map(|focus| json!({
                "rect": focus.rect,
                "frame": focus.frame,
                "box": focus.region_box.to_array(),
                "inset": focus.inset.to_array(),
                "developed": focus.developed,
                "pending": focus.pending,
                "error": focus.error,
                "region": focus.region.as_ref().map(|region| json!({
                    "item": region.item,
                    "rect": region.rect,
                    "frame": region.frame,
                    "origin": region.origin.as_str(),
                })),
            })),
            "hints": model.hints.iter().map(|(key, _)| key).collect::<Vec<_>>(),
            "status": model.status,
            "area": model.area.to_array(),
            "frames": self.select.loupe.frames.summary(),
            "regions": self.select.loupe.focus.summary(),
            "settled": self.loupe_settled(),
        })
    }

    /// What the view borrows to draw the loupe.
    pub(crate) fn loupe_images(&self) -> LoupeImages<'_> {
        self.select.loupe.images(&self.select.previews)
    }
}

/// After every message while the loupe is open: its geometry from the window, what it wants read
/// and decoded, the region under the pointer, and what it holds mirrored for the model.
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    if !editor.loupe_open() {
        return Task::none();
    }
    let loupe = &mut editor.select.state.loupe;
    loupe.window = editor.view_state.window;
    loupe.scale = editor.view_state.scale_factor;
    let wants = editor.loupe_wants();
    let batch = editor.select.loupe.frames.want(wants.clone());
    let frames = editor.frames_task(batch);
    // The region is named in the frame the last region answered, which the mirror carries.
    editor.loupe_mirror(&wants);
    let request = editor.loupe_region();
    let next = editor.select.loupe.focus.want(request);
    let region = loupe_region::task(&editor.owner, editor.client, next, region_message);
    editor.loupe_mirror(&wants);
    Task::batch([frames, region])
}

/// The loupe's signal while it is open. A signal posted while it is closed waits for it.
pub(super) fn subscription(editor: &Editor) -> Subscription<Message> {
    if editor.loupe_open() {
        Subscription::run(|| signal().stream())
    } else {
        Subscription::none()
    }
}

/// What the loupe's view borrows: each frame's handle by its own item and preview key, the
/// region's handle, and the grid's previews for the strip. Plain lookups, so drawing makes no
/// handle and uploads nothing again.
#[derive(Clone, Copy)]
pub(crate) struct LoupeImages<'a> {
    frames: &'a LoupeFrames,
    focus: &'a FocusCheck,
    grid: &'a SelectPreviews,
}

impl<'a> LoupeImages<'a> {
    /// The handle of `picture`, held for exactly its item and key.
    pub(crate) fn picture(&self, picture: &Picture) -> Option<&'a Handle> {
        let frames: &'a LoupeFrames = self.frames;
        frames.handle(picture)
    }

    /// The handle of `region`, the one the focus check holds.
    pub(crate) fn region(&self, region: &Region) -> Option<&'a Handle> {
        let focus: &'a FocusCheck = self.focus;
        focus.handle(region)
    }

    /// A strip frame's grid preview, as the grid holds it for a file near the active one.
    pub(crate) fn thumbnail(&self, item: &PreviewItem) -> Option<&'a Handle> {
        let grid: &'a SelectPreviews = self.grid;
        match item {
            PreviewItem::File { file_id } => grid.handle(*file_id),
            PreviewItem::Photo { .. } => None,
        }
    }
}

#[cfg(test)]
#[path = "loupe_tests.rs"]
mod tests;

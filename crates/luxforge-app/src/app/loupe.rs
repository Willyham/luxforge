//! The loupe ([catalog design](../../../../docs/design/catalog.md#browsing-at-speed), TASK-020): the
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
//! - **Picking.** `P` is lane D's pick (TASK-019), not yet on this branch: [`Editor::loupe_pick`]
//!   is the one hook that calls it, and [`Editor::loupe_picked`] is P7, which the pick's answer
//!   calls to move on from a picked burst frame to the next moment.
//! - **Timing evidence.** Where a run writes events, the loupe records what a timing harness pairs:
//!   each key that moves the active frame (`loupe_key`, stamped `pressed_ms` from the start of its
//!   handling, with whether the frame it moved to was already held at its size), `Z` (`loupe_focus`)
//!   and each region asked for (`loupe_region_asked`), and, from the model just derived — the
//!   view's source, drawn by the redraw that update requests — each picture the active frame
//!   presents (`loupe_presented`, under its own item and preview key, a stand-in said so) and each
//!   region the inset presents (`loupe_region_presented`). Presented means that update, as it
//!   does for the photograph's `preview_displayed`: not display scanout.
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
    trace: Trace,
}

/// What the loupe has reported to the evidence log: how many keys moved it, and the picture and
/// the region it presented last, so each is reported once, when it changes. Kept whether or not a
/// log is written; nothing else reads it.
#[derive(Debug, Default)]
struct Trace {
    keys: u64,
    /// The active frame's picture: its view position, item and preview key.
    picture: Option<(u32, PreviewItem, String)>,
    /// The inset's region, by its request's number.
    region: Option<u64>,
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
            LoupeMessage::Frame(travel) => self.loupe_key(Goto::Frame(travel), travel),
            LoupeMessage::Moment(travel) => self.loupe_key(Goto::Moment(travel), travel),
            LoupeMessage::Jump(index) => {
                let travel = self.select.state.loupe.travel;
                self.loupe_key(Goto::Frame0(index), travel);
            }
            LoupeMessage::ToggleFocus => {
                let pressed_ms = self.log_ms();
                let loupe = &mut self.select.state.loupe;
                if loupe.open {
                    loupe.focus = !loupe.focus;
                    loupe.compare = false;
                    if !loupe.focus {
                        self.select.loupe.focus.release();
                    }
                    let on = loupe.focus;
                    let position = self.loupe_position();
                    self.event(
                        "loupe_focus",
                        || json!({"on": on, "position": position, "pressed_ms": pressed_ms}),
                    );
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
                return self.region_task(next);
            }
            LoupeMessage::Woken => {
                let owner = OWNER_WOKE.swap(false, Ordering::AcqRel);
                let batch = self.select.loupe.frames.woken(owner);
                let next = self.select.loupe.focus.woken(owner);
                return Task::batch([self.frames_task(batch), self.region_task(next)]);
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

    /// A key that moves the active frame as `goto` says: moved, and recorded for the evidence log
    /// with the moment its handling started, where it moved from and to, and whether the frame it
    /// moved to was already held at the size it is drawn at, so this update presents it.
    fn loupe_key(&mut self, goto: Goto, travel: Travel) {
        let pressed_ms = self.log_ms();
        let from = self.loupe_position();
        self.loupe_goto(goto, travel);
        self.select.loupe.trace.keys += 1;
        let key = self.select.loupe.trace.keys;
        self.event("loupe_key", || {
            let (kind, index) = match goto {
                Goto::Frame(_) => ("frame", None),
                Goto::Moment(_) => ("moment", None),
                Goto::Frame0(index) => ("jump", Some(index)),
            };
            json!({
                "key": key,
                "kind": kind,
                "index": index,
                "travel": format!("{travel:?}").to_lowercase(),
                "from": from,
                "to": self.loupe_position(),
                "ready": self.loupe_active_ready(),
                "pressed_ms": pressed_ms,
            })
        });
    }

    /// The milliseconds since the run started that every event is stamped with.
    fn log_ms(&self) -> f64 {
        self.log.started.elapsed().as_secs_f64() * 1000.0
    }

    /// The view position of the active frame, when the loupe has one.
    fn loupe_position(&self) -> Option<u32> {
        subject(self.select.state.summary.as_ref(), &self.session.browse)
            .map(|subject| subject.position)
    }

    /// The active frame's own picture, not a stand-in, is held at the size it is drawn at.
    fn loupe_active_ready(&self) -> bool {
        self.loupe_wants()
            .first()
            .is_some_and(|want| want.shown && self.select.loupe.frames.ready(want))
    }

    /// The look-ahead is warm: the loupe has settled, the rows of every frame ahead in the direction
    /// of travel are read, and every frame it wants — on screen and ahead — is decoded at the size
    /// it is drawn at, or has nothing more to wait for. A key that moves to the next frame then
    /// presents it in its own update. An evidence step that times the loupe presses once it is.
    pub(crate) fn loupe_warm(&self) -> bool {
        let state = &self.select.state;
        let (Some(summary), Some(subject)) = (
            state.summary.as_ref(),
            subject(state.summary.as_ref(), &self.session.browse),
        ) else {
            return false;
        };
        self.loupe_open()
            && self.loupe_settled()
            && model::look_ahead(summary, subject.position, state.loupe.travel)
                .iter()
                .all(|position| state.rows.row(*position).is_some())
            && self.select.loupe.frames.all_settled(&self.loupe_wants())
    }

    /// The frames the look-ahead wants now and how many of them are ready, for the evidence log.
    pub(crate) fn loupe_ahead(&self) -> (usize, usize) {
        let wants = self.loupe_wants();
        let ahead: Vec<&Want> = wants.iter().filter(|want| !want.shown).collect();
        let ready = ahead
            .iter()
            .filter(|want| self.select.loupe.frames.ready(want))
            .count();
        (ahead.len(), ready)
    }

    /// The owner task the focus check's `next` is, with a region asked for recorded for the
    /// evidence log.
    fn region_task(&self, next: Option<loupe_region::Next>) -> Task<Message> {
        if let Some(loupe_region::Next::Start(serial, request)) = &next {
            self.event("loupe_region_asked", || {
                json!({
                    "serial": serial,
                    "item": request.item,
                    "rect": request.rect,
                    "frame": request.frame,
                })
            });
        }
        loupe_region::task(&self.owner, self.client, next, region_message)
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

    /// `P`: pick or clear the active frame.
    ///
    /// HOOK (lane D, TASK-019): picks are lane D's, and `Editor::select_pick(items, picked)` with its
    /// completion `SelectMessage::Picked { items, picked, result }` is not on this branch yet. Once
    /// it is, this sends it for the active frame and the completion calls
    /// [`Self::loupe_picked`]. Until then `P` picks nothing and says so.
    fn loupe_pick(&mut self) -> Task<Message> {
        if self.loupe_open() {
            self.status.text = "Picking from the loupe comes with picks (not yet available)".into();
        }
        Task::none()
    }

    /// A pick lane D's `select_pick` answered: P7. When it picked the loupe's active frame and that
    /// frame is a burst's, the loupe moves on to the next moment's first frame; a clear, a
    /// bracket's frame, a single, a failed pick or another frame's pick stay where they are.
    /// `positions` are the view positions the pick named.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "lane D's pick answer (TASK-019) calls it; not on this branch yet"
        )
    )]
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
    let region = editor.region_task(next);
    editor.loupe_mirror(&wants);
    Task::batch([frames, region])
}

/// After the screen is derived: what the loupe's model now draws — the active frame's picture and
/// the inset's region — reported to the evidence log when it changes. The model is the view's
/// source, so this is the update whose redraw draws them. Compare draws its cells instead of the
/// one picture, so nothing is presented for the active frame while it is on.
pub(super) fn after_derive(editor: &mut Editor) -> Task<Message> {
    let open = editor.loupe_open();
    let model = &editor.workspace.select.loupe;
    let picture = (open && !editor.select.state.loupe.compare)
        .then_some(model.frame.as_ref())
        .flatten()
        .and_then(|frame| Some((frame.position, frame.picture.as_ref()?)));
    let region = open
        .then_some(model.focus.as_ref())
        .flatten()
        .and_then(|focus| focus.region.as_ref());
    let trace = &editor.select.loupe.trace;
    let picture_changed = trace
        .picture
        .as_ref()
        .map(|(at, item, key)| (*at, item, key.as_str()))
        != picture.map(|(at, picture)| (at, &picture.item, picture.key.as_str()));
    let region_changed = trace.region != region.map(|region| region.serial);
    if !picture_changed && !region_changed {
        return Task::none();
    }
    let picture = picture.map(|(at, picture)| (at, picture.clone()));
    let region = region.cloned();
    let trace = &mut editor.select.loupe.trace;
    let key = trace.keys;
    if picture_changed {
        trace.picture = picture
            .as_ref()
            .map(|(at, picture)| (*at, picture.item.clone(), picture.key.clone()));
    }
    if region_changed {
        trace.region = region.as_ref().map(|region| region.serial);
    }
    if picture_changed && let Some((position, picture)) = picture {
        editor.event("loupe_presented", || {
            json!({
                "key": key,
                "position": position,
                "item": picture.item,
                "preview_key": picture.key,
                "origin": picture.origin.as_str(),
                "stand_in": picture.stand_in,
                "approximate": picture.approximate,
                "width": picture.width,
                "height": picture.height,
            })
        });
    }
    if region_changed && let Some(region) = region {
        editor.event("loupe_region_presented", || {
            json!({
                "serial": region.serial,
                "item": region.item,
                "rect": region.rect,
                "frame": region.frame,
                "origin": region.origin.as_str(),
            })
        });
    }
    Task::none()
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

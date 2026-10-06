//! The core draft lifecycle as one pure state machine.
//!
//! Every desktop gesture — a slider, a mask shape or stroke, the crop frame — is one [`CoreDraft`].
//! It holds no owner handle and no framework type: it opens on the draft `draft.begin` answered
//! with, takes an [`Event`] (the gesture offered fields, an owner answer arrived, the pointer was
//! released, Discard or Reapply was pressed, the asset moved) and answers with the one [`Step`] the
//! driver must take next. The driver in [`crate::app::gesture`] runs the step against the owner and
//! feeds the answer back, so every rule of the lifecycle lives here once:
//!
//! - `draft.begin` and `draft.cancel` are answered in the update that sends them, and so are
//!   `draft.set` and `draft.reapply` unless their plan reads a pixel — a colour-limited stroke's
//!   seed — which the interface thread never waits for; those and `draft.commit` outlive an update,
//!   and nothing else is sent while one is in flight;
//! - the newest offered fields win, and fields equal to the ones already accepted are not re-sent;
//! - a gesture whose fields are costly to build — a brush stroke's decimated path — only says it
//!   changed ([`Event::Changed`]), and its fields are built when they can be sent, once per send;
//! - a release commits exactly once, after every offered field has reached the core draft;
//! - a conflicted draft is never committed: release is refused until Discard or Reapply;
//! - Discard during a commit lets the commit decide and cancels only if the commit is refused;
//! - the commit's answer names the gesture and the core draft it belongs to, so a stale one is
//!   recognised and dropped rather than adopted by a newer gesture.
use luxforge_core::{Draft, DraftId, ErrorKind};
use serde_json::{Value, json};

/// A desktop-local identity for one gesture, minted when it opens, which the answers of its owner
/// tasks — the commit, a mask gesture's `render.transform` — name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GestureId(pub(crate) u64);

/// The owner round trip a draft is waiting on. `draft.set` and `draft.reapply` are in flight from
/// [`Step::Set`] or [`Step::Reapply`] to the [`Event::Set`] or [`Event::Reapplied`] that answers
/// it: in the same update, unless the call reads a pixel and answers as a message. The commit is
/// always an owner task, in flight until its answer arrives as a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Round {
    Set,
    Reapply,
    Commit,
}

/// How a gesture that ended while a round trip was in flight is to finish once it answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Finish {
    Commit,
    Cancel,
}

/// Something that happened to the gesture.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Event {
    /// The gesture's fields now, as one JSON object: what the next `draft.set` carries.
    Offer(Value),
    /// The gesture's fields may have changed. They are built, through the builder the driver hands
    /// [`CoreDraft::handle_with`], only when they can be sent: at once when nothing is in flight,
    /// otherwise once the round trip in flight has answered. A brush stroke's path is decimated and
    /// serialized once per `draft.set`, however many moves changed it meanwhile.
    Changed,
    /// The `draft.set` answered: the draft it accepted, or why no frame follows.
    Set(Result<Draft, String>),
    /// The pointer was released, a key came up or Apply was pressed: commit once.
    Release,
    /// Escape, Discard or a script: end the gesture and commit nothing.
    Cancel,
    /// The Changed elsewhere notice's Reapply.
    Reapply,
    /// The `draft.reapply` that [`Step::Reapply`] asked for answered.
    Reapplied(Result<Draft, String>),
    /// `draft.commit` answered: `Ok` for an entry or a no-op, which ends the gesture, the refusal
    /// otherwise.
    Committed(Result<(), String>),
    /// A new authoritative asset revision arrived.
    Revision(u64),
}

/// What the driver does next.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Step {
    /// Nothing to send.
    None,
    /// Send these fields with `draft.set`, and feed the answer back: at once, or as a message when
    /// the set reads a pixel.
    Set { draft_id: DraftId, fields: Value },
    /// Commit the core draft once, expecting the revision it is based on.
    Commit {
        draft_id: DraftId,
        expected_revision: u64,
    },
    /// End the core draft with `draft.cancel`, synchronously: the gesture is over.
    Cancel(DraftId),
    /// Rebase the core draft on the current revision with `draft.reapply`, and feed the answer
    /// back: at once, or as a message when the rebased draft reads a pixel.
    Reapply(DraftId),
    /// The draft has just become conflicted: say so, keep it.
    Conflicted,
    /// Release was refused because the draft is conflicted.
    Refused,
}

/// One gesture's core draft, as the desktop tracks it. The authoritative draft lives in this
/// client's core session; this is the correlation the desktop needs to bound its requests.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CoreDraft {
    pub(crate) gesture: GestureId,
    pub(crate) draft_id: DraftId,
    pub(crate) base_revision: u64,
    pub(crate) draft_revision: u64,
    pub(crate) conflicted: bool,
    in_flight: Option<Round>,
    /// The newest fields the gesture offered that no `draft.set` has carried yet.
    pending: Option<Value>,
    /// The gesture changed while a round trip was in flight, and its fields are still to be built
    /// ([`Event::Changed`]): within an update, or across updates while a set or reapply that reads
    /// a pixel or the commit is out.
    changed: bool,
    /// The fields the last accepted `draft.set` carried.
    sent: Option<Value>,
    finish: Option<Finish>,
}

/// The driver's builder of the gesture's fields, run when [`Event::Changed`] fields can be sent:
/// `None` when the gesture has no fields to offer, a failed capture's.
pub(crate) type Build<'a> = dyn FnMut() -> Option<Value> + 'a;

impl CoreDraft {
    /// A Discard was pressed while the commit was in flight. The commit decided publication; the
    /// gesture still ends as a cancelled one does.
    pub(crate) fn cancel_requested(&self) -> bool {
        self.finish == Some(Finish::Cancel)
    }

    /// A gesture opens on the draft its synchronous `draft.begin` answered with. `fields` are what
    /// the gesture already holds, sent at once.
    pub(crate) fn open(gesture: GestureId, opened: Draft, fields: Option<Value>) -> (Self, Step) {
        let mut draft = Self {
            gesture,
            draft_id: opened.draft_id,
            base_revision: opened.base_revision,
            draft_revision: opened.draft_revision,
            conflicted: opened.conflicted,
            in_flight: None,
            pending: fields,
            changed: false,
            sent: None,
            finish: None,
        };
        let step = match draft.advance(&mut || None) {
            Step::None if draft.conflicted => Step::Conflicted,
            step => step,
        };
        (draft, step)
    }

    /// The round trip in flight, if any.
    pub(crate) fn in_flight(&self) -> Option<Round> {
        self.in_flight
    }

    /// The fields the last accepted `draft.set` carried.
    pub(crate) fn sent(&self) -> Option<&Value> {
        self.sent.as_ref()
    }

    /// Fields are waiting to be sent: offered, and not the ones already accepted, or changed and not
    /// yet built.
    fn outstanding(&self) -> bool {
        self.changed || (self.pending.is_some() && self.pending != self.sent)
    }

    /// Nothing is in flight, nothing is waiting and nothing is due: the frame the last `draft.set`
    /// asked for is the gesture's newest.
    pub(crate) fn drained(&self) -> bool {
        self.in_flight.is_none() && !self.outstanding() && self.finish.is_none()
    }

    /// Something the gesture asked for will still put a frame of its own on screen: a round trip
    /// whose answer brings one, fields waiting to be sent, or a commit due. Fields a conflicted
    /// draft holds back until Reapply bring none, so a frame on screen then is the gesture's newest.
    pub(crate) fn frame_pending(&self) -> bool {
        match self.in_flight {
            Some(_) => true,
            None => (self.outstanding() && !self.conflicted) || self.finish.is_some(),
        }
    }

    /// The core draft has accepted fields at least once, so drafted frames may be in the queue.
    pub(crate) fn drafted(&self) -> bool {
        self.draft_revision > 0
    }

    /// The draft's commit is out: it takes no more fields and no second release.
    fn settled(&self) -> bool {
        self.in_flight == Some(Round::Commit)
    }

    /// Whether an owner answer belongs to this draft: the gesture that asked, and its core draft.
    pub(crate) fn answers(&self, gesture: GestureId, draft: &DraftId) -> bool {
        self.gesture == gesture && draft == &self.draft_id
    }

    /// The draft as this desktop knows it, in the shape `session.state` reports, for the evidence
    /// frame while the session's own copy has not caught up.
    pub(crate) fn summary(&self, action: &str) -> Value {
        json!({
            "draft_id": self.draft_id.as_str(),
            "action": action,
            "fields": self.sent.clone().unwrap_or_else(|| json!({})),
            "base_revision": self.base_revision,
            "draft_revision": self.draft_revision,
            "conflicted": self.conflicted,
        })
    }

    /// [`Self::handle_with`] for a gesture that offers its fields ([`Event::Offer`]) and has none
    /// to build, as the tests of the lifecycle drive one.
    #[cfg(test)]
    pub(crate) fn handle(&mut self, event: Event) -> Step {
        self.handle_with(event, &mut || None)
    }

    /// Take one event and answer the step it calls for, building the gesture's fields with `build`
    /// when it changed ([`Event::Changed`]) and they can be sent.
    pub(crate) fn handle_with(&mut self, event: Event, build: &mut Build<'_>) -> Step {
        match event {
            Event::Offer(_) | Event::Changed | Event::Release if self.settled() => Step::None,
            Event::Offer(fields) => {
                self.pending = Some(fields);
                self.changed = false;
                self.advance(build)
            }
            Event::Changed => {
                self.changed = true;
                self.advance(build)
            }
            Event::Set(answer) => {
                if self.in_flight != Some(Round::Set) {
                    return Step::None;
                }
                self.in_flight = None;
                if let Ok(set) = answer {
                    self.draft_revision = set.draft_revision;
                    self.conflicted = set.conflicted;
                }
                self.advance(build)
            }
            Event::Release => {
                if self.conflicted {
                    self.finish = None;
                    return Step::Refused;
                }
                self.finish = Some(Finish::Commit);
                self.advance(build)
            }
            Event::Cancel => {
                // During a commit the commit decides: an entry ends the gesture, a refusal cancels
                // it. Otherwise the draft is cancelled now.
                self.pending = None;
                self.changed = false;
                self.finish = Some(Finish::Cancel);
                self.advance(build)
            }
            Event::Reapply => {
                if self.in_flight.is_some() || self.finish.is_some() {
                    return Step::None;
                }
                self.in_flight = Some(Round::Reapply);
                Step::Reapply(self.draft_id.clone())
            }
            Event::Reapplied(answer) => {
                if self.in_flight != Some(Round::Reapply) {
                    return Step::None;
                }
                self.in_flight = None;
                if let Ok(rebased) = answer {
                    self.base_revision = rebased.base_revision;
                    self.draft_revision = rebased.draft_revision;
                    self.conflicted = rebased.conflicted;
                    // Re-send what this client set: the rebased draft still holds it, but the
                    // frame on screen belongs to the revision that displaced it.
                    self.pending = self.pending.take().or_else(|| self.sent.clone());
                    self.sent = None;
                }
                self.advance(build)
            }
            Event::Committed(answer) => {
                if self.in_flight != Some(Round::Commit) {
                    return Step::None;
                }
                self.in_flight = None;
                match answer {
                    // The gesture is over, a Discard pressed meanwhile included: the answer's
                    // handler takes it out of the slot.
                    Ok(()) => {
                        self.finish = None;
                        Step::None
                    }
                    Err(error) => {
                        if error.starts_with(ErrorKind::Conflict.code()) {
                            self.conflicted = true;
                        }
                        if self.finish == Some(Finish::Cancel) {
                            return self.advance(build);
                        }
                        self.finish = None;
                        Step::None
                    }
                }
            }
            Event::Revision(revision) => {
                // While the commit is in flight the new revision is most likely its own; the
                // commit's answer says whether it was refused as stale, so it decides.
                if self.conflicted
                    || self.in_flight == Some(Round::Commit)
                    || revision == self.base_revision
                {
                    return Step::None;
                }
                self.conflicted = true;
                Step::Conflicted
            }
        }
    }

    /// Nothing is in flight: send what the gesture asked for meanwhile. Discard first, then the
    /// newest fields, then the commit — a commit sends the core draft's fields, not the desktop's,
    /// so every offered field must be there before it goes. Fields the gesture changed are built
    /// here, once; a gesture that cannot build them leaves what it offered before.
    fn advance(&mut self, build: &mut Build<'_>) -> Step {
        if self.in_flight.is_some() {
            return Step::None;
        }
        let draft_id = self.draft_id.clone();
        if self.finish == Some(Finish::Cancel) {
            return Step::Cancel(draft_id);
        }
        if std::mem::take(&mut self.changed)
            && let Some(fields) = build()
        {
            self.pending = Some(fields);
        }
        if self.outstanding() && !self.conflicted {
            let fields = self.pending.take().expect("outstanding fields");
            self.sent = Some(fields.clone());
            self.in_flight = Some(Round::Set);
            return Step::Set { draft_id, fields };
        }
        if self.finish == Some(Finish::Commit) {
            self.finish = None;
            if self.conflicted {
                return Step::Refused;
            }
            self.in_flight = Some(Round::Commit);
            return Step::Commit {
                draft_id,
                expected_revision: self.base_revision,
            };
        }
        Step::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_core::AssetId;

    const GESTURE: GestureId = GestureId(7);

    fn answer(base_revision: u64) -> Draft {
        Draft::new("set-basic", AssetId::new(), base_revision)
    }

    fn fields(value: f64) -> Value {
        json!({ "exposure": value })
    }

    /// A draft opened on revision 4 with nothing to send yet.
    fn begun() -> (CoreDraft, DraftId) {
        let (draft, step) = CoreDraft::open(GESTURE, answer(4), None);
        assert_eq!(step, Step::None);
        let id = draft.draft_id.clone();
        (draft, id)
    }

    fn accepted(draft: &CoreDraft, revision: u64) -> Draft {
        let mut set = answer(draft.base_revision);
        set.draft_id = draft.draft_id.clone();
        set.draft_revision = revision;
        set
    }

    /// Offer `value` and answer the `draft.set` it sends, accepted at `revision`.
    fn set(draft: &mut CoreDraft, value: f64, revision: u64) {
        draft.handle(Event::Offer(fields(value)));
        let set = accepted(draft, revision);
        draft.handle(Event::Set(Ok(set)));
    }

    #[test]
    fn a_gesture_opening_with_fields_sends_them_at_once() {
        let opened = answer(4);
        let id = opened.draft_id.clone();
        let (mut draft, step) = CoreDraft::open(GESTURE, opened, Some(fields(0.3)));
        assert_eq!(
            step,
            Step::Set {
                draft_id: id,
                fields: fields(0.3)
            },
            "the begin has answered, so the fields go in the same update"
        );
        assert!(!draft.drained(), "the set is in flight");
        let set = accepted(&draft, 1);
        assert_eq!(draft.handle(Event::Set(Ok(set))), Step::None);
        assert!(draft.drained() && draft.drafted());
    }

    #[test]
    fn a_draft_opened_conflicted_says_so_and_holds_its_fields() {
        let mut opened = answer(4);
        opened.conflicted = true;
        let (draft, step) = CoreDraft::open(GESTURE, opened, Some(fields(0.3)));
        assert_eq!(step, Step::Conflicted);
        assert!(!draft.frame_pending(), "held back until Reapply");
    }

    #[test]
    fn a_frame_is_pending_only_while_something_asked_will_bring_one() {
        // A draft opened with nothing to send, as the crop's is: no frame is coming, and nothing
        // is waiting.
        let (empty, _) = begun();
        assert!(!empty.frame_pending() && empty.drained());
        let (mut draft, _) = begun();
        draft.handle(Event::Offer(fields(0.2)));
        assert!(draft.frame_pending(), "the set is answered with a frame");
        let answered = accepted(&draft, 1);
        draft.handle(Event::Set(Ok(answered)));
        assert!(!draft.frame_pending());
        draft.handle(Event::Revision(5));
        draft.handle(Event::Offer(fields(0.3)));
        assert!(
            !draft.frame_pending() && !draft.drained(),
            "a conflicted draft holds its fields back and asks for no frame"
        );
        draft.handle(Event::Reapply);
        let mut rebased = answer(5);
        rebased.draft_id = draft.draft_id.clone();
        draft.handle(Event::Reapplied(Ok(rebased)));
        assert!(draft.frame_pending(), "the reapply re-sends them");
    }

    #[test]
    fn fields_equal_to_the_accepted_ones_are_not_sent_again() {
        let (mut draft, id) = begun();
        assert_eq!(
            draft.handle(Event::Offer(fields(0.5))),
            Step::Set {
                draft_id: id,
                fields: fields(0.5)
            }
        );
        let set = accepted(&draft, 1);
        draft.handle(Event::Set(Ok(set)));
        assert_eq!(draft.handle(Event::Offer(fields(0.5))), Step::None);
        assert!(draft.drained());
    }

    #[test]
    fn a_release_while_a_set_is_in_flight_commits_once_it_has_answered() {
        let (mut draft, id) = begun();
        draft.handle(Event::Offer(fields(0.4)));
        assert_eq!(draft.handle(Event::Release), Step::None);
        let set = accepted(&draft, 1);
        assert_eq!(
            draft.handle(Event::Set(Ok(set))),
            Step::Commit {
                draft_id: id,
                expected_revision: 4
            },
            "the newest fields go first, then the commit"
        );
        assert_eq!(draft.handle(Event::Committed(Ok(()))), Step::None);
        assert!(draft.drained(), "nothing is left in flight");
    }

    #[test]
    fn a_release_while_nothing_is_outstanding_commits_at_once() {
        let (mut draft, id) = begun();
        assert_eq!(
            draft.handle(Event::Release),
            Step::Commit {
                draft_id: id,
                expected_revision: 4
            }
        );
        assert_eq!(draft.handle(Event::Release), Step::None, "commits once");
        assert_eq!(
            draft.handle(Event::Offer(fields(1.0))),
            Step::None,
            "nothing is sent behind a commit"
        );
    }

    #[test]
    fn a_conflicted_draft_refuses_release_and_reapply_resends_its_fields() {
        let (mut draft, id) = begun();
        set(&mut draft, 0.5, 1);
        assert_eq!(draft.handle(Event::Revision(4)), Step::None);
        assert_eq!(draft.handle(Event::Revision(5)), Step::Conflicted);
        assert_eq!(draft.handle(Event::Revision(6)), Step::None, "once");
        assert_eq!(draft.handle(Event::Offer(fields(0.6))), Step::None);
        assert_eq!(draft.handle(Event::Release), Step::Refused);
        assert_eq!(
            draft.handle(Event::Reapply),
            Step::Reapply(id.clone()),
            "Reapply rebases the draft"
        );
        let mut rebased = answer(6);
        rebased.draft_id = id.clone();
        assert_eq!(
            draft.handle(Event::Reapplied(Ok(rebased))),
            Step::Set {
                draft_id: id,
                fields: fields(0.6)
            },
            "the newest fields go out again on the new base"
        );
        assert_eq!(draft.base_revision, 6);
        assert!(!draft.conflicted);
    }

    #[test]
    fn a_reapply_with_nothing_newer_resends_the_accepted_fields() {
        let (mut draft, id) = begun();
        set(&mut draft, 0.5, 1);
        draft.handle(Event::Revision(5));
        draft.handle(Event::Reapply);
        let mut rebased = answer(5);
        rebased.draft_id = id.clone();
        assert_eq!(
            draft.handle(Event::Reapplied(Ok(rebased))),
            Step::Set {
                draft_id: id,
                fields: fields(0.5)
            }
        );
    }

    #[test]
    fn a_refused_reapply_keeps_the_draft_conflicted() {
        let (mut draft, _) = begun();
        set(&mut draft, 0.5, 1);
        draft.handle(Event::Revision(5));
        draft.handle(Event::Reapply);
        assert_eq!(
            draft.handle(Event::Reapplied(Err("not-found: gone".into()))),
            Step::None
        );
        assert!(draft.conflicted && draft.base_revision == 4);
        assert_eq!(draft.handle(Event::Release), Step::Refused);
    }

    /// A reapply that reads a pixel answers in a later update. Meanwhile it is in flight: a second
    /// Reapply sends nothing, a move is held, and a Discard waits for the answer, which then ends
    /// the gesture rather than re-sending its fields.
    #[test]
    fn a_reapply_answered_later_is_in_flight_until_it_answers() {
        let (mut draft, id) = begun();
        set(&mut draft, 0.5, 1);
        draft.handle(Event::Revision(5));
        assert_eq!(draft.handle(Event::Reapply), Step::Reapply(id.clone()));
        assert_eq!(draft.in_flight(), Some(Round::Reapply));
        assert!(draft.frame_pending());
        assert_eq!(draft.handle(Event::Reapply), Step::None);
        assert_eq!(draft.handle(Event::Offer(fields(0.7))), Step::None);
        assert_eq!(draft.handle(Event::Cancel), Step::None);
        let mut rebased = answer(5);
        rebased.draft_id = id.clone();
        assert_eq!(
            draft.handle(Event::Reapplied(Ok(rebased))),
            Step::Cancel(id)
        );
    }

    #[test]
    fn no_reapply_is_sent_behind_a_commit() {
        let (mut draft, _) = begun();
        draft.handle(Event::Release);
        assert_eq!(draft.handle(Event::Reapply), Step::None);
    }

    #[test]
    fn a_revision_during_the_commit_is_left_to_the_commit_to_answer() {
        let (mut draft, _) = begun();
        draft.handle(Event::Release);
        assert_eq!(
            draft.handle(Event::Revision(5)),
            Step::None,
            "most likely the commit's own revision: no notice for it"
        );
        assert!(!draft.conflicted);
        assert_eq!(draft.handle(Event::Committed(Ok(()))), Step::None);
        assert!(draft.drained(), "nothing is left in flight");
    }

    #[test]
    fn a_commit_the_owner_refuses_as_stale_keeps_the_draft_conflicted() {
        let (mut draft, _) = begun();
        draft.handle(Event::Release);
        assert_eq!(
            draft.handle(Event::Committed(Err(format!(
                "{}: stale revision",
                ErrorKind::Conflict.code()
            )))),
            Step::None
        );
        assert!(draft.conflicted && draft.drained());
        assert_eq!(draft.handle(Event::Release), Step::Refused);
    }

    #[test]
    fn cancel_while_idle_ends_the_draft_through_one_cancel() {
        let (mut draft, id) = begun();
        set(&mut draft, 0.5, 1);
        draft.handle(Event::Revision(5));
        draft.handle(Event::Offer(fields(0.6)));
        assert_eq!(
            draft.handle(Event::Cancel),
            Step::Cancel(id),
            "a discarded draft sends no held field first"
        );
    }

    #[test]
    fn cancel_during_a_commit_lets_the_commit_decide() {
        let (mut draft, _) = begun();
        draft.handle(Event::Release);
        assert_eq!(draft.handle(Event::Cancel), Step::None, "no racing cancel");
        assert_eq!(draft.handle(Event::Committed(Ok(()))), Step::None);
        assert!(draft.drained(), "nothing is left in flight");

        let (mut refused, id) = begun();
        refused.handle(Event::Release);
        refused.handle(Event::Cancel);
        assert_eq!(
            refused.handle(Event::Committed(Err("conflict: stale".into()))),
            Step::Cancel(id),
            "a refused commit leaves a draft, which the Discard then cancels"
        );
    }

    #[test]
    fn answers_name_the_gesture_and_the_draft_they_belong_to() {
        let (draft, id) = begun();
        assert!(draft.answers(GESTURE, &id));
        assert!(!draft.answers(GestureId(8), &id));
        assert!(!draft.answers(GESTURE, &DraftId::new()));
    }

    #[test]
    fn an_answer_for_a_round_not_in_flight_changes_nothing() {
        let (mut draft, _) = begun();
        let before = draft.clone();
        assert_eq!(draft.handle(Event::Committed(Ok(()))), Step::None);
        assert_eq!(
            draft.handle(Event::Set(Ok(answer(9)))),
            Step::None,
            "a set nobody sent"
        );
        assert_eq!(draft, before);
    }

    #[test]
    fn a_refused_set_is_still_drained() {
        let (mut draft, _) = begun();
        draft.handle(Event::Offer(fields(0.2)));
        assert_eq!(
            draft.handle(Event::Set(Err("preparation-required: 7".into()))),
            Step::None
        );
        assert!(draft.drained() && !draft.drafted());
    }

    use crate::{
        app::gesture::{Kind, MaskGesture},
        mask_draft::{BRUSH, MaskDraft, NEUTRAL_BRUSH, brush_counts},
    };

    /// A brush stroke's gesture, pressed at the start of a long horizontal wave, with the production
    /// builder of its fields ([`Kind::build`]).
    fn stroke() -> Kind {
        let mut shape = MaskDraft::creating(BRUSH, NEUTRAL_BRUSH).expect("a painted kind");
        shape.paint_begin(wave(0));
        Kind::Mask(MaskGesture {
            shape,
            map: None,
            map_draft: None,
        })
    }

    fn wave(step: usize) -> (f64, f64) {
        let along = step as f64 / 2000.0;
        (0.05 + 0.9 * along, 0.5 + 0.3 * (along * 40.0).sin())
    }

    /// Extend the stroke by one position; it must grow the path.
    fn paint(kind: &mut Kind, step: usize) {
        let Kind::Mask(mask) = kind else {
            unreachable!("a stroke");
        };
        assert!(
            mask.shape.paint_to(wave(step)),
            "move {step} grows the path"
        );
    }

    /// One event, with the fields built as the driver builds them.
    fn drive(draft: &mut CoreDraft, kind: &Kind, event: Event) -> Step {
        draft.handle_with(event, &mut || kind.build(&mut None))
    }

    fn built(kind: &Kind) -> Value {
        kind.build(&mut None).expect("an accepted capture")
    }

    #[test]
    fn moves_while_a_set_is_in_flight_build_nothing_and_its_answer_builds_once() {
        let (mut draft, id) = begun();
        let mut kind = stroke();
        brush_counts::take();
        // Nothing is in flight: the change is built and sent at once, decimated and serialized
        // once.
        let Step::Set { fields, .. } = drive(&mut draft, &kind, Event::Changed) else {
            panic!("the first change is sent");
        };
        assert_eq!(brush_counts::take(), (1, 1));
        assert_eq!(fields, built(&kind));
        brush_counts::take();
        // While that set is in flight, a long run of moves grows the path and builds nothing.
        for step in 1..=1500 {
            paint(&mut kind, step);
            assert_eq!(drive(&mut draft, &kind, Event::Changed), Step::None);
            assert!(draft.frame_pending() && !draft.drained());
        }
        assert_eq!(
            brush_counts::take(),
            (0, 0),
            "no move decimated or serialized the path while the set was in flight"
        );
        // Its answer sends the newest fields, built once.
        let set = accepted(&draft, 1);
        let Step::Set { draft_id, fields } = drive(&mut draft, &kind, Event::Set(Ok(set))) else {
            panic!("the answer sends what the moves changed");
        };
        assert_eq!(
            brush_counts::take(),
            (1, 1),
            "the send decimated and serialized the path once"
        );
        assert_eq!(draft_id, id);
        assert_eq!(fields, built(&kind), "the newest path goes");
        assert_eq!(draft.sent(), Some(&fields));
        let set = accepted(&draft, 2);
        assert_eq!(drive(&mut draft, &kind, Event::Set(Ok(set))), Step::None);
        assert!(draft.drained() && !draft.frame_pending());
    }

    #[test]
    fn a_long_stroke_reads_drained_between_moves_and_reports_what_it_sent() {
        let (mut draft, id) = begun();
        let mut kind = stroke();
        let summary = |draft: &CoreDraft| draft.summary("mask.add-stroke");
        for step in 1..=300 {
            paint(&mut kind, step);
            brush_counts::take();
            // As the driver does: the change is built and sent in the update of the move, and the
            // synchronous answer is taken up before the update ends.
            let Step::Set { draft_id, fields } = drive(&mut draft, &kind, Event::Changed) else {
                panic!("move {step} is sent");
            };
            assert_eq!(brush_counts::take(), (1, 1), "move {step} built once");
            assert_eq!(draft_id, id);
            assert!(draft.frame_pending() && !draft.drained());
            let set = accepted(&draft, step as u64);
            assert_eq!(drive(&mut draft, &kind, Event::Set(Ok(set))), Step::None);
            assert!(
                draft.drained() && !draft.frame_pending(),
                "move {step}: nothing is left in flight or waiting"
            );
            assert_eq!(draft.sent(), Some(&fields));
            assert_eq!(summary(&draft)["fields"], fields);
            assert_eq!(summary(&draft)["draft_revision"], step as u64);
        }
    }

    #[test]
    fn a_change_that_builds_the_sent_fields_sends_nothing() {
        let (mut draft, _) = begun();
        let mut kind = stroke();
        paint(&mut kind, 1);
        drive(&mut draft, &kind, Event::Changed);
        let set = accepted(&draft, 1);
        drive(&mut draft, &kind, Event::Set(Ok(set)));
        brush_counts::take();
        // Built again from the same path: equal to what was sent, so nothing goes.
        assert_eq!(drive(&mut draft, &kind, Event::Changed), Step::None);
        assert!(draft.drained());
        assert_eq!(
            brush_counts::take(),
            (0, 1),
            "the held decimation was serialized again and compared, not sent"
        );
        // A move into the cell before it holds no new position: the path, its decimation and the
        // fields are the ones sent.
        let Kind::Mask(mask) = &mut kind else {
            unreachable!("a stroke");
        };
        let (x, y) = wave(1);
        let cell = 1.0 / luxforge_core::path::COORDINATE_STEPS_PER_UNIT;
        assert!(mask.shape.paint_to((x + 0.1 * cell, y)));
        assert_eq!(drive(&mut draft, &kind, Event::Changed), Step::None);
        assert!(draft.drained());
        assert_eq!(brush_counts::take(), (0, 1), "nothing was decimated again");
        // The same holds for an offer.
        let sent = draft.sent().cloned().expect("sent fields");
        assert_eq!(draft.handle(Event::Offer(sent)), Step::None);
        assert!(draft.drained());
    }

    #[test]
    fn a_changed_gesture_that_cannot_build_keeps_what_it_offered() {
        let (mut draft, id) = begun();
        draft.handle(Event::Offer(fields(0.4)));
        // While the offer is in flight the gesture changes into something it cannot build.
        assert_eq!(draft.handle(Event::Changed), Step::None);
        let set = accepted(&draft, 1);
        assert_eq!(
            draft.handle(Event::Set(Ok(set))),
            Step::None,
            "a failed build sends nothing new"
        );
        assert!(draft.drained());
        assert_eq!(draft.sent(), Some(&fields(0.4)));
        // Held back while conflicted, a change is built at once and re-sent by Reapply.
        draft.handle(Event::Revision(5));
        let mut built = || Some(fields(0.7));
        assert_eq!(draft.handle_with(Event::Changed, &mut built), Step::None);
        assert!(!draft.frame_pending() && !draft.drained());
        draft.handle(Event::Reapply);
        let mut rebased = answer(5);
        rebased.draft_id = id.clone();
        assert_eq!(
            draft.handle(Event::Reapplied(Ok(rebased))),
            Step::Set {
                draft_id: id,
                fields: fields(0.7)
            }
        );
    }
}

//! The core draft lifecycle as one pure state machine.
//!
//! Every desktop gesture — a slider, a mask shape or stroke, the crop frame — is one [`CoreDraft`].
//! It holds no owner handle and no framework type: it takes an [`Event`] (the gesture offered
//! fields, an owner answer arrived, the pointer was released, Discard or Reapply was pressed, the
//! asset moved) and answers with the one [`Step`] the driver must take next. The
//! driver in [`crate::app::gesture`] runs the step against the owner and feeds the answer back, so
//! every rule of the lifecycle lives here once:
//!
//! - at most one owner round trip is in flight, and nothing else is sent while it is;
//! - the newest offered fields win, and fields equal to the ones already accepted are not re-sent;
//! - a release commits exactly once, after every offered field has reached the core draft, however
//!   early it came — even before `draft.begin` has answered;
//! - a conflicted draft is never committed: release is refused until Discard or Reapply;
//! - Discard while a round trip is in flight waits for its answer and then cancels the draft that
//!   answer names, so no request races another; Discard during a commit lets the commit decide and
//!   cancels only if the commit is refused;
//! - every answer names the gesture (and, once known, the core draft) it belongs to, so a stale one
//!   is recognised and dropped rather than adopted by a newer gesture.
use luxforge_core::{Draft, DraftId, ErrorKind};
use serde_json::{Value, json};

/// A desktop-local identity for one gesture, minted when it opens. The core's draft id is unknown
/// until `draft.begin` answers, so that answer is matched to its gesture by this instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GestureId(pub(crate) u64);

/// The owner round trip a draft is waiting on. `draft.set` is answered in the update that sends it,
/// so it is in flight only between [`Step::Set`] and the [`Event::Set`] that follows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Round {
    Begin,
    Set,
    Commit,
    Reapply,
    Cancel,
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
    /// `draft.begin` answered. `seen` is the newest asset revision the desktop knows, so a commit
    /// that landed between the begin and its answer still marks the draft conflicted.
    Begun {
        answer: Result<Draft, String>,
        seen: u64,
    },
    /// The synchronous `draft.set` answered: the draft it accepted, or why no frame follows.
    Set(Result<Draft, String>),
    /// The pointer was released, a key came up or Apply was pressed: commit once.
    Release,
    /// Escape, Discard or a script: end the gesture and commit nothing.
    Cancel,
    /// The Changed elsewhere notice's Reapply.
    Reapply,
    /// `draft.reapply` answered.
    Reapplied(Result<Draft, String>),
    /// `draft.commit` answered: `Ok` for an entry or a no-op, the refusal otherwise.
    Committed(Result<(), String>),
    /// `draft.cancel` answered, either way: the draft is over.
    Cancelled,
    /// A new authoritative asset revision arrived.
    Revision(u64),
}

/// What the driver does next.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Step {
    /// Nothing to send.
    None,
    /// Open the core draft with `draft.begin`.
    Begin,
    /// Send these fields with `draft.set`, synchronously, and feed the answer back.
    Set { draft_id: DraftId, fields: Value },
    /// Commit the core draft once, expecting the revision it is based on.
    Commit {
        draft_id: DraftId,
        expected_revision: u64,
    },
    /// End the core draft with `draft.cancel`; the gesture is closing.
    Cancel(DraftId),
    /// Rebase the core draft on the current revision with `draft.reapply`.
    Reapply(DraftId),
    /// The draft has just become conflicted: say so, keep it.
    Conflicted,
    /// Release was refused because the draft is conflicted.
    Refused,
    /// The gesture is over and nothing of it remains at the owner.
    Done,
}

/// One gesture's core draft, as the desktop tracks it. The authoritative draft lives in this
/// client's core session; this is the correlation the desktop needs to bound its requests.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CoreDraft {
    pub(crate) gesture: GestureId,
    /// Known once `draft.begin` has answered.
    pub(crate) draft_id: Option<DraftId>,
    pub(crate) base_revision: u64,
    pub(crate) draft_revision: u64,
    pub(crate) conflicted: bool,
    in_flight: Option<Round>,
    /// The newest fields the gesture offered that no `draft.set` has carried yet.
    pending: Option<Value>,
    /// The fields the last accepted `draft.set` carried.
    sent: Option<Value>,
    finish: Option<Finish>,
}

impl CoreDraft {
    /// A gesture opens: `draft.begin` goes out at once, carrying nothing but the action. `fields`
    /// are what the gesture already holds, sent as soon as the draft exists.
    pub(crate) fn open(
        gesture: GestureId,
        base_revision: u64,
        fields: Option<Value>,
    ) -> (Self, Step) {
        let draft = Self {
            gesture,
            draft_id: None,
            base_revision,
            draft_revision: 0,
            conflicted: false,
            in_flight: Some(Round::Begin),
            pending: fields,
            sent: None,
            finish: None,
        };
        (draft, Step::Begin)
    }

    /// The round trip in flight, if any.
    pub(crate) fn in_flight(&self) -> Option<Round> {
        self.in_flight
    }

    /// How the gesture finishes once the round trip in flight answers, if it has ended.
    #[cfg(test)]
    pub(crate) fn finishing(&self) -> Option<Finish> {
        self.finish
    }

    /// The fields the last accepted `draft.set` carried.
    pub(crate) fn sent(&self) -> Option<&Value> {
        self.sent.as_ref()
    }

    /// Fields are waiting to be sent: offered, and not the ones already accepted.
    fn outstanding(&self) -> bool {
        self.pending.is_some() && self.pending != self.sent
    }

    /// Nothing is in flight, nothing is waiting and nothing is due: the frame the last `draft.set`
    /// asked for is the gesture's newest.
    pub(crate) fn drained(&self) -> bool {
        self.in_flight.is_none() && !self.outstanding() && self.finish.is_none()
    }

    /// Something the gesture asked for will still put a frame of its own on screen: a round trip
    /// whose answer brings one, fields waiting to be sent, or a commit due. A `draft.begin` with
    /// nothing to send — an armed brush opening — brings none, and neither do fields a conflicted
    /// draft holds back until Reapply, so a frame on screen then is the gesture's newest.
    pub(crate) fn frame_pending(&self) -> bool {
        match self.in_flight {
            Some(Round::Begin) => self.pending.is_some() || self.finish.is_some(),
            Some(_) => true,
            None => (self.outstanding() && !self.conflicted) || self.finish.is_some(),
        }
    }

    /// The core draft has accepted fields at least once, so drafted frames may be in the queue.
    pub(crate) fn drafted(&self) -> bool {
        self.draft_revision > 0
    }

    /// The gesture was discarded and is waiting only for its owner round trips to end. It shows
    /// nothing and accepts nothing, but it still holds this client's one draft slot.
    pub(crate) fn closing(&self) -> bool {
        self.finish == Some(Finish::Cancel) && self.in_flight != Some(Round::Commit)
    }

    /// The draft takes no more fields and no second release: it is closing, or its commit is out.
    fn settled(&self) -> bool {
        self.closing() || self.in_flight == Some(Round::Commit)
    }

    /// Nothing is in flight and the core draft is open: the gesture could give its draft up now.
    pub(crate) fn idle(&self) -> bool {
        self.in_flight.is_none() && self.draft_id.is_some() && self.finish.is_none()
    }

    /// Whether an answer from the owner belongs to this draft: the gesture that asked, and once the
    /// core draft is known, that draft.
    pub(crate) fn answers(&self, gesture: GestureId, draft: Option<&DraftId>) -> bool {
        self.gesture == gesture && (draft.is_none() || draft == self.draft_id.as_ref())
    }

    /// The draft as this desktop knows it, in the shape `session.state` reports, for the evidence
    /// frame while the session's own copy has not caught up.
    pub(crate) fn summary(&self, action: &str) -> Value {
        json!({
            "draft_id": self.draft_id.as_ref().map(DraftId::as_str),
            "action": action,
            "fields": self.sent.clone().unwrap_or_else(|| json!({})),
            "base_revision": self.base_revision,
            "draft_revision": self.draft_revision,
            "conflicted": self.conflicted,
        })
    }

    /// Take one event and answer the step it calls for.
    pub(crate) fn handle(&mut self, event: Event) -> Step {
        match event {
            Event::Offer(_) | Event::Release if self.settled() => Step::None,
            Event::Offer(fields) => {
                self.pending = Some(fields);
                self.advance()
            }
            Event::Begun { answer, seen } => {
                if self.in_flight != Some(Round::Begin) {
                    return Step::None;
                }
                self.in_flight = None;
                match answer {
                    Ok(opened) => {
                        self.draft_id = Some(opened.draft_id);
                        self.base_revision = opened.base_revision;
                        self.draft_revision = opened.draft_revision;
                        self.conflicted = opened.conflicted || seen > opened.base_revision;
                        match self.advance() {
                            // Opened on a revision already replaced: say so, as a revision that
                            // arrives later would.
                            Step::None if self.conflicted => Step::Conflicted,
                            step => step,
                        }
                    }
                    Err(_) => Step::Done,
                }
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
                self.advance()
            }
            Event::Release => {
                if self.in_flight.is_some() {
                    self.finish = Some(Finish::Commit);
                    return Step::None;
                }
                if self.conflicted {
                    self.finish = None;
                    return Step::Refused;
                }
                self.finish = Some(Finish::Commit);
                self.advance()
            }
            Event::Cancel => {
                if self.closing() {
                    return Step::None;
                }
                self.finish = Some(Finish::Cancel);
                self.pending = None;
                match self.in_flight {
                    // The commit decides: an entry ends the gesture, a refusal cancels it.
                    Some(_) => Step::None,
                    None => self.advance(),
                }
            }
            Event::Reapply => {
                let Some(draft_id) = self.draft_id.clone() else {
                    return Step::None;
                };
                if self.in_flight.is_some() || self.finish.is_some() {
                    return Step::None;
                }
                self.in_flight = Some(Round::Reapply);
                Step::Reapply(draft_id)
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
                self.advance()
            }
            Event::Committed(answer) => {
                if self.in_flight != Some(Round::Commit) {
                    return Step::None;
                }
                self.in_flight = None;
                match answer {
                    Ok(()) => Step::Done,
                    Err(error) => {
                        if error.starts_with(ErrorKind::Conflict.code()) {
                            self.conflicted = true;
                        }
                        if self.finish == Some(Finish::Cancel) {
                            return self.advance();
                        }
                        self.finish = None;
                        Step::None
                    }
                }
            }
            Event::Cancelled => {
                if self.in_flight != Some(Round::Cancel) {
                    return Step::None;
                }
                self.in_flight = None;
                Step::Done
            }
            Event::Revision(revision) => {
                // While the commit is in flight the new revision is most likely its own; the
                // commit's answer says whether it was refused as stale, so it decides.
                if self.draft_id.is_none()
                    || self.closing()
                    || self.conflicted
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
    /// so every offered field must be there before it goes.
    fn advance(&mut self) -> Step {
        if self.in_flight.is_some() {
            return Step::None;
        }
        let Some(draft_id) = self.draft_id.clone() else {
            return Step::None;
        };
        if self.finish == Some(Finish::Cancel) {
            self.in_flight = Some(Round::Cancel);
            return Step::Cancel(draft_id);
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

    /// A gesture opened on revision 4, whose `draft.begin` is on its way.
    fn opened(fields: Option<Value>) -> CoreDraft {
        let (draft, step) = CoreDraft::open(GESTURE, 4, fields);
        assert_eq!(step, Step::Begin, "opening sends draft.begin");
        draft
    }

    fn answer(base_revision: u64) -> Draft {
        Draft::new("set-basic", AssetId::new(), base_revision)
    }

    fn fields(value: f64) -> Value {
        json!({ "exposure": value })
    }

    /// Answer `draft`'s begin with a draft on revision 4, the desktop having seen `seen`.
    fn begin(draft: &mut CoreDraft, seen: u64) -> (Step, DraftId) {
        let opened = answer(4);
        let id = opened.draft_id.clone();
        let step = draft.handle(Event::Begun {
            answer: Ok(opened),
            seen,
        });
        (step, id)
    }

    /// A draft that has opened on revision 4, with its begin answered.
    fn begun() -> (CoreDraft, DraftId) {
        let mut draft = opened(None);
        let (step, id) = begin(&mut draft, 4);
        assert_eq!(step, Step::None);
        (draft, id)
    }

    fn accepted(draft: &CoreDraft, revision: u64) -> Draft {
        let mut set = answer(draft.base_revision);
        set.draft_id = draft.draft_id.clone().unwrap();
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
    fn offers_before_the_begin_answers_are_sent_once_newest_first() {
        let mut draft = opened(Some(fields(0.1)));
        assert_eq!(draft.in_flight(), Some(Round::Begin));
        assert_eq!(draft.handle(Event::Offer(fields(0.2))), Step::None);
        assert_eq!(draft.handle(Event::Offer(fields(0.3))), Step::None);
        let (step, id) = begin(&mut draft, 4);
        assert_eq!(
            step,
            Step::Set {
                draft_id: id,
                fields: fields(0.3)
            }
        );
        assert!(!draft.drained(), "the set is in flight");
        let set = accepted(&draft, 1);
        assert_eq!(draft.handle(Event::Set(Ok(set))), Step::None);
        assert!(draft.drained() && draft.drafted());
    }

    #[test]
    fn a_frame_is_pending_only_while_something_asked_will_bring_one() {
        // An armed brush opening sends nothing once its begin answers: no frame is coming.
        let brush = opened(None);
        assert!(!brush.frame_pending() && !brush.drained());
        // A shape opening sends its geometry once the draft exists.
        let shape = opened(Some(fields(0.1)));
        assert!(shape.frame_pending());
        let (mut draft, _) = begun();
        assert!(!draft.frame_pending());
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
    fn a_release_before_the_begin_answers_commits_once_it_has() {
        let mut draft = opened(Some(fields(0.4)));
        assert_eq!(draft.handle(Event::Release), Step::None);
        let (step, id) = begin(&mut draft, 4);
        assert!(matches!(step, Step::Set { .. }));
        let set = accepted(&draft, 1);
        assert_eq!(
            draft.handle(Event::Set(Ok(set))),
            Step::Commit {
                draft_id: id,
                expected_revision: 4
            },
            "the newest fields go first, then the commit"
        );
        assert_eq!(draft.handle(Event::Committed(Ok(()))), Step::Done);
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
        assert_eq!(draft.handle(Event::Reapply), Step::None, "one at a time");
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
    fn a_revision_during_the_commit_is_left_to_the_commit_to_answer() {
        let (mut draft, _) = begun();
        draft.handle(Event::Release);
        assert_eq!(
            draft.handle(Event::Revision(5)),
            Step::None,
            "most likely the commit's own revision: no notice for it"
        );
        assert!(!draft.conflicted);
        assert_eq!(draft.handle(Event::Committed(Ok(()))), Step::Done);
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
    fn a_commit_that_lands_while_the_begin_is_in_flight_conflicts_the_draft() {
        let mut draft = opened(None);
        assert_eq!(draft.handle(Event::Revision(5)), Step::None, "no draft yet");
        assert_eq!(begin(&mut draft, 5).0, Step::Conflicted);
        assert!(draft.conflicted, "the revision seen meanwhile is newer");
    }

    #[test]
    fn cancel_while_idle_ends_the_draft_through_one_cancel() {
        let (mut draft, id) = begun();
        assert_eq!(draft.handle(Event::Cancel), Step::Cancel(id));
        assert!(draft.closing());
        assert_eq!(draft.handle(Event::Offer(fields(0.1))), Step::None);
        assert_eq!(draft.handle(Event::Release), Step::None);
        assert_eq!(draft.handle(Event::Cancel), Step::None, "one cancel");
        assert_eq!(draft.handle(Event::Cancelled), Step::Done);
    }

    #[test]
    fn cancel_while_the_begin_is_in_flight_cancels_the_draft_it_answers_with() {
        let mut draft = opened(Some(fields(0.1)));
        assert_eq!(draft.handle(Event::Cancel), Step::None);
        assert!(draft.closing(), "the slot is held until the begin answers");
        let (step, id) = begin(&mut draft, 4);
        assert_eq!(
            step,
            Step::Cancel(id),
            "no field is sent to a discarded draft"
        );
        assert_eq!(draft.handle(Event::Cancelled), Step::Done);
    }

    #[test]
    fn cancel_while_the_begin_is_in_flight_is_over_when_the_begin_is_refused() {
        let mut draft = opened(None);
        draft.handle(Event::Cancel);
        assert_eq!(
            draft.handle(Event::Begun {
                answer: Err("conflict: held".into()),
                seen: 4
            }),
            Step::Done
        );
    }

    #[test]
    fn cancel_during_a_commit_lets_the_commit_decide() {
        let (mut draft, _) = begun();
        draft.handle(Event::Release);
        assert_eq!(draft.handle(Event::Cancel), Step::None, "no racing cancel");
        assert!(!draft.closing(), "the commit is still the gesture's");
        assert_eq!(draft.handle(Event::Committed(Ok(()))), Step::Done);

        let (mut refused, _) = begun();
        refused.handle(Event::Release);
        refused.handle(Event::Cancel);
        assert_eq!(
            refused.handle(Event::Committed(Err("conflict: stale".into()))),
            Step::Cancel(refused.draft_id.clone().unwrap()),
            "a refused commit leaves a draft, which the Discard then cancels"
        );
    }

    #[test]
    fn cancel_during_a_reapply_waits_for_its_answer() {
        let (mut draft, id) = begun();
        draft.handle(Event::Revision(5));
        draft.handle(Event::Reapply);
        assert_eq!(draft.handle(Event::Cancel), Step::None);
        assert!(draft.closing());
        assert_eq!(
            draft.handle(Event::Reapplied(Err("not-found".into()))),
            Step::Cancel(id)
        );
    }

    #[test]
    fn answers_name_the_gesture_and_the_draft_they_belong_to() {
        let (draft, id) = begun();
        assert!(draft.answers(GESTURE, Some(&id)));
        assert!(draft.answers(GESTURE, None));
        assert!(!draft.answers(GestureId(8), Some(&id)));
        assert!(!draft.answers(GESTURE, Some(&DraftId::new())));
    }

    #[test]
    fn an_answer_for_a_round_not_in_flight_changes_nothing() {
        let (mut draft, _) = begun();
        let before = draft.clone();
        assert_eq!(
            draft.handle(Event::Reapplied(Err("late".into()))),
            Step::None
        );
        assert_eq!(draft.handle(Event::Committed(Ok(()))), Step::None);
        assert_eq!(draft.handle(Event::Cancelled), Step::None);
        assert_eq!(
            draft.handle(Event::Begun {
                answer: Ok(answer(9)),
                seen: 9
            }),
            Step::None
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
}

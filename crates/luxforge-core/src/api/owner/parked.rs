//! Calls parked on the owner while the tile service reads a pixel their plans asked for
//! ([performance rule 5]): the owner's driver of the one deferred-read replay
//! ([`crate::editor::pixels::Replay`]), which a library batch drives too
//! (`library/batch.rs`).
//!
//! A parked read is read under its own cancellation, which the owner trips when nobody wants its
//! answer any more:
//!
//! - **Superseded.** The same client parks another read for the same method and photograph, or
//!   asks to prepare another photograph (`source.prepare`, which every open of the desktop's one
//!   photograph sends). The call is answered `cancelled`.
//! - **Stale.** After any message, the photograph's entry or revision, or the client's draft, is
//!   no longer the one the read was read from. The call is replayed exactly as if the read had
//!   finished stale: without the pixel, answering `conflict` if it needs one again and as it would
//!   have otherwise.
//! - **Disconnected.** Its client disconnects: the read is cancelled and the call dropped
//!   unanswered, as the tile service drops the client's waiting calls.
//!
//! A cancelled read stops before its next row, tile or solver chunk; one still waiting in the
//! service's queue is answered at once when its turn comes. Every answer, a cancelled one
//! included, comes back through the owner's own channel of answers.
//!
//! [performance rule 5]: ../../../../../docs/engineering/performance-rules.md#rules
use super::{Owed, Owner, OwnerCall, OwnerMessage, Replay};
use crate::{
    AssetId, Cancel, ClientId, Error,
    editor::pixels::{DeferredRead, PixelAnswer, PixelMemo, PixelReadKey},
};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// A call parked on the owner while a pixel read is answered off the owner: the call, replayed
/// once the read comes back, what the read was read from, how many of the call's reads it is, the
/// read's cancellation and why the owner tripped it, if it has.
pub(super) struct ParkedRead {
    pub(super) call: OwnerCall,
    key: PixelReadKey,
    reads: usize,
    cancel: Cancel,
    withdrawn: Option<Withdrawn>,
}

/// Why the owner cancelled a parked read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Withdrawn {
    /// The client asked for something that replaces it.
    Superseded,
    /// What it reads from moved on.
    Stale,
}

impl ParkedRead {
    /// The photograph the read reads.
    pub(super) fn asset(&self) -> &AssetId {
        &self.key.asset_id
    }

    fn withdraw(&mut self, why: Withdrawn) {
        if self.withdrawn.is_none() {
            self.withdrawn = Some(why);
            self.cancel.cancel();
        }
    }
}

/// The answer to a parked read, by its ticket, which the tile service hands back to the owner.
pub(super) type PixelsRead = (u64, Result<PixelAnswer, Error>);

impl Owner {
    /// Serve one pass of a call that may read pixels: `pass` with the owner, every read its plans
    /// make answered from `memo` or deferred, keyed with `client`'s draft (none for a batch), and
    /// the read it deferred, if any, for the caller to park ([`Replay::park`]). The one pass the
    /// owner's calls and a library batch's photographs both take.
    pub(super) fn pixel_pass<T>(
        &mut self,
        client: Option<ClientId>,
        memo: PixelMemo,
        pass: impl FnOnce(&mut Self) -> T,
    ) -> (T, Option<DeferredRead>) {
        let draft = client
            .and_then(|client| self.sessions.get(&client))
            .and_then(|session| session.draft.as_ref());
        self.service.begin_pixel_call(draft, memo);
        let result = pass(self);
        (result, self.service.finish_pixel_call())
    }

    /// Park `call` until the tile service has read `read`, the call's `reads`th, which it is
    /// handed now, under a cancellation of its own. At most one call more than
    /// [`crate::tiles::TILE_QUEUE_CAPACITY`] is parked — the one being read and those waiting
    /// behind it — past which a call is refused with `resource-limit`. A read the same client
    /// parked earlier for the same method and photograph is superseded. The read's answer comes
    /// back through the owner's own channel of them, which never blocks whoever hands it back,
    /// this thread included when the service refuses the read as it is submitted.
    pub(super) fn park(&mut self, call: OwnerCall, read: DeferredRead, reads: usize) {
        if self.parked_reads.len() > crate::tiles::TILE_QUEUE_CAPACITY {
            self.refuse(
                call,
                Error::resource_limit(
                    "calls that read pixels are already waiting; retry after one is answered",
                ),
            );
            return;
        }
        let client = call.client;
        let (method, asset) = (call.request.method.clone(), read.key.asset_id.clone());
        self.supersede_parked_reads(client, |parked| {
            parked.call.request.method == method && parked.asset() == &asset
        });
        self.next_pixel_ticket = self.next_pixel_ticket.wrapping_add(1);
        let ticket = self.next_pixel_ticket;
        let cancel = Cancel::new();
        self.parked_reads.insert(
            ticket,
            ParkedRead {
                call,
                key: read.key.clone(),
                reads,
                cancel: cancel.clone(),
                withdrawn: None,
            },
        );
        let (answers, wake) = (self.pixel_answers.clone(), self.pixel_completions.clone());
        self.tiles
            .submit(read.tile_call(client, cancel, move |result| {
                if answers.send((ticket, result)).is_ok() {
                    // A full channel already holds messages, after each of which the owner takes
                    // every answer waiting.
                    let _ = wake.try_send(OwnerMessage::PixelsRead);
                }
            }));
    }

    /// Cancel every read `client` parked that `superseded` names: each call is answered
    /// `cancelled` once its read comes back, at once when it was still waiting.
    pub(super) fn supersede_parked_reads(
        &mut self,
        client: ClientId,
        superseded: impl Fn(&ParkedRead) -> bool,
    ) {
        for parked in self.parked_reads.values_mut() {
            if parked.call.client == client && superseded(parked) {
                parked.withdraw(Withdrawn::Superseded);
            }
        }
    }

    /// Cancel every parked read whose photograph's entry or revision, or whose client's draft, is
    /// no longer the one it was read from: its replay finds it stale whatever it reads. Head and
    /// session checks only, `O(parked reads)`, at most [`crate::tiles::TILE_QUEUE_CAPACITY`] + 1;
    /// the source is checked when the call is replayed.
    pub(super) fn cancel_stale_reads(&mut self) {
        if self.parked_reads.is_empty() {
            return;
        }
        let (service, sessions) = (&self.service, &self.sessions);
        for parked in self.parked_reads.values_mut() {
            if parked.withdrawn.is_some() {
                continue;
            }
            let draft = sessions
                .get(&parked.call.client)
                .and_then(|session| session.draft.as_ref());
            if !service
                .pixel_head_current(&parked.key, draft)
                .unwrap_or(false)
            {
                parked.withdraw(Withdrawn::Stale);
            }
        }
    }

    /// Drop a disconnected client's parked calls unanswered, cancelling their reads first.
    pub(super) fn drop_parked_reads(&mut self, client: ClientId) {
        self.parked_reads.retain(|_, parked| {
            let kept = parked.call.client != client;
            if !kept {
                parked.cancel.cancel();
            }
            kept
        });
    }

    /// Take every parked read the tile service has answered and replay its call once: with the
    /// pixel in the session's memo when what it was read from is still current, and without it
    /// when the stack, the draft or the source changed while it was read, so a replay that needs
    /// the pixel again answers `conflict` and one that no longer does — a retry its request log
    /// answers — answers as it would have. A read that failed answers its call with the failure,
    /// and a superseded one `cancelled`. A panic while replaying one is contained as a call's is.
    pub(super) fn pixels_read(&mut self) {
        while let Ok((ticket, result)) = self.answered_pixels.try_recv() {
            let Some(parked) = self.parked_reads.remove(&ticket) else {
                continue;
            };
            let client = parked.call.client;
            let owed = Owed::Call(parked.call.request.id.clone(), parked.call.response.clone());
            let replayed = catch_unwind(AssertUnwindSafe(|| self.replay(parked, result)));
            if replayed.is_err() {
                self.contained(owed);
            }
            self.notify_watchers(Some(client));
            self.wake_event_waits();
        }
    }

    fn replay(&mut self, parked: ParkedRead, result: Result<PixelAnswer, Error>) {
        let ParkedRead {
            call,
            key,
            reads,
            withdrawn,
            ..
        } = parked;
        let answer = match (withdrawn, result) {
            (Some(Withdrawn::Superseded), _) => {
                return self.refuse(
                    call,
                    Error::cancelled(
                        "superseded: this client asked again, or for another photograph, while \
                         the pixels were read",
                    ),
                );
            }
            // Cancelled because it was stale: replayed as a stale read that finished would be.
            (Some(Withdrawn::Stale), _) => None,
            (None, Ok(answer)) => Some(answer),
            (None, Err(error)) => return self.refuse(call, error),
        };
        let session = self.sessions.entry(call.client).or_default();
        let current = answer.is_some()
            && self
                .service
                .pixel_key_current(&key, session.draft.as_ref())
                .unwrap_or(false);
        let replay = match answer {
            Some(answer) => Replay::answered(reads, answer, current, &mut session.pixel_memo),
            None => {
                session.pixel_memo.clear();
                Replay::Stale
            }
        };
        self.call_round(call, replay);
    }
}

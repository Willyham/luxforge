//! A tile service standing in for the desktop's GPU tile worker in the rendered tiers' tests
//! ([`BandTiles`]): on a thread of its own it renders a stream's output stage with the reference
//! renderer and hands it over in bands, as the GPU worker streams its rows of tiles, so a tier it
//! draws is the reference's own frame reduced from bands; or each code's lowest bit flipped
//! ([`Draw::Marked`]), so a tier it drew is told apart from the reference's; or it refuses the
//! stream, or stops drawing it part-way, for a reason the reference draws the tiers for instead.
use crate::{
    Cancel, ClientId, Error, Evaluation, SnapshotId,
    tiles::{Answered, Band, BandStream, TileCall, TileFallback, TileService, TileStatus},
};
use luxforge_testbase::Gate;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

/// What a [`BandTiles`] draws.
#[derive(Clone, Debug)]
pub(crate) enum Draw {
    /// The reference's frame, in bands of this many rows.
    Bands(u32),
    /// The reference's frame with every code's lowest bit flipped, in bands of this many rows.
    Marked(u32),
    /// Refuse the stream at once, for this reason.
    Refuse(TileFallback),
    /// Send this many bands of `rows` rows, then stop drawing for this reason.
    StopAfter {
        rows: u32,
        bands: usize,
        fallback: TileFallback,
    },
}

/// See the [module documentation](self).
pub(crate) struct BandTiles {
    draw: Mutex<Draw>,
    /// Passed before each stream's first band, so a test can hold a render inside its stream.
    pub(crate) gate: Arc<Gate>,
    /// Streams asked for, and the name of the thread each was asked on.
    streams: AtomicUsize,
    threads: Mutex<Vec<String>>,
    /// Bands sent.
    sent: Arc<AtomicUsize>,
}

impl BandTiles {
    pub(crate) fn new(draw: Draw) -> Arc<Self> {
        Arc::new(Self {
            draw: Mutex::new(draw),
            gate: Arc::new(Gate::new()),
            streams: AtomicUsize::new(0),
            threads: Mutex::new(Vec::new()),
            sent: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub(crate) fn streams(&self) -> usize {
        self.streams.load(Ordering::SeqCst)
    }

    pub(crate) fn sent(&self) -> usize {
        self.sent.load(Ordering::SeqCst)
    }

    /// The name of the thread each stream was asked for on, in order.
    pub(crate) fn threads(&self) -> Vec<String> {
        self.threads.lock().unwrap().clone()
    }
}

impl TileService for BandTiles {
    fn status(&self) -> TileStatus {
        TileStatus::Gpu
    }

    fn submit(&self, call: TileCall) {
        call.refuse(Error::internal("the band service reads no pixels"));
    }

    fn disconnect(&self, _: ClientId) {}

    fn stream(&self, evaluation: &Evaluation, cancel: &Cancel) -> Result<BandStream, TileFallback> {
        self.streams.fetch_add(1, Ordering::SeqCst);
        self.threads.lock().unwrap().push(
            std::thread::current()
                .name()
                .unwrap_or("unnamed")
                .to_owned(),
        );
        let (rows, marked, stop) = match self.draw.lock().unwrap().clone() {
            Draw::Refuse(reason) => return Err(reason),
            Draw::Bands(rows) => (rows, false, None),
            Draw::Marked(rows) => (rows, true, None),
            Draw::StopAfter {
                rows,
                bands,
                fallback,
            } => (rows, true, Some((bands, fallback))),
        };
        let identity = evaluation.identity().expect("the stack's identity");
        let width = identity.width;
        let (sender, stream) = BandStream::channel(width, identity.height, Answered::gpu());
        let (evaluation, cancel, gate, sent) = (
            evaluation.clone(),
            cancel.clone(),
            Arc::clone(&self.gate),
            Arc::clone(&self.sent),
        );
        std::thread::spawn(move || {
            gate.pass();
            let frame = match evaluation
                .exact(&Cancel::never())
                .and_then(|render| render.frame(SnapshotId::new()))
            {
                Ok(frame) => frame,
                Err(error) => {
                    sender.send(Err(error));
                    return;
                }
            };
            drop(evaluation);
            let stride = width as usize * 4;
            for (index, rgba) in frame.rgba.chunks(rows as usize * stride).enumerate() {
                if let Some((after, fallback)) = &stop
                    && index == *after
                {
                    sender.fall_back(fallback.clone());
                    return;
                }
                if let Err(cancelled) = cancel.check() {
                    sender.send(Err(cancelled));
                    return;
                }
                let rgba = if marked {
                    rgba.chunks_exact(4)
                        .flat_map(|pixel| [pixel[0] ^ 1, pixel[1] ^ 1, pixel[2] ^ 1, 255])
                        .collect()
                } else {
                    rgba.to_vec()
                };
                let band = Band {
                    y0: index as u32 * rows,
                    rows: (rgba.len() / stride) as u32,
                    rgba,
                };
                if !sender.send(Ok(band)) {
                    return;
                }
                sent.fetch_add(1, Ordering::SeqCst);
            }
        });
        Ok(stream)
    }

    fn stop(&self) {}
}

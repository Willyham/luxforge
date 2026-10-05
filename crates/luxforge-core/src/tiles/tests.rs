//! The contract's own parts: a pixel read's reply, and an export's bands.

use super::*;
use crate::{
    AssetId, EntryId, ProxyIdentity,
    editor::pixels::{PixelRead, PixelReadKey},
};
use luxforge_testbase::HANG;
use std::sync::atomic::AtomicUsize;

fn band(y0: u32, rows: u32, width: u32) -> Band {
    Band {
        y0,
        rows,
        rgba: vec![7; (width * rows * 4) as usize],
    }
}

/// A stream hands its bands over in order and ends at its stage's last row; a band out of place,
/// one of the wrong size and a provider that stops early each end it with an error; and a dropped
/// stream refuses its provider's next band, which abandons the export.
#[test]
fn a_band_stream_hands_its_bands_in_order_and_ends_at_its_last_row() {
    let (sender, mut stream) = BandStream::channel(3, 5, Answered::gpu());
    // Both bands wait for the encoder at once, which is as many as a stream holds.
    assert!(sender.send(Ok(band(0, 2, 3))));
    assert!(sender.send(Ok(band(2, 3, 3))));
    assert_eq!(stream.answered(), &Answered::gpu());
    assert_eq!(stream.next().unwrap().unwrap(), band(0, 2, 3));
    assert_eq!(stream.next().unwrap().unwrap(), band(2, 3, 3));
    assert!(stream.next().is_none(), "the last row has arrived");

    for wrong in [band(1, 2, 3), band(0, 2, 4), band(0, 6, 3)] {
        let (sender, mut stream) = BandStream::channel(3, 5, Answered::gpu());
        assert!(sender.send(Ok(wrong.clone())));
        assert_eq!(
            stream.next().unwrap().unwrap_err().kind.code(),
            "internal",
            "{} rows from row {} in {} bytes",
            wrong.rows,
            wrong.y0,
            wrong.rgba.len()
        );
        assert!(stream.next().is_none(), "an error ends the stream");
    }

    let (sender, mut stream) = BandStream::channel(3, 5, Answered::gpu());
    assert!(sender.send(Ok(band(0, 2, 3))));
    drop(sender);
    assert!(stream.next().unwrap().is_ok());
    assert_eq!(stream.next().unwrap().unwrap_err().kind.code(), "internal");
    assert!(stream.next().is_none());

    let (sender, stream) = BandStream::channel(3, 5, Answered::gpu());
    drop(stream);
    assert!(
        !sender.send(Ok(band(0, 2, 3))),
        "a dropped stream abandons its provider"
    );
}

/// A waking stream tells its provider each time it hands a band to its encoder, which leaves room
/// for one more, and once when it is dropped, which its provider also reads: so a provider on a
/// thread of its own sends a band only when the stream has room for it. A stream asked for without
/// a wake is still read as abandoned once dropped.
#[test]
fn a_waking_stream_tells_its_provider_of_each_band_taken_and_of_its_drop() {
    let woken = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&woken);
    let (sender, mut stream) = BandStream::waking(3, 5, Answered::gpu(), move || {
        counted.fetch_add(1, Ordering::SeqCst);
    });
    assert!(sender.send(Ok(band(0, 2, 3))));
    assert!(sender.send(Ok(band(2, 3, 3))));
    assert_eq!(woken.load(Ordering::SeqCst), 0, "nothing taken yet");
    assert_eq!(stream.next().unwrap().unwrap(), band(0, 2, 3));
    assert_eq!(woken.load(Ordering::SeqCst), 1, "one band taken");
    assert_eq!(stream.next().unwrap().unwrap(), band(2, 3, 3));
    assert!(stream.next().is_none(), "the last row has arrived");
    assert_eq!(
        woken.load(Ordering::SeqCst),
        2,
        "an ended stream takes nothing more"
    );
    assert!(!sender.abandoned());
    drop(stream);
    assert_eq!(
        woken.load(Ordering::SeqCst),
        3,
        "the drop wakes the provider"
    );
    assert!(sender.abandoned());
    assert!(!sender.send(Ok(band(0, 2, 3))));

    let (sender, stream) = BandStream::channel(3, 5, Answered::gpu());
    assert!(!sender.abandoned());
    drop(stream);
    assert!(sender.abandoned());
}

/// A stream its provider cannot go on drawing ends with an error naming why in its data, and says
/// why itself, which an error of the provider's own or a cancellation does not: the export lane
/// renders the export again with the reference only for the first, naming the reason in the
/// session's shape.
#[test]
fn a_stream_the_gpu_cannot_go_on_drawing_names_why() {
    let budget = TileFallback::Budget {
        requested: 1_331_500_000,
        budget: 805_306_368,
    };
    let (sender, mut stream) = BandStream::channel(3, 5, Answered::gpu());
    assert!(sender.send(Ok(band(0, 2, 3))));
    assert!(sender.fall_back(budget.clone()));
    assert!(stream.next().unwrap().is_ok());
    assert_eq!(stream.fallback(), None, "nothing has stopped it yet");
    let ended = stream.next().unwrap().unwrap_err();
    assert_eq!(ended.kind.code(), "render");
    assert_eq!(
        ended.data.as_deref(),
        Some(
            &json!({"fallback": "tiles-budget", "requested": 1_331_500_000_u64,
            "budget": 805_306_368_u64})
        )
    );
    assert_eq!(stream.fallback(), Some(&budget));
    assert!(stream.next().is_none(), "the fallback ends the stream");

    let lost = TileFallback::Unavailable(TileUnavailable::DeviceLost);
    let (sender, mut stream) = BandStream::channel(3, 5, Answered::gpu());
    assert!(sender.fall_back(lost.clone()));
    let ended = stream.next().unwrap().unwrap_err();
    assert_eq!(
        ended.data.as_deref(),
        Some(&json!({"fallback": "tiles-unavailable", "unavailable": "device-lost"}))
    );
    assert_eq!(stream.fallback(), Some(&lost));

    for error in [Error::cancelled("stop"), Error::internal("a panic")] {
        let (sender, mut stream) = BandStream::channel(3, 5, Answered::gpu());
        assert!(sender.send(Err(error.clone())));
        assert_eq!(stream.next().unwrap().unwrap_err().kind, error.kind);
        assert_eq!(stream.fallback(), None, "{error:?} names no fallback");
    }

    let reason = |fallback: TileFallback| RendererReason::from(&fallback).as_str();
    for (unavailable, code) in [
        (TileUnavailable::Pending, "surface-pending"),
        (TileUnavailable::NoAdapter, "no-adapter"),
        (TileUnavailable::Refused, "refused"),
        (TileUnavailable::DeviceLost, "device-lost"),
        (TileUnavailable::AdapterMismatch, "adapter-mismatch"),
    ] {
        assert_eq!(unavailable.as_str(), code);
        assert_eq!(reason(TileFallback::Unavailable(unavailable)), code);
    }
    assert_eq!(reason(budget), "tiles-budget");
    assert_eq!(
        reason(TileFallback::Plan(GpuFallback::RegionEstimate { layer: 2 })),
        "region-estimate"
    );
    assert_eq!(
        reason(TileFallback::Stage("pipeline-failed")),
        "pipeline-failed"
    );
}

/// The reason a provider's GPU stage gives is answered under the stage's own code.
#[test]
fn a_stage_reason_answers_its_own_code() {
    for code in [
        "pipeline-failed",
        "texture-limit",
        "buffer-limit",
        "source-missing",
        "warp-grid",
    ] {
        assert_eq!(TileFallback::Stage(code).code(), code);
    }
}

/// A mutation's pixel read hands its pixels back to the owner's delivery when it runs, and its
/// refusal when it is refused.
#[test]
fn a_pixel_read_returns_its_pixels_or_its_refusal_to_the_owner() {
    let key = PixelReadKey {
        asset_id: AssetId::new(),
        entry_id: EntryId::new(),
        revision: 1,
        draft: None,
        prefix_hash: [0; 32],
        input_wide: false,
        input_mode: crate::render::MaskInputMode::Boundary,
        source: ProxyIdentity::Jpeg {
            fingerprint: "pixels".into(),
            width: 4,
            height: 4,
            orientation: 1,
        },
    };
    let answer = PixelAnswer {
        key,
        read: PixelRead {
            index: 1,
            x: 2,
            y: 3,
        },
        rgba: Some([1, 2, 3, 255]),
        linear: Some([0.25, 0.5, 0.75]),
    };
    let read = |deliver: std::sync::mpsc::SyncSender<Result<PixelAnswer, Error>>| {
        let pixels = answer.clone();
        TileCall::pixels(
            crate::ClientId::testing(1),
            Cancel::new(),
            move |_, _| Ok(pixels),
            move |result| {
                let _ = deliver.send(result);
            },
        )
    };
    let (sender, delivered) = sync_channel(1);
    read(sender).run(&ReferenceReads);
    assert_eq!(delivered.recv_timeout(HANG).unwrap().unwrap(), answer);
    let (sender, delivered) = sync_channel(1);
    read(sender).refuse(Error::resource_limit("full"));
    assert_eq!(
        delivered
            .recv_timeout(HANG)
            .unwrap()
            .unwrap_err()
            .kind
            .code(),
        "resource-limit"
    );
}

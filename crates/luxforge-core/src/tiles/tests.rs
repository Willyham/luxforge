//! The contract's own parts: a pixel read's reply, and an export's bands.

use super::*;
use crate::{
    AssetId, EntryId, ProxyIdentity,
    editor::pixels::{PixelRead, PixelReadKey},
};
use luxforge_testbase::HANG;

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

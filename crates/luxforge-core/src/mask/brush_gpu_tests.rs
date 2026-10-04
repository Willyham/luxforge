//! The brush's GPU storage block against the index it serializes: every segment, cell list and
//! stroke record is the CPU field's own, narrowed once; a stroke painted tick by tick only appends
//! its new segments to the segment part, which the rest of the block follows; and the block stays
//! within the bound the stroke limits set.
use super::{
    BrushStrokes, Compiled, GPU_BLOCK_WORDS_MAX, GPU_ERASE, GPU_HARD, GPU_LIMITED,
    GPU_RECORD_WORDS, GPU_SEGMENT_WORDS, SEGMENTS_PER_PIXEL,
};
use crate::{
    mask::{ColourLimit, ComponentField, Stroke},
    modules::Stage,
    path::{StrokeId, StrokeTable},
};

const STAGE: Stage = Stage {
    width: 1200,
    height: 800,
};

fn compiled(strokes: &[Stroke], stage: Stage) -> Compiled {
    let mut table = StrokeTable::new("the brush GPU block tests");
    let ids: Vec<StrokeId> = strokes
        .iter()
        .map(|stroke| table.insert(stroke.clone()))
        .collect();
    Compiled::new(
        &BrushStrokes { strokes: ids },
        stage,
        &table,
        "Mask 1",
        "Brush 1",
    )
    .expect("a legal brush")
}

fn f32_bits(value: f64) -> u32 {
    (value as f32).to_bits()
}

/// Every part of the block decodes to the field's own index: the segments in stored stroke order,
/// each cell's `(stroke, segment)` list in its order, and each stroke's terms, colour limit
/// included; and the words carry the grid's geometry and the parts' offsets.
#[test]
fn the_block_is_the_cpu_index_narrowed_once() {
    let strokes = vec![
        Stroke::capture(
            &[[0.1, 0.2], [0.4, 0.25], [0.6, 0.5]],
            0.04,
            50.0,
            100.0,
            false,
        )
        .unwrap(),
        // A hard edge, an erase stroke and a one-point stroke, whose one segment is degenerate.
        Stroke::capture(&[[0.3, 0.6], [0.9, 0.7]], 0.03, 0.0, 60.0, false).unwrap(),
        Stroke::capture(&[[0.2, 0.25], [0.5, 0.3]], 0.02, 20.0, 100.0, true).unwrap(),
        Stroke::capture(&[[0.8, 0.2]], 0.06, 70.0, 45.0, false).unwrap(),
        Stroke::capture(
            &[[0.5, 0.5], [0.7, 0.8], [1.1, 0.4]],
            0.05,
            40.0,
            90.0,
            false,
        )
        .unwrap()
        .with_colour_limit(ColourLimit::sampled([180, 90, 40], 62.5).unwrap()),
    ];
    let field = compiled(&strokes, STAGE);
    let ([cells_at, entries_at, records_at], block) = field.gpu_block();
    let (cells_at, entries_at, records_at) =
        (cells_at as usize, entries_at as usize, records_at as usize);
    // The segments, five words each.
    let mut first = Vec::new();
    let mut at = 0;
    for stroke in &field.strokes {
        first.push((at / GPU_SEGMENT_WORDS) as u32);
        for segment in &stroke.segments {
            let terms = [segment.ax, segment.ay, segment.ex, segment.ey, segment.len2];
            assert_eq!(block[at..at + 5], terms.map(f32_bits));
            at += GPU_SEGMENT_WORDS;
        }
    }
    assert_eq!(at, cells_at, "the segments end where the cell table starts");
    // The cell table and the entries.
    let index = &field.index;
    let cells = index.offsets.len().saturating_sub(1);
    assert_eq!(entries_at - cells_at, cells + 1);
    for cell in 0..cells {
        let listed = &index.entries[index.offsets[cell]..index.offsets[cell + 1]];
        let from = block[cells_at + cell] as usize;
        let to = block[cells_at + cell + 1] as usize;
        let decoded: Vec<(u32, u32)> = block[entries_at + from..entries_at + to]
            .iter()
            .map(|entry| {
                let stroke = entry >> 24;
                (stroke, (entry & 0xff_ffff) - first[stroke as usize])
            })
            .collect();
        assert_eq!(decoded, listed, "cell {cell}");
    }
    assert_eq!(records_at - entries_at, index.entries.len());
    // The records.
    assert_eq!(block.len(), records_at + GPU_RECORD_WORDS * strokes.len());
    for (number, stroke) in field.strokes.iter().enumerate() {
        let record = &block[records_at + GPU_RECORD_WORDS * number..][..GPU_RECORD_WORDS];
        let flags = (if stroke.hard { GPU_HARD } else { 0 })
            | (if stroke.erase { GPU_ERASE } else { 0 })
            | (if stroke.colour.is_some() {
                GPU_LIMITED
            } else {
                0
            });
        assert_eq!(
            record[..4],
            [
                f32_bits(stroke.r),
                f32_bits(stroke.band),
                flags,
                f32_bits(stroke.amount)
            ]
        );
        match &stroke.colour {
            None => assert_eq!(record[4..], [0, 0, 0]),
            Some(limit) => {
                let (mut seeds, radius) = limit.gpu_terms();
                let [a, b] = seeds.next().expect("one seed");
                assert_eq!(record[4..], [a.to_bits(), b.to_bits(), radius.to_bits()]);
                assert!(seeds.next().is_none());
            }
        }
    }
    assert_eq!(
        [field.strokes[1].hard, field.strokes[2].erase],
        [true, true]
    );
    // The words: the stage's height, the grid's corner, side and size, and the three offsets.
    let description = field.gpu(STAGE).expect("a program");
    assert_eq!(
        description.words,
        vec![
            800f32.to_bits(),
            f32_bits(index.u0),
            f32_bits(index.v0),
            f32_bits(index.cell),
            index.cols as u32,
            index.rows as u32,
            cells_at as u32,
            entries_at as u32,
            records_at as u32,
        ]
    );
    assert_eq!(description.block.as_deref(), Some(&block[..]));
    assert!(block.len() <= GPU_BLOCK_WORDS_MAX);
}

/// One painted stroke over 200 ticks, after two committed strokes: each tick compiles the component
/// the draft would leave behind, as the owner does for a tick's plan. The committed strokes' segments
/// and every segment the stroke already had are the same words in the same place, so a tick appends
/// its new segments to the segment part and rewrites only the index and records after it; the
/// block, and every cell's occupancy, stay within the stroke limits.
#[test]
fn a_painted_stroke_appends_its_new_segments_each_tick() {
    let committed = vec![
        Stroke::capture(&[[0.1, 0.2], [0.9, 0.25]], 0.03, 50.0, 100.0, false).unwrap(),
        Stroke::capture(
            &[[0.2, 0.8], [0.6, 0.7], [1.3, 0.85]],
            0.05,
            30.0,
            70.0,
            false,
        )
        .unwrap(),
    ];
    // A zigzag whose every vertex lies well off the chord of its neighbours, so the host's
    // decimation keeps every position it has been given.
    let path: Vec<[f64; 2]> = (0..201)
        .map(|tick| {
            let x = 0.1 + 0.6 * f64::from(tick) / 200.0;
            let y = 0.5 + if tick % 2 == 0 { -0.03 } else { 0.03 };
            [x, y]
        })
        .collect();
    let mut previous: Option<(usize, std::sync::Arc<[u32]>)> = None;
    let (mut appended, mut rewritten, mut largest) = (0usize, 0usize, 0usize);
    for tick in 1..=200 {
        let painted = Stroke::capture(&path[..=tick], 0.02, 40.0, 80.0, false).unwrap();
        assert_eq!(
            painted.point_count(),
            tick + 1,
            "every position is kept at tick {tick}"
        );
        let mut strokes = committed.clone();
        strokes.push(painted);
        let field = compiled(&strokes, STAGE);
        assert!(
            field.index.densest() <= SEGMENTS_PER_PIXEL,
            "tick {tick}: a cell lists {} segments",
            field.index.densest()
        );
        let ([cells_at, ..], block) = field.gpu_block();
        let cells_at = cells_at as usize;
        assert!(block.len() <= GPU_BLOCK_WORDS_MAX, "tick {tick}");
        largest = largest.max(block.len());
        if let Some((held_segments, held)) = &previous {
            assert!(cells_at > *held_segments, "tick {tick} adds a segment");
            assert_eq!(
                block[..*held_segments],
                held[..*held_segments],
                "tick {tick} rewrote a segment it already had"
            );
            let first_change = block
                .iter()
                .zip(held.iter())
                .position(|(new, old)| new != old)
                .unwrap_or(held.len().min(block.len()));
            assert!(first_change >= *held_segments, "tick {tick}");
            appended += cells_at - held_segments;
            rewritten += block.len() - cells_at;
        }
        previous = Some((cells_at, block));
    }
    eprintln!(
        "200 ticks: {appended} segment words appended, {rewritten} index and record words \
         rewritten, the largest block {largest} words; the bound is {} words",
        GPU_BLOCK_WORDS_MAX
    );
    assert_eq!(appended, 199 * GPU_SEGMENT_WORDS, "one new segment a tick");
}

/// A pixel tests exactly the cell the CPU tests: the block's cell table is indexed as the CPU's grid
/// is, columns within a row, so the cell a mask-space point falls in lists the same segments.
#[test]
fn the_cell_table_is_indexed_as_the_cpus_grid() {
    let strokes = vec![
        Stroke::capture(
            &[[0.1, 0.1], [0.5, 0.9], [0.9, 0.2]],
            0.04,
            50.0,
            100.0,
            false,
        )
        .unwrap(),
        Stroke::capture(&[[0.2, 0.5], [1.4, 0.55]], 0.08, 10.0, 100.0, true).unwrap(),
    ];
    let field = compiled(&strokes, STAGE);
    let ([cells_at, ..], block) = field.gpu_block();
    let index = &field.index;
    let height = f64::from(STAGE.height);
    let mut tested = 0;
    for y in (0..STAGE.height).step_by(37) {
        for x in (0..STAGE.width).step_by(41) {
            let u = (f64::from(x) + 0.5) / height;
            let v = (f64::from(y) + 0.5) / height;
            let col = ((u - index.u0) / index.cell).floor();
            let row = ((v - index.v0) / index.cell).floor();
            let listed = index.at(u, v);
            if !(col >= 0.0 && row >= 0.0 && col < index.cols as f64 && row < index.rows as f64) {
                assert!(listed.is_empty());
                continue;
            }
            let cell = cells_at as usize + row as usize * index.cols + col as usize;
            let count = (block[cell + 1] - block[cell]) as usize;
            assert_eq!(count, listed.len(), "pixel ({x}, {y})");
            tested += 1;
        }
    }
    assert!(tested > 100);
}

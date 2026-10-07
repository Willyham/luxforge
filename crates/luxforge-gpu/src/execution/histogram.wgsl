// The histogram and clipping counts of one rectangle of 8-bit output codes, added into the counts
// buffer: each channel's 256 bins, then the pixels by the class of their channels' endpoints, as
// the core's analysis reducer counts them ("Histogram and clipping contract",
// docs/design/basic-and-histogram.md). The surface prepends the constants this file names, from
// the figures in `histogram.rs`, so the two can never disagree.
//
// The codes are read through the texture's own `rgba8unorm` format, the format the chain's last
// pass writes them through, never an sRGB-typed view: `textureLoad` answers code / 255 and the
// rounding below gives the code back exactly. Alpha is never read.
//
// Each workgroup counts its texels into bins of its own in workgroup memory, then adds each bin
// that is not zero into the buffer. Every count is an integer addition, which commutes, so the
// result is the same whatever order the workgroups, the tiles or the submissions run in.

@group(0) @binding(0) var lf_histogram_codes: texture_2d<f32>;

// The rectangle of the texture this dispatch counts: its first texel, then its width and height.
@group(0) @binding(1) var<uniform> lf_histogram_rect: vec4<u32>;

@group(0) @binding(2) var<storage, read_write> lf_histogram_counts: array<atomic<u32>, LF_HISTOGRAM_WORDS>;

var<workgroup> lf_histogram_bins: array<atomic<u32>, LF_HISTOGRAM_WORDS>;

// Which endpoints one channel's code sits at, as bits: bit 0 for code 0, bit 1 for code 255. The OR
// of a pixel's three is its any-channel class and the AND its all-channel class (0 none, 1 shadow,
// 2 highlight, 3 both), as the reducer's `EXTREME` table gives them.
fn lf_histogram_ends(code: u32) -> u32 {
    return select(0u, 1u, code == 0u) | select(0u, 2u, code == 255u);
}

// Count one texel at `at` of the rectangle into the workgroup's bins. A class of 0, a pixel with no
// channel at an endpoint, is never counted: the report reads only the other three.
fn lf_histogram_count(at: vec2<u32>) {
    // An `rgba8unorm` texel is within [0, 1], so each code is within [0, 255].
    let codes = vec3<u32>(round(textureLoad(lf_histogram_codes, lf_histogram_rect.xy + at, 0).rgb * 255.0));
    atomicAdd(&lf_histogram_bins[codes.r], 1u);
    atomicAdd(&lf_histogram_bins[LF_HISTOGRAM_BINS + codes.g], 1u);
    atomicAdd(&lf_histogram_bins[2u * LF_HISTOGRAM_BINS + codes.b], 1u);
    let r = lf_histogram_ends(codes.r);
    let g = lf_histogram_ends(codes.g);
    let b = lf_histogram_ends(codes.b);
    let any = r | g | b;
    let every = r & g & b;
    if any != 0u {
        atomicAdd(&lf_histogram_bins[LF_HISTOGRAM_ANY + any - 1u], 1u);
    }
    if every != 0u {
        atomicAdd(&lf_histogram_bins[LF_HISTOGRAM_ALL + every - 1u], 1u);
    }
}

// One workgroup covers a square of LF_HISTOGRAM_LANES x LF_HISTOGRAM_REACH texels a side of the
// rectangle, each lane LF_HISTOGRAM_REACH x LF_HISTOGRAM_REACH of them a lane apart, so neighbouring
// lanes read neighbouring texels. A texel past the rectangle's width or height is never counted.
// Every loop runs a constant number of times, so each barrier is reached by the whole workgroup.
@compute @workgroup_size(LF_HISTOGRAM_LANES, LF_HISTOGRAM_LANES, 1)
fn lf_histogram_reduce(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_id) lane: vec3<u32>,
    @builtin(local_invocation_index) index: u32,
) {
    let lanes = LF_HISTOGRAM_LANES * LF_HISTOGRAM_LANES;
    // Zeroed here rather than left to the pipeline's own initialization option.
    for (var sweep = 0u; sweep < LF_HISTOGRAM_SWEEPS; sweep += 1u) {
        let word = index + sweep * lanes;
        if word < LF_HISTOGRAM_WORDS {
            atomicStore(&lf_histogram_bins[word], 0u);
        }
    }
    workgroupBarrier();
    let size = lf_histogram_rect.zw;
    let first = group.xy * (LF_HISTOGRAM_LANES * LF_HISTOGRAM_REACH) + lane.xy;
    for (var row = 0u; row < LF_HISTOGRAM_REACH; row += 1u) {
        for (var column = 0u; column < LF_HISTOGRAM_REACH; column += 1u) {
            let at = first + vec2<u32>(column, row) * LF_HISTOGRAM_LANES;
            if all(at < size) {
                lf_histogram_count(at);
            }
        }
    }
    workgroupBarrier();
    for (var sweep = 0u; sweep < LF_HISTOGRAM_SWEEPS; sweep += 1u) {
        let word = index + sweep * lanes;
        if word < LF_HISTOGRAM_WORDS {
            let count = atomicLoad(&lf_histogram_bins[word]);
            if count != 0u {
                atomicAdd(&lf_histogram_counts[word], count);
            }
        }
    }
}

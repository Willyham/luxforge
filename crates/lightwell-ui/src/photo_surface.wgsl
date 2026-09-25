// The photo surface's quad for one tile of one layer: two triangles covering the part of the
// destination rectangle the tile draws, textured with the tile's part of the frame.
//
// `viewport` is the render pass's viewport — the part of this widget that is on screen — and
// `destination` is where the whole picture goes, both in physical pixels of the whole frame. The
// picture may reach far outside the viewport at a percentage zoom; the rasterizer clips what does.
// For the photograph both corners of the destination are snapped to the pixel grid with WGSL's own
// `round`, which is what the toolkit's image shader does for `snap: true`, so the picture lands on
// exactly the physical pixels the image widget would have landed it on. The overlays are drawn into
// the same snapped destination as the photograph under them.
//
// `region` is the part of the picture this tile draws, as fractions of the whole, and `texels` is
// that part in the tile texture's own coordinates. Every tile's quad is cut from the same
// destination with the same fractions its neighbours use, so neighbouring quads share their edge
// exactly. A frame held in one texture is one tile whose region is the whole picture and whose
// texels are the whole texture.
//
// `turn` is `[sin, cos, dim, snap]`. A crop draft's input stage is turned by the draft angle: every
// corner of every tile is turned about the destination's centre, which is one rigid rotation of the
// whole stage, and the texture coordinates ride along with the corners. A sine of exactly zero is
// no turn at all, so the photograph's corners are never touched by a rotation's rounding. `bright`
// is `[x0, y0, x1, y1]` in physical pixels of the whole frame: the part drawn at full opacity, with
// `dim` elsewhere — the crop stage dimmed outside the crop rectangle. The opacity scales the sampled
// alpha, as the toolkit's image shader applies an image's opacity.

struct Placement {
    viewport: vec4<f32>,
    destination: vec4<f32>,
    region: vec4<f32>,
    texels: vec4<f32>,
    bright: vec4<f32>,
    turn: vec4<f32>,
}

@group(0) @binding(0) var<uniform> placement: Placement;
@group(0) @binding(1) var photo: texture_2d<f32>;
@group(0) @binding(2) var photo_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

// Two triangles: top left, top right, bottom left, then bottom left, top right, bottom right.
fn corner_of(index: u32) -> vec2<f32> {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    return corners[index];
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    let corner = corner_of(index);
    var top_left = placement.destination.xy;
    var bottom_right = placement.destination.xy + placement.destination.zw;
    if placement.turn.w > 0.5 {
        top_left = round(top_left);
        bottom_right = round(bottom_right);
    }
    let tile_start = mix(top_left, bottom_right, placement.region.xy);
    let tile_end = mix(top_left, bottom_right, placement.region.zw);
    var position = mix(tile_start, tile_end, corner);
    if placement.turn.x != 0.0 {
        let centre = (top_left + bottom_right) * 0.5;
        let offset = position - centre;
        let sine = placement.turn.x;
        let cosine = placement.turn.y;
        position = centre + vec2<f32>(
            offset.x * cosine - offset.y * sine,
            offset.x * sine + offset.y * cosine,
        );
    }

    // Normalised device coordinates span exactly the viewport: the visible part of the widget.
    let relative = (position - placement.viewport.xy) / max(placement.viewport.zw, vec2<f32>(1.0));

    var out: VertexOutput;
    out.position = vec4<f32>(relative.x * 2.0 - 1.0, 1.0 - relative.y * 2.0, 0.0, 1.0);
    out.uv = mix(placement.texels.xy, placement.texels.zw, corner);
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let colour = textureSample(photo, photo_sampler, input.uv);
    // The fragment's position is in framebuffer pixels, the frame `bright` is given in.
    let at = input.position.xy;
    let inside = all(at >= placement.bright.xy) && all(at < placement.bright.zw);
    return vec4<f32>(colour.rgb, colour.a * select(placement.turn.z, 1.0, inside));
}

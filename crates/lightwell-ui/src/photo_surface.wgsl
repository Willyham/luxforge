// The photo surface's quad for one tile: two triangles covering the part of the destination
// rectangle the tile draws, textured with the tile's part of the raster.
//
// `viewport` is the render pass's viewport — the part of this widget that is on screen — and
// `destination` is where the whole picture goes, both in physical pixels of the whole frame. The
// picture may reach far outside the viewport at a percentage zoom; the rasterizer clips what does.
// Both corners of the destination are snapped to the pixel grid with WGSL's own `round`, which is
// what the toolkit's image shader does for `snap: true`, so the picture lands on exactly the
// physical pixels the image widget would have landed it on.
//
// `region` is the part of the picture this tile draws, as fractions of the whole, and `texels` is
// that part in the tile texture's own coordinates. Every tile's quad is cut from the same snapped
// destination with the same fractions its neighbours use, so neighbouring quads share their edge
// exactly. A raster held in one texture is one tile whose region is the whole picture and whose
// texels are the whole texture.

struct Placement {
    viewport: vec4<f32>,
    destination: vec4<f32>,
    region: vec4<f32>,
    texels: vec4<f32>,
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
    let top_left = round(placement.destination.xy);
    let bottom_right = round(placement.destination.xy + placement.destination.zw);
    let tile_start = mix(top_left, bottom_right, placement.region.xy);
    let tile_end = mix(top_left, bottom_right, placement.region.zw);
    let position = mix(tile_start, tile_end, corner);

    // Normalised device coordinates span exactly the viewport: the visible part of the widget.
    let relative = (position - placement.viewport.xy) / max(placement.viewport.zw, vec2<f32>(1.0));

    var out: VertexOutput;
    out.position = vec4<f32>(relative.x * 2.0 - 1.0, 1.0 - relative.y * 2.0, 0.0, 1.0);
    out.uv = mix(placement.texels.xy, placement.texels.zw, corner);
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(photo, photo_sampler, input.uv);
}

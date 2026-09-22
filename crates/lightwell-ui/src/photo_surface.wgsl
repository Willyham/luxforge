// The photo surface's quad: two triangles covering the destination rectangle, textured with the
// raster the pipeline holds.
//
// `viewport` is the render pass's viewport — this widget's bounds — and `destination` is where the
// picture goes, both in physical pixels of the whole frame. Both corners are snapped to the pixel
// grid with WGSL's own `round`, which is what the toolkit's image shader does for `snap: true`, so
// the picture lands on exactly the physical pixels the image widget would have landed it on.

struct Placement {
    viewport: vec4<f32>,
    destination: vec4<f32>,
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
    let position = mix(top_left, bottom_right, corner);

    // The viewport is the widget's bounds, so normalised device coordinates span exactly them.
    let relative = (position - placement.viewport.xy) / max(placement.viewport.zw, vec2<f32>(1.0));

    var out: VertexOutput;
    out.position = vec4<f32>(relative.x * 2.0 - 1.0, 1.0 - relative.y * 2.0, 0.0, 1.0);
    out.uv = corner;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(photo, photo_sampler, input.uv);
}

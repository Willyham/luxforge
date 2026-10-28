//! An external module's GPU program (`docs/design/module-manager.md#contract`, "GPU program"):
//! checked and registered under the surface's convention, and run alone over the fixed probe
//! patches on a device, for the host's first-use probe and `cargo xtask module-check`.
//!
//! - **Registering** ([`register`]) parses `program.wgsl` with `naga` after the surface's
//!   [`PRELUDE`] and refuses, naming each fault, a program that declares anything but functions and
//!   constants (a binding, global, override, named type, alias or entry point), a function or
//!   constant whose name does not start with [`ENTRY`], or no [`ENTRY`] function. It then gives
//!   every name the program declares the caller's unique entry in place of [`ENTRY`]: the names
//!   come from the parse, and only identifier tokens equal to one of them are rewritten, never
//!   text in a comment or inside a longer identifier. The rewritten program is parsed again,
//!   must declare exactly the renamed names, and must validate with a harness that calls its entry
//!   with the colour signature, `fn(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) ->
//!   vec3<f32>`. Two external programs, and an external program and a built-in one, can then share
//!   one assembled shader.
//! - **Running** ([`Harness`]) compiles the registered program into a compute pipeline that reads
//!   one pixel of the concatenated patches an invocation, calls the entry at the pixel's position
//!   within its patch with the program's words at base 0 and an empty block, and writes the
//!   answer. [`Harness::run`] runs one set of words and reads every patch back: it waits on the
//!   device, so it runs on a worker that owns one, never the interface thread.
use super::PRELUDE;
use super::answered;
use luxforge_gpu_types::probe::Patch;
use std::borrow::Cow;
use std::collections::HashSet;
use wgpu::naga;

/// The entry every package's program declares, and the prefix of every name it declares.
pub const ENTRY: &str = "lf_module";

/// The most words a program's harness reads: as many as a unit holds.
pub const MAX_WORDS: usize = 4096;

/// The workgroup size of the harness.
const LANES: u32 = 64;

/// A program that passed [`register`]: its unique entry and its rewritten source, functions and
/// constants only, every name starting with the entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registered {
    pub entry: String,
    pub source: String,
}

fn parse(source: &str) -> Result<naga::Module, String> {
    naga::front::wgsl::parse_str(source).map_err(|error| error.emit_to_string(source))
}

/// The harness: each invocation reads one pixel of the concatenated patches, calls `entry` at its
/// position within its patch of `side × side`, and writes the answer.
fn harness(entry: &str, pixels: u32, side: u32) -> String {
    format!(
        "\n@group(0) @binding(2) var<storage, read> lf_probe_in: array<f32>;\n\
         @group(0) @binding(3) var<storage, read_write> lf_probe_out: array<f32>;\n\
         @compute @workgroup_size({LANES})\n\
         fn lf_probe(@builtin(global_invocation_id) id: vec3<u32>) {{\n    \
             let i = id.x;\n    \
             if i >= {pixels}u {{ return; }}\n    \
             let at = 3u * i;\n    \
             let j = i % {patch}u;\n    \
             let rgb = vec3<f32>(lf_probe_in[at], lf_probe_in[at + 1u], lf_probe_in[at + 2u]);\n    \
             let pos = vec2<f32>(f32(j % {side}u), f32(j / {side}u));\n    \
             let out: vec3<f32> = {entry}(rgb, pos, 0u, 0u);\n    \
             lf_probe_out[at] = out.x;\n    \
             lf_probe_out[at + 1u] = out.y;\n    \
             lf_probe_out[at + 2u] = out.z;\n\
         }}\n",
        patch = side * side,
    )
}

/// The names `program` declares after the prelude, checked against the convention: its functions
/// and constants, every one starting with `entry`, one of them the function `entry`.
fn declared(program: &str, entry: &str) -> Result<HashSet<String>, String> {
    let host = parse(PRELUDE).map_err(|error| format!("the prelude does not parse: {error}"))?;
    let module = parse(&format!("{PRELUDE}\n{program}"))?;
    let named = |name: &Option<String>| name.clone().unwrap_or_default();
    let host_functions: HashSet<String> =
        host.functions.iter().map(|(_, f)| named(&f.name)).collect();
    let host_types: HashSet<String> = host
        .types
        .iter()
        .filter_map(|(_, ty)| ty.name.clone())
        .collect();
    let mut names = HashSet::new();
    let mut refusals = Vec::new();
    for (_, function) in module.functions.iter() {
        let name = named(&function.name);
        if host_functions.contains(&name) {
            continue;
        }
        if !name.starts_with(entry) {
            refusals.push(format!("function `{name}` is not named {entry}…"));
        }
        names.insert(name);
    }
    for (_, constant) in module.constants.iter() {
        let name = named(&constant.name);
        if !name.starts_with(entry) {
            refusals.push(format!("constant `{name}` is not named {entry}…"));
        }
        names.insert(name);
    }
    for (_, global) in module
        .global_variables
        .iter()
        .skip(host.global_variables.len())
    {
        refusals.push(format!(
            "`{}` is a global variable; a program declares only functions and constants",
            named(&global.name)
        ));
    }
    for (_, value) in module.overrides.iter() {
        refusals.push(format!(
            "`{}` is an override; a program declares only functions and constants",
            named(&value.name)
        ));
    }
    for point in &module.entry_points {
        refusals.push(format!(
            "`{}` is an entry point; the host owns every entry point",
            point.name
        ));
    }
    for (_, ty) in module.types.iter() {
        if let Some(name) = &ty.name
            && !host_types.contains(name)
        {
            refusals.push(format!(
                "`{name}` is a type; a program declares only functions and constants"
            ));
        }
    }
    if !names.contains(entry) {
        refusals.push(format!("it declares no `{entry}` entry function"));
    }
    if refusals.is_empty() {
        Ok(names)
    } else {
        Err(refusals.join("; "))
    }
}

/// Whether `program`, after the prelude, validates with a harness that calls `entry` with the
/// colour signature.
fn validate(program: &str, entry: &str) -> Result<(), String> {
    let shader = format!("{PRELUDE}\n{program}{}", harness(entry, 1, 1));
    let module = parse(&shader)?;
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::default(),
    )
    .validate(&module)
    .map(|_| ())
    .map_err(|error| error.emit_to_string(&shader))
}

/// One lexical piece of WGSL text, as far as renaming needs: an identifier, or anything else
/// copied as it is (a comment, a number, an operator or white space).
enum Piece<'a> {
    Identifier(&'a str),
    Other(&'a str),
}

/// `source` split into identifiers and everything else. A line comment runs to its line's end and
/// a block comment to its matching close, nested; a number runs through the letters, digits, `_`
/// and `.` that follow its first digit, so a suffix or an exponent is never an identifier.
fn pieces(source: &str) -> Vec<Piece<'_>> {
    let bytes = source.as_bytes();
    let mut pieces = Vec::new();
    let mut at = 0;
    let word = |c: char| c == '_' || c.is_alphanumeric();
    while at < source.len() {
        let rest = &source[at..];
        let end = if rest.starts_with("//") {
            at + rest.find('\n').unwrap_or(rest.len())
        } else if rest.starts_with("/*") {
            let mut depth = 0usize;
            let mut i = at;
            loop {
                if i + 1 >= bytes.len() {
                    break bytes.len();
                }
                match (bytes[i], bytes[i + 1]) {
                    (b'/', b'*') => {
                        depth += 1;
                        i += 2;
                    }
                    (b'*', b'/') => {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break i;
                        }
                    }
                    _ => i += 1,
                }
            }
        } else {
            let first = rest.chars().next().unwrap_or(' ');
            let run = |accept: &dyn Fn(char) -> bool| {
                at + rest
                    .char_indices()
                    .find(|&(_, c)| !accept(c))
                    .map_or(rest.len(), |(i, _)| i)
            };
            if first == '_' || first.is_alphabetic() {
                let end = run(&word);
                pieces.push(Piece::Identifier(&source[at..end]));
                at = end;
                continue;
            }
            if first.is_ascii_digit() {
                run(&|c| word(c) || c == '.')
            } else {
                at + first.len_utf8()
            }
        };
        pieces.push(Piece::Other(&source[at..end]));
        at = end;
    }
    pieces
}

/// Check `program` and give its names `entry` in place of [`ENTRY`] (the module documentation),
/// or say why it is refused. `entry` is the host's unique name for this program, an identifier
/// that starts with `lf_` and none of the surface's own names.
pub fn register(program: &str, entry: &str) -> Result<Registered, String> {
    let names = declared(program, ENTRY)?;
    validate(program, ENTRY)?;
    let pieces = pieces(program);
    if pieces
        .iter()
        .any(|piece| matches!(piece, Piece::Identifier("alias")))
    {
        return Err(
            "it declares a type alias; a program declares only functions and constants".into(),
        );
    }
    let mut source = String::with_capacity(program.len() + names.len() * entry.len());
    for piece in &pieces {
        match piece {
            Piece::Identifier(name) if names.contains(*name) => {
                source.push_str(entry);
                source.push_str(&name[ENTRY.len()..]);
            }
            Piece::Identifier(text) | Piece::Other(text) => source.push_str(text),
        }
    }
    let renamed: HashSet<String> = names
        .iter()
        .map(|name| format!("{entry}{}", &name[ENTRY.len()..]))
        .collect();
    let declared = declared(&source, entry)
        .map_err(|error| format!("the renamed program does not check: {error}"))?;
    if declared != renamed {
        return Err(format!(
            "the renamed program declares {declared:?}, not {renamed:?}"
        ));
    }
    validate(&source, entry)
        .map_err(|error| format!("the renamed program does not validate: {error}"))?;
    Ok(Registered {
        entry: entry.to_owned(),
        source,
    })
}

/// A registered program compiled with its harness over a fixed set of patches, and the buffers it
/// runs in: the patches' pixels, the words, an empty block, the answers and their readback, about
/// 25 bytes a pixel.
pub struct Harness {
    pipeline: wgpu::ComputePipeline,
    group: wgpu::BindGroup,
    words: wgpu::Buffer,
    output: wgpu::Buffer,
    readback: wgpu::Buffer,
    pixels: u32,
    /// Each patch's pixel count, in order.
    lengths: Vec<usize>,
}

fn storage(
    device: &wgpu::Device,
    label: &str,
    bytes: &[u8],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    use wgpu::util::DeviceExt;
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytes,
        usage,
    })
}

impl Harness {
    /// `program` compiled to run over `patches`, each square and of one side, or why the device
    /// refused it.
    pub fn new(
        device: &wgpu::Device,
        program: &Registered,
        patches: &[Patch],
    ) -> Result<Self, String> {
        let side = patches.first().map_or(1, |patch| patch.width);
        if patches
            .iter()
            .any(|patch| patch.width != side || patch.height != side)
        {
            return Err("the probe patches are not all one square size".into());
        }
        let lengths: Vec<usize> = patches.iter().map(|patch| patch.rgb.len()).collect();
        let pixels = lengths.iter().sum::<usize>() as u32;
        let shader = format!(
            "{PRELUDE}\n{}{}",
            program.source,
            harness(&program.entry, pixels, side)
        );
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("external-program"),
            source: wgpu::ShaderSource::Wgsl(Cow::Owned(shader)),
        });
        let entry = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("external-program"),
            entries: &[
                entry(0, true),
                entry(1, true),
                entry(2, true),
                entry(3, false),
            ],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("external-program"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("external-program"),
                    bind_group_layouts: &[&layout],
                    push_constant_ranges: &[],
                }),
            ),
            module: &module,
            entry_point: Some("lf_probe"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let input: Vec<u8> = patches
            .iter()
            .flat_map(|patch| patch.rgb.iter().flatten())
            .flat_map(|value| value.to_le_bytes())
            .collect();
        let size = input.len() as u64;
        let input = storage(
            device,
            "external-program.in",
            &input,
            wgpu::BufferUsages::STORAGE,
        );
        let words = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("external-program.words"),
            size: (MAX_WORDS * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let blocks = storage(
            device,
            "external-program.blocks",
            &[0; 4],
            wgpu::BufferUsages::STORAGE,
        );
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("external-program.out"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("external-program.readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("external-program"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: words.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: blocks.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: input.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: output.as_entire_binding(),
                },
            ],
        });
        match answered(device.pop_error_scope()) {
            Some(None) => {}
            Some(Some(error)) => return Err(format!("the device refuses the program: {error}")),
            None => return Err("the device did not answer whether it accepts the program".into()),
        }
        Ok(Self {
            pipeline,
            group,
            words,
            output,
            readback,
            pixels,
            lengths,
        })
    }

    /// The program over every patch with `words`, read back: each patch's pixels in order. Waits
    /// for the device.
    pub fn run(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        words: &[u32],
    ) -> Result<Vec<Vec<[f32; 3]>>, String> {
        if words.len() > MAX_WORDS {
            return Err(format!(
                "{} words, more than the {MAX_WORDS} a program reads",
                words.len()
            ));
        }
        if !words.is_empty() {
            let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
            queue.write_buffer(&self.words, 0, &bytes);
        }
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("external-program"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("external-program"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.group, &[]);
            pass.dispatch_workgroups(self.pixels.div_ceil(LANES), 1, 1);
        }
        let size = self.output.size();
        encoder.copy_buffer_to_buffer(&self.output, 0, &self.readback, 0, size);
        let index = queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        self.readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: None,
            })
            .map_err(|error| format!("the device did not finish: {error}"))?;
        receiver
            .recv()
            .map_err(|_| "the readback was dropped".to_owned())?
            .map_err(|error| format!("the readback did not map: {error}"))?;
        let values: Vec<f32> = self
            .readback
            .slice(..)
            .get_mapped_range()
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .collect();
        self.readback.unmap();
        let mut patches = Vec::with_capacity(self.lengths.len());
        let mut start = 0;
        for &length in &self.lengths {
            patches.push(
                values[3 * start..3 * (start + length)]
                    .chunks_exact(3)
                    .map(|rgb| [rgb[0], rgb[1], rgb[2]])
                    .collect(),
            );
            start += length;
        }
        Ok(patches)
    }
}

#[cfg(test)]
#[path = "external_program_tests.rs"]
mod tests;

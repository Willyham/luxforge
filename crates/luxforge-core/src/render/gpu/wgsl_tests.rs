//! Every WGSL program the core ships, validated by `naga` under the photo surface's calling
//! convention: a test-only copy of the surface's prelude, the program, and a harness entry point
//! that calls the program's entry with its kind's signature and uses the result. The program must
//! parse and validate in that module, declare only functions and constants, every one named after
//! its entry, and no binding, entry point, type or attribute of its own.
//!
//! A spatial program is held to the spatial convention ([`super::spatial`]): the same prelude with
//! the spatial declarations after it, stood in for by stubs of the same signatures, and a harness
//! that runs every kernel and apply its units' descriptions name, each with its own signature.
//!
//! The real prelude is the surface's (`luxforge-ui`), which the core cannot name; a test in
//! `luxforge-app` checks the same programs against it.
use super::program::testing;
use super::{GpuProgram, GpuProgramKind};
use naga::{
    front::wgsl,
    valid::{Capabilities, ValidationFlags, Validator},
};
use std::collections::HashSet;
use std::path::Path;

/// Every GPU program a built-in module ships ([`crate::GPU_PROGRAMS`]). Each is a `.wgsl` file
/// beside its unit, and [`every_wgsl_file_in_the_core_is_a_shipped_program`] fails for a file
/// missing there.
static SHIPPED: &[&GpuProgram] = crate::GPU_PROGRAMS;

/// A copy of the convention's prelude: the concatenated uniform words and storage blocks, and the
/// four helpers a program reads them through.
pub(super) const PRELUDE: &str = "\
@group(0) @binding(0) var<storage, read> lf_words: array<u32>;
@group(0) @binding(1) var<storage, read> lf_blocks: array<u32>;
fn lf_word(i: u32) -> u32 { return lf_words[i]; }
fn lf_f32(i: u32) -> f32 { return bitcast<f32>(lf_words[i]); }
fn lf_block_word(i: u32) -> u32 { return lf_blocks[i]; }
fn lf_block_f32(i: u32) -> f32 { return bitcast<f32>(lf_blocks[i]); }
";

/// A copy of the spatial convention's declarations, as stubs of the signatures the surface
/// generates for each module that holds a spatial program.
pub(super) const SPATIAL_PRELUDE: &str = "\
var<workgroup> lf_shared: array<f32, 1024>;
fn lf_plane(slot: u32, at: vec2<i32>) -> vec4<f32> { return vec4<f32>(f32(slot), vec2<f32>(at), 1.0); }
fn lf_plane_size(slot: u32) -> vec2<i32> { return vec2<i32>(i32(slot) + 1); }
fn lf_source(at: vec2<i32>) -> vec3<f32> { return vec3<f32>(vec2<f32>(at), 0.5); }
fn lf_origin() -> vec2<i32> { return vec2<i32>(0); }
fn lf_size() -> vec2<i32> { return vec2<i32>(1); }
fn lf_store(at: vec2<i32>, value: vec4<f32>) {}
";

/// The prelude a program of `kind` is assembled after.
pub(super) fn prelude(kind: GpuProgramKind) -> String {
    match kind {
        GpuProgramKind::Spatial => format!("{PRELUDE}{SPATIAL_PRELUDE}"),
        GpuProgramKind::Colour | GpuProgramKind::Coverage => PRELUDE.to_owned(),
    }
}

/// The names the prelude and the harness declare, which a program's own names must not be.
const HOST_FUNCTIONS: &[&str] = &[
    "lf_word",
    "lf_f32",
    "lf_block_word",
    "lf_block_f32",
    "lf_harness",
    "lf_plane",
    "lf_plane_size",
    "lf_source",
    "lf_origin",
    "lf_size",
    "lf_store",
];
const HOST_GLOBALS: &[&str] = &["lf_words", "lf_blocks", "lf_harness_out", "lf_shared"];

/// Every kernel and apply the descriptions of a shipped spatial program name, to call in its
/// harness.
fn spatial_functions(program: &GpuProgram) -> (Vec<&'static str>, Vec<&'static str>) {
    match program.entry {
        "lf_presence" => crate::modules::presence_gpu_functions(),
        other => panic!("{other}: name the kernels and applies its descriptions use"),
    }
}

/// An entry point that calls `program`'s entry with its kind's argument types, binds the result
/// to its kind's return type and writes it, so a wrong signature fails validation.
fn harness(program: &GpuProgram) -> String {
    let call = match program.kind {
        GpuProgramKind::Colour => format!(
            "let rgb: vec3<f32> = {}(vec3<f32>(f32(id.x), 0.5, -0.25), \
             vec2<f32>(f32(id.x), f32(id.y)), id.z, id.z + 1u);\n    \
             lf_harness_out[0] = rgb.x + rgb.y + rgb.z;",
            program.entry
        ),
        GpuProgramKind::Coverage => format!(
            "let coverage: f32 = {}(vec2<f32>(f32(id.x), f32(id.y)), \
             vec3<f32>(0.25, 0.5, -0.75), id.z, id.z + 1u);\n    \
             lf_harness_out[0] = coverage;",
            program.entry
        ),
        GpuProgramKind::Spatial => {
            let (kernels, applies) = spatial_functions(program);
            let mut call = String::new();
            for kernel in kernels {
                call.push_str(&format!(
                    "{kernel}(vec2<i32>(id.xy), id.z, id.z + 1u);\n    "
                ));
            }
            call.push_str("var rgb = vec3<f32>(0.25, 0.5, -0.75);\n    ");
            for apply in applies {
                call.push_str(&format!(
                    "rgb = {apply}(rgb, vec2<i32>(id.xy), id.z, id.z + 1u, id.z + 2u);\n    "
                ));
            }
            call.push_str("lf_harness_out[0] = rgb.x + rgb.y + rgb.z;");
            call
        }
    };
    format!(
        "@group(0) @binding(2) var<storage, read_write> lf_harness_out: array<f32>;\n\
         @compute @workgroup_size(1)\n\
         fn lf_harness(@builtin(global_invocation_id) id: vec3<u32>) {{\n    {call}\n}}\n"
    )
}

/// The text with its comments removed, for the convention's textual rules.
fn without_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut depth = 0_usize;
    while let Some(c) = chars.next() {
        match (c, chars.peek().copied()) {
            ('/', Some('*')) => {
                chars.next();
                depth += 1;
            }
            ('*', Some('/')) if depth > 0 => {
                chars.next();
                depth -= 1;
            }
            ('/', Some('/')) if depth == 0 => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            _ if depth > 0 => {}
            _ => out.push(c),
        }
    }
    out
}

/// `program` validated under the convention, or what it breaks.
fn validate(program: &GpuProgram) -> Result<(), String> {
    let entry = program.entry;
    if !entry.starts_with("lf_") || HOST_FUNCTIONS.contains(&entry) {
        return Err(format!("{entry}: an entry is named lf_<module>_<unit>"));
    }
    if without_comments(program.source).contains('@') {
        return Err(format!(
            "{entry}: a program declares no attribute, binding or entry point"
        ));
    }
    let source = format!(
        "{}\n{}\n{}",
        prelude(program.kind),
        program.source,
        harness(program)
    );
    let module = wgsl::parse_str(&source).map_err(|error| error.emit_to_string(&source))?;
    Validator::new(ValidationFlags::all(), Capabilities::empty())
        .validate(&module)
        .map_err(|error| error.emit_to_string(&source))?;
    let own = |name: &Option<String>| -> Result<(), String> {
        match name {
            Some(name) if name.starts_with(entry) => Ok(()),
            other => Err(format!(
                "{entry}: declares {other:?}, which does not start with its entry name"
            )),
        }
    };
    let mut declares_entry = false;
    for (_, function) in module.functions.iter() {
        if function
            .name
            .as_deref()
            .is_some_and(|name| HOST_FUNCTIONS.contains(&name))
        {
            continue;
        }
        own(&function.name)?;
        declares_entry |= function.name.as_deref() == Some(entry);
    }
    // A spatial program's entry names the program, which starts every kernel and apply.
    if !declares_entry && program.kind != GpuProgramKind::Spatial {
        return Err(format!("{entry}: the text declares no function {entry}"));
    }
    for (_, constant) in module.constants.iter() {
        own(&constant.name)?;
    }
    for (_, global) in module.global_variables.iter() {
        if !global
            .name
            .as_deref()
            .is_some_and(|name| HOST_GLOBALS.contains(&name))
        {
            return Err(format!("{entry}: declares the variable {:?}", global.name));
        }
    }
    if module.entry_points.len() != 1 || !module.overrides.is_empty() {
        return Err(format!("{entry}: declares an entry point or an override"));
    }
    if let Some((_, named)) = module.types.iter().find(|(_, ty)| ty.name.is_some()) {
        return Err(format!("{entry}: declares the type {:?}", named.name));
    }
    Ok(())
}

#[test]
fn every_shipped_program_validates_under_the_surface_convention() {
    for program in SHIPPED.iter().chain(testing::PROGRAMS) {
        if let Err(error) = validate(program) {
            panic!("{} does not validate:\n{error}", program.entry);
        }
    }
    let entries: HashSet<_> = SHIPPED
        .iter()
        .chain(testing::PROGRAMS)
        .map(|program| program.entry)
        .collect();
    assert_eq!(
        entries.len(),
        SHIPPED.len() + testing::PROGRAMS.len(),
        "two programs share an entry name, which would collide in one module"
    );
}

/// The shipped list is complete: every `.wgsl` file under the core's sources is a shipped
/// program's text, and every shipped program's text is a file beside its unit.
#[test]
fn every_wgsl_file_in_the_core_is_a_shipped_program() {
    fn files(directory: &Path, found: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files(&path, found);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "wgsl")
            {
                found.push((
                    path.display().to_string(),
                    std::fs::read_to_string(&path).unwrap(),
                ));
            }
        }
    }
    let mut found = Vec::new();
    files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut found,
    );
    for (path, text) in &found {
        assert!(
            SHIPPED.iter().any(|program| program.source == text),
            "{path} is not a shipped program: list it in SHIPPED so it is validated"
        );
    }
    for program in SHIPPED {
        assert!(
            found.iter().any(|(_, text)| text == program.source),
            "{} is not kept in a .wgsl file beside its unit",
            program.entry
        );
    }
}

/// The checks above have teeth: each convention a program can break is refused, with a reason.
#[test]
fn programs_that_break_the_convention_are_refused() {
    let program = |entry: &'static str, kind, source: &'static str| GpuProgram {
        entry,
        source,
        kind,
        words: 0,
        enabled: false,
    };
    let colour = GpuProgramKind::Colour;
    let refused = [
        program(
            "lf_bad_binding",
            colour,
            "@group(1) @binding(0) var<uniform> lf_bad_binding_data: vec4<f32>;\n\
             fn lf_bad_binding(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) \
             -> vec3<f32> { return rgb; }",
        ),
        program(
            "lf_bad_entry",
            colour,
            "fn lf_bad_entry(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) \
             -> vec3<f32> { return rgb; }\n\
             @compute @workgroup_size(1) fn lf_bad_entry_main() {}",
        ),
        program(
            "lf_bad_name",
            colour,
            "fn helper(x: f32) -> f32 { return x; }\n\
             fn lf_bad_name(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) \
             -> vec3<f32> { return rgb * helper(1.0); }",
        ),
        program(
            "lf_bad_constant",
            colour,
            "const scale: f32 = 2.0;\n\
             fn lf_bad_constant(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) \
             -> vec3<f32> { return rgb * scale; }",
        ),
        program(
            "lf_bad_signature",
            colour,
            "fn lf_bad_signature(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) \
             -> f32 { return rgb.x; }",
        ),
        program(
            "lf_bad_kind",
            GpuProgramKind::Coverage,
            "fn lf_bad_kind(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) \
             -> vec3<f32> { return rgb; }",
        ),
        program(
            "lf_bad_type",
            colour,
            "struct lf_bad_type_pair { a: f32, b: f32 }\n\
             fn lf_bad_type(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) \
             -> vec3<f32> { let p = lf_bad_type_pair(1.0, 2.0); return rgb * p.a; }",
        ),
        program(
            "lf_bad_syntax",
            colour,
            "fn lf_bad_syntax(rgb: vec3<f32>) -> vec3<f32> { return rgb * ; }",
        ),
        program(
            "lf_missing",
            colour,
            "fn lf_missing_helper(rgb: vec3<f32>) -> vec3<f32> { return rgb; }",
        ),
        program(
            "lf_word",
            colour,
            "fn lf_word_x(rgb: vec3<f32>) -> vec3<f32> { return rgb; }",
        ),
    ];
    for program in &refused {
        assert!(
            validate(program).is_err(),
            "{} broke the convention and was accepted",
            program.entry
        );
    }
    // A comment may say anything, an `@` included.
    let commented = program(
        "lf_commented",
        colour,
        "// Not @group(0): comments are not declarations. /* nor @binding */\n\
         /* a /* nested */ block */\n\
         fn lf_commented(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) \
         -> vec3<f32> { return rgb; }",
    );
    validate(&commented).unwrap();
}

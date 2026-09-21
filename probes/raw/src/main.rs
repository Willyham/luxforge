//! Developer-only decoder comparison; never used by the editor.
use rawler::{decoders::RawDecodeParams, rawsource::RawSource, rawimage::{RawImageData, RawPhotometricInterpretation}};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{fs::{self, File, OpenOptions}, io::{Read, Write, BufWriter}, path::Path, sync::Arc, time::Instant};
type Result<T=()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 { return Err("usage: lightwell-raw-probe SOURCE NEW_OUTPUT_DIRECTORY".into()); }
    let path = Path::new(&args[0]);
    let out = Path::new(&args[1]);
    fs::create_dir(out)?;
    let start = Instant::now();
    let file = File::open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > 128 * 1024 * 1024 { return Err("input bound".into()); }
    let mut bytes = Vec::new();
    file.take(128 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 128 * 1024 * 1024 { return Err("input grew".into()); }
    let source_hash = format!("{:x}", Sha256::digest(&bytes));
    let read_ms = start.elapsed().as_secs_f64()*1000.;
    let input = RawSource::new_from_shared_vec(Arc::new(bytes));
    let start = Instant::now();
    let decoder = rawler::get_decoder(&input)?;
    let identify_ms = start.elapsed().as_secs_f64()*1000.;
    let start = Instant::now();
    let raw = decoder.raw_image(&input, &RawDecodeParams::default(), false)?;
    let unpack_ms = start.elapsed().as_secs_f64()*1000.;
    if raw.width == 0 || raw.height == 0 || raw.width > 16384 || raw.height > 16384 || raw.width.checked_mul(raw.height).ok_or("overflow")? > 64_000_000 || raw.cpp != 1 { return Err("unsupported probe layout".into()); }
    let RawImageData::Integer(samples) = &raw.data else { return Err("probe requires integer mosaic".into()); };
    if samples.len() != raw.width * raw.height { return Err("sample length".into()); }
    let mut hash = Sha256::new();
    let mut stream = BufWriter::new(OpenOptions::new().write(true).create_new(true).open(out.join("samples.u16le"))?);
    for v in samples { let b=v.to_le_bytes(); hash.update(b); stream.write_all(&b)?; }
    stream.flush()?;
    let cfa = match &raw.photometric {
        RawPhotometricInterpretation::Cfa(c) => json!({"width":c.cfa.width,"height":c.cfa.height,"pattern":(0..c.cfa.height).map(|y| (0..c.cfa.width).map(|x| c.cfa.color_at(y,x)).collect::<Vec<_>>()).collect::<Vec<_>>(), "sensor":format!("{:?}",c.sensor)}),
        _ => return Err("probe requires CFA".into())
    };
    let report=json!({"format":1,"backend":"rawler 0.8.0","scope":"unpack experiment; not editor qualification or production memory bound", "source_sha256":source_hash,
        "make":raw.make,"model":raw.model,"width":raw.width,"height":raw.height,"bps":raw.bps,"cpp":raw.cpp,
        "active_area":raw.active_area,"crop_area":raw.crop_area,"orientation":format!("{:?}",raw.orientation),
        "cfa":cfa,"black":raw.blacklevel.as_vec(),"white":raw.whitelevel.as_vec(),"wb":raw.wb_coeffs,"matrices":format!("{:?}",raw.color_matrix),
        "mosaic_sha256":format!("{:x}",hash.finalize()),"read_hash_ms":read_ms,"identify_ms":identify_ms,"unpack_ms":unpack_ms});
    let mut f=OpenOptions::new().write(true).create_new(true).open(out.join("result.json"))?;
    serde_json::to_writer_pretty(&mut f,&report)?; f.write_all(b"\n")?;
    println!("{} {}: {}x{}; unpack {:.1} ms", raw.make, raw.model, raw.width, raw.height, unpack_ms);
    Ok(())
}

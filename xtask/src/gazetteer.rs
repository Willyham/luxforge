//! `cargo xtask gazetteer`: derives the bundled gazetteer asset
//! (`crates/luxforge-core/assets/gazetteer/places.tsv`) from GeoNames' extracted `cities15000.txt`.
//!
//! The command reads one local file and writes one new file; it never touches the network, so the
//! download and its hashes are recorded by hand in `crates/luxforge-core/THIRD_PARTY.md`. The
//! output is a pure function of the source's rows: each row is validated, six of its 19 columns are
//! kept, and the rows are sorted, so a regenerated asset changes only where GeoNames changed a
//! place. The core's lookup (`organize/gazetteer.rs`) reads exactly this layout.

use crate::*;
use std::{cmp::Ordering, io::Write};

/// The first line of the asset: the column names, which is also the format marker the core's parser
/// requires.
const HEADER: &str = "name\tcountry\tfeature\tlatitude\tlongitude\tpopulation";
/// A refusal is the message naming what is wrong; `run` adds where.
type Checked<T> = std::result::Result<T, String>;
/// Columns of a GeoNames `geoname` table row (readme.txt, "The main 'geoname' table").
const GEONAMES_COLUMNS: usize = 19;
const NAME: usize = 1;
const LATITUDE: usize = 4;
const LONGITUDE: usize = 5;
const FEATURE_CLASS: usize = 6;
const FEATURE_CODE: usize = 7;
const COUNTRY: usize = 8;
const POPULATION: usize = 14;
/// GeoNames' own bound on a name (`varchar(200)`).
const MAX_NAME_CHARS: usize = 200;
/// GeoNames' own bound on a feature code (`varchar(10)`).
const MAX_FEATURE_CHARS: usize = 10;
/// Refusals listed before the rest are only counted.
const MAX_REPORTED: usize = 10;

/// One validated place. Coordinates keep the source's decimal text, so the asset is a strict column
/// selection of the upstream rows; the numbers order the rows.
struct Place {
    name: String,
    country: String,
    feature: String,
    latitude: (f64, String),
    longitude: (f64, String),
    population: u32,
}

impl Place {
    fn line(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            self.name,
            self.country,
            self.feature,
            self.latitude.1,
            self.longitude.1,
            self.population
        )
    }

    /// Country, name, latitude, longitude, population, feature code, then the text: two rows tie
    /// only when their output lines are identical, so the order never depends on the source's order.
    fn order(&self, other: &Self) -> Ordering {
        self.country
            .cmp(&other.country)
            .then_with(|| self.name.cmp(&other.name))
            .then_with(|| self.latitude.0.total_cmp(&other.latitude.0))
            .then_with(|| self.longitude.0.total_cmp(&other.longitude.0))
            .then_with(|| self.population.cmp(&other.population))
            .then_with(|| self.feature.cmp(&other.feature))
            .then_with(|| self.line().cmp(&other.line()))
    }
}

/// A plain decimal degree: an optional minus sign, digits, and optionally a point and digits. No
/// plus sign, exponent or whitespace, so the text and the number always agree.
fn coordinate(text: &str, limit: f64, what: &str) -> Checked<(f64, String)> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, "0"));
    let plain = !whole.is_empty()
        && !fraction.is_empty()
        && whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit());
    if !plain {
        return Err(format!("{what} `{text}` is not a plain decimal number"));
    }
    let value: f64 = text
        .parse()
        .map_err(|_| format!("{what} `{text}` is not a number"))?;
    if !value.is_finite() || value.abs() > limit {
        return Err(format!("{what} {text} is outside -{limit} to {limit}"));
    }
    Ok((value, text.to_owned()))
}

/// Validates one GeoNames row: 19 tab-separated columns, a name, a two-letter country code,
/// feature class P with a feature code, coordinates in range and a whole population.
fn place(line: &str) -> Checked<Place> {
    let columns: Vec<&str> = line.split('\t').collect();
    if columns.len() != GEONAMES_COLUMNS {
        return Err(format!(
            "expected {GEONAMES_COLUMNS} tab-separated columns, found {}",
            columns.len()
        ));
    }
    if columns[FEATURE_CLASS] != "P" {
        return Err(format!(
            "feature class `{}` is not P (populated place)",
            columns[FEATURE_CLASS]
        ));
    }
    let name = columns[NAME];
    if name.is_empty()
        || name.trim() != name
        || name.chars().any(char::is_control)
        || name.chars().count() > MAX_NAME_CHARS
    {
        return Err(format!(
            "name `{name}` is empty, padded, has a control character or is over {MAX_NAME_CHARS} characters"
        ));
    }
    let feature = columns[FEATURE_CODE];
    if feature.is_empty()
        || feature.len() > MAX_FEATURE_CHARS
        || !feature
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return Err(format!(
            "feature code `{feature}` is not one to {MAX_FEATURE_CHARS} capital letters or digits"
        ));
    }
    let country = columns[COUNTRY];
    if country.len() != 2 || !country.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(format!(
            "country code `{country}` is not two capital letters"
        ));
    }
    let digits = columns[POPULATION];
    let population = if digits.bytes().all(|b| b.is_ascii_digit()) {
        digits.parse::<u32>().ok()
    } else {
        None
    };
    let population = population
        .ok_or_else(|| format!("population `{digits}` is not a whole number that fits 32 bits"))?;
    Ok(Place {
        name: name.to_owned(),
        country: country.to_owned(),
        feature: feature.to_owned(),
        latitude: coordinate(columns[LATITUDE], 90.0, "latitude")?,
        longitude: coordinate(columns[LONGITUDE], 180.0, "longitude")?,
        population,
    })
}

/// The asset for a GeoNames `cities15000.txt`, or every refusal (up to [`MAX_REPORTED`]) naming the
/// line it found. Blank lines are refused too: the source has none.
fn convert(source: &str) -> Checked<String> {
    let mut places = Vec::new();
    let mut refusals = Vec::new();
    for (index, line) in source.lines().enumerate() {
        match place(line) {
            Ok(place) => places.push(place),
            Err(why) => refusals.push(format!("line {}: {why}", index + 1)),
        }
    }
    if places.is_empty() && refusals.is_empty() {
        refusals.push("the source has no rows".to_owned());
    }
    if !refusals.is_empty() {
        let hidden = refusals.len().saturating_sub(MAX_REPORTED);
        refusals.truncate(MAX_REPORTED);
        let more = if hidden > 0 {
            format!("\n... and {hidden} more")
        } else {
            String::new()
        };
        return Err(format!("{}{more}", refusals.join("\n")));
    }
    places.sort_by(Place::order);
    let mut asset = String::with_capacity(source.len() / 4);
    asset.push_str(HEADER);
    asset.push('\n');
    let mut last: Option<String> = None;
    for place in &places {
        let line = place.line();
        if last.as_deref() == Some(line.as_str()) {
            return Err(format!("duplicate place: {line}"));
        }
        asset.push_str(&line);
        asset.push('\n');
        last = Some(line);
    }
    Ok(asset)
}

/// Writes the asset for the GeoNames file at `source` to `output`, which must not exist.
pub fn run(source: &Path, output: &Path) -> Result {
    ensure(
        !output.exists(),
        format!("Gazetteer output must be new: {}", output.display()),
    )?;
    let text = fs::read_to_string(source)
        .map_err(|error| format!("Cannot read {}: {error}", source.display()))?;
    let asset = convert(&text).map_err(|why| format!("{}:\n{why}", source.display()))?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?
        .write_all(asset.as_bytes())?;
    println!(
        "Gazetteer: {} places, {} bytes\n  source {} sha256 {}\n  output {} sha256 {}",
        asset.lines().count() - 1,
        asset.len(),
        source.display(),
        hash(source)?,
        output.display(),
        hash(output)?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A GeoNames row with the columns this command reads and the others as upstream fills them.
    fn row(name: &str, latitude: &str, longitude: &str, country: &str, population: &str) -> String {
        row_of(name, "PPL", latitude, longitude, country, population)
    }

    fn row_of(
        name: &str,
        feature: &str,
        latitude: &str,
        longitude: &str,
        country: &str,
        population: &str,
    ) -> String {
        [
            "1",
            name,
            name,
            "Alt,Names",
            latitude,
            longitude,
            "P",
            feature,
            country,
            "",
            "01",
            "",
            "",
            "",
            population,
            "",
            "12",
            "Europe/Berlin",
            "2026-01-01",
        ]
        .join("\t")
    }

    #[test]
    fn a_small_source_converts_to_the_sorted_selection_of_its_columns() {
        let source = [
            row_of("Zürich", "PPLA", "47.36667", "8.55", "CH", "341730"),
            row_of("Konstanz", "PPLA3", "47.66033", "9.17582", "DE", "83713"),
            row_of("Bern", "PPLC", "46.94809", "7.44744", "CH", "133883"),
            row_of("Konstanz", "PPLX", "47.5", "-9.5", "DE", "0"),
        ]
        .join("\n");
        assert_eq!(
            convert(&source).unwrap(),
            "name\tcountry\tfeature\tlatitude\tlongitude\tpopulation\n\
             Bern\tCH\tPPLC\t46.94809\t7.44744\t133883\n\
             Zürich\tCH\tPPLA\t47.36667\t8.55\t341730\n\
             Konstanz\tDE\tPPLX\t47.5\t-9.5\t0\n\
             Konstanz\tDE\tPPLA3\t47.66033\t9.17582\t83713\n"
        );
    }

    #[test]
    fn rows_that_differ_only_in_their_feature_code_are_both_kept_in_a_fixed_order() {
        let rows = [
            row_of("A", "PPLX", "1", "2", "AA", "3"),
            row_of("A", "PPL", "1", "2", "AA", "3"),
        ];
        let forward = convert(&rows.join("\n")).unwrap();
        assert!(forward.ends_with("A\tAA\tPPL\t1\t2\t3\nA\tAA\tPPLX\t1\t2\t3\n"));
        assert_eq!(
            convert(&[rows[1].clone(), rows[0].clone()].join("\n")).unwrap(),
            forward
        );
    }

    #[test]
    fn the_output_does_not_depend_on_the_source_order() {
        let rows = [
            row("A", "1", "2", "AA", "3"),
            row("B", "-1.5", "-2.5", "AA", "4"),
            row("A", "1", "2", "AB", "3"),
        ];
        let forward = convert(&rows.join("\n")).unwrap();
        let reversed: Vec<_> = rows.iter().rev().cloned().collect();
        assert_eq!(convert(&reversed.join("\n")).unwrap(), forward);
    }

    #[test]
    fn a_malformed_row_is_refused_with_its_line_number() {
        let good = row("Konstanz", "47.66", "9.17", "DE", "83713");
        let cases: [(String, &str); 16] = [
            (
                row_of("A", "", "1", "2", "DE", "3"),
                "line 2: feature code ``",
            ),
            (
                row_of("A", "pplx", "1", "2", "DE", "3"),
                "line 2: feature code `pplx`",
            ),
            (
                row_of("A", "PPL-X", "1", "2", "DE", "3"),
                "line 2: feature code `PPL-X`",
            ),
            (
                row_of("A", "PPLAAAAAAAA", "1", "2", "DE", "3"),
                "line 2: feature code `PPLAAAAAAAA`",
            ),
            ("Konstanz\t47.66\t9.17".to_owned(), "line 2: expected 19"),
            (row("", "1", "2", "DE", "3"), "line 2: name ``"),
            (row(" X", "1", "2", "DE", "3"), "line 2: name ` X`"),
            (row("A\u{7}B", "1", "2", "DE", "3"), "line 2: name"),
            (
                row("A", "91", "2", "DE", "3"),
                "line 2: latitude 91 is outside",
            ),
            (
                row("A", "1", "180.5", "DE", "3"),
                "line 2: longitude 180.5 is outside",
            ),
            (
                row("A", "1e1", "2", "DE", "3"),
                "line 2: latitude `1e1` is not a plain",
            ),
            (
                row("A", "1", "NaN", "DE", "3"),
                "line 2: longitude `NaN` is not a plain",
            ),
            (row("A", "1", "2", "de", "3"), "line 2: country code `de`"),
            (row("A", "1", "2", "DE", "-3"), "line 2: population `-3`"),
            (row("A", "1", "2", "DE", ""), "line 2: population ``"),
            (
                row("A", "1", "2", "DE", "3").replace("\tP\t", "\tA\t"),
                "line 2: feature class `A`",
            ),
        ];
        for (bad, expected) in cases {
            let error = convert(&format!("{good}\n{bad}\n{good}")).unwrap_err();
            assert!(error.contains(expected), "{expected:?} not in {error:?}");
        }
    }

    #[test]
    fn several_refusals_are_listed_and_a_duplicate_or_empty_source_is_refused() {
        let good = row("Konstanz", "47.66", "9.17", "DE", "83713");
        let bad = row("A", "1", "2", "", "3");
        let error = convert(&format!("{bad}\n{good}\n{bad}")).unwrap_err();
        assert!(
            error.contains("line 1:") && error.contains("line 3:"),
            "{error}"
        );
        let many = vec![bad; MAX_REPORTED + 3].join("\n");
        assert!(convert(&many).unwrap_err().ends_with("... and 3 more"));
        assert!(
            convert(&format!("{good}\n{good}"))
                .unwrap_err()
                .starts_with("duplicate place: Konstanz\tDE")
        );
        assert!(convert("").unwrap_err().contains("no rows"));
        assert!(
            convert(&format!("{good}\n\n"))
                .unwrap_err()
                .contains("line 2:")
        );
    }

    #[test]
    fn the_output_must_be_new_and_is_written_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("cities15000.txt");
        fs::write(
            &source,
            row("Konstanz", "47.66", "9.17", "DE", "83713") + "\n",
        )
        .unwrap();
        let output = tmp.path().join("nested/places.tsv");
        run(&source, &output).unwrap();
        assert_eq!(
            fs::read_to_string(&output).unwrap(),
            "name\tcountry\tfeature\tlatitude\tlongitude\tpopulation\n\
             Konstanz\tDE\tPPL\t47.66\t9.17\t83713\n"
        );
        let error = run(&source, &output).unwrap_err().to_string();
        assert!(error.contains("must be new"), "{error}");
        let rejected = tmp.path().join("rejected.tsv");
        fs::write(&source, "not a row\n").unwrap();
        assert!(run(&source, &rejected).is_err());
        assert!(!rejected.exists(), "a refused source leaves no output");
    }

    /// The committed asset is what this command produces: converting its own rows, dressed as
    /// GeoNames rows, gives it back byte for byte (validated, sorted, no duplicates, plain
    /// coordinates). Regenerating from a downloaded source is the manual check in THIRD_PARTY.md.
    #[test]
    fn the_committed_asset_is_in_the_form_this_command_writes() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("crates/luxforge-core/assets/gazetteer/places.tsv");
        let asset = fs::read_to_string(&path).unwrap();
        let mut lines = asset.lines();
        assert_eq!(lines.next(), Some(HEADER));
        let source: Vec<String> = lines
            .map(|line| {
                let field: Vec<&str> = line.split('\t').collect();
                assert_eq!(field.len(), 6, "{line}");
                row_of(field[0], field[2], field[3], field[4], field[1], field[5])
            })
            .collect();
        assert!(source.len() > 30_000);
        assert!(convert(&source.join("\n")).unwrap() == asset);
    }
}

//! The lookup against a brute-force scan of every place, the known places, the antimeridian and the
//! poles, invalid positions, ties, and the bundled asset's own shape.

use super::*;
use std::collections::HashMap;

/// Rows in the bundled asset, as `crates/luxforge-core/THIRD_PARTY.md` records them.
const ASSET_ROWS: usize = 34_152;

fn places() -> &'static [Place] {
    &index().places
}

/// A fixed linear congruential sequence, so the probe positions are the same on every run.
struct Sequence(u64);

impl Sequence {
    /// The next number in [0, 1).
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / (1_u64 << 53) as f64
    }

    /// A latitude uniform in area, not in angle.
    fn latitude(&mut self) -> f64 {
        (2.0 * self.next() - 1.0).asin().to_degrees()
    }

    fn longitude(&mut self) -> f64 {
        360.0 * self.next() - 180.0
    }
}

/// The haversine formula on the sphere the lookup uses, from the latitudes and longitudes alone.
/// It shares no step with the unit vectors and the tree.
fn haversine_km(from: (f64, f64), to: (f64, f64)) -> f64 {
    let (phi_1, phi_2) = (from.0.to_radians(), to.0.to_radians());
    let delta_lambda = (to.1 - from.1).to_radians();
    let h = ((phi_2 - phi_1) / 2.0).sin().powi(2)
        + phi_1.cos() * phi_2.cos() * (delta_lambda / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_KM * h.sqrt().min(1.0).asin()
}

fn position(place: &Place) -> (f64, f64) {
    (place.latitude, place.longitude)
}

/// The row of `place`, which must be one of `places`.
fn row_of(places: &[Place], place: &Place) -> usize {
    places
        .iter()
        .position(|candidate| std::ptr::eq(candidate, place))
        .expect("the answer is one of the table's places")
}

/// The nearest place by scanning all of them: smallest haversine distance, the higher rank
/// (population, then row) on an exact tie.
fn scan(places: &[Place], at: (f64, f64)) -> (usize, f64) {
    let mut best = (0, f64::INFINITY);
    for (row, place) in places.iter().enumerate() {
        let distance = haversine_km(at, position(place));
        let higher = places[row].population > places[best.0].population;
        if distance < best.1 || (distance == best.1 && higher) {
            best = (row, distance);
        }
    }
    best
}

/// The lookup's answer at `at` equals the scan's: the same distance, and the same place unless two
/// places are the same distance away to within the two formulas' rounding.
fn assert_matches_scan(places: &[Place], index: &'static Index, at: (f64, f64)) {
    let found = index.nearest(&unit_vector(at.0, at.1));
    let (row, distance) = scan(places, at);
    assert!(
        (found.distance_km - distance).abs() < 1e-6,
        "{at:?}: {} km against {distance} km",
        found.distance_km
    );
    let found_row = row_of(places, found.place);
    assert!(
        found_row == row
            || (haversine_km(at, position(&places[found_row])) - distance).abs() < 1e-6,
        "{at:?}: {:?} against {:?}",
        places[found_row],
        places[row]
    );
}

/// An index over a table written here, kept for the rest of the run.
fn small(table: &str) -> &'static Index {
    let text: &'static str = Box::leak(format!("{HEADER}\n{table}").into_boxed_str());
    Box::leak(Box::new(Index::new(parse(text).unwrap())))
}

#[test]
fn the_bundled_asset_parses_to_its_recorded_shape() {
    let parsed = parse(ASSET).unwrap();
    assert_eq!(parsed.len(), ASSET_ROWS);
    assert_eq!(places().len(), ASSET_ROWS);
    for place in &parsed {
        assert!(!place.name.is_empty(), "{place:?}");
        assert!((-90.0..=90.0).contains(&place.latitude), "{place:?}");
        assert!((-180.0..=180.0).contains(&place.longitude), "{place:?}");
        assert!(
            place.country.len() == 2 && place.country.bytes().all(|b| b.is_ascii_uppercase()),
            "{place:?}"
        );
    }
    // The rows are in the order `cargo xtask gazetteer` writes (country, name, position,
    // population), which is strictly increasing, so no row repeats and the row order that breaks a
    // tie is the same on every regeneration.
    for pair in parsed.windows(2) {
        let ordered = (pair[0].country, pair[0].name)
            .cmp(&(pair[1].country, pair[1].name))
            .then(pair[0].latitude.total_cmp(&pair[1].latitude))
            .then(pair[0].longitude.total_cmp(&pair[1].longitude))
            .then(pair[0].population.cmp(&pair[1].population));
        assert!(ordered.is_lt(), "{:?} is not before {:?}", pair[0], pair[1]);
    }
}

#[test]
fn a_table_that_breaks_the_layout_is_refused_with_its_line() {
    let cases = [
        (
            "name\tcountry\tlat\tlon\tpopulation\nA\tDE\t1\t2\t3\n",
            "header",
        ),
        ("", "header"),
        (
            "name\tcountry\tlatitude\tlongitude\tpopulation\n",
            "no places",
        ),
        (
            "name\tcountry\tlatitude\tlongitude\tpopulation\nA\tDE\t1\t2\t3\nB\tDE\t1\t2\n",
            "line 3: expected five",
        ),
        (
            "name\tcountry\tlatitude\tlongitude\tpopulation\nA\tDE\t1\t2\t3\t4\n",
            "line 2: expected five",
        ),
        (
            "name\tcountry\tlatitude\tlongitude\tpopulation\n\tDE\t1\t2\t3\n",
            "line 2: the name",
        ),
        (
            "name\tcountry\tlatitude\tlongitude\tpopulation\nA\tde\t1\t2\t3\n",
            "line 2: the country",
        ),
        (
            "name\tcountry\tlatitude\tlongitude\tpopulation\nA\tDE\tNorth\t2\t3\n",
            "line 2: the latitude",
        ),
        (
            "name\tcountry\tlatitude\tlongitude\tpopulation\nA\tDE\t91\t2\t3\n",
            "line 2: the position",
        ),
        (
            "name\tcountry\tlatitude\tlongitude\tpopulation\nA\tDE\t1\tNaN\t3\n",
            "line 2: the position",
        ),
        (
            "name\tcountry\tlatitude\tlongitude\tpopulation\nA\tDE\t1\t2\t-3\n",
            "line 2: the population",
        ),
    ];
    for (table, expected) in cases {
        let error = parse(table).unwrap_err();
        assert!(error.contains(expected), "{expected:?} not in {error:?}");
    }
}

#[test]
fn the_lookup_equals_a_brute_force_scan_over_every_place() {
    let index = index();
    let places = places();
    let mut random = Sequence(0x2545_f491_4f6c_dd1d);
    let mut probes: Vec<(f64, f64)> = Vec::new();
    // Anywhere on the sphere, land and open ocean alike.
    for _ in 0..1500 {
        probes.push((random.latitude(), random.longitude()));
    }
    // Close to places, where the answer is a real neighbour choice.
    for _ in 0..1000 {
        let place = &places[(random.next() * places.len() as f64) as usize];
        probes.push((
            (place.latitude + random.next() - 0.5).clamp(-90.0, 90.0),
            (place.longitude + 2.0 * random.next() - 1.0).clamp(-180.0, 180.0),
        ));
    }
    // Either side of the antimeridian, at every latitude, and on it.
    for _ in 0..400 {
        let side = if random.next() < 0.5 { -1.0 } else { 1.0 };
        probes.push((random.latitude(), side * (179.9 + 0.1 * random.next())));
    }
    for (latitude, longitude) in [
        (-17.7, 179.99),
        (-17.7, -179.99),
        (64.7, 179.99),
        (64.7, -179.99),
        (-16.5, 180.0),
        (-16.5, -180.0),
        (0.0, 180.0),
        (65.0, 179.999),
    ] {
        probes.push((latitude, longitude));
    }
    // Round both poles.
    for _ in 0..300 {
        let side = if random.next() < 0.5 { -1.0 } else { 1.0 };
        probes.push((side * (89.0 + random.next()), random.longitude()));
    }
    for longitude in (-180..=180).step_by(20) {
        probes.push((90.0, f64::from(longitude)));
        probes.push((-90.0, f64::from(longitude)));
    }
    // The origin and a grid of open ocean.
    probes.push((0.0, 0.0));
    for latitude in (-60..=60).step_by(20) {
        for longitude in [-150.0, -120.0, -30.0, 70.0, 90.0] {
            probes.push((f64::from(latitude), longitude));
        }
    }
    assert!(probes.len() > 3000);
    for at in probes {
        assert_matches_scan(places, index, at);
    }
}

#[test]
fn a_position_on_a_place_answers_that_place_at_distance_zero() {
    let places = places();
    // With two places at one position, the higher-ranked answers.
    let mut ranked: HashMap<(u64, u64), usize> = HashMap::new();
    for (row, place) in places.iter().enumerate() {
        let key = (place.latitude.to_bits(), place.longitude.to_bits());
        let best = ranked.entry(key).or_insert(row);
        if place.population > places[*best].population {
            *best = row;
        }
    }
    for place in places {
        let found = nearest(place.latitude, place.longitude).unwrap();
        assert_eq!(found.distance_km, 0.0, "{place:?}");
        let key = (place.latitude.to_bits(), place.longitude.to_bits());
        assert!(
            std::ptr::eq(found.place, &places[ranked[&key]]),
            "{place:?}"
        );
    }
}

#[test]
fn known_places_are_found_where_they_are() {
    for (at, name, country) in [
        ((47.66, 9.18), "Konstanz", "DE"),
        ((51.5074, -0.1278), "London", "GB"),
        ((-33.8688, 151.2093), "Sydney", "AU"),
    ] {
        let found = nearest(at.0, at.1).unwrap();
        assert_eq!((found.place.name, found.place.country), (name, country));
        assert!(found.distance_km < 1.0, "{name}: {} km", found.distance_km);
    }
    // The middle of the Pacific is far from anything, and says so rather than refusing.
    let found = nearest(0.0, -140.0).unwrap();
    assert!(found.distance_km > 1000.0, "{} km", found.distance_km);
    let (_, distance) = scan(places(), (0.0, -140.0));
    assert!((found.distance_km - distance).abs() < 1e-6);
}

#[test]
fn the_antimeridian_is_not_an_edge() {
    let west = nearest(-17.7, 179.99).unwrap();
    let east = nearest(-17.7, -179.99).unwrap();
    assert_eq!(west.place.country, "FJ");
    assert!(std::ptr::eq(west.place, east.place));
    // Two hundredths of a degree of longitude at 17.7°S is about 2.1 km.
    assert!((west.distance_km - east.distance_km).abs() < 2.2);
    let chukotka = nearest(64.7, -179.99).unwrap();
    assert_eq!(
        (chukotka.place.name, chukotka.place.country),
        ("Anadyr", "RU")
    );
    assert!(std::ptr::eq(
        chukotka.place,
        nearest(64.7, 179.99).unwrap().place
    ));
    // 180° east and 180° west are one meridian.
    for latitude in [-45.0, 0.0, 10.0, 60.0] {
        let (a, b) = (
            nearest(latitude, 180.0).unwrap(),
            nearest(latitude, -180.0).unwrap(),
        );
        assert!(std::ptr::eq(a.place, b.place));
        assert!((a.distance_km - b.distance_km).abs() < 1e-6);
    }
}

#[test]
fn a_pole_is_one_position_at_every_longitude() {
    for latitude in [90.0, -90.0] {
        let first = nearest(latitude, 0.0).unwrap();
        assert!(first.distance_km > 1000.0);
        for longitude in [-180.0, -135.0, -90.0, -45.0, 30.0, 90.0, 135.0, 180.0] {
            let found = nearest(latitude, longitude).unwrap();
            assert!(
                std::ptr::eq(found.place, first.place),
                "{latitude} {longitude}"
            );
            assert!((found.distance_km - first.distance_km).abs() < 1e-6);
        }
    }
}

#[test]
fn a_position_that_is_not_one_answers_nothing() {
    for (latitude, longitude) in [
        (f64::NAN, 0.0),
        (0.0, f64::NAN),
        (f64::INFINITY, 0.0),
        (0.0, f64::NEG_INFINITY),
        (f64::NEG_INFINITY, f64::INFINITY),
        (90.0001, 0.0),
        (-90.0001, 0.0),
        (0.0, 180.0001),
        (0.0, -180.0001),
        (47.66, 360.0),
        (f64::MAX, f64::MAX),
    ] {
        assert!(
            nearest(latitude, longitude).is_none(),
            "{latitude} {longitude}"
        );
    }
    // The edges of the ranges are positions.
    for (latitude, longitude) in [(90.0, 180.0), (-90.0, -180.0), (-0.0, 0.0)] {
        assert!(
            nearest(latitude, longitude).is_some(),
            "{latitude} {longitude}"
        );
    }
}

#[test]
fn equal_distances_go_to_the_larger_population_then_the_earlier_row() {
    // Two places 10° either side of the origin are exactly as far from it as each other; three
    // places share one position.
    for (table, at, winner) in [
        (
            "West\tAA\t0\t-10\t5\nEast\tAA\t0\t10\t9\n",
            (0.0, 0.0),
            "East",
        ),
        (
            "West\tAA\t0\t-10\t9\nEast\tAA\t0\t10\t5\n",
            (0.0, 0.0),
            "West",
        ),
        (
            "West\tAA\t0\t-10\t7\nEast\tAA\t0\t10\t7\n",
            (0.0, 0.0),
            "West",
        ),
        (
            "East\tAA\t0\t10\t7\nWest\tAA\t0\t-10\t7\n",
            (0.0, 0.0),
            "East",
        ),
        (
            "A\tAA\t5\t5\t1\nB\tAA\t5\t5\t3\nC\tAA\t5\t5\t3\nD\tAA\t0\t-10\t2\n",
            (5.0, 5.0),
            "B",
        ),
        (
            "C\tAA\t5\t5\t3\nB\tAA\t5\t5\t3\nA\tAA\t5\t5\t1\nD\tAA\t0\t-10\t2\n",
            (5.0, 5.0),
            "C",
        ),
    ] {
        let found = small(table).nearest(&unit_vector(at.0, at.1));
        assert_eq!(found.place.name, winner, "{table:?}");
    }
}

#[test]
fn small_and_degenerate_tables_agree_with_the_scan() {
    let mut random = Sequence(0x9e37_79b9_7f4a_7c15);
    // One place; two; and tables full of shared coordinates and shared axis values, where a
    // median split has many equal keys to divide.
    for count in [1_usize, 2, 3, 7, 64, 300] {
        for lattice in [true, false] {
            let mut table = String::new();
            for row in 0..count {
                let (latitude, longitude) = if lattice {
                    (
                        f64::from(((random.next() * 5.0) as i32 - 2) * 30),
                        f64::from(((random.next() * 7.0) as i32 - 3) * 60),
                    )
                } else {
                    (random.latitude(), random.longitude())
                };
                let population = (random.next() * 4.0) as u32;
                table.push_str(&format!(
                    "P{row}\tAA\t{latitude}\t{longitude}\t{population}\n"
                ));
            }
            let index = small(&table);
            for _ in 0..300 {
                assert_matches_scan(
                    &index.places,
                    index,
                    (random.latitude(), random.longitude()),
                );
            }
            for place in &index.places {
                assert_matches_scan(&index.places, index, position(place));
            }
        }
    }
}

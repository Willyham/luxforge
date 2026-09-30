//! The lookup, with and without a rule for which places may answer, against a brute-force scan of
//! every place; the known places, the antimeridian and the poles, invalid positions, ties, the
//! feature helpers, and the bundled asset's own shape.

use super::*;
use std::collections::HashMap;

/// Rows in the bundled asset, as `crates/luxforge-core/THIRD_PARTY.md` records them.
const ASSET_ROWS: usize = 34_152;
const TABLE_HEADER: &str = "name\tcountry\tfeature\tlatitude\tlongitude\tpopulation\n";

/// A rule for which places may answer.
type Rule<'a> = &'a dyn Fn(&Place) -> bool;

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

/// The nearest accepted place by scanning all of them: smallest haversine distance, the higher rank
/// (population, then row) on an exact tie; `None` when none is accepted.
fn scan(
    places: &[Place],
    at: (f64, f64),
    matches: &dyn Fn(&Place) -> bool,
) -> Option<(usize, f64)> {
    let mut best: Option<(usize, f64)> = None;
    for (row, place) in places.iter().enumerate() {
        if !matches(place) {
            continue;
        }
        let distance = haversine_km(at, position(place));
        let better = best.is_none_or(|(held, nearest)| {
            distance < nearest
                || (distance == nearest && place.population > places[held].population)
        });
        if better {
            best = Some((row, distance));
        }
    }
    best
}

/// The lookup's answer at `at` equals the scan's: none in the same cases, otherwise the same
/// distance and the same place, unless two places are the same distance away to within the two
/// formulas' rounding.
fn assert_matches_scan(
    places: &[Place],
    at: (f64, f64),
    found: Option<Nearest>,
    matches: &dyn Fn(&Place) -> bool,
) {
    let Some((row, distance)) = scan(places, at, matches) else {
        assert!(
            found.is_none(),
            "{at:?}: {found:?} where nothing is accepted"
        );
        return;
    };
    let found = found.unwrap_or_else(|| panic!("{at:?}: nothing found, want {:?}", places[row]));
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

/// The most populous accepted place within `radius_km`, by scanning all of them with their
/// haversine `distances` from the position: more people, else nearer, else the earlier row.
fn scan_within(
    places: &[Place],
    distances: &[f64],
    radius_km: f64,
    matches: &dyn Fn(&Place) -> bool,
) -> Option<(usize, f64)> {
    let mut best: Option<(usize, f64)> = None;
    for (row, place) in places.iter().enumerate() {
        let distance = distances[row];
        if distance > radius_km || !matches(place) {
            continue;
        }
        let better = best.is_none_or(|(held, nearest)| {
            let people = places[held].population;
            place.population > people || (place.population == people && distance < nearest)
        });
        if better {
            best = Some((row, distance));
        }
    }
    best
}

/// An index over a table written here (rows after the header), kept for the rest of the run.
fn small(table: &str) -> &'static Index {
    let text: &'static str = Box::leak(format!("{TABLE_HEADER}{table}").into_boxed_str());
    Box::leak(Box::new(Index::new(parse(text).unwrap())))
}

/// The positions the scans probe: anywhere on the sphere, close to places, on both sides of the
/// antimeridian and on it, round both poles, and the open ocean.
fn probes(places: &[Place]) -> Vec<(f64, f64)> {
    let mut random = Sequence(0x2545_f491_4f6c_dd1d);
    let mut probes: Vec<(f64, f64)> = Vec::new();
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
    probes
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
        assert!(
            (1..=MAX_FEATURE_CHARS).contains(&place.feature.len())
                && place.feature.starts_with(|c: char| c.is_ascii_uppercase()),
            "{place:?}"
        );
    }
    // The rows are in the order `cargo xtask gazetteer` writes (country, name, position,
    // population, feature code), which is strictly increasing, so no row repeats and the row order
    // that breaks a tie is the same on every regeneration.
    for pair in parsed.windows(2) {
        let ordered = (pair[0].country, pair[0].name)
            .cmp(&(pair[1].country, pair[1].name))
            .then(pair[0].latitude.total_cmp(&pair[1].latitude))
            .then(pair[0].longitude.total_cmp(&pair[1].longitude))
            .then(pair[0].population.cmp(&pair[1].population))
            .then(pair[0].feature.cmp(pair[1].feature));
        assert!(ordered.is_lt(), "{:?} is not before {:?}", pair[0], pair[1]);
    }
    // The rows a caller looks a section up by are there, and are the minority.
    let sections = parsed.iter().filter(|place| place.is_section()).count();
    assert!(
        sections > 0 && sections * 5 < parsed.len(),
        "{sections} sections"
    );
    assert!(parsed.iter().any(Place::is_capital));
    assert!(parsed.iter().any(|place| place.admin_seat() == Some(1)));
}

#[test]
fn a_table_that_breaks_the_layout_is_refused_with_its_line() {
    let cases = [
        (
            "name\tcountry\tlatitude\tlongitude\tpopulation\nA\tDE\t1\t2\t3\n".to_owned(),
            "header",
        ),
        (String::new(), "header"),
        (TABLE_HEADER.to_owned(), "no places"),
        (
            format!("{TABLE_HEADER}A\tDE\tPPL\t1\t2\t3\nB\tDE\tPPL\t1\t2\n"),
            "line 3: expected six",
        ),
        (
            format!("{TABLE_HEADER}A\tDE\tPPL\t1\t2\t3\t4\n"),
            "line 2: expected six",
        ),
        (
            format!("{TABLE_HEADER}\tDE\tPPL\t1\t2\t3\n"),
            "line 2: the name",
        ),
        (
            format!("{TABLE_HEADER}A\tde\tPPL\t1\t2\t3\n"),
            "line 2: the country",
        ),
        (
            format!("{TABLE_HEADER}A\tDE\t\t1\t2\t3\n"),
            "line 2: the feature code",
        ),
        (
            format!("{TABLE_HEADER}A\tDE\tppl\t1\t2\t3\n"),
            "line 2: the feature code",
        ),
        (
            format!("{TABLE_HEADER}A\tDE\tPPLAAAAAAAA\t1\t2\t3\n"),
            "line 2: the feature code",
        ),
        (
            format!("{TABLE_HEADER}A\tDE\tPPL\tNorth\t2\t3\n"),
            "line 2: the latitude",
        ),
        (
            format!("{TABLE_HEADER}A\tDE\tPPL\t91\t2\t3\n"),
            "line 2: the position",
        ),
        (
            format!("{TABLE_HEADER}A\tDE\tPPL\t1\tNaN\t3\n"),
            "line 2: the position",
        ),
        (
            format!("{TABLE_HEADER}A\tDE\tPPL\t1\t2\t-3\n"),
            "line 2: the population",
        ),
    ];
    for (table, expected) in cases {
        let table: &'static str = Box::leak(table.into_boxed_str());
        let error = parse(table).unwrap_err();
        assert!(error.contains(expected), "{expected:?} not in {error:?}");
    }
}

#[test]
fn the_lookup_equals_a_brute_force_scan_over_every_place() {
    let places = places();
    for at in probes(places) {
        assert_matches_scan(places, at, nearest(at.0, at.1), &|_| true);
    }
}

#[test]
fn a_rule_gives_the_nearest_place_it_accepts_exactly_as_a_scan_does() {
    let places = places();
    let rules: [(&str, Rule); 5] = [
        ("towns, not sections", &|place| !place.is_section()),
        ("at least 100,000 inhabitants", &|place| {
            place.population >= 100_000
        }),
        ("capitals", &|place| place.is_capital()),
        ("one country's places", &|place| place.country == "FJ"),
        ("nothing", &|_| false),
    ];
    // Every fourth probe: a rule few places pass makes the tree walk most of it, and the scan
    // measures every place, so all of them would dominate the suite.
    let probes: Vec<_> = probes(places).into_iter().step_by(4).collect();
    for (name, matches) in rules {
        let mut none = 0;
        for &at in &probes {
            let found = nearest_matching(at.0, at.1, matches);
            assert_matches_scan(places, at, found, matches);
            none += usize::from(found.is_none());
            assert!(
                found.is_none_or(|found| matches(found.place)),
                "{name}: {at:?}"
            );
        }
        // Only the rule that accepts nothing finds nothing.
        assert_eq!(
            none,
            if name == "nothing" { probes.len() } else { 0 },
            "{name}"
        );
    }
    // The accepted place is an answer wherever the query is: the only place in the table.
    let only = nearest_matching(-33.87, 151.21, |place| place.name == "Suva").unwrap();
    assert_eq!((only.place.name, only.place.country), ("Suva", "FJ"));
    assert!((only.distance_km - haversine_km((-33.87, 151.21), position(only.place))).abs() < 1e-6);
    // A position that is not one is refused before the rule is asked anything.
    assert!(nearest_matching(91.0, 0.0, |_| panic!("asked")).is_none());
    assert!(nearest_matching(f64::NAN, 0.0, |_| panic!("asked")).is_none());
}

#[test]
fn a_city_centre_is_named_by_its_district_unless_the_rule_says_otherwise() {
    // Berlin's centre is nearer to its Mitte district than to Berlin itself.
    let district = nearest(52.52, 13.405).unwrap();
    assert_eq!(
        (district.place.name, district.place.feature),
        ("Mitte", "PPLX")
    );
    let city = nearest_matching(52.52, 13.405, |place| !place.is_section()).unwrap();
    assert_eq!((city.place.name, city.place.country), ("Berlin", "DE"));
    assert!(city.place.is_capital() && city.distance_km > district.distance_km);
    // Paris's arrondissements are ordinary populated places, so only size tells Paris from them.
    let louvre = nearest_matching(48.8606, 2.3376, |place| place.population >= 100_000).unwrap();
    assert_eq!((louvre.place.name, louvre.place.country), ("Paris", "FR"));
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
    let (_, distance) = scan(places(), (0.0, -140.0), &|_| true).unwrap();
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
            "West\tAA\tPPL\t0\t-10\t5\nEast\tAA\tPPL\t0\t10\t9\n",
            (0.0, 0.0),
            "East",
        ),
        (
            "West\tAA\tPPL\t0\t-10\t9\nEast\tAA\tPPL\t0\t10\t5\n",
            (0.0, 0.0),
            "West",
        ),
        (
            "West\tAA\tPPL\t0\t-10\t7\nEast\tAA\tPPL\t0\t10\t7\n",
            (0.0, 0.0),
            "West",
        ),
        (
            "East\tAA\tPPL\t0\t10\t7\nWest\tAA\tPPL\t0\t-10\t7\n",
            (0.0, 0.0),
            "East",
        ),
        (
            "A\tAA\tPPL\t5\t5\t1\nB\tAA\tPPL\t5\t5\t3\nC\tAA\tPPL\t5\t5\t3\nD\tAA\tPPL\t0\t-10\t2\n",
            (5.0, 5.0),
            "B",
        ),
        (
            "C\tAA\tPPL\t5\t5\t3\nB\tAA\tPPL\t5\t5\t3\nA\tAA\tPPL\t5\t5\t1\nD\tAA\tPPL\t0\t-10\t2\n",
            (5.0, 5.0),
            "C",
        ),
    ] {
        let found = small(table)
            .nearest(&unit_vector(at.0, at.1), &|_: &Place| true)
            .unwrap();
        assert_eq!(found.place.name, winner, "{table:?}");
    }
    // A place the rule refuses does not take part in the tie.
    let index = small("B\tAA\tPPLX\t5\t5\t9\nC\tAA\tPPL\t5\t5\t3\nD\tAA\tPPL\t5\t5\t1\n");
    let found = index
        .nearest(&unit_vector(5.0, 5.0), &|place: &Place| !place.is_section())
        .unwrap();
    assert_eq!(found.place.name, "C");
}

#[test]
fn feature_codes_say_what_kind_of_place_a_row_is() {
    let index = small(
        "Town\tAA\tPPL\t0\t0\t1\nRegion\tAA\tPPLA\t0\t1\t1\nCounty\tAA\tPPLA2\t0\t2\t1\n\
         Parish\tAA\tPPLA3\t0\t3\t1\nWard\tAA\tPPLA4\t0\t4\t1\nHamlet\tAA\tPPLA5\t0\t5\t1\n\
         Capital\tAA\tPPLC\t0\t6\t1\nDistrict\tAA\tPPLX\t0\t7\t1\nSeat\tAA\tPPLG\t0\t8\t1\n\
         Odd\tAA\tPPLA6\t0\t9\t1\nOdder\tAA\tPPLAX\t0\t10\t1\n",
    );
    let kinds: Vec<_> = index
        .places
        .iter()
        .map(|place| {
            (
                place.name,
                place.is_section(),
                place.is_capital(),
                place.admin_seat(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("Town", false, false, None),
            ("Region", false, false, Some(1)),
            ("County", false, false, Some(2)),
            ("Parish", false, false, Some(3)),
            ("Ward", false, false, Some(4)),
            ("Hamlet", false, false, Some(5)),
            ("Capital", false, true, None),
            ("District", true, false, None),
            ("Seat", false, false, None),
            ("Odd", false, false, None),
            ("Odder", false, false, None),
        ]
    );
}

#[test]
fn the_most_populous_place_within_a_radius_equals_a_scan() {
    let places = places();
    let rules: [(&str, Rule); 2] = [
        ("any place", &|_| true),
        ("towns, not sections", &|place| !place.is_section()),
    ];
    // Every eighth probe, and the distances to every place measured once per probe.
    for &at in probes(places).iter().step_by(8) {
        let distances: Vec<f64> = places
            .iter()
            .map(|place| haversine_km(at, position(place)))
            .collect();
        for radius in [0.0, 10.0, 100.0, 2000.0, 25_000.0, f64::INFINITY] {
            for (name, matches) in rules {
                let found = most_populous_within(at.0, at.1, radius, matches);
                let want = scan_within(places, &distances, radius, matches);
                let context = format!("{name}, {radius} km of {at:?}");
                let (Some(found), Some((row, distance))) = (found, want) else {
                    assert!(found.is_none() && want.is_none(), "{context}: {found:?}");
                    continue;
                };
                let found_row = row_of(places, found.place);
                assert!(
                    (found.distance_km - distances[found_row]).abs() < 1e-6,
                    "{context}: {} km",
                    found.distance_km
                );
                // The same place, or one as populous and as far away as the scan's to within
                // the two formulas' rounding.
                assert!(
                    found_row == row
                        || (places[found_row].population == places[row].population
                            && (distances[found_row] - distance).abs() < 1e-6),
                    "{context}: {:?} against {:?}",
                    places[found_row],
                    places[row]
                );
                assert!(matches(found.place) && found.distance_km <= radius + 1e-6);
            }
        }
    }
}

#[test]
fn a_radius_of_nothing_finds_the_place_underneath() {
    let places = places();
    for place in places.iter().step_by(97) {
        let found = most_populous_within(place.latitude, place.longitude, 0.0, |_| true).unwrap();
        assert_eq!(found.distance_km, 0.0, "{place:?}");
        assert_eq!(position(found.place), position(place), "{place:?}");
        assert!(found.place.population >= place.population, "{place:?}");
    }
    // Nothing underneath, nothing found.
    assert!(most_populous_within(0.0, -140.0, 0.0, |_| true).is_none());
}

#[test]
fn a_city_is_named_for_the_biggest_place_around_not_the_one_beside() {
    for (at, radius, name, country) in [
        ((52.52, 13.405), 10.0, "Berlin", "DE"),
        ((48.8606, 2.3376), 10.0, "Paris", "FR"),
        ((48.8867, 2.3431), 10.0, "Paris", "FR"),
        ((40.758, -73.9855), 10.0, "New York City", "US"),
        ((-33.8568, 151.2153), 10.0, "Sydney", "AU"),
        ((35.6595, 139.7005), 10.0, "Tokyo", "JP"),
        // A town stays a town: nothing bigger is within reach.
        ((47.6595, 9.178), 10.0, "Konstanz", "DE"),
        ((47.7047, 9.1954), 10.0, "Konstanz", "DE"),
        // Far from the town, a bigger place is the answer only when the radius reaches it.
        ((47.5622, 13.6493), 60.0, "Salzburg", "AT"),
    ] {
        let found = most_populous_within(at.0, at.1, radius, |_| true).unwrap();
        assert_eq!(
            (found.place.name, found.place.country),
            (name, country),
            "{at:?}"
        );
        assert!(found.distance_km <= radius);
    }
    // Hallstatt has no place within 10 km, however small.
    assert!(most_populous_within(47.5622, 13.6493, 10.0, |_| true).is_none());
}

#[test]
fn the_most_populous_place_breaks_ties_by_distance_then_row_and_honours_the_radius_and_rule() {
    let query = unit_vector(0.0, 0.0);
    let bound2 = |degrees: f64| (2.0 * (degrees.to_radians() / 2.0).sin()).powi(2);
    for (table, degrees, rule, winner) in [
        // More people beats nearer; a bigger place outside the radius does not count.
        (
            "Near\tAA\tPPL\t0\t1\t5\nFar\tAA\tPPL\t0\t3\t9\nOut\tAA\tPPL\t0\t6\t99\n",
            4.0,
            true,
            "Far",
        ),
        // As many people: the nearer.
        (
            "Far\tAA\tPPL\t0\t3\t9\nNear\tAA\tPPL\t0\t1\t9\n",
            4.0,
            true,
            "Near",
        ),
        // As many people, as far: the earlier row.
        (
            "West\tAA\tPPL\t0\t-2\t9\nEast\tAA\tPPL\t0\t2\t9\n",
            4.0,
            true,
            "West",
        ),
        (
            "East\tAA\tPPL\t0\t2\t9\nWest\tAA\tPPL\t0\t-2\t9\n",
            4.0,
            true,
            "East",
        ),
        // The rule takes the district out, and the next in size answers.
        (
            "District\tAA\tPPLX\t0\t1\t9\nTown\tAA\tPPL\t0\t2\t5\nHamlet\tAA\tPPL\t0\t1\t1\n",
            4.0,
            false,
            "Town",
        ),
    ] {
        let index = small(table);
        let found = if rule {
            index.most_populous_within(&query, bound2(degrees), &|_: &Place| true)
        } else {
            index.most_populous_within(&query, bound2(degrees), &|place: &Place| {
                !place.is_section()
            })
        };
        assert_eq!(found.unwrap().place.name, winner, "{table:?}");
    }
    // A radius or a position that is not one answers nothing.
    for radius in [-1.0, f64::NAN, -0.0001, f64::NEG_INFINITY] {
        assert!(
            most_populous_within(0.0, 0.0, radius, |_| true).is_none(),
            "{radius}"
        );
    }
    assert!(most_populous_within(91.0, 0.0, 10.0, |_| true).is_none());
    assert!(most_populous_within(0.0, f64::NAN, 10.0, |_| true).is_none());
    assert!(most_populous_within(0.0, 0.0, 10.0, |_| false).is_none());
    // The whole globe is in reach at infinity: the biggest place of all.
    let biggest = places().iter().map(|place| place.population).max().unwrap();
    let found = most_populous_within(0.0, 0.0, f64::INFINITY, |_| true).unwrap();
    assert_eq!(found.place.population, biggest);
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
                let feature = ["PPL", "PPLX", "PPLA", "PPLC"][(random.next() * 4.0) as usize];
                table.push_str(&format!(
                    "P{row}\tAA\t{feature}\t{latitude}\t{longitude}\t{population}\n"
                ));
            }
            let index = small(&table);
            let rules: [&dyn Fn(&Place) -> bool; 3] =
                [&|_| true, &|place| !place.is_section(), &|place| {
                    place.is_capital()
                }];
            for rule in rules {
                let ask = |at: (f64, f64)| index.nearest(&unit_vector(at.0, at.1), &rule);
                for _ in 0..200 {
                    let at = (random.latitude(), random.longitude());
                    assert_matches_scan(&index.places, at, ask(at), rule);
                }
                for place in &index.places {
                    let at = position(place);
                    assert_matches_scan(&index.places, at, ask(at), rule);
                }
            }
        }
    }
}

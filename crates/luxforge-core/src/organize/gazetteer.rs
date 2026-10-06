//! The offline gazetteer: the populated place nearest to a position, from a table compiled into the
//! binary.
//!
//! **The data.** `assets/gazetteer/places.tsv` is derived by `cargo xtask gazetteer` from GeoNames'
//! `cities15000` extract (every populated place with more than 15,000 inhabitants, and every
//! capital), which is licensed CC BY 4.0. It keeps six of GeoNames' columns (name, ISO 3166-1
//! country code, feature code, latitude and longitude in WGS84 degrees, population) and sorts the
//! rows. The source, hashes, licence and the changes made are in
//! `crates/luxforge-core/THIRD_PARTY.md`. The feature code tells a town from a section of one (a
//! neighbourhood, district or ward of a larger place is PPLX), which matters because the nearest
//! place to a city centre is often one of its districts: [`nearest_matching`] takes the caller's
//! own rule for which places may answer.
//!
//! **The query.** [`nearest`] answers the place at the smallest great-circle distance from a
//! position, and that distance; it reads no file and makes no request, ever. What counts as too far
//! to name anything is the caller's decision, so the distance is always returned.
//! [`most_populous_within`] answers instead with the biggest place around a position, which is how
//! a photograph in a city's centre is named for the city and not for the arrondissement it is in.
//!
//! **The index.** The table is parsed and indexed once, on the first query (never at startup), into
//! a k-d tree over the places' unit vectors in 3-D. The chord between two unit vectors grows with
//! the great-circle distance between them, so the nearest by chord is the nearest on the sphere
//! with no special case at the antimeridian or the poles.
//!
//! **Bounds.** The place count is fixed by the compiled-in asset (about 34,000 rows, 1.4 MB of
//! text). The index holds the parsed rows (72 bytes each; the names and codes borrow from the
//! asset) and one 32-byte tree node per place, about 3.5 MB of heap, and never grows. The first
//! query parses the table and builds the tree in about 10 ms in a release build; every later one
//! allocates nothing and visits about 30 nodes near a place (about 190 in open ocean), roughly a
//! microsecond; a radius query of 10 to 50 km visits fewer than 70 and takes under a microsecond. A
//! query whose rule few places satisfy walks further (about 1,000 nodes, under 10 µs, for capitals
//! only), at most the whole tree.

use std::sync::OnceLock;

/// The bundled table: a header line, then one place per line with tab-separated fields in the
/// header's order. The header doubles as the format marker: a different one is refused.
const ASSET: &str = include_str!("../../assets/gazetteer/places.tsv");
const HEADER: &str = "name\tcountry\tfeature\tlatitude\tlongitude\tpopulation";
/// GeoNames' own bound on a feature code (`varchar(10)`).
const MAX_FEATURE_CHARS: usize = 10;
/// The IUGG mean Earth radius in kilometres, for turning an angle between two positions into a
/// distance.
const EARTH_RADIUS_KM: f64 = 6371.0088;

/// One populated place, as GeoNames names and places it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Place {
    /// GeoNames' `name`: the place's name in UTF-8, in the local form GeoNames gives it.
    pub name: &'static str,
    /// Its ISO 3166-1 alpha-2 country code, in capitals ("DE").
    pub country: &'static str,
    /// GeoNames' feature code in capitals and digits: "PPL" (a populated place), "PPLA" to "PPLA5"
    /// (the seat of an administrative division of that order), "PPLC" (a country's capital),
    /// "PPLX" (a section of a populated place) and a few rarer ones. See [`Place::is_section`],
    /// [`Place::is_capital`] and [`Place::admin_seat`].
    pub feature: &'static str,
    /// Degrees north, -90 to 90.
    pub latitude: f64,
    /// Degrees east, -180 to 180.
    pub longitude: f64,
    /// Inhabitants as GeoNames records them; 0 for a capital it has no count for.
    pub population: u32,
}

impl Place {
    /// A section of a larger populated place: a neighbourhood, district or ward (feature PPLX).
    pub(crate) fn is_section(&self) -> bool {
        self.feature == "PPLX"
    }

    /// The capital of a country or other political entity (feature PPLC).
    #[allow(dead_code, reason = "only the gazetteer's tests read it yet")]
    pub(crate) fn is_capital(&self) -> bool {
        self.feature == "PPLC"
    }

    /// The order of the administrative division this place is the seat of: 1 for PPLA (a state or
    /// province), 2 to 5 for PPLA2 to PPLA5 (a county, district, municipality, ...); `None` for a
    /// place that is not a seat, or a capital.
    #[allow(dead_code, reason = "only the gazetteer's tests read it yet")]
    pub(crate) fn admin_seat(&self) -> Option<u8> {
        match self.feature.strip_prefix("PPLA")? {
            "" => Some(1),
            order @ ("2" | "3" | "4" | "5") => order.parse().ok(),
            _ => None,
        }
    }
}

/// The answer to [`nearest`] and [`nearest_matching`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Nearest {
    pub place: &'static Place,
    /// The great-circle distance from the queried position to the place, in kilometres.
    pub distance_km: f64,
}

/// The place nearest to `latitude` and `longitude` (degrees), or `None` for a position that is not
/// one: not finite, latitude outside -90 to 90 or longitude outside -180 to 180. Two places at
/// exactly the same distance are told apart by the larger population, then by row order in the
/// asset, so the answer never depends on how the index was built.
pub(crate) fn nearest(latitude: f64, longitude: f64) -> Option<Nearest> {
    nearest_matching(latitude, longitude, |_| true)
}

/// The nearest place that `matches` accepts, by the same distance and tie rule as [`nearest`]; a
/// place the rule refuses is never the answer, wherever it is. `None` for a position that is not
/// one, or when no place is accepted. The rule is called on the places the search would otherwise
/// keep, and must answer the same for the same place every time.
pub(crate) fn nearest_matching(
    latitude: f64,
    longitude: f64,
    matches: impl Fn(&Place) -> bool,
) -> Option<Nearest> {
    if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
        return None;
    }
    index().nearest(&unit_vector(latitude, longitude), &matches)
}

/// The most populous place that `matches` accepts within `radius_km` (great-circle, inclusive) of
/// the position, and its distance; the nearer of two equally populous places, then the earlier
/// row. `None` for a position that is not one, a radius that is negative or not a number, or when
/// no accepted place is that close. This names a photograph's surroundings ("Paris", not the
/// arrondissement it stands in) where [`nearest_matching`] names what it stands next to.
pub(crate) fn most_populous_within(
    latitude: f64,
    longitude: f64,
    radius_km: f64,
    matches: impl Fn(&Place) -> bool,
) -> Option<Nearest> {
    if !(-90.0..=90.0).contains(&latitude)
        || !(-180.0..=180.0).contains(&longitude)
        || radius_km.is_nan()
        || radius_km < 0.0
    {
        return None;
    }
    // The chord of an arc of the radius; an arc past half the globe is the whole globe.
    let chord = 2.0 * ((radius_km / EARTH_RADIUS_KM).min(std::f64::consts::PI) / 2.0).sin();
    index().most_populous_within(&unit_vector(latitude, longitude), chord * chord, &matches)
}

/// The places and their tree, built once by the first query.
struct Index {
    /// In asset order, the order that breaks a tie of population.
    places: Vec<Place>,
    /// The tree: each slice's middle element is its node, its lower half the left subtree and its
    /// upper half the right, splitting on x, y and z in turn by depth.
    tree: Vec<Node>,
}

#[derive(Debug, Clone, Copy)]
struct Node {
    /// The place's position on the unit sphere.
    point: [f64; 3],
    /// Its row in [`Index::places`].
    place: u32,
}

/// The best place found so far by one query.
struct Best {
    /// The squared chord to it; infinity while there is none.
    distance2: f64,
    place: Option<u32>,
}

fn index() -> &'static Index {
    static INDEX: OnceLock<Index> = OnceLock::new();
    INDEX.get_or_init(|| {
        let places =
            parse(ASSET).expect("the bundled gazetteer is well formed (see the asset test)");
        Index::new(places)
    })
}

/// The unit vector of a position: x through (0°, 0°), y through (0°, 90°E), z through the north
/// pole.
fn unit_vector(latitude: f64, longitude: f64) -> [f64; 3] {
    let (latitude, longitude) = (latitude.to_radians(), longitude.to_radians());
    let ring = latitude.cos();
    [
        ring * longitude.cos(),
        ring * longitude.sin(),
        latitude.sin(),
    ]
}

fn squared_chord(a: &[f64; 3], b: &[f64; 3]) -> f64 {
    let (x, y, z) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    x * x + y * y + z * z
}

impl Index {
    fn new(places: Vec<Place>) -> Self {
        let mut tree: Vec<Node> = places
            .iter()
            .enumerate()
            .map(|(row, place)| Node {
                point: unit_vector(place.latitude, place.longitude),
                place: u32::try_from(row).expect("parse bounds the row count"),
            })
            .collect();
        build(&mut tree, 0);
        Self { places, tree }
    }

    /// Whether the place at row `a` outranks the one at row `b` at an equal distance.
    fn outranks(&self, a: u32, b: u32) -> bool {
        let (a_population, b_population) = (
            self.places[a as usize].population,
            self.places[b as usize].population,
        );
        a_population > b_population || (a_population == b_population && a < b)
    }

    fn nearest(
        &'static self,
        query: &[f64; 3],
        matches: &impl Fn(&Place) -> bool,
    ) -> Option<Nearest> {
        let mut best = Best {
            distance2: f64::INFINITY,
            place: None,
        };
        self.search(&self.tree, 0, query, matches, &mut best);
        Some(self.answer(best.place?, best.distance2))
    }

    /// The place in `row` at the squared chord `squared` from the query. The chord c between unit
    /// vectors subtends the angle 2·asin(c/2).
    fn answer(&'static self, row: u32, squared: f64) -> Nearest {
        Nearest {
            place: &self.places[row as usize],
            distance_km: 2.0 * EARTH_RADIUS_KM * (squared.sqrt() / 2.0).min(1.0).asin(),
        }
    }

    fn most_populous_within(
        &'static self,
        query: &[f64; 3],
        bound2: f64,
        matches: &impl Fn(&Place) -> bool,
    ) -> Option<Nearest> {
        let mut best = None;
        self.gather(&self.tree, 0, query, bound2, matches, &mut best);
        let (row, squared) = best?;
        Some(self.answer(row, squared))
    }

    /// Visits the subtree in `nodes` for the most populous accepted place within the squared chord
    /// `bound2`: every node the query's side of a splitting plane holds, and the other side only
    /// when the plane is within the bound.
    fn gather(
        &self,
        nodes: &[Node],
        depth: usize,
        query: &[f64; 3],
        bound2: f64,
        matches: &impl Fn(&Place) -> bool,
        best: &mut Option<(u32, f64)>,
    ) {
        if nodes.is_empty() {
            return;
        }
        let middle = nodes.len() / 2;
        let node = &nodes[middle];
        let squared = squared_chord(&node.point, query);
        // More people, else nearer, else the earlier row.
        let larger = |held: (u32, f64)| {
            let (this, that) = (
                self.places[node.place as usize].population,
                self.places[held.0 as usize].population,
            );
            this > that || (this == that && (squared, node.place) < (held.1, held.0))
        };
        if squared <= bound2
            && best.is_none_or(larger)
            && matches(&self.places[node.place as usize])
        {
            *best = Some((node.place, squared));
        }
        let axis = depth % 3;
        let offset = query[axis] - node.point[axis];
        let (near, far) = if offset < 0.0 {
            (&nodes[..middle], &nodes[middle + 1..])
        } else {
            (&nodes[middle + 1..], &nodes[..middle])
        };
        self.gather(near, depth + 1, query, bound2, matches, best);
        if offset * offset <= bound2 {
            self.gather(far, depth + 1, query, bound2, matches, best);
        }
    }

    /// Visits the subtree in `nodes`: the node, then the side of its splitting plane the query is
    /// on, then the other side only when the plane is no farther than the best so far (equal
    /// distances are visited so that a tie is broken by rank, not by tree shape). A node the rule
    /// refuses is passed through like any other but never becomes the best; until one is accepted
    /// nothing is pruned.
    fn search(
        &self,
        nodes: &[Node],
        depth: usize,
        query: &[f64; 3],
        matches: &impl Fn(&Place) -> bool,
        best: &mut Best,
    ) {
        if nodes.is_empty() {
            return;
        }
        let middle = nodes.len() / 2;
        let node = &nodes[middle];
        let squared = squared_chord(&node.point, query);
        let closer = squared < best.distance2
            || (squared == best.distance2
                && best
                    .place
                    .is_some_and(|held| self.outranks(node.place, held)));
        if closer && matches(&self.places[node.place as usize]) {
            *best = Best {
                distance2: squared,
                place: Some(node.place),
            };
        }
        let axis = depth % 3;
        let offset = query[axis] - node.point[axis];
        let (near, far) = if offset < 0.0 {
            (&nodes[..middle], &nodes[middle + 1..])
        } else {
            (&nodes[middle + 1..], &nodes[..middle])
        };
        self.search(near, depth + 1, query, matches, best);
        if offset * offset <= best.distance2 {
            self.search(far, depth + 1, query, matches, best);
        }
    }
}

/// Arranges `nodes` as a subtree: the median on this depth's axis in the middle, no greater on its
/// left and no smaller on its right, each half arranged the same way one level down.
fn build(nodes: &mut [Node], depth: usize) {
    if nodes.len() < 2 {
        return;
    }
    let axis = depth % 3;
    let middle = nodes.len() / 2;
    nodes.select_nth_unstable_by(middle, |a, b| a.point[axis].total_cmp(&b.point[axis]));
    let (left, rest) = nodes.split_at_mut(middle);
    build(left, depth + 1);
    build(&mut rest[1..], depth + 1);
}

/// The places of a table in the asset's layout, or the first line that breaks it. The asset test
/// runs this over the bundled table, so a malformed asset never reaches a query.
fn parse(text: &'static str) -> Result<Vec<Place>, String> {
    let mut lines = text.lines();
    if lines.next() != Some(HEADER) {
        return Err("the first line is not the expected header".to_owned());
    }
    let mut places = Vec::with_capacity(text.len() / 42);
    for (number, line) in lines.enumerate() {
        let place = row(line).map_err(|why| format!("line {}: {why}", number + 2))?;
        places.push(place);
    }
    if places.is_empty() {
        return Err("the table has no places".to_owned());
    }
    if u32::try_from(places.len()).is_err() {
        return Err("the table has more places than the index numbers".to_owned());
    }
    Ok(places)
}

fn row(line: &'static str) -> Result<Place, &'static str> {
    let mut fields = line.split('\t');
    let (
        Some(name),
        Some(country),
        Some(feature),
        Some(latitude),
        Some(longitude),
        Some(population),
        None,
    ) = (
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
    )
    else {
        return Err("expected six tab-separated fields");
    };
    if name.is_empty() {
        return Err("the name is empty");
    }
    if country.len() != 2 || !country.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err("the country is not two capital letters");
    }
    if feature.is_empty()
        || feature.len() > MAX_FEATURE_CHARS
        || !feature
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        return Err("the feature code is not capital letters and digits");
    }
    let latitude: f64 = latitude
        .parse()
        .map_err(|_| "the latitude is not a number")?;
    let longitude: f64 = longitude
        .parse()
        .map_err(|_| "the longitude is not a number")?;
    if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
        return Err("the position is out of range");
    }
    Ok(Place {
        name,
        country,
        feature,
        latitude,
        longitude,
        population: population
            .parse()
            .map_err(|_| "the population is not a whole number")?,
    })
}

#[cfg(test)]
mod tests;

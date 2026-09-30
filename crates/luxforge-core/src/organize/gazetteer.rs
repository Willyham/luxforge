//! The offline gazetteer: the populated place nearest to a position, from a table compiled into the
//! binary.
//!
//! **The data.** `assets/gazetteer/places.tsv` is derived by `cargo xtask gazetteer` from GeoNames'
//! `cities15000` extract (every populated place with more than 15,000 inhabitants, and every
//! capital), which is licensed CC BY 4.0. It keeps five of GeoNames' columns (name, ISO 3166-1
//! country code, latitude and longitude in WGS84 degrees, population) and sorts the rows. The
//! source, hashes, licence and the changes made are in `crates/luxforge-core/THIRD_PARTY.md`.
//! Section rows (GeoNames feature code PPLX, a neighbourhood of a larger place) are included, so
//! the nearest place to a city centre can be one of its districts.
//!
//! **The query.** [`nearest`] answers the place at the smallest great-circle distance from a
//! position, and that distance; it reads no file and makes no request, ever. What counts as too far
//! to name anything is the caller's decision, so the distance is always returned.
//!
//! **The index.** The table is parsed and indexed once, on the first query (never at startup), into
//! a k-d tree over the places' unit vectors in 3-D. The chord between two unit vectors grows with
//! the great-circle distance between them, so the nearest by chord is the nearest on the sphere
//! with no special case at the antimeridian or the poles.
//!
//! **Bounds.** The place count is fixed by the compiled-in asset (about 34,000 rows, 1.3 MB of
//! text). The index holds the parsed rows (56 bytes each; the names borrow from the asset) and one
//! 32-byte tree node per place, about 3 MB of heap, and never grows. The first query parses the
//! table and builds the tree in under 10 ms in a release build; every later one allocates nothing
//! and visits about 30 nodes near a place (about 190 in open ocean) in a microsecond or less.

// The organizer that consumes the lookup is a later task; drop this when it does.
#![allow(dead_code)]

use std::sync::OnceLock;

/// The bundled table: a header line, then one place per line with tab-separated fields in the
/// header's order. The header doubles as the format marker: a different one is refused.
const ASSET: &str = include_str!("../../assets/gazetteer/places.tsv");
const HEADER: &str = "name\tcountry\tlatitude\tlongitude\tpopulation";
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
    /// Degrees north, -90 to 90.
    pub latitude: f64,
    /// Degrees east, -180 to 180.
    pub longitude: f64,
    /// Inhabitants as GeoNames records them; 0 for a capital it has no count for.
    pub population: u32,
}

/// The answer to [`nearest`].
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
    if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
        return None;
    }
    Some(index().nearest(&unit_vector(latitude, longitude)))
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
    distance2: f64,
    place: u32,
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

    fn nearest(&'static self, query: &[f64; 3]) -> Nearest {
        let mut best = Best {
            distance2: f64::INFINITY,
            place: 0,
        };
        self.search(&self.tree, 0, query, &mut best);
        // The chord c between unit vectors subtends the angle 2·asin(c/2).
        let chord = best.distance2.sqrt();
        Nearest {
            place: &self.places[best.place as usize],
            distance_km: 2.0 * EARTH_RADIUS_KM * (chord / 2.0).min(1.0).asin(),
        }
    }

    /// Visits the subtree in `nodes`: the node, then the side of its splitting plane the query is
    /// on, then the other side only when the plane is no farther than the best so far (equal
    /// distances are visited so that a tie is broken by rank, not by tree shape).
    fn search(&self, nodes: &[Node], depth: usize, query: &[f64; 3], best: &mut Best) {
        if nodes.is_empty() {
            return;
        }
        let middle = nodes.len() / 2;
        let node = &nodes[middle];
        let squared = squared_chord(&node.point, query);
        if squared < best.distance2
            || (squared == best.distance2 && self.outranks(node.place, best.place))
        {
            *best = Best {
                distance2: squared,
                place: node.place,
            };
        }
        let axis = depth % 3;
        let offset = query[axis] - node.point[axis];
        let (near, far) = if offset < 0.0 {
            (&nodes[..middle], &nodes[middle + 1..])
        } else {
            (&nodes[middle + 1..], &nodes[..middle])
        };
        self.search(near, depth + 1, query, best);
        if offset * offset <= best.distance2 {
            self.search(far, depth + 1, query, best);
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
    let mut places = Vec::with_capacity(text.len() / 36);
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
    let (Some(name), Some(country), Some(latitude), Some(longitude), Some(population), None) = (
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
    ) else {
        return Err("expected five tab-separated fields");
    };
    if name.is_empty() {
        return Err("the name is empty");
    }
    if country.len() != 2 || !country.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err("the country is not two capital letters");
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
        latitude,
        longitude,
        population: population
            .parse()
            .map_err(|_| "the population is not a whole number")?,
    })
}

#[cfg(test)]
mod tests;

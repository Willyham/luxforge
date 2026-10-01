//! Curated product names for fixed-lens cameras whose EXIF and Lensfun records name only a camera
//! code, so a drone reads "DJI Air 2S (FC3411)" rather than "DJI FC3411". Display and issue
//! reports only: matching never reads this table.
use super::{index::IndexCamera, resolve::normalize};

/// `(EXIF Make, EXIF Model, product)`. Mavic 2 Pro and Mavic 3 photographs carry a Hasselblad make,
/// so their codes are listed under both makes; a code names one camera whichever make it carries.
const PRODUCTS: &[(&str, &str, &str)] = &[
    ("DJI", "FC3411", "DJI Air 2S"),
    ("DJI", "FC6310", "DJI Phantom 4 Pro"),
    ("DJI", "FC3170", "DJI Mavic Air 2"),
    ("DJI", "FC7203", "DJI Mavic Mini"),
    ("DJI", "FC7303", "DJI Mini 2"),
    ("DJI", "FC3582", "DJI Mini 3 Pro"),
    ("DJI", "FC8482", "DJI Mini 4 Pro"),
    ("DJI", "L1D-20c", "DJI Mavic 2 Pro"),
    ("Hasselblad", "L1D-20c", "DJI Mavic 2 Pro"),
    ("DJI", "FC2204", "DJI Mavic 2 Zoom"),
    ("DJI", "L2D-20c", "DJI Mavic 3"),
    ("Hasselblad", "L2D-20c", "DJI Mavic 3"),
    ("DJI", "FC2103", "DJI Mavic Air"),
    ("DJI", "FC220", "DJI Mavic Pro"),
    ("DJI", "FC300X", "DJI Phantom 3 Professional"),
    ("DJI", "FC200", "DJI Phantom 2 Vision"),
];

/// The curated product name for a normalized EXIF make and model, if the table lists it.
pub(crate) fn product(make: Option<&str>, model: Option<&str>) -> Option<&'static str> {
    let (make, model) = (normalize(make?), normalize(model?));
    PRODUCTS
        .iter()
        .find(|(m, code, _)| normalize(m) == make && normalize(code) == model)
        .map(|(_, _, product)| *product)
}

/// `maker model` without repeating a maker the model already starts with ("Nikon Corporation" and
/// "Nikon Z 6" read "Nikon Z 6"; "Sony" and "ILCE-7RM4" read "Sony ILCE-7RM4").
fn joined(maker: &str, model: &str) -> String {
    let (maker, model) = (maker.trim(), model.trim());
    let brand = maker.split_whitespace().next().unwrap_or_default();
    if maker.is_empty()
        || model
            .split_whitespace()
            .next()
            .is_some_and(|first| normalize(first) == normalize(brand))
    {
        model.to_owned()
    } else if model.is_empty() {
        maker.to_owned()
    } else {
        format!("{maker} {model}")
    }
}

/// How the panel and an issue report name a camera: the curated product with its EXIF code, else
/// the database record's own name, else the photo's EXIF make and model.
pub(crate) fn camera_name(
    make: Option<&str>,
    model: Option<&str>,
    record: Option<&IndexCamera>,
) -> String {
    if let Some(product) = product(make, model) {
        return format!("{product} ({})", model.unwrap_or_default().trim());
    }
    match (record, make, model) {
        (Some(record), _, _) => joined(&record.maker, &record.model),
        (None, make, model) if make.is_some() || model.is_some() => {
            joined(make.unwrap_or_default(), model.unwrap_or_default())
        }
        _ => "This camera".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(maker: &str, model: &str) -> IndexCamera {
        IndexCamera {
            maker: maker.into(),
            model: model.into(),
            mount: "m".into(),
            crop_factor: 1.0,
        }
    }

    #[test]
    fn curated_drone_names_read_product_and_code_and_never_guess() {
        assert_eq!(product(Some("DJI"), Some("FC3411")), Some("DJI Air 2S"));
        assert_eq!(
            product(Some(" dji "), Some("fc3411")),
            Some("DJI Air 2S"),
            "normalized like Lensfun's comparison"
        );
        assert_eq!(
            product(Some("Hasselblad"), Some("L1D-20c")),
            Some("DJI Mavic 2 Pro")
        );
        assert_eq!(product(Some("DJI"), Some("FC9999")), None);
        assert_eq!(product(Some("NIKON CORPORATION"), Some("FC3411")), None);
        assert_eq!(product(None, Some("FC3411")), None);
        assert_eq!(
            camera_name(Some("DJI"), Some("FC3411"), Some(&record("DJI", "FC3411"))),
            "DJI Air 2S (FC3411)"
        );
        // Every listed code is unique within its make.
        for (i, (make, code, _)) in PRODUCTS.iter().enumerate() {
            assert!(PRODUCTS[i + 1..].iter().all(
                |(m, c, _)| normalize(m) != normalize(make) || normalize(c) != normalize(code)
            ));
        }
    }

    #[test]
    fn camera_names_fall_back_to_the_record_then_exif_without_repeating_the_maker() {
        assert_eq!(
            camera_name(
                Some("NIKON CORPORATION"),
                Some("NIKON Z 6"),
                Some(&record("Nikon Corporation", "Nikon Z 6"))
            ),
            "Nikon Z 6"
        );
        assert_eq!(
            camera_name(
                Some("SONY"),
                Some("ILCE-7RM4"),
                Some(&record("Sony", "ILCE-7RM4"))
            ),
            "Sony ILCE-7RM4"
        );
        assert_eq!(
            camera_name(Some("FUJIFILM"), Some("X100VI"), None),
            "FUJIFILM X100VI"
        );
        assert_eq!(camera_name(None, None, None), "This camera");
    }
}

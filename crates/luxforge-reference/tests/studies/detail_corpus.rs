//! The photographic matrix states missing coverage explicitly rather than treating admission as quality.
use serde_json::Value;

#[test]
fn detail_corpus_manifest_is_complete_or_records_gaps() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/detail/corpus.json");
    let corpus: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let sources = corpus["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 4);
    let scenes = [
        "skin",
        "hair-or-fur",
        "foliage-or-fabric",
        "skies-and-shadows",
        "coloured-edges",
        "repeating-texture",
    ];
    for source in sources {
        let hash = source["sha256"].as_str().unwrap();
        assert_eq!(hash.len(), 64);
        assert!(
            hash.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        );
        assert!(!source["provenance"].as_str().unwrap().is_empty());
        if source["iso"].is_null() {
            assert!(!source["metadata_gap"].as_str().unwrap().is_empty());
        }
        let size = source["dimensions"].as_array().unwrap();
        let (w, h) = (size[0].as_u64().unwrap(), size[1].as_u64().unwrap());
        let crops = source["inspection_crops"].as_array().unwrap();
        assert!((2..=4).contains(&crops.len()));
        for crop in crops {
            let crop: Vec<u64> = crop
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap())
                .collect();
            assert_eq!(crop.len(), 4);
            assert!(crop[2] > 0 && crop[3] > 0 && crop[0] + crop[2] <= w && crop[1] + crop[3] <= h);
        }
        for iso in ["low", "high"] {
            for scene in scenes {
                let cells: Vec<_> = corpus["required_cells"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|cell| {
                        cell["source"] == source["manifest_key"]
                            && cell["iso_class"] == iso
                            && cell["scene"] == scene
                    })
                    .collect();
                assert_eq!(cells.len(), 1);
                assert!(
                    !cells[0]["gap"].as_str().unwrap().is_empty() || !cells[0]["capture"].is_null()
                );
            }
        }
    }
}

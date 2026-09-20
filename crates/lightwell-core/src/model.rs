use crate::{Error, ErrorKind, modules::CropPayload};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::{Value, json};
use std::collections::HashSet;

pub const RECIPE_FORMAT: u32 = 1;
pub const PIXEL_EFFECT: &str = "lightwell.pixel.replace";
pub const TRANSFORM_EFFECT: &str = "lightwell.geometry.transform";
pub const CROP_EFFECT: &str = "lightwell.geometry.crop";
pub const EFFECT_FORMAT: u32 = 1;

fn valid_id(value: &str, prefix: &str) -> bool {
    value.len() > prefix.len() + 8
        && value.len() <= 96
        && value.starts_with(prefix)
        && value[prefix.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

macro_rules! identifier {
    ($name:ident, $prefix:literal) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);
        impl $name {
            pub fn new() -> Self {
                Self(format!(
                    concat!($prefix, "{}"),
                    uuid::Uuid::new_v4().simple()
                ))
            }
            pub fn parse(value: impl Into<String>) -> Result<Self, Error> {
                let value = value.into();
                if valid_id(&value, $prefix) {
                    Ok(Self(value))
                } else {
                    Err(Error::new(
                        ErrorKind::Validation,
                        concat!("invalid ", stringify!($name)),
                    ))
                }
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Self::parse(value).map_err(de::Error::custom)
            }
        }
    };
}

identifier!(AssetId, "asset-");
identifier!(LayerId, "layer-");
identifier!(SnapshotId, "snapshot-");
identifier!(EntryId, "entry-");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layer {
    pub id: LayerId,
    pub effect_id: String,
    pub effect_format: u32,
    pub payload: Value,
}

impl Layer {
    pub fn pixel(x: u32, y: u32, rgb: [u8; 3]) -> Self {
        Self {
            id: LayerId::new(),
            effect_id: PIXEL_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"x": x, "y": y, "rgb": rgb}),
        }
    }
    pub fn transform(transform: Transform) -> Self {
        Self {
            id: LayerId::new(),
            effect_id: TRANSFORM_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: serde_json::to_value(transform).expect("transform is serializable"),
        }
    }
    /// The one crop layer of a stack: straightening and a rectangle over its own input stage.
    pub fn crop(payload: CropPayload) -> Self {
        Self {
            id: LayerId::new(),
            effect_id: CROP_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: serde_json::to_value(payload).expect("crop payload is serializable"),
        }
    }
    /// Structural only: effect availability and payload shape belong to the providing module,
    /// reached through [`crate::ModuleRegistry`].
    pub fn validate(&self) -> Result<(), Error> {
        if self.effect_id.is_empty() {
            return Err(Error::new(
                ErrorKind::Validation,
                "layer has no effect identity",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PixelReplace {
    pub x: u32,
    pub y: u32,
    pub rgb: [u8; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transform {
    RotateLeft,
    RotateRight,
    MirrorHorizontal,
    FlipVertical,
}

impl Transform {
    pub fn action_id(self) -> &'static str {
        match self {
            Self::RotateLeft => "rotate-left",
            Self::RotateRight => "rotate-right",
            Self::MirrorHorizontal => "mirror-horizontal",
            Self::FlipVertical => "flip-vertical",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub format: u32,
    pub layers: Vec<Layer>,
}

impl Default for Recipe {
    fn default() -> Self {
        Self {
            format: RECIPE_FORMAT,
            layers: Vec::new(),
        }
    }
}

impl Recipe {
    pub fn validate(&self) -> Result<(), Error> {
        if self.format != RECIPE_FORMAT {
            return Err(Error::new(
                ErrorKind::Incompatible,
                format!("unsupported recipe format {}", self.format),
            ));
        }
        let mut ids = HashSet::with_capacity(self.layers.len());
        for layer in &self.layers {
            if !ids.insert(&layer.id) {
                return Err(Error::new(
                    ErrorKind::Validation,
                    "duplicate layer identity",
                ));
            }
            layer.validate()?;
        }
        Ok(())
    }
    pub fn appended(&self, layer: Layer) -> Result<Self, Error> {
        layer.validate()?;
        let mut next = self.clone();
        next.layers.push(layer);
        next.validate()?;
        Ok(next)
    }
    /// Replace the layer with the same identity in place, keeping every other layer and every
    /// position. An identity that is not in this recipe is a validation error.
    pub fn with_layer_replaced(&self, layer: Layer) -> Result<Self, Error> {
        layer.validate()?;
        let position = self
            .layers
            .iter()
            .position(|existing| existing.id == layer.id)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Validation,
                    "plan updates a layer that is not in the stack",
                )
            })?;
        let mut next = self.clone();
        next.layers[position] = layer;
        next.validate()?;
        Ok(next)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub id: SnapshotId,
    pub asset_id: AssetId,
    pub recipe: Recipe,
}

impl Snapshot {
    pub fn original(asset_id: AssetId) -> Self {
        Self {
            id: SnapshotId::new(),
            asset_id,
            recipe: Recipe::default(),
        }
    }
    pub fn append(&self, layer: Layer) -> Result<Self, Error> {
        Ok(Self {
            id: SnapshotId::new(),
            asset_id: self.asset_id.clone(),
            recipe: self.recipe.appended(layer)?,
        })
    }
    /// A new snapshot whose stack differs only in the layer with this identity. Earlier snapshots
    /// keep their own recipe, so history stays immutable.
    pub fn with_layer_replaced(&self, layer: Layer) -> Result<Self, Error> {
        Ok(Self {
            id: SnapshotId::new(),
            asset_id: self.asset_id.clone(),
            recipe: self.recipe.with_layer_replaced(layer)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryEntry {
    pub id: EntryId,
    pub asset_id: AssetId,
    pub sequence: u64,
    pub action_id: String,
    /// The one-line label history rows show, rendered by the host when the entry was committed
    /// from the requested action's summary template, or its title. Stored with the entry so a row
    /// reads the same however the providers change.
    pub label: String,
    pub parameters: Value,
    pub actor: String,
    pub timestamp_ms: i64,
    pub request_id: Option<String>,
    pub base_revision: u64,
    pub result_revision: u64,
    pub snapshot: Snapshot,
    pub undo_parent: Option<EntryId>,
    pub restore_target: Option<EntryId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mutation {
    pub expected_revision: u64,
    pub request_id: String,
    pub actor: String,
}

impl Mutation {
    pub fn validate(&self) -> Result<(), Error> {
        if self.request_id.is_empty() || self.request_id.len() > 128 {
            return Err(Error::new(
                ErrorKind::Validation,
                "request_id must contain 1..128 characters",
            ));
        }
        if self.actor.is_empty() || self.actor.len() > 128 {
            return Err(Error::new(
                ErrorKind::Validation,
                "actor must contain 1..128 characters",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_are_complete_immutable_and_round_trip() {
        let original = Snapshot::original(AssetId::new());
        let a = original.append(Layer::pixel(1, 2, [3, 4, 5])).unwrap();
        let b = a.append(Layer::pixel(1, 2, [6, 7, 8])).unwrap();
        assert!(original.recipe.layers.is_empty());
        assert_eq!(a.recipe.layers.len(), 1);
        assert_eq!(b.recipe.layers.len(), 2);
        let encoded = serde_json::to_vec(&b).unwrap();
        assert_eq!(serde_json::from_slice::<Snapshot>(&encoded).unwrap(), b);
    }

    #[test]
    fn invalid_identity_structure_and_duplicates_are_rejected() {
        assert!(serde_json::from_str::<AssetId>("\"bad\"").is_err());
        let mut recipe = Recipe::default();
        let layer = Layer::pixel(0, 0, [1, 2, 3]);
        recipe.layers.extend([layer.clone(), layer]);
        assert!(recipe.validate().is_err());
        let unsupported = Recipe {
            format: 99,
            layers: Vec::new(),
        };
        assert_eq!(
            unsupported.validate().unwrap_err().kind,
            ErrorKind::Incompatible
        );
        let nameless = Layer {
            id: LayerId::new(),
            effect_id: String::new(),
            effect_format: EFFECT_FORMAT,
            payload: json!({}),
        };
        assert_eq!(nameless.validate().unwrap_err().kind, ErrorKind::Validation);
        // Payload shape and effect format are the providing module's business, not the model's.
        assert!(
            Layer {
                effect_format: 99,
                payload: json!({"x": 1}),
                ..Layer::pixel(0, 0, [1, 2, 3])
            }
            .validate()
            .is_ok()
        );
    }
}

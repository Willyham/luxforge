//! Named tensors, the values a [`super::Session`] takes and answers.
use crate::Error;
use serde::{Deserialize, Serialize};

/// The most bytes one tensor may hold: 1 GiB, far past any input or output of the pinned models
/// (EfficientViT-SAM's embedding is about 16 MiB), so a malformed shape is refused before it is
/// allocated rather than after.
pub const MAX_TENSOR_BYTES: u64 = 1 << 30;

/// What one element of a tensor is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ElementType {
    F32,
    /// IEEE half floats, held as their bits.
    F16,
    I64,
    I32,
    U8,
    Bool,
}

impl ElementType {
    pub fn bytes(self) -> u64 {
        match self {
            Self::F32 | Self::I32 => 4,
            Self::F16 => 2,
            Self::I64 => 8,
            Self::U8 | Self::Bool => 1,
        }
    }
}

/// One input or output a model declares: its name, element type and shape, `None` for a
/// dimension the model leaves free.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TensorSpec {
    pub name: String,
    pub element: ElementType,
    pub shape: Vec<Option<u64>>,
}

impl TensorSpec {
    /// Whether `tensor` is one this spec admits: the same element type and rank, and every fixed
    /// dimension equal.
    pub fn admits(&self, tensor: &Tensor) -> bool {
        tensor.data.element() == self.element
            && tensor.shape.len() == self.shape.len()
            && self
                .shape
                .iter()
                .zip(&tensor.shape)
                .all(|(fixed, actual)| fixed.is_none_or(|fixed| fixed == *actual))
    }
}

/// A tensor's elements, row-major.
#[derive(Clone, Debug, PartialEq)]
pub enum TensorData {
    F32(Vec<f32>),
    F16(Vec<u16>),
    I64(Vec<i64>),
    I32(Vec<i32>),
    U8(Vec<u8>),
    Bool(Vec<bool>),
}

impl TensorData {
    pub fn element(&self) -> ElementType {
        match self {
            Self::F32(_) => ElementType::F32,
            Self::F16(_) => ElementType::F16,
            Self::I64(_) => ElementType::I64,
            Self::I32(_) => ElementType::I32,
            Self::U8(_) => ElementType::U8,
            Self::Bool(_) => ElementType::Bool,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::F32(values) => values.len(),
            Self::F16(values) => values.len(),
            Self::I64(values) => values.len(),
            Self::I32(values) => values.len(),
            Self::U8(values) => values.len(),
            Self::Bool(values) => values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One named tensor: a shape and exactly as many elements as the shape holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Tensor {
    name: String,
    shape: Vec<u64>,
    data: TensorData,
}

impl Tensor {
    /// A tensor named `name` of `shape` holding `data`, refused with `validation` when the element
    /// count is not the shape's product and with `resource-limit` past [`MAX_TENSOR_BYTES`].
    pub fn new(name: impl Into<String>, shape: Vec<u64>, data: TensorData) -> Result<Self, Error> {
        let name = name.into();
        let count = shape
            .iter()
            .try_fold(1_u64, |count, side| count.checked_mul(*side))
            .ok_or_else(|| Error::resource_limit(format!("tensor {name}'s shape overflows")))?;
        if count.saturating_mul(data.element().bytes()) > MAX_TENSOR_BYTES {
            return Err(Error::resource_limit(format!(
                "tensor {name} of shape {shape:?} exceeds {MAX_TENSOR_BYTES} bytes"
            )));
        }
        if count != data.len() as u64 {
            return Err(Error::validation(format!(
                "tensor {name} of shape {shape:?} holds {count} elements, not {}",
                data.len()
            )));
        }
        Ok(Self { name, shape, data })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn shape(&self) -> &[u64] {
        &self.shape
    }

    pub fn data(&self) -> &TensorData {
        &self.data
    }

    pub fn into_data(self) -> TensorData {
        self.data
    }

    /// The elements when they are `f32`.
    pub fn f32(&self) -> Option<&[f32]> {
        match &self.data {
            TensorData::F32(values) => Some(values),
            _ => None,
        }
    }
}

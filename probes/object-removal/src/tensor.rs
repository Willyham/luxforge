//! A backend-neutral tensor: a shape and a flat, row-major buffer.

#[derive(Clone, Debug, PartialEq)]
pub enum Data {
    F32(Vec<f32>),
    I64(Vec<i64>),
    I32(Vec<i32>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tensor {
    pub shape: Vec<usize>,
    pub data: Data,
}

impl Tensor {
    pub fn f32(shape: impl Into<Vec<usize>>, data: Vec<f32>) -> Self {
        let shape = shape.into();
        assert_eq!(
            shape.iter().product::<usize>(),
            data.len(),
            "tensor shape and length disagree"
        );
        Self {
            shape,
            data: Data::F32(data),
        }
    }

    pub fn i64(shape: impl Into<Vec<usize>>, data: Vec<i64>) -> Self {
        let shape = shape.into();
        assert_eq!(
            shape.iter().product::<usize>(),
            data.len(),
            "tensor shape and length disagree"
        );
        Self {
            shape,
            data: Data::I64(data),
        }
    }

    pub fn i32(shape: impl Into<Vec<usize>>, data: Vec<i32>) -> Self {
        let shape = shape.into();
        assert_eq!(
            shape.iter().product::<usize>(),
            data.len(),
            "tensor shape and length disagree"
        );
        Self {
            shape,
            data: Data::I32(data),
        }
    }

    pub fn len(&self) -> usize {
        self.shape.iter().product()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The f32 buffer, or an error naming the element type the tensor holds.
    pub fn as_f32(&self) -> Result<&[f32], String> {
        match &self.data {
            Data::F32(v) => Ok(v),
            other => Err(format!("expected an f32 tensor, found {}", other.kind())),
        }
    }

    pub fn into_f32(self) -> Result<Vec<f32>, String> {
        match self.data {
            Data::F32(v) => Ok(v),
            other => Err(format!("expected an f32 tensor, found {}", other.kind())),
        }
    }
}

impl Data {
    pub fn kind(&self) -> &'static str {
        match self {
            Data::F32(_) => "f32",
            Data::I64(_) => "i64",
            Data::I32(_) => "i32",
        }
    }
}

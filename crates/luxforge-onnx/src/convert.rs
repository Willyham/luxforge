//! The port's tensors and specs to ONNX Runtime's values and back.
use luxforge_core::{
    Error,
    inference::{ElementType, MAX_TENSOR_BYTES, Tensor, TensorData, TensorSpec},
};
use ort::{
    session::SessionInputValue,
    value::{DynValue, Outlet, TensorElementType, ValueType},
};

/// The port's element type of an ONNX Runtime one, or `None` for one the port does not carry.
pub(crate) fn element(ty: TensorElementType) -> Option<ElementType> {
    Some(match ty {
        TensorElementType::Float32 => ElementType::F32,
        TensorElementType::Float16 => ElementType::F16,
        TensorElementType::Int64 => ElementType::I64,
        TensorElementType::Int32 => ElementType::I32,
        TensorElementType::Uint8 => ElementType::U8,
        TensorElementType::Bool => ElementType::Bool,
        _ => return None,
    })
}

/// The spec of one declared input or output, refused `incompatible` when it is not a tensor of an
/// element type the port carries. A free dimension, which ONNX Runtime reports as `-1`, is `None`.
pub(crate) fn spec(outlet: &Outlet) -> Result<TensorSpec, Error> {
    let ValueType::Tensor { ty, shape, .. } = outlet.dtype() else {
        return Err(Error::incompatible(format!(
            "model value {} is a {}, not a tensor",
            outlet.name(),
            outlet.dtype()
        )));
    };
    let element = element(*ty).ok_or_else(|| {
        Error::incompatible(format!(
            "model tensor {} holds {ty}, which the inference port does not carry",
            outlet.name()
        ))
    })?;
    Ok(TensorSpec {
        name: outlet.name().to_owned(),
        element,
        shape: shape.iter().map(|&side| u64::try_from(side).ok()).collect(),
    })
}

/// The specs of every declared input or output.
pub(crate) fn specs(outlets: &[Outlet]) -> Result<Vec<TensorSpec>, Error> {
    outlets.iter().map(spec).collect()
}

fn shape(tensor: &Tensor) -> Result<Vec<i64>, Error> {
    tensor
        .shape()
        .iter()
        .map(|&side| {
            i64::try_from(side).map_err(|_| {
                Error::resource_limit(format!("tensor {}'s shape overflows", tensor.name()))
            })
        })
        .collect()
}

/// `tensor` as an input ONNX Runtime reads, borrowing its elements where their layout is the
/// runtime's (every type but `f16`, whose bits are copied into half floats).
pub(crate) fn input(tensor: &Tensor) -> Result<SessionInputValue<'_>, Error> {
    let shape = shape(tensor)?;
    let failed = |error: ort::Error| {
        Error::validation(format!(
            "tensor {} could not be handed to ONNX Runtime: {}",
            tensor.name(),
            error.message()
        ))
    };
    use ort::value::{Tensor as OrtTensor, TensorRef};
    Ok(match tensor.data() {
        TensorData::F32(values) => TensorRef::from_array_view((shape, values.as_slice()))
            .map_err(failed)?
            .into(),
        TensorData::I64(values) => TensorRef::from_array_view((shape, values.as_slice()))
            .map_err(failed)?
            .into(),
        TensorData::I32(values) => TensorRef::from_array_view((shape, values.as_slice()))
            .map_err(failed)?
            .into(),
        TensorData::U8(values) => TensorRef::from_array_view((shape, values.as_slice()))
            .map_err(failed)?
            .into(),
        TensorData::Bool(values) => TensorRef::from_array_view((shape, values.as_slice()))
            .map_err(failed)?
            .into(),
        TensorData::F16(bits) => OrtTensor::from_array((
            shape,
            bits.iter()
                .map(|&bits| half::f16::from_bits(bits))
                .collect::<Vec<_>>(),
        ))
        .map_err(failed)?
        .into(),
    })
}

/// The output `name` ONNX Runtime answered, as a port tensor.
pub(crate) fn output(name: &str, value: &DynValue) -> Result<Tensor, Error> {
    let ValueType::Tensor { ty, shape, .. } = value.dtype() else {
        return Err(Error::incompatible(format!(
            "model output {name} is a {}, not a tensor",
            value.dtype()
        )));
    };
    // Refused before its elements are copied out of the runtime.
    let bytes = element(*ty).map_or(1, ElementType::bytes);
    let count = shape.iter().try_fold(1_u64, |count, &side| {
        u64::try_from(side)
            .ok()
            .and_then(|side| count.checked_mul(side))
    });
    if count.is_none_or(|count| count.saturating_mul(bytes) > MAX_TENSOR_BYTES) {
        return Err(Error::resource_limit(format!(
            "model output {name} of shape {:?} exceeds {MAX_TENSOR_BYTES} bytes",
            &shape[..]
        )));
    }
    let failed = |error: ort::Error| {
        Error::render(format!(
            "output {name} could not be read from ONNX Runtime: {}",
            error.message()
        ))
    };
    let dims = |shape: &ort::value::Shape| -> Result<Vec<u64>, Error> {
        shape
            .iter()
            .map(|&side| {
                u64::try_from(side)
                    .map_err(|_| Error::render(format!("output {name} has a negative dimension")))
            })
            .collect()
    };
    let (shape, data) = match ty {
        TensorElementType::Float32 => {
            let (shape, data) = value.try_extract_tensor::<f32>().map_err(failed)?;
            (dims(shape)?, TensorData::F32(data.to_vec()))
        }
        TensorElementType::Float16 => {
            let (shape, data) = value.try_extract_tensor::<half::f16>().map_err(failed)?;
            (
                dims(shape)?,
                TensorData::F16(data.iter().map(|value| value.to_bits()).collect()),
            )
        }
        TensorElementType::Int64 => {
            let (shape, data) = value.try_extract_tensor::<i64>().map_err(failed)?;
            (dims(shape)?, TensorData::I64(data.to_vec()))
        }
        TensorElementType::Int32 => {
            let (shape, data) = value.try_extract_tensor::<i32>().map_err(failed)?;
            (dims(shape)?, TensorData::I32(data.to_vec()))
        }
        TensorElementType::Uint8 => {
            let (shape, data) = value.try_extract_tensor::<u8>().map_err(failed)?;
            (dims(shape)?, TensorData::U8(data.to_vec()))
        }
        TensorElementType::Bool => {
            let (shape, data) = value.try_extract_tensor::<bool>().map_err(failed)?;
            (dims(shape)?, TensorData::Bool(data.to_vec()))
        }
        other => {
            return Err(Error::incompatible(format!(
                "model output {name} holds {other}, which the inference port does not carry"
            )));
        }
    };
    Tensor::new(name, shape, data)
}

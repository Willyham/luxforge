//! Tiny ONNX models the tests write themselves, encoded here field by field in the ONNX protobuf
//! format, so no model file is checked in and no Python or `onnx` package is needed.
//!
//! [`affine`] is `y = x * scale + bias` over an `f32` input `x` of shape `[batch, 3]`, with
//! `scale` and `bias` constant vectors of three; `batch` is a named free dimension (or a fixed one,
//! or an unnamed free one, as the test asks).

/// A protobuf message under construction.
#[derive(Default)]
struct Message(Vec<u8>);

impl Message {
    fn varint(&mut self, mut value: u64) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                self.0.push(byte);
                return;
            }
            self.0.push(byte | 0x80);
        }
    }

    fn key(&mut self, field: u64, wire: u64) {
        self.varint(field << 3 | wire);
    }

    fn int(mut self, field: u64, value: u64) -> Self {
        self.key(field, 0);
        self.varint(value);
        self
    }

    fn bytes(mut self, field: u64, bytes: &[u8]) -> Self {
        self.key(field, 2);
        self.varint(bytes.len() as u64);
        self.0.extend_from_slice(bytes);
        self
    }

    fn text(self, field: u64, text: &str) -> Self {
        self.bytes(field, text.as_bytes())
    }

    fn message(self, field: u64, message: Message) -> Self {
        self.bytes(field, &message.0)
    }
}

/// One dimension of a declared shape.
#[derive(Clone, Copy)]
pub(crate) enum Side {
    Fixed(u64),
    Named(&'static str),
    /// Only the Core ML tests, on macOS, declare an unnamed free dimension.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Unnamed,
}

/// `ValueInfoProto` of an `f32` tensor named `name` of `shape`.
fn value_info(name: &str, shape: &[Side]) -> Message {
    let mut dims = Message::default();
    for side in shape {
        let dim = match side {
            Side::Fixed(value) => Message::default().int(1, *value),
            Side::Named(symbol) => Message::default().text(2, symbol),
            Side::Unnamed => Message::default(),
        };
        dims = dims.message(1, dim);
    }
    // TypeProto { tensor_type: Tensor { elem_type: FLOAT, shape } }
    let tensor = Message::default().int(1, 1).message(2, dims);
    Message::default()
        .text(1, name)
        .message(2, Message::default().message(1, tensor))
}

/// `TensorProto` of an `f32` vector named `name`.
fn initializer(name: &str, values: &[f32]) -> Message {
    let raw: Vec<u8> = values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    Message::default()
        .int(1, values.len() as u64)
        .int(2, 1)
        .text(8, name)
        .bytes(9, &raw)
}

fn node(op: &str, inputs: &[&str], output: &str, name: &str) -> Message {
    let mut node = Message::default();
    for input in inputs {
        node = node.text(1, input);
    }
    node.text(2, output).text(3, name).text(4, op)
}

/// The affine model's bytes, its batch dimension declared as `batch`.
pub(crate) fn affine(batch: Side, scale: [f32; 3], bias: [f32; 3]) -> Vec<u8> {
    let graph = Message::default()
        .message(1, node("Mul", &["x", "scale"], "scaled", "mul"))
        .message(1, node("Add", &["scaled", "bias"], "y", "add"))
        .text(2, "affine")
        .message(5, initializer("scale", &scale))
        .message(5, initializer("bias", &bias))
        .message(11, value_info("x", &[batch, Side::Fixed(3)]))
        .message(12, value_info("y", &[batch, Side::Fixed(3)]));
    Message::default()
        .int(1, 8)
        .text(2, "luxforge-onnx tests")
        .message(7, graph)
        .message(8, Message::default().text(1, "").int(2, 17))
        .0
}

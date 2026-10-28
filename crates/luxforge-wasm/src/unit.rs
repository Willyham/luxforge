//! A layer's compiled unit: the words a module's `coefficients` gave for the layer's values, and
//! the module whose `apply-row` runs them.
use crate::Adjustment;
use luxforge_core::module_package::{AdjustmentUnits, FieldEntry, ProgramState};
use luxforge_core::{
    ColorOperation, CompileStage, Error, GpuDescription, OperationIdentity, PointwiseColor,
};
use std::sync::Arc;

/// The kind every external unit's identity names; the package hash and the words tell them apart.
const IDENTITY: &str = "luxforge-wasm.adjust";

/// A loaded module as its manifest module's units.
pub(crate) struct Units(pub(crate) Arc<Adjustment>);

impl AdjustmentUnits for Units {
    /// One unit of the words the values give. The unit does not depend on the stage: `apply-row`
    /// is handed each run's coordinates in the stage the host evaluates it at.
    fn compile(&self, values: &[FieldEntry], _: CompileStage) -> Result<ColorOperation, Error> {
        let words = self.0.coefficients(values)?;
        Ok(ColorOperation::new(vec![Arc::new(Unit {
            module: self.0.clone(),
            words: words.into(),
        })]))
    }

    fn program_state(&self) -> ProgramState {
        self.0.program_state()
    }
}

/// One layer's colour unit in an external module: its words and its module.
pub struct Unit {
    module: Arc<Adjustment>,
    words: Arc<[u32]>,
}

impl Unit {
    /// The words `coefficients` answered for the layer.
    pub fn words(&self) -> &[u32] {
        &self.words
    }
}

impl PointwiseColor for Unit {
    /// The row through `apply-row`; a failure leaves every value not a number, which the host's
    /// finite check refuses, so even a caller that cannot see the error produces no frame.
    fn apply_row(&self, y: u32, x0: u32, rgb: &mut [[f32; 3]]) {
        if self.try_apply_row(y, x0, rgb).is_err() {
            rgb.as_flattened_mut().fill(f32::NAN);
        }
    }

    fn try_apply_row(&self, y: u32, x0: u32, rgb: &mut [[f32; 3]]) -> Result<(), Error> {
        self.module.apply_row(&self.words, y, x0, rgb)
    }

    /// The words are opaque to the host, which cannot tell a float among them from an integer.
    fn is_finite(&self) -> bool {
        true
    }

    /// The package's code and the words: equal values give equal words, so equal identities.
    fn identity(&self) -> OperationIdentity {
        OperationIdentity::new(
            IDENTITY,
            self.module
                .identity
                .iter()
                .copied()
                .chain([self.words.len() as u64])
                .chain(self.words.iter().map(|&word| u64::from(word))),
        )
    }

    /// The module's registered program over the words: enabled once its probe enabled it for
    /// this many words, and disabled until then, so a plan falls back naming it. None without a
    /// registered program, so a plan falls back naming the unit ([`Self::describe`]).
    fn gpu(&self) -> Option<GpuDescription> {
        let program = self.module.registered()?;
        Some(GpuDescription::new(
            program.described(self.words.len()),
            self.words.to_vec(),
        ))
    }

    /// The module and its word count, and that its program was refused when it was.
    fn describe(&self) -> String {
        let refused = if self.module.program_refused() {
            "; its program.wgsl was refused at load"
        } else {
            ""
        };
        format!("{} ({} words{refused})", self.module.id(), self.words.len())
    }
}

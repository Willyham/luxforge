//! The high-zoom minimap's script steps: pointer gestures routed as real mouse events through the
//! desktop's laid-out widgets onto the minimap's card, and the same-client API pan equivalent to a
//! click.
use serde::{Deserialize, Serialize};

/// The furthest outside the thumbnail, as a fraction of it, a point may lie: a captured drag is
/// followed past the card's edge.
const REACH: f32 = 2.0;

/// One gesture on the minimap. Each waits for the card to be shown with an overview navigation can
/// act on (at most ten seconds), resolves its point against the card as it then stands, and routes
/// its mouse events through the window's widgets in one redraw; the frame is captured once the
/// messages they published are handled.
///
/// - `{"press": P}` presses the left button at `P` and holds it; `{"move": P}` moves the pointer
///   there; `{"release": P}` releases it there; `{"click": P}` presses and releases at `P`.
/// - `{"wheel": [x, y]}` scrolls a trackpad's pixel delta with the pointer over the card's centre.
/// - `{"pinch": delta}` is a native magnification increment over the card's centre, through the
///   native input's own message.
/// - `{"api": P}` sends `view.set` on the desktop's own session with the pan that centres the view
///   on the stage point under `P` as the schema's pan units define it — what an API client
///   computes — and the desktop follows the answer.
/// - `"ready"` sends nothing: it waits for the card to show the current stage's own overview,
///   ready rather than updating, pending or unavailable (at most ten seconds), and captures.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MinimapStep {
    Press(MinimapPoint),
    Move(MinimapPoint),
    Release(MinimapPoint),
    Click(MinimapPoint),
    Wheel([f32; 2]),
    Pinch(f64),
    Api(MinimapPoint),
    Ready,
}

/// A point on the minimap, as fractions: of the stage's thumbnail (`image`, which is the stage),
/// of the viewport's rectangle over it (`rectangle`), or of the whole card (`card`, whose edge is
/// the letterbox around the thumbnail).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MinimapPoint {
    Image([f32; 2]),
    Rectangle([f32; 2]),
    Card([f32; 2]),
}

impl MinimapPoint {
    fn validate(&self) -> Result<(), String> {
        let [x, y] = match self {
            Self::Image(point) | Self::Rectangle(point) | Self::Card(point) => *point,
        };
        if [x, y]
            .iter()
            .all(|value| value.is_finite() && (-REACH..=1.0 + REACH).contains(value))
        {
            Ok(())
        } else {
            Err(format!(
                "a minimap point's fractions must be finite and within {}..={}",
                -REACH,
                1.0 + REACH
            ))
        }
    }
}

impl MinimapStep {
    pub(crate) fn validate(&self) -> Result<(), String> {
        match self {
            Self::Press(point)
            | Self::Move(point)
            | Self::Release(point)
            | Self::Click(point)
            | Self::Api(point) => point.validate(),
            Self::Ready => Ok(()),
            Self::Wheel([x, y]) => {
                if x.is_finite() && y.is_finite() && x.abs() <= 10_000.0 && y.abs() <= 10_000.0 {
                    Ok(())
                } else {
                    Err("a minimap wheel delta must be finite and at most 10000 points".into())
                }
            }
            Self::Pinch(delta) => {
                if delta.is_finite() && *delta != 0.0 {
                    Ok(())
                } else {
                    Err("a minimap pinch delta must be finite and not zero".into())
                }
            }
        }
    }
}

impl From<MinimapStep> for crate::Step {
    fn from(step: MinimapStep) -> Self {
        Self::Minimap(step)
    }
}

//! Image information through the actual keymap and palette, including history and live crop sizes.
use crate::{
    scenario::{Checked, Checks, Plan, Run, Step, plan::only},
    *,
};
use luxforge_evidence::{
    self as script, DraftStep, PaletteStep, PreviewStep, ViewStep, WorkspaceStep,
};

pub const FIXTURE: &str = "fixtures/geometry/z6-24-70-35mm-grid.jpg";

pub fn plan(_: &[PathBuf]) -> Plan {
    let key = || script::Step::Key { key: "i".into() };
    Plan::new(vec![
        Step::opened("opened").workspace("information", json!(false)),
        Step::new("information", key())
            .commits(0)
            .workspace("information", json!(true)),
        Step::new("zoomed", ViewStep::Percent(200.0)).commits(0),
        Step::new("fit", ViewStep::Fit).commits(0),
        Step::new(
            "cropped",
            script::Step::call(
                "edit.crop",
                json!({"x":0.25,"y":0.25,"width":0.5,"height":0.5}),
            ),
        )
        .commits(1),
        Step::new("original", PreviewStep::Sequence(0)).commits(0),
        Step::new("current", PreviewStep::Current).commits(0),
        Step::new("draft", DraftStep::Start).commits(0),
        Step::new("draft-unlocked", DraftStep::Lock).commits(0),
        Step::new(
            "draft-resized",
            DraftStep::Rect([150.0, 100.0, 200.0, 150.0]),
        )
        .commits(0),
        Step::new("cancelled", DraftStep::Cancel).commits(0),
        Step::new(
            "panels-hidden",
            WorkspaceStep::default()
                .state_panel(false)
                .tools_panel(false),
        )
        .commits(0),
        Step::new("hidden", key())
            .commits(0)
            .workspace("information", json!(false)),
        Step::new("reshown", key())
            .commits(0)
            .workspace("information", json!(true)),
        Step::new(
            "palette-hidden",
            PaletteStep::Run("Hide information".into()),
        )
        .commits(0)
        .workspace("information", json!(false)),
    ])
}

pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();
    for (name, dimensions) in [
        ("information", [600, 400]),
        ("zoomed", [600, 400]),
        ("fit", [600, 400]),
        ("cropped", [300, 200]),
        ("original", [600, 400]),
        ("current", [300, 200]),
        ("draft", [300, 200]),
        ("draft-unlocked", [300, 200]),
        ("draft-resized", [200, 150]),
        ("cancelled", [300, 200]),
        ("panels-hidden", [300, 200]),
        ("reshown", [300, 200]),
    ] {
        let frame = launch.at(name)?;
        let information = &frame.state()["information"];
        ensure(
            information["dimensions"] == json!(dimensions),
            format!("{name}: incorrect information dimensions"),
        )?;
        let rows = information["rows"]
            .as_array()
            .ok_or("missing information rows")?;
        for (label, value) in [
            ("Camera", "NIKON Z 6"),
            ("Lens", "NIKKOR Z 24-70mm f/4 S"),
            ("Focal length", "35 mm"),
            ("Aperture", "Unavailable"),
            ("Shutter", "Unavailable"),
            ("ISO", "Unavailable"),
        ] {
            ensure(
                rows.contains(&json!([label, value])),
                format!("{name}: missing capture field {label}"),
            )?;
        }
        checks.note(
            frame,
            "selected size and original capture fields",
            information.clone(),
        );
    }
    for name in ["opened", "hidden", "palette-hidden"] {
        ensure(
            launch.at(name)?.state()["information"].is_null(),
            format!("{name}: information remains visible"),
        )?;
    }
    let before = launch.at("opened")?;
    let after = launch.at("information")?;
    for field in ["version", "texture_writes", "generation"] {
        ensure(
            before.state()["surface"][field] == after.state()["surface"][field],
            format!("information toggle changed photo {field}"),
        )?;
    }
    // A real card must appear in the upper-left canvas, beyond the mode strip's changed icon.
    let image_before = before.image()?;
    let image_after = after.image()?;
    let left = crate::scenario::frame::columns(after)?.ok_or("missing canvas columns")?[0];
    let scale = f64::from(image_after.width()) / 1440.0;
    let mut changed = 0;
    for y in (60.0 * scale) as u32..(210.0 * scale) as u32 {
        for x in left + (15.0 * scale) as u32..left + (300.0 * scale) as u32 {
            if image_before.get_pixel(x, y) != image_after.get_pixel(x, y) {
                changed += 1;
            }
        }
    }
    ensure(
        changed > 300,
        "information card did not draw on the photo surface",
    )?;
    checks.note(
        after,
        "overlay draws without rendering or uploading the photograph",
        json!({"changed_card_pixels":changed}),
    );
    checks.write(&launch.evidence, "information", json!({"scope":"native captures, correlated state and logs; source hash checked by the smoke runner"}))
}

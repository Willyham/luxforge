//! The minimap's navigation through the editor: a press centres the real view through `view.set`,
//! a drag keeps its grab and clamps at the stage's edges, Escape and focus loss end a drag (Escape
//! consumed), a pinch over the card never zooms the photograph, and a card whose overview is not of
//! the stage drawn cannot navigate. Expected values are worked from the layout by hand.
use super::*;
use crate::app::{
    overview::{Desired, Facts, Lent, OverviewContent, Reduced, Work},
    tasks,
    testing::{boot, finish, real_photo},
};
use iced::keyboard::{Location, Modifiers, key::Physical};
use luxforge_core::{ClientSession, Zoom};
use std::sync::Arc;

/// A photograph of `dimensions` on screen: its frame presented as content 7.
fn drawn_picture(editor: &mut Editor, dimensions: (u32, u32)) {
    let (_, raster) = crate::app::testing::analysed(editor, 1, &[[80, 90, 100, 255]], 1, 1);
    editor.presentation.presenter.show_reduced(&raster, 7);
    editor.presentation.presented_content = 7;
    editor.presentation.dimensions = Some(dimensions);
    editor.rederive();
}

fn view_set(editor: &mut Editor, params: serde_json::Value) {
    let (value, _) = tasks::call(&editor.owner, editor.client, "view.set", params).unwrap();
    editor.adopt(serde_json::from_value(value).unwrap());
}

/// The stage's overview made by the reference and held by the surface: the minimap shows it ready.
fn overview_ready(editor: &mut Editor) {
    editor
        .overviews
        .admitted(7, Some(OverviewContent::Frame(7)));
    let desired = editor.overview_desired(false).expect("an eligible view");
    let key = desired.key.clone().expect("a keyed content");
    let wanted = Desired {
        asset: desired.asset.clone(),
        stage: desired.stage,
        key: Some(key.clone()),
        lent: Some(Lent::Raster(Arc::new(luxforge_core::Raster {
            width: 1,
            height: 1,
            rgba: Arc::new(vec![0; 4]),
            source_fingerprint: "sha256:minimap".into(),
            snapshot_id: luxforge_core::SnapshotId::new(),
        }))),
    };
    let facts = |held: Option<u64>| {
        let mut figures = luxforge_ui::OverviewFigures::default();
        figures.held[0] = held.map(|version| luxforge_ui::OverviewShown {
            version,
            width: key.size.0,
            height: key.size.1,
        });
        Facts {
            moving: false,
            gpu_allowed: false,
            work_allowed: true,
            surface: Some(figures),
            sweepable: None,
            sweep_failed: false,
        }
    };
    let Work::Reduce { version, .. } = editor.overviews.reconcile(Some(&wanted), &facts(None))
    else {
        panic!("a reduction was expected");
    };
    let (width, height) = key.size;
    assert!(editor.overviews.reduced(Reduced {
        version,
        result: Ok(luxforge_core::Overview {
            width,
            height,
            rgba: Arc::new(vec![0; (width * height * 4) as usize]),
        }),
    }));
    let held = facts(Some(version));
    editor.gpu.overview_report = held.surface;
    editor.overviews.reconcile(Some(&wanted), &held);
    // The lifecycle's first sight of the photograph let go of what it held before it, the
    // presented content's identity among it, which the presentation admits again as it would.
    editor
        .overviews
        .admitted(7, Some(OverviewContent::Frame(7)));
    assert!(matches!(
        editor.minimap_overview(),
        MinimapOverview::Ready(_)
    ));
}

/// A 6000 × 4000 photograph at 400% on a 2× display in a 1440 × 900 window with both panels open:
/// a canvas of 898 × 830 logical pixels from (241, 44), two logical pixels per stage pixel.
fn at_400(catalog: &std::path::Path) -> Editor {
    let (mut editor, _, _) = real_photo(catalog);
    drawn_picture(&mut editor, (6000, 4000));
    editor.view_state.window = (1440.0, 900.0);
    editor.view_state.set_system_scale_factor(2.0);
    view_set(&mut editor, json!({"zoom": {"mode": "percent", "value": 400.0}}));
    editor
}

fn escape() -> iced::Event {
    iced::Event::Keyboard(KeyEvent::KeyPressed {
        key: Key::Named(Named::Escape),
        modified_key: Key::Named(Named::Escape),
        physical_key: Physical::Code(iced::keyboard::key::Code::Escape),
        location: Location::Standard,
        modifiers: Modifiers::empty(),
        text: None,
        repeat: false,
    })
}

fn pan(editor: &Editor) -> (f32, f32) {
    (
        editor.session.preview.view.pan_x,
        editor.session.preview.view.pan_y,
    )
}

#[test]
fn the_card_shows_at_200_percent_and_only_navigates_once_its_overview_is_the_stage() {
    let (empty, catalog) = boot();
    assert!(empty.minimap(&empty.surfaces()).is_none(), "no photograph");
    finish(empty, catalog.clone());
    let mut editor = at_400(&catalog);
    view_set(&mut editor, json!({"zoom": {"mode": "percent", "value": 199.9}}));
    assert!(editor.minimap(&editor.surfaces()).is_none(), "199.9%");
    assert!(editor.surfaces().minimap.is_none());
    view_set(&mut editor, json!({"zoom": {"mode": "percent", "value": 200.0}}));
    // Shown at once, pending: a quiet placeholder with no rectangle, and a press does nothing.
    let model = editor.surfaces().minimap.expect("a card at 200%");
    assert_eq!(model.shows, Shows::Pending);
    assert!(model.rectangle.is_none() && model.overview.is_none());
    let before = pan(&editor);
    let _ = editor.minimap_press(Point::new(10.0, 10.0));
    assert_eq!(pan(&editor), before);
    assert!(editor.minimap_nav.drag.is_none());
    // Ready: the image and the rectangle. A 3:2 stage fills the whole 180 × 120 area, the card
    // 12 pt in from the canvas's right and bottom edges.
    overview_ready(&mut editor);
    let model = editor.surfaces().minimap.expect("a card");
    assert_eq!(model.shows, Shows::Ready);
    assert!(model.overview.is_some() && model.rectangle.is_some());
    assert_eq!(model.card.area, Size::new(180.0, 120.0));
    let minimap = editor.minimap(&editor.surfaces()).unwrap();
    assert_eq!(
        minimap.card_window(),
        Rectangle::new(
            Point::new(241.0 + 898.0 - 12.0 - 188.0, 44.0 + 830.0 - 12.0 - 128.0),
            Size::new(188.0, 128.0)
        )
    );
    // Fit has none.
    view_set(&mut editor, json!({"zoom": {"mode": "fit"}}));
    assert!(editor.surfaces().minimap.is_none());
    finish(editor, catalog);
}

#[test]
fn a_press_centres_the_real_view_and_a_drag_keeps_its_grab_and_clamps() {
    let (empty, catalog) = boot();
    finish(empty, catalog.clone());
    let mut editor = at_400(&catalog);
    overview_ready(&mut editor);
    let generation = editor.session.preview.generation;
    // The thumbnail is 180 × 120 for 6000 × 4000: 0.03 points per stage pixel. Its centre is
    // stage (3000, 2000): 6000 × 4000 logical less half the 898 × 830 canvas.
    let _ = editor.minimap_press(Point::new(90.0, 60.0));
    assert_eq!(pan(&editor), (5551.0, 3585.0));
    assert_eq!(editor.view_state.local_pan, (5551.0, 3585.0));
    assert!(editor.minimap_nav.drag.is_some());
    let _ = editor.minimap_drag(Point::new(90.0, 60.0));
    assert_eq!(pan(&editor), (5551.0, 3585.0), "a still pointer moves nothing");
    editor.minimap_end("release");
    // A press inside the rectangle grabs it where it is: the view stays.
    let rectangle = editor.surfaces().minimap.unwrap().rectangle.unwrap();
    assert!(rectangle.contains(Point::new(95.0, 65.0)));
    let _ = editor.minimap_press(Point::new(95.0, 65.0));
    assert_eq!(pan(&editor), (5551.0, 3585.0));
    // Dragged 18 × 12 points: 600 × 400 stage pixels, 1200 × 800 logical, the grab kept.
    let _ = editor.minimap_drag(Point::new(113.0, 77.0));
    let (x, y) = pan(&editor);
    assert!((x - 6751.0).abs() < 0.01 && (y - 4385.0).abs() < 0.01, "{x} {y}");
    // Far outside the card the pan clamps at the stage's far edges.
    let _ = editor.minimap_drag(Point::new(1000.0, 1000.0));
    assert_eq!(pan(&editor), (12000.0 - 898.0, 8000.0 - 830.0));
    editor.minimap_end("release");
    let _ = editor.minimap_drag(Point::new(0.0, 0.0));
    assert_eq!(
        pan(&editor),
        (12000.0 - 898.0, 8000.0 - 830.0),
        "a released drag moves nothing"
    );
    // The letterbox around the thumbnail is inert.
    let _ = editor.minimap_press(Point::new(-2.0, 60.0));
    assert!(editor.minimap_nav.drag.is_none());
    assert_eq!(pan(&editor), (12000.0 - 898.0, 8000.0 - 830.0));
    // The view's zoom and the preview's content never changed.
    assert_eq!(
        editor.session.preview.view.zoom,
        Zoom::Percent { value: 400.0 }
    );
    assert_eq!(editor.session.preview.generation, generation);
    // The owner's session holds the pan the screen shows.
    let (own, _) = tasks::call(&editor.owner, editor.client, "session.state", json!({})).unwrap();
    let own: ClientSession = serde_json::from_value(own).unwrap();
    assert_eq!(own.preview.view, editor.session.preview.view);
    finish(editor, catalog);
}

#[test]
fn escape_and_focus_loss_end_a_drag_and_escape_goes_no_further() {
    let (empty, catalog) = boot();
    finish(empty, catalog.clone());
    let mut editor = at_400(&catalog);
    overview_ready(&mut editor);
    assert!(
        editor.minimap_key(&escape()).is_none(),
        "no drag: Escape is the key table's"
    );
    let _ = editor.minimap_press(Point::new(30.0, 30.0));
    let held = pan(&editor);
    assert!(editor.minimap_key(&escape()).is_some(), "consumed by the drag");
    assert!(editor.minimap_nav.drag.is_none());
    let _ = editor.minimap_drag(Point::new(150.0, 100.0));
    assert_eq!(pan(&editor), held, "the last view stays");
    let _ = editor.minimap_press(Point::new(30.0, 30.0));
    let unfocused = iced::Event::Window(iced::window::Event::Unfocused);
    assert!(
        editor.minimap_key(&unfocused).is_none(),
        "focus loss goes on to the key table"
    );
    assert!(editor.minimap_nav.drag.is_none());
    finish(editor, catalog);
}

#[test]
fn a_pinch_over_the_card_is_the_cards_and_a_changed_stage_ends_a_drag() {
    let (empty, catalog) = boot();
    finish(empty, catalog.clone());
    let mut editor = at_400(&catalog);
    overview_ready(&mut editor);
    let card = editor.minimap(&editor.surfaces()).unwrap().card_window();
    let view = editor.session.preview.view.clone();
    let _ = editor.pinch(luxforge_input::Pinch {
        delta: 0.2,
        x: f64::from(card.center_x()),
        y: f64::from(card.center_y()),
    });
    assert_eq!(
        editor.session.preview.view, view,
        "the photograph is not zoomed"
    );
    // Beside the card the pinch zooms as ever.
    let _ = editor.pinch(luxforge_input::Pinch {
        delta: 0.2,
        x: f64::from(card.x - 40.0),
        y: f64::from(card.center_y()),
    });
    assert_ne!(editor.session.preview.view.zoom, view.zoom);
    // Another stage under a drag ends it, keeping the view.
    view_set(&mut editor, json!({"zoom": {"mode": "percent", "value": 400.0}}));
    let _ = editor.minimap_press(Point::new(30.0, 30.0));
    assert!(editor.minimap_nav.drag.is_some());
    let held = pan(&editor);
    editor.presentation.dimensions = Some((4000, 6000));
    editor.rederive();
    let before = Before::of(&editor);
    let _ = after_message(&mut editor, &before);
    assert!(editor.minimap_nav.drag.is_none());
    assert_eq!(pan(&editor), held);
    finish(editor, catalog);
}

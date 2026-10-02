//! Evidence of the frames the Develop surface draws: one `surface_frame_drawn` event for each frame
//! it first draws, stamped with the instant of that draw on the run's own clock, so a harness times
//! an input to the draw of the frame that carries it on either path. A GPU frame is named by its
//! boundary and the draft revision its plan was drawn for; a CPU frame by its picture version and
//! the preview generation and draft revision the desktop handed the surface it as.
//!
//! The surface stamps a frame in its draw ([`luxforge_ui::FirstDrawn`]); the desktop reads the stamp
//! after its next message, which carries the draw's own instant, however late it comes. Nothing
//! here runs unless the run logs events, and nothing wakes the editor.
use super::{Before, Editor, Message};
use luxforge_ui::FirstDrawn;
use serde_json::json;
use std::collections::VecDeque;

/// How many CPU frames handed to the surface are remembered, for naming the one a draw drew.
const PICTURES: usize = 16;

/// The surface's last stamped frame the desktop logged, and the CPU frames it has handed it.
#[derive(Default)]
pub(crate) struct DrawnFrames {
    logged: Option<FirstDrawn>,
    /// The CPU frames handed to the surface, newest last: picture version, preview generation and
    /// draft revision.
    pictures: VecDeque<(u64, u64, Option<u64>)>,
}

pub(super) fn after_message(editor: &mut Editor, _: &Before) -> iced::Task<Message> {
    if editor.log.diagnostics.is_none() && !editor.log.verbose {
        return iced::Task::none();
    }
    remember_picture(editor);
    let first = luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE).first_drawn;
    log_drawn(editor, first);
    iced::Task::none()
}

/// Remember the CPU frame on screen under the generation and draft revision that put it there.
fn remember_picture(editor: &mut Editor) {
    let Some(version) = editor.surfaces().photo.map(luxforge_ui::Frame::version) else {
        return;
    };
    let pictures = &editor.drawn_frames.pictures;
    if pictures
        .back()
        .is_some_and(|(newest, _, _)| *newest == version)
    {
        return;
    }
    let handed = (
        version,
        editor.presentation.presented_generation,
        editor.presentation.displayed_draft_revision,
    );
    let pictures = &mut editor.drawn_frames.pictures;
    if pictures.len() == PICTURES {
        pictures.pop_front();
    }
    pictures.push_back(handed);
}

/// Log the frame the surface last stamped, once.
fn log_drawn(editor: &mut Editor, first: Option<FirstDrawn>) {
    let Some(first) = first else {
        return;
    };
    if editor.drawn_frames.logged == Some(first) {
        return;
    }
    editor.drawn_frames.logged = Some(first);
    let drawn_ms = first
        .at
        .saturating_duration_since(editor.log.started)
        .as_secs_f64()
        * 1000.0;
    let handed = first.picture.and_then(|picture| {
        editor
            .drawn_frames
            .pictures
            .iter()
            .rev()
            .find(|(version, _, _)| *version == picture)
            .copied()
    });
    let draft = first
        .tag
        .and(editor.gpu.draft())
        .map(|draft| draft.as_str().to_owned());
    editor.event("surface_frame_drawn", || {
        json!({
            "path": first.path.as_str(),
            "drawn_ms": drawn_ms,
            "boundary": first.boundary,
            "draft_revision": first.tag,
            "draft_id": draft,
            "picture": first.picture,
            "generation": handed.map(|(_, generation, _)| generation),
            "picture_draft_revision": handed.and_then(|(_, _, revision)| revision),
        })
    });
}

#[cfg(test)]
mod tests {
    use super::super::testing::{attach_log, events, finish, logged, real_photo};
    use super::*;
    use luxforge_ui::photo_surface::DrawingPath;
    use std::time::Duration;

    /// Each frame the surface first draws is logged once, at the draw's own instant on the run's
    /// clock: a CPU frame under the generation that handed it, a GPU frame under its boundary and
    /// draft revision, and a redraw of either not again.
    #[test]
    fn each_first_drawn_frame_is_logged_once_at_its_draws_instant() {
        let catalog = std::env::temp_dir().join(format!(
            "luxforge-drawn-frames-{}.sqlite",
            std::process::id()
        ));
        let (mut editor, _, _) = real_photo(&catalog);
        luxforge_testbase::wait_until("the photograph on screen", || {
            let _ = editor.update(Message::Preview(
                super::super::message::preview::PreviewMessage::Poll,
            ));
            editor.surfaces().photo.is_some()
        });
        let log = attach_log(&mut editor);
        remember_picture(&mut editor);
        let version = editor
            .surfaces()
            .photo
            .map(luxforge_ui::Frame::version)
            .expect("a photograph on screen");
        let at = editor.log.started + Duration::from_millis(250);
        let cpu = FirstDrawn {
            path: DrawingPath::Cpu,
            picture: Some(version),
            boundary: None,
            tag: None,
            at,
        };
        log_drawn(&mut editor, Some(cpu));
        log_drawn(&mut editor, Some(cpu));
        let gpu = FirstDrawn {
            path: DrawingPath::Gpu,
            picture: None,
            boundary: Some(3),
            tag: Some(7),
            at: at + Duration::from_millis(10),
        };
        log_drawn(&mut editor, Some(gpu));
        log_drawn(&mut editor, None);
        let records = logged(&mut editor, &log);
        let drawn = events(&records, "surface_frame_drawn");
        assert_eq!(drawn.len(), 2, "{drawn:?}");
        assert_eq!(drawn[0]["path"], "cpu");
        assert_eq!(drawn[0]["drawn_ms"], json!(250.0));
        assert_eq!(drawn[0]["picture"], json!(version));
        assert_eq!(
            drawn[0]["generation"],
            json!(editor.presentation.presented_generation)
        );
        assert_eq!(drawn[1]["path"], "gpu");
        assert_eq!(drawn[1]["drawn_ms"], json!(260.0));
        assert_eq!(
            (&drawn[1]["boundary"], &drawn[1]["draft_revision"]),
            (&json!(3), &json!(7))
        );
        finish(editor, catalog);
    }
}

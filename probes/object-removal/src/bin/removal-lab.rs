//! `removal-lab`: one image in a window, to try object removal by hand.
//!
//! The window holds no removal logic: pointer and key events become calls on
//! [`luxforge_removal_probe::lab::Lab`], and the canvas draws what the lab exposes. Workers wake the
//! window through one persistent channel (the editor's `app/waker.rs` pattern); each wake applies
//! every waiting result in one update, so a hover result reaches the screen in the frame after it
//! arrives. Image handles are made in `update` when a raster first appears, never in `view`.
//!
//! `--script FILE` runs the same lab without a window; automated checks use it, and the window is
//! for the owner to launch. `--script FILE --hidden-window` sends the script's steps to an invisible
//! window as its own messages, so the window's update, handles and canvas draw run and log
//! `hover_drawn`; launch it from a background-only bundle so it never activates.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::pin::Pin;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use iced::futures::Stream;
use iced::futures::channel::mpsc::{
    Receiver, Sender, UnboundedReceiver, UnboundedSender, channel, unbounded,
};
use iced::widget::canvas::{self, Action, Event, Frame, Geometry, Path, Stroke as Line, Text};
use iced::widget::image::{FilterMethod, Handle};
use iced::widget::{column, container, text};
use iced::{
    Color, Element, Length, Rectangle, Renderer, Size, Subscription, Task, Theme, alignment,
    keyboard, mouse, window,
};
use serde_json::json;

use luxforge_removal_probe::args::Args;
use luxforge_removal_probe::lab::log::{Log, ms};
use luxforge_removal_probe::lab::script::{self, Controls, Observed, Step};
use luxforge_removal_probe::lab::{self, Config, Lab, raster};
use luxforge_removal_probe::mask::Point;
use luxforge_removal_probe::runtime::Backend;
use luxforge_removal_probe::segment::Family;
use luxforge_removal_probe::stroke::Stroke;

const USAGE: &str = "usage: removal-lab IMAGE [--family evit-l0|sam21-small] [--backend B] [--fill-backend B]
                   [--fill-model lama|lama-fp32]
                   [--param NAME=VALUE]... [--models DIR] [--out DIR]
                   [--script FILE [--hidden-window]]

  Hover highlights the object under the pointer and a click removes it. B toggles the brush, or hold
  Shift; [ and ] size it. A stroke over one object removes it with the stroke; over several, click
  their numbered pins (or the objects) and press Enter, or Esc to cancel; over none, the painted area
  is removed. Esc also cancels a removal in progress. Cmd+Z undoes, Cmd+S saves a PNG copy.

  --out DIR        saved copies and the JSON-lines log (default: removal-lab in the system temp dir)
  --param N=V      set a stroke::Params field, e.g. min_predicted_iou=0.6
  --script FILE    run the steps in FILE without a window (see src/lab/script.rs) and exit
  --hidden-window  with --script: send the steps to an invisible window instead; launch the binary
                   from a background-only (LSBackgroundOnly) bundle so it never activates";

/// How close to a pin, in logical pixels, a click still hits it.
const PIN_HIT: f32 = 14.0;
const PIN_RADIUS: f32 = 11.0;
/// After the photograph changes, the previous one is drawn beneath it this long, so the frame
/// never shows nothing while the new texture uploads.
const KEEP_PREVIOUS: Duration = Duration::from_secs(1);

fn main() -> ExitCode {
    match run(Args::new(std::env::args().skip(1).collect())) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(mut a: Args) -> Result<(), String> {
    if a.flag("--help") || a.flag("-h") {
        println!("{USAGE}");
        return Ok(());
    }
    let family: Family = a.parsed("--family")?.unwrap_or(Family::EfficientVitL0);
    let backend: Backend = a.parsed("--backend")?.unwrap_or(Backend::OrtCpu);
    let fill_backend: Backend = a.parsed("--fill-backend")?.unwrap_or(Backend::OrtCpu);
    let fill_model = a.parsed("--fill-model")?.unwrap_or_default();
    let models = a.path("--models")?;
    let out = a.path("--out")?;
    let script_path = a.path("--script")?;
    let hidden = a.flag("--hidden-window");
    let params = a.all("--param")?;
    let image = PathBuf::from(a.positional().ok_or(USAGE)?);
    a.done()?;

    let out = out.unwrap_or_else(|| std::env::temp_dir().join("removal-lab"));
    let mut config = Config::new(image, out);
    config.family = family;
    config.stroke = luxforge_removal_probe::stroke::Params::for_family(family);
    config.backend = backend;
    config.fill_backend = fill_backend;
    config.fill_model = fill_model;
    config.models = luxforge_removal_probe::models::directory(models);
    for p in &params {
        lab::stroke_param(&mut config.stroke, p)?;
    }
    match (script_path, hidden) {
        (Some(path), false) => script::run(config, &path),
        (Some(path), true) => {
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            open_window(config, Some(script::parse(&text)?))
        }
        (None, true) => Err("--hidden-window drives a --script".into()),
        (None, false) => open_window(config, None),
    }
}

/// Opens the window, visible unless a script drives it.
fn open_window(config: Config, drive: Option<Vec<Step>>) -> Result<(), String> {
    let log = Arc::new(Log::create(&config.out)?);
    eprintln!(
        "removal-lab: saved copies and the log go to {} (log {})",
        config.out.display(),
        log.path()
            .map_or(String::new(), |p| p.display().to_string())
    );
    let visible = drive.is_none();
    let shared = drive.as_ref().map(|_| Arc::new(Shared::new()));
    let boot = Mutex::new(Some((config, log, drive, shared.clone())));
    iced::application(
        move || {
            let (config, log, drive, shared) = boot
                .lock()
                .expect("boot state is never poisoned")
                .take()
                .expect("the lab boots once");
            App::new(config, log, drive, shared)
        },
        App::update,
        App::view,
    )
    .title(App::title)
    .theme(Theme::Dark)
    .subscription(App::subscription)
    .window(window::Settings {
        size: Size::new(1400.0, 950.0),
        visible,
        ..window::Settings::default()
    })
    .run()
    .map_err(|e| e.to_string())?;
    match shared.map(|s| s.outcome.lock().ok().and_then(|mut o| o.take())) {
        None => Ok(()),
        Some(Some(outcome)) => outcome,
        Some(None) => Err("the window closed before the script finished".into()),
    }
}

// ---- Waking the window ----

/// One persistent channel the workers post to. It holds one signal: a full channel already says
/// there is something to apply, and one update applies everything waiting.
struct Signal {
    sender: Mutex<Sender<()>>,
    receiver: Mutex<Option<Receiver<()>>>,
}

static SIGNAL: OnceLock<Signal> = OnceLock::new();

fn signal() -> &'static Signal {
    SIGNAL.get_or_init(|| {
        let (sender, receiver) = channel(1);
        Signal {
            sender: Mutex::new(sender),
            receiver: Mutex::new(Some(receiver)),
        }
    })
}

fn wake() {
    if let Ok(mut s) = signal().sender.lock() {
        let _ = s.try_send(());
    }
}

/// The subscription's stream, lent the receiver and returning it when dropped.
struct Wakes {
    receiver: Option<Receiver<()>>,
}

fn wakes() -> Wakes {
    Wakes {
        receiver: signal().receiver.lock().ok().and_then(|mut r| r.take()),
    }
}

impl Stream for Wakes {
    type Item = Message;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Message>> {
        match self.receiver.as_mut() {
            Some(r) => Pin::new(r).poll_next(cx).map(|s| s.map(|()| Message::Wake)),
            None => Poll::Ready(None),
        }
    }
}

impl Drop for Wakes {
    fn drop(&mut self) {
        if let (Some(r), Ok(mut slot)) = (self.receiver.take(), signal().receiver.lock()) {
            *slot = Some(r);
        }
    }
}

// ---- Driving the window from a script ----

/// The channel a script's steps reach the window through, as its own messages.
struct Drive {
    sender: UnboundedSender<Message>,
    receiver: Mutex<Option<UnboundedReceiver<Message>>>,
}

static DRIVE: OnceLock<Drive> = OnceLock::new();

fn drive() -> &'static Drive {
    DRIVE.get_or_init(|| {
        let (sender, receiver) = unbounded();
        Drive {
            sender,
            receiver: Mutex::new(Some(receiver)),
        }
    })
}

struct Steps {
    receiver: Option<UnboundedReceiver<Message>>,
}

fn steps() -> Steps {
    Steps {
        receiver: drive().receiver.lock().ok().and_then(|mut r| r.take()),
    }
}

impl Stream for Steps {
    type Item = Message;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Message>> {
        match self.receiver.as_mut() {
            Some(r) => Pin::new(r).poll_next(cx),
            None => Poll::Ready(None),
        }
    }
}

impl Drop for Steps {
    fn drop(&mut self) {
        if let (Some(r), Ok(mut slot)) = (self.receiver.take(), drive().receiver.lock()) {
            *slot = Some(r);
        }
    }
}

/// What the window publishes after every update for the script's thread to wait on.
struct Shared {
    /// The last script message the window has handled.
    applied: AtomicU64,
    busy: AtomicBool,
    observed: Mutex<Observed>,
    outcome: Mutex<Option<Result<(), String>>>,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            applied: AtomicU64::new(0),
            // Busy until the window has published once.
            busy: AtomicBool::new(true),
            observed: Mutex::new(Observed::default()),
            outcome: Mutex::new(None),
        }
    }
}

/// The script's side: each step becomes the message the window would get from the pointer or the
/// keyboard, numbered so `settle` knows when the window has handled it.
struct Driver {
    shared: Arc<Shared>,
    sent: u64,
}

impl Driver {
    fn send(&mut self, message: Message) {
        self.sent += 1;
        let _ = drive()
            .sender
            .unbounded_send(Message::Driven(self.sent, Box::new(message)));
    }
}

impl Controls for Driver {
    fn pointer(&mut self, p: Point) {
        self.send(Message::Pointer {
            at: p,
            when: Instant::now(),
        });
    }

    fn press(&mut self, p: Point) {
        self.send(Message::Press {
            at: p,
            when: Instant::now(),
            tolerance: 0.0,
        });
    }

    fn release(&mut self) {
        self.send(Message::Release);
    }

    fn hold_brush(&mut self, held: bool) {
        self.send(Message::Shift(held));
    }

    fn set_brush_radius(&mut self, radius: f32) {
        self.send(Message::BrushRadius(radius));
    }

    fn pick(&mut self, number: usize) {
        self.send(Message::Pick(number));
    }

    fn confirm(&mut self) {
        self.send(Message::Key(Key::Confirm));
    }

    fn cancel(&mut self) {
        self.send(Message::Key(Key::Cancel));
    }

    fn undo(&mut self) {
        self.send(Message::Key(Key::Undo));
    }

    fn save(&mut self) {
        self.send(Message::Key(Key::Save));
    }

    fn pump(&mut self, deadline: Instant) {
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
    }

    fn settle(&mut self, timeout: Duration) -> Result<(), String> {
        let started = Instant::now();
        loop {
            // `busy` is stored before `applied`, so once the window has handled every message sent,
            // `busy` describes the state after them.
            if self.shared.applied.load(Ordering::SeqCst) >= self.sent
                && !self.shared.busy.load(Ordering::SeqCst)
            {
                return Ok(());
            }
            if started.elapsed() > timeout {
                return Err(format!(
                    "still busy after {:.0} s: {}",
                    timeout.as_secs_f64(),
                    self.observe().status
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn observe(&self) -> Observed {
        self.shared
            .observed
            .lock()
            .map(|o| o.clone())
            .unwrap_or_default()
    }
}

// ---- The application ----

#[derive(Clone, Debug)]
enum Message {
    /// A worker has results waiting.
    Wake,
    Pointer {
        at: Point,
        when: Instant,
    },
    Left,
    Press {
        at: Point,
        when: Instant,
        tolerance: f32,
    },
    Release,
    Key(Key),
    Shift(bool),
    /// A script's step, numbered in the order sent.
    Driven(u64, Box<Message>),
    BrushRadius(f32),
    Pick(usize),
    /// The script ended: summarize, record the outcome and close.
    Finish(Result<(), String>),
}

#[derive(Clone, Copy, Debug)]
enum Key {
    Brush,
    Grow,
    Shrink,
    Confirm,
    Cancel,
    Undo,
    Save,
}

struct App {
    lab: Lab,
    log: Arc<Log>,
    /// One handle per picture or raster id the lab still shows.
    handles: HashMap<u64, Handle>,
    picture: Option<u64>,
    previous: Option<(Handle, Instant)>,
    /// The hover raster last drawn, so each is logged once.
    drawn: Cell<u64>,
    /// Present while a script drives the window.
    shared: Option<Arc<Shared>>,
}

impl App {
    fn new(
        config: Config,
        log: Arc<Log>,
        drive_with: Option<Vec<Step>>,
        shared: Option<Arc<Shared>>,
    ) -> App {
        let lab = Lab::start(config, log.clone(), Arc::new(wake));
        if let (Some(steps), Some(shared)) = (drive_with, &shared) {
            let shared = shared.clone();
            std::thread::spawn(move || {
                let mut driver = Driver { shared, sent: 0 };
                let outcome = script::run_steps(&mut driver, &steps)
                    .and_then(|()| script::finish(&mut driver));
                let _ = drive().sender.unbounded_send(Message::Finish(outcome));
            });
        }
        App {
            lab,
            log,
            handles: HashMap::new(),
            picture: None,
            previous: None,
            drawn: Cell::new(0),
            shared,
        }
    }

    fn title(&self) -> String {
        format!("removal-lab — {}", self.lab.config().image.display())
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut all = vec![Subscription::run(wakes), iced::event::listen_with(keys)];
        if self.shared.is_some() {
            all.push(Subscription::run(steps));
        }
        Subscription::batch(all)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        let (driven, message) = match message {
            Message::Driven(n, m) => (Some(n), *m),
            m => (None, m),
        };
        match message {
            Message::Wake => {}
            Message::Pointer { at, when } => self.lab.pointer_moved(at, when),
            Message::Left => self.lab.pointer_left(),
            Message::Press {
                at,
                when,
                tolerance,
            } => self.lab.press(at, when, tolerance),
            Message::Release => self.lab.release(),
            Message::Shift(held) => self.lab.set_shift(held),
            Message::Key(k) => match k {
                Key::Brush => self.lab.toggle_brush(),
                Key::Grow => self.lab.grow_brush(),
                Key::Shrink => self.lab.shrink_brush(),
                Key::Confirm => self.lab.confirm(),
                Key::Cancel => self.lab.cancel(),
                Key::Undo => self.lab.undo(),
                Key::Save => self.lab.save(),
            },
            Message::BrushRadius(r) => self.lab.set_brush_radius(r),
            Message::Pick(n) => {
                if let Err(e) = self.lab.toggle_pin(n) {
                    self.log.event("script_error", json!({"error": e}));
                }
            }
            Message::Driven(..) => {}
            Message::Finish(outcome) => {
                script::summarize(&self.lab);
                if let Some(s) = &self.shared
                    && let Ok(mut o) = s.outcome.lock()
                {
                    *o = Some(outcome);
                }
                return iced::exit();
            }
        }
        // Whatever arrived since the last wake is applied now, in this update.
        self.lab.poll();
        self.sync_handles();
        if let Some(s) = &self.shared {
            if let Ok(mut o) = s.observed.lock() {
                *o = Observed::of(&self.lab);
            }
            s.busy.store(self.lab.busy(), Ordering::SeqCst);
            if let Some(n) = driven {
                s.applied.store(n, Ordering::SeqCst);
            }
        }
        Task::none()
    }

    /// Makes a handle for each new picture or raster and drops those no longer shown.
    fn sync_handles(&mut self) {
        let mut live = HashSet::new();
        if let Some(p) = self.lab.picture_mut() {
            live.insert(p.id);
            if let Some(rgba) = p.rgba.take() {
                self.handles
                    .insert(p.id, Handle::from_rgba(p.size.0, p.size.1, rgba));
            }
            if self.picture != Some(p.id) {
                let old = self.picture.and_then(|id| self.handles.get(&id).cloned());
                self.previous = old.map(|h| (h, Instant::now()));
                self.picture = Some(p.id);
            }
        }
        for r in self.lab.rasters_mut() {
            live.insert(r.id);
            if let Some(rgba) = r.rgba.take() {
                self.handles
                    .insert(r.id, Handle::from_rgba(r.size.0, r.size.1, rgba));
            }
        }
        self.handles.retain(|id, _| live.contains(id));
        if self
            .previous
            .as_ref()
            .is_some_and(|(_, since)| since.elapsed() > KEEP_PREVIOUS)
        {
            self.previous = None;
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let stage = canvas::Canvas::new(Stage { app: self })
            .width(Length::Fill)
            .height(Length::Fill);
        let status = container(text(self.lab.status()).size(14))
            .padding([6, 12])
            .width(Length::Fill)
            .style(|_| container::Style {
                background: Some(Color::from_rgb8(18, 18, 20).into()),
                text_color: Some(Color::from_rgb8(225, 225, 225)),
                ..container::Style::default()
            });
        column![stage, status].into()
    }
}

fn keys(event: iced::Event, _: iced::event::Status, _: window::Id) -> Option<Message> {
    use keyboard::key::Named;
    use keyboard::{Event as K, Key as Code};
    let iced::Event::Keyboard(k) = event else {
        return None;
    };
    match k {
        K::ModifiersChanged(m) => Some(Message::Shift(m.shift())),
        K::KeyPressed { key, modifiers, .. } => {
            let command = modifiers.command();
            let key = match key.as_ref() {
                Code::Named(Named::Enter) => Key::Confirm,
                Code::Named(Named::Escape) => Key::Cancel,
                Code::Character(c) if command && c.eq_ignore_ascii_case("z") => Key::Undo,
                Code::Character(c) if command && c.eq_ignore_ascii_case("s") => Key::Save,
                Code::Character(c) if !command && c.eq_ignore_ascii_case("b") => Key::Brush,
                Code::Character("[" | "{") if !command => Key::Shrink,
                Code::Character("]" | "}") if !command => Key::Grow,
                _ => return None,
            };
            Some(Message::Key(key))
        }
        _ => None,
    }
}

// ---- The canvas ----

/// Where the picture sits in the canvas: logical pixels per source pixel, and the top-left corner.
#[derive(Clone, Copy)]
struct Fit {
    x: f32,
    y: f32,
    scale: f32,
    size: Size,
}

impl Fit {
    fn new(bounds: Rectangle, source: (u32, u32)) -> Fit {
        let (w, h) = (source.0 as f32, source.1 as f32);
        let scale = (bounds.width / w).min(bounds.height / h);
        let size = Size::new(w * scale, h * scale);
        Fit {
            x: (bounds.width - size.width) / 2.0,
            y: (bounds.height - size.height) / 2.0,
            scale,
            size,
        }
    }

    fn contains(&self, p: iced::Point) -> bool {
        p.x >= self.x
            && p.y >= self.y
            && p.x < self.x + self.size.width
            && p.y < self.y + self.size.height
    }

    fn source(&self, p: iced::Point) -> Point {
        Point::new((p.x - self.x) / self.scale, (p.y - self.y) / self.scale)
    }

    fn screen(&self, p: Point) -> iced::Point {
        iced::Point::new(self.x + p.x * self.scale, self.y + p.y * self.scale)
    }

    fn rect(&self, r: [f32; 4]) -> Rectangle {
        Rectangle {
            x: self.x + r[0] * self.scale,
            y: self.y + r[1] * self.scale,
            width: r[2] * self.scale,
            height: r[3] * self.scale,
        }
    }

    fn whole(&self) -> Rectangle {
        Rectangle::new(iced::Point::new(self.x, self.y), self.size)
    }
}

struct Stage<'a> {
    app: &'a App,
}

/// Whether the pointer was over the picture, so leaving it clears the hover once.
#[derive(Default)]
struct Over(bool);

fn local(cursor: mouse::Cursor, bounds: Rectangle) -> Option<iced::Point> {
    cursor
        .position()
        .map(|p| iced::Point::new(p.x - bounds.x, p.y - bounds.y))
}

impl Stage<'_> {
    fn fit(&self, bounds: Rectangle) -> Option<Fit> {
        self.app.lab.source_size().map(|s| Fit::new(bounds, s))
    }

    fn over_pin(&self, fit: &Fit, p: iced::Point) -> bool {
        self.app.lab.choice().is_some_and(|c| {
            c.items.iter().filter_map(|i| i.pin).any(|q| {
                let s = fit.screen(q);
                (s.x - p.x).hypot(s.y - p.y) <= PIN_HIT
            })
        })
    }

    fn image(&self, frame: &mut Frame, fit: &Fit, r: &raster::Raster, opacity: f32) {
        if let Some(h) = self.app.handles.get(&r.id) {
            frame.draw_image(
                fit.rect(r.rect),
                canvas::Image::new(h.clone())
                    .filter_method(FilterMethod::Linear)
                    .opacity(opacity),
            );
        }
    }

    fn stroke(&self, frame: &mut Frame, fit: &Fit, s: &Stroke, alpha: f32) {
        let Some(first) = s.points.first() else {
            return;
        };
        let path = Path::new(|b| {
            b.move_to(fit.screen(*first));
            for p in &s.points[1..] {
                b.line_to(fit.screen(*p));
            }
            // A single dab is a zero-length line; its round cap is the dab.
            if s.points.len() == 1 {
                b.line_to(fit.screen(*first));
            }
        });
        frame.stroke(
            &path,
            Line::default()
                .with_color(Color::from_rgba8(255, 225, 0, alpha))
                .with_width((2.0 * s.radius * fit.scale).max(1.0))
                .with_line_cap(canvas::LineCap::Round)
                .with_line_join(canvas::LineJoin::Round),
        );
    }
}

impl canvas::Program<Message> for Stage<'_> {
    type State = Over;

    fn update(
        &self,
        over: &mut Over,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        let fit = self.fit(bounds)?;
        let now = Instant::now();
        match event {
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let p = local(cursor, bounds)?;
                // A stroke keeps following the pointer off the picture; the lab clamps it.
                if self.app.lab.painting() {
                    return Some(Action::publish(Message::Pointer {
                        at: fit.source(p),
                        when: now,
                    }));
                }
                if cursor.is_over(bounds) && fit.contains(p) {
                    over.0 = true;
                    Some(Action::publish(Message::Pointer {
                        at: fit.source(p),
                        when: now,
                    }))
                } else if std::mem::take(&mut over.0) {
                    Some(Action::publish(Message::Left))
                } else {
                    None
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) => {
                std::mem::take(&mut over.0).then(|| Action::publish(Message::Left))
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_in(bounds)?;
                if !fit.contains(p) {
                    return None;
                }
                Some(
                    Action::publish(Message::Press {
                        at: fit.source(p),
                        when: now,
                        tolerance: PIN_HIT / fit.scale,
                    })
                    .and_capture(),
                )
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => self
                .app
                .lab
                .painting()
                .then(|| Action::publish(Message::Release).and_capture()),
            _ => None,
        }
    }

    fn draw(
        &self,
        _: &Over,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        frame.fill_rectangle(
            iced::Point::ORIGIN,
            bounds.size(),
            Color::from_rgb8(30, 30, 32),
        );
        let Some(fit) = self.fit(bounds) else {
            return vec![frame.into_geometry()];
        };
        let lab = &self.app.lab;
        if let Some((h, _)) = &self.app.previous {
            frame.draw_image(
                fit.whole(),
                canvas::Image::new(h.clone()).filter_method(FilterMethod::Linear),
            );
        }
        if let Some(h) = self.app.picture.and_then(|id| self.app.handles.get(&id)) {
            frame.draw_image(
                fit.whole(),
                canvas::Image::new(h.clone()).filter_method(FilterMethod::Linear),
            );
        }
        if let Some(job) = lab.job() {
            if let Some(s) = &job.stroke {
                self.stroke(&mut frame, &fit, s, 0.25);
            }
            for r in &job.rasters {
                self.image(&mut frame, &fit, r, 1.0);
            }
        }
        if let Some(choice) = lab.choice() {
            self.stroke(&mut frame, &fit, &choice.stroke, 0.15);
            for item in &choice.items {
                if let Some(r) = &item.raster {
                    self.image(&mut frame, &fit, r, if item.selected { 1.0 } else { 0.6 });
                }
            }
            for item in &choice.items {
                let Some(p) = item.pin else { continue };
                let centre = fit.screen(p);
                let [r, g, b] = raster::palette(item.number - 1);
                let fill = if item.selected {
                    Color::WHITE
                } else {
                    Color::from_rgb8(r, g, b)
                };
                frame.fill(&Path::circle(centre, PIN_RADIUS), fill);
                frame.stroke(
                    &Path::circle(centre, PIN_RADIUS + 0.5),
                    Line::default().with_color(Color::BLACK).with_width(1.5),
                );
                frame.fill_text(Text {
                    content: item.number.to_string(),
                    position: centre,
                    color: Color::BLACK,
                    size: 13.0.into(),
                    align_x: alignment::Horizontal::Center.into(),
                    align_y: alignment::Vertical::Center,
                    ..Text::default()
                });
            }
        }
        if let Some(s) = lab.stroke() {
            self.stroke(&mut frame, &fit, s, if lab.painting() { 0.4 } else { 0.25 });
        }
        if let Some(h) = lab.hover()
            && let Some(r) = &h.raster
        {
            self.image(&mut frame, &fit, r, 1.0);
            // The first frame that draws this overlay: its pointer-to-overlay time. Presenting the
            // frame follows and is not included.
            if self.app.drawn.get() != r.id && self.app.handles.contains_key(&r.id) {
                self.app.drawn.set(r.id);
                self.app.log.event(
                    "hover_drawn",
                    json!({"seq": h.raster_seq, "pointer_to_draw_ms": ms(h.raster_at.elapsed())}),
                );
            }
        }
        if lab.brush_active()
            && let Some(p) = cursor.position_in(bounds)
        {
            let r = (lab.brush_radius() * fit.scale).max(1.0);
            frame.stroke(
                &Path::circle(p, r + 1.0),
                Line::default().with_color(Color::BLACK).with_width(1.0),
            );
            frame.stroke(
                &Path::circle(p, r),
                Line::default().with_color(Color::WHITE).with_width(1.0),
            );
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _: &Over,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        let (Some(fit), Some(p)) = (self.fit(bounds), cursor.position_in(bounds)) else {
            return mouse::Interaction::default();
        };
        if !fit.contains(p) {
            mouse::Interaction::default()
        } else if self.app.lab.job().is_some() {
            mouse::Interaction::Progress
        } else if self.over_pin(&fit, p) {
            mouse::Interaction::Pointer
        } else if self.app.lab.brush_active() {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keyboard::key::{Named, NativeCode, Physical};
    use keyboard::{Key as Code, Modifiers};

    fn press(key: Code, modifiers: Modifiers) -> Option<Message> {
        keys(
            iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: key,
                physical_key: Physical::Unidentified(NativeCode::Unidentified),
                location: keyboard::Location::Standard,
                modifiers,
                text: None,
                repeat: false,
            }),
            iced::event::Status::Ignored,
            window::Id::unique(),
        )
    }

    fn ch(c: &str) -> Code {
        Code::Character(c.into())
    }

    fn command(m: Option<Message>) -> Option<String> {
        match m {
            Some(Message::Key(k)) => Some(format!("{k:?}")),
            _ => None,
        }
    }

    #[test]
    fn keys_map_to_the_labs_commands() {
        let none = Modifiers::default();
        let cmd = Modifiers::COMMAND;
        let cases = [
            (ch("z"), cmd, "Undo"),
            (ch("s"), cmd, "Save"),
            (ch("b"), none, "Brush"),
            (ch("B"), Modifiers::SHIFT, "Brush"),
            (ch("["), none, "Shrink"),
            (ch("]"), none, "Grow"),
            (Code::Named(Named::Enter), none, "Confirm"),
            (Code::Named(Named::Escape), none, "Cancel"),
        ];
        for (key, modifiers, expected) in cases {
            assert_eq!(command(press(key, modifiers)).as_deref(), Some(expected));
        }
        // A plain z or s is not undo or save, and ⌘B is not the brush.
        assert!(press(ch("z"), none).is_none());
        assert!(press(ch("s"), none).is_none());
        assert!(press(ch("b"), cmd).is_none());
        let shift = keys(
            iced::Event::Keyboard(keyboard::Event::ModifiersChanged(Modifiers::SHIFT)),
            iced::event::Status::Ignored,
            window::Id::unique(),
        );
        assert!(matches!(shift, Some(Message::Shift(true))));
    }

    #[test]
    fn the_fit_centres_the_picture_and_maps_both_ways() {
        let bounds = Rectangle {
            x: 10.0,
            y: 20.0,
            width: 200.0,
            height: 100.0,
        };
        let fit = Fit::new(bounds, (400, 400));
        assert_eq!(fit.scale, 0.25);
        assert_eq!((fit.x, fit.y), (50.0, 0.0));
        let p = Point::new(123.0, 321.0);
        let back = fit.source(fit.screen(p));
        assert!((back.x - p.x).abs() < 1e-3 && (back.y - p.y).abs() < 1e-3);
        assert!(fit.contains(iced::Point::new(51.0, 1.0)));
        assert!(!fit.contains(iced::Point::new(49.0, 1.0)));
        let r = fit.rect([40.0, 80.0, 20.0, 40.0]);
        assert_eq!((r.x, r.y, r.width, r.height), (60.0, 20.0, 5.0, 10.0));
    }
}

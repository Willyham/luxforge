//! `removal-lab --script FILE`: the lab driven by a script. Without a window, the runner makes the
//! calls on [`Lab`] the window makes for pointer and key events and applies results as they arrive;
//! with `--hidden-window` the same steps are sent to a hidden window as its messages. Both write the
//! same log.
//!
//! One step per line; `#` starts a comment. Coordinates are source pixels after EXIF rotation.
//!
//! ```text
//! settle [SECONDS]          wait until nothing is running (default 300 s)
//! wait MS                   let MS milliseconds pass, applying results
//! hover X,Y                 move the pointer to X,Y
//! path X,Y X,Y N MS         N pointer moves along the segment, MS milliseconds apart
//! click X,Y                 move there, wait for the hover, click, and settle
//! brush R                   set the brush radius in source pixels
//! stroke X,Y X,Y ...        paint a stroke with the brush and settle
//! pick N[,M...]             toggle the numbered objects of an open choice
//! confirm | cancel          Enter | Escape
//! undo | save               ⌘Z | ⌘S
//! expect decision one|several|nothing
//! expect removals N
//! expect choosing N         a choice is open with N objects
//! ```
//!
//! Every step that asks for work (click, stroke, pick, confirm, cancel, undo, save) settles after
//! it; hover and path do not, so pointer moves overtake unanswered ones as they would by hand.
//! At the end it settles, saves the image if a removal left it unsaved, and logs a summary of the
//! hover latencies.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;

use super::log::{Log, round3};
use super::{Config, Lab, Wake};
use crate::mask::Point;

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Settle(Duration),
    Wait(Duration),
    Hover(Point),
    Path {
        from: Point,
        to: Point,
        moves: usize,
        interval: Duration,
    },
    Click(Point),
    Brush(f32),
    Stroke(Vec<Point>),
    Pick(Vec<usize>),
    Confirm,
    Cancel,
    Undo,
    Save,
    ExpectDecision(String),
    ExpectRemovals(usize),
    ExpectChoosing(usize),
}

const SETTLE: Duration = Duration::from_secs(300);

fn point(s: &str) -> Result<Point, String> {
    let v: Vec<f32> = s
        .split(',')
        .map(|t| t.trim().parse::<f32>())
        .collect::<Result<_, _>>()
        .map_err(|_| format!("{s:?}: expected x,y"))?;
    match v[..] {
        [x, y] if x.is_finite() && y.is_finite() => Ok(Point::new(x, y)),
        _ => Err(format!("{s:?}: expected x,y")),
    }
}

fn number<T: std::str::FromStr>(s: &str) -> Result<T, String> {
    s.parse().map_err(|_| format!("{s:?}: expected a number"))
}

fn millis(s: &str) -> Result<Duration, String> {
    let v: f64 = number(s)?;
    if !(v >= 0.0 && v.is_finite()) {
        return Err(format!("{s:?}: expected milliseconds"));
    }
    Ok(Duration::from_secs_f64(v / 1e3))
}

pub fn parse(text: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        let e = |m: String| format!("script line {}: {m}", n + 1);
        let step = match words[..] {
            ["settle"] => Step::Settle(SETTLE),
            ["settle", s] => Step::Settle(Duration::from_secs_f64(number::<f64>(s).map_err(e)?)),
            ["wait", ms] => Step::Wait(millis(ms).map_err(e)?),
            ["hover", p] => Step::Hover(point(p).map_err(e)?),
            ["path", a, b, moves, ms] => Step::Path {
                from: point(a).map_err(e)?,
                to: point(b).map_err(e)?,
                moves: number::<usize>(moves).map_err(e)?.max(1),
                interval: millis(ms).map_err(e)?,
            },
            ["click", p] => Step::Click(point(p).map_err(e)?),
            ["brush", r] => Step::Brush(number(r).map_err(e)?),
            ["stroke", ref pts @ ..] if !pts.is_empty() => Step::Stroke(
                pts.iter()
                    .map(|p| point(p))
                    .collect::<Result<_, _>>()
                    .map_err(e)?,
            ),
            ["pick", list] => Step::Pick(
                list.split(',')
                    .map(number::<usize>)
                    .collect::<Result<_, _>>()
                    .map_err(e)?,
            ),
            ["confirm"] => Step::Confirm,
            ["cancel"] => Step::Cancel,
            ["undo"] => Step::Undo,
            ["save"] => Step::Save,
            ["expect", "decision", d] if ["one", "several", "nothing"].contains(&d) => {
                Step::ExpectDecision(d.into())
            }
            ["expect", "removals", n] => Step::ExpectRemovals(number(n).map_err(e)?),
            ["expect", "choosing", n] => Step::ExpectChoosing(number(n).map_err(e)?),
            _ => return Err(e(format!("cannot read {line:?}"))),
        };
        steps.push(step);
    }
    Ok(steps)
}

/// Runs a script file against a new lab writing into `config.out`, without a window.
pub fn run(config: Config, script: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(script).map_err(|e| format!("{}: {e}", script.display()))?;
    let steps = parse(&text)?;
    let log = Arc::new(Log::create(&config.out)?);
    eprintln!(
        "removal-lab script: writing to {} (log {})",
        config.out.display(),
        log.path()
            .map_or(String::new(), |p| p.display().to_string())
    );
    let wake: Wake = Arc::new(|| {});
    let mut lab = Lab::start(config, log.clone(), wake);
    let outcome = run_steps(&mut lab, &steps).and_then(|()| finish(&mut lab));
    summarize(&lab);
    outcome
}

/// What a script can observe of the lab.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Observed {
    pub loaded: bool,
    pub decision: Option<&'static str>,
    pub removals: usize,
    /// The number of objects offered, while a choice is open.
    pub choosing: Option<usize>,
    pub unsaved: bool,
    pub status: String,
}

impl Observed {
    pub fn of(lab: &Lab) -> Observed {
        Observed {
            loaded: lab.image().is_some(),
            decision: lab.last_decision(),
            removals: lab.removals(),
            choosing: lab.choice().map(|c| c.items.len()),
            unsaved: lab.unsaved(),
            status: lab.status(),
        }
    }
}

/// The person's side of the lab, as a script drives it: the lab itself without a window, or a
/// window through its messages.
pub trait Controls {
    fn pointer(&mut self, p: Point);
    fn press(&mut self, p: Point);
    fn release(&mut self);
    /// Holds or releases the brush modifier (Shift).
    fn hold_brush(&mut self, held: bool);
    fn set_brush_radius(&mut self, radius: f32);
    fn pick(&mut self, number: usize);
    fn confirm(&mut self);
    fn cancel(&mut self);
    fn undo(&mut self);
    fn save(&mut self);
    /// Lets time pass until `deadline`, applying results as they arrive.
    fn pump(&mut self, deadline: Instant);
    /// Waits until everything asked for so far has been handled and nothing is running.
    fn settle(&mut self, timeout: Duration) -> Result<(), String>;
    fn observe(&self) -> Observed;
}

impl Controls for Lab {
    fn pointer(&mut self, p: Point) {
        self.pointer_moved(p, Instant::now());
        self.poll();
    }

    fn press(&mut self, p: Point) {
        Lab::press(self, p, Instant::now(), 0.0);
    }

    fn release(&mut self) {
        Lab::release(self);
    }

    fn hold_brush(&mut self, held: bool) {
        self.set_shift(held);
    }

    fn set_brush_radius(&mut self, radius: f32) {
        Lab::set_brush_radius(self, radius);
    }

    fn pick(&mut self, number: usize) {
        if let Err(e) = self.toggle_pin(number) {
            self.log().event("script_error", json!({"error": e}));
        }
    }

    fn confirm(&mut self) {
        Lab::confirm(self);
    }

    fn cancel(&mut self) {
        Lab::cancel(self);
    }

    fn undo(&mut self) {
        Lab::undo(self);
    }

    fn save(&mut self) {
        Lab::save(self);
    }

    fn pump(&mut self, deadline: Instant) {
        loop {
            self.poll();
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            self.wait(deadline - now);
        }
    }

    fn settle(&mut self, timeout: Duration) -> Result<(), String> {
        let started = Instant::now();
        loop {
            self.poll();
            if !self.busy() {
                return Ok(());
            }
            if started.elapsed() > timeout {
                return Err(format!(
                    "still busy after {:.0} s: {}",
                    timeout.as_secs_f64(),
                    self.status()
                ));
            }
            self.wait(Duration::from_millis(100));
        }
    }

    fn observe(&self) -> Observed {
        Observed::of(self)
    }
}

/// Runs steps. Every step but `settle` and `wait` first waits for the image to load.
pub fn run_steps(c: &mut impl Controls, steps: &[Step]) -> Result<(), String> {
    for step in steps {
        if !matches!(step, Step::Settle(_) | Step::Wait(_)) {
            loaded(c)?;
        }
        match step {
            Step::Settle(timeout) => c.settle(*timeout)?,
            Step::Wait(d) => c.pump(Instant::now() + *d),
            Step::Hover(p) => c.pointer(*p),
            Step::Path {
                from,
                to,
                moves,
                interval,
            } => {
                let mut next = Instant::now();
                for i in 0..*moves {
                    let t = if *moves == 1 {
                        1.0
                    } else {
                        i as f32 / (*moves - 1) as f32
                    };
                    c.pointer(Point::new(
                        from.x + (to.x - from.x) * t,
                        from.y + (to.y - from.y) * t,
                    ));
                    next += *interval;
                    c.pump(next);
                }
            }
            Step::Click(p) => {
                c.pointer(*p);
                c.settle(SETTLE)?;
                c.press(*p);
                c.release();
                c.settle(SETTLE)?;
            }
            Step::Brush(r) => c.set_brush_radius(*r),
            Step::Stroke(points) => {
                c.hold_brush(true);
                c.press(points[0]);
                for p in &points[1..] {
                    c.pointer(*p);
                }
                c.release();
                c.hold_brush(false);
                c.settle(SETTLE)?;
            }
            Step::Pick(numbers) => {
                let open = c.observe().choosing;
                for &n in numbers {
                    match open {
                        Some(k) if (1..=k).contains(&n) => c.pick(n),
                        _ => {
                            return Err(format!(
                                "pick {n}: the open choice is {open:?}; {}",
                                c.observe().status
                            ));
                        }
                    }
                }
                c.settle(SETTLE)?;
            }
            Step::Confirm => {
                c.confirm();
                c.settle(SETTLE)?;
            }
            Step::Cancel => {
                c.cancel();
                c.settle(SETTLE)?;
            }
            Step::Undo => {
                c.undo();
                c.settle(SETTLE)?;
            }
            Step::Save => {
                c.save();
                c.settle(SETTLE)?;
            }
            Step::ExpectDecision(d) => {
                let o = c.observe();
                if o.decision != Some(d.as_str()) {
                    return Err(format!(
                        "expected decision {d}, got {:?}; {}",
                        o.decision, o.status
                    ));
                }
            }
            Step::ExpectRemovals(n) => {
                let o = c.observe();
                if o.removals != *n {
                    return Err(format!(
                        "expected {n} removals, got {}; {}",
                        o.removals, o.status
                    ));
                }
            }
            Step::ExpectChoosing(n) => {
                let o = c.observe();
                if o.choosing != Some(*n) {
                    return Err(format!(
                        "expected a choice of {n}, got {:?}; {}",
                        o.choosing, o.status
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Settles, then saves the image if a removal left it unsaved.
pub fn finish(c: &mut impl Controls) -> Result<(), String> {
    c.settle(SETTLE)?;
    if c.observe().unsaved {
        c.save();
        c.settle(SETTLE)?;
    }
    Ok(())
}

/// Waits until everything asked for so far has been handled and nothing is running.
pub fn settle(c: &mut impl Controls, timeout: Duration) -> Result<(), String> {
    c.settle(timeout)
}

fn loaded(c: &mut impl Controls) -> Result<(), String> {
    let started = Instant::now();
    loop {
        let o = c.observe();
        if o.loaded {
            return Ok(());
        }
        if o.status.starts_with("Could not open") || started.elapsed() > SETTLE {
            return Err(format!("the image did not load: {}", o.status));
        }
        c.pump(Instant::now() + Duration::from_millis(20));
    }
}

/// Nearest-rank percentile of sorted values.
fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil().max(1.0) as usize;
    Some(sorted[rank.min(sorted.len()) - 1])
}

fn stats(mut values: Vec<f64>) -> serde_json::Value {
    values.sort_by(f64::total_cmp);
    json!({
        "n": values.len(),
        "p50": percentile(&values, 50.0),
        "p95": percentile(&values, 95.0),
        "max": values.last(),
        "mean": (!values.is_empty()).then(|| round3(values.iter().sum::<f64>() / values.len() as f64)),
    })
}

/// Logs the hover latencies: for every hover result applied, the pointer event to the result
/// applied (where a window makes the overlay's handle and asks for a frame) and the decode time;
/// in a window, also the pointer event to the draw of the first frame with each new overlay.
pub fn summarize(lab: &Lab) {
    let log = lab.log();
    let hovers: Vec<_> = log
        .records("hover")
        .into_iter()
        .filter(|h| h["click"] == false && h["applied"] == true)
        .collect();
    let field = |rows: &[serde_json::Value], name: &str| {
        rows.iter()
            .filter_map(|h| h[name].as_f64())
            .collect::<Vec<_>>()
    };
    let changed: Vec<_> = hovers
        .iter()
        .filter(|h| h["changed"] == true)
        .cloned()
        .collect();
    let replaced: u64 = log
        .records("hover")
        .iter()
        .filter_map(|h| h["replaced"].as_u64())
        .sum();
    let decisions: Vec<_> = log
        .records("decision")
        .iter()
        .map(|d| d["decision"].clone())
        .collect();
    let drawn = log.records("hover_drawn");
    log.event(
        "script_summary",
        json!({
            "hover_results_applied": hovers.len(),
            "hover_points_replaced_unanswered": replaced,
            "pointer_to_result_ms": stats(field(&hovers, "pointer_to_result_ms")),
            "pointer_to_result_ms_changed": stats(field(&changed, "pointer_to_result_ms")),
            "pointer_to_draw_ms": stats(field(&drawn, "pointer_to_draw_ms")),
            "decode_ms": stats(field(&hovers, "decode_ms")),
            "raster_ms": stats(field(&changed, "raster_ms")),
            "decisions": decisions,
            "removals": lab.removals(),
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_parse() {
        let steps = parse(
            "# a comment\nsettle\nhover 10,20\npath 0,0 10,10 5 8 # trailing\nclick 1.5,2\n\
             brush 40\nstroke 1,1 2,2 3,3\npick 1,3\nconfirm\ncancel\nundo\nsave\nwait 16\n\
             expect decision several\nexpect removals 2\nexpect choosing 3\nsettle 5\n",
        )
        .unwrap();
        assert_eq!(steps.len(), 16);
        assert_eq!(steps[1], Step::Hover(Point::new(10.0, 20.0)));
        assert_eq!(
            steps[2],
            Step::Path {
                from: Point::new(0.0, 0.0),
                to: Point::new(10.0, 10.0),
                moves: 5,
                interval: Duration::from_millis(8),
            }
        );
        assert_eq!(
            steps[5],
            Step::Stroke(vec![
                Point::new(1.0, 1.0),
                Point::new(2.0, 2.0),
                Point::new(3.0, 3.0)
            ])
        );
        assert_eq!(steps[6], Step::Pick(vec![1, 3]));
        assert_eq!(steps[15], Step::Settle(Duration::from_secs(5)));
        for bad in [
            "hover 1",
            "stroke",
            "expect decision maybe",
            "jump 1,2",
            "wait -1",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn percentiles_are_nearest_rank() {
        let v: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(percentile(&v, 50.0), Some(10.0));
        assert_eq!(percentile(&v, 95.0), Some(19.0));
        assert_eq!(percentile(&v, 100.0), Some(20.0));
        assert_eq!(percentile(&[], 50.0), None);
    }
}

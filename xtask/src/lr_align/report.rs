//! The measurement's figures: small static SVG line charts and PNG contact sheets, written beside
//! each setting's page. Lightroom is drawn as points, Luxforge at the identity conversion as a
//! solid line and Luxforge through the fitted map as a dashed line with square markers, so no
//! series is told from another by colour alone; every chart's numbers are also in its page's
//! tables.
use crate::*;

/// The three series' colours: a categorical palette validated for colour-vision deficiency on the
/// light surface (the dataviz palette's first three slots).
pub const LIGHTROOM: &str = "#2a78d6";
pub const LUXFORGE: &str = "#eb6834";
pub const CALIBRATED: &str = "#1baf7a";
const SURFACE: &str = "#fcfcfb";
const INK: &str = "#0b0b0b";
const MUTED: &str = "#52514e";
const GRID: &str = "#e4e3df";

/// How one series is drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// Circles only.
    Points,
    /// A solid line.
    Line,
    /// A dashed line with square markers.
    Dashed,
}

pub struct Series {
    pub name: String,
    pub colour: &'static str,
    pub mark: Mark,
    pub points: Vec<[f64; 2]>,
}

impl Series {
    pub fn new(
        name: impl Into<String>,
        colour: &'static str,
        mark: Mark,
        points: Vec<[f64; 2]>,
    ) -> Self {
        Self {
            name: name.into(),
            colour,
            mark,
            points: points
                .into_iter()
                .filter(|p| p[0].is_finite() && p[1].is_finite())
                .collect(),
        }
    }
}

fn nice_step(span: f64) -> f64 {
    let raw = span / 5.0;
    let magnitude = 10f64.powf(raw.abs().log10().floor());
    let normalized = raw / magnitude;
    let nice = if normalized < 1.5 {
        1.0
    } else if normalized < 3.5 {
        2.0
    } else if normalized < 7.5 {
        5.0
    } else {
        10.0
    };
    nice * magnitude
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A line chart of `series` on one pair of axes, with a legend above the plot.
pub fn chart(title: &str, x_label: &str, y_label: &str, series: &[Series]) -> String {
    let (width, height) = (640.0, 400.0);
    let (left, right, top, bottom) = (64.0, 16.0, 64.0, 48.0);
    let all: Vec<[f64; 2]> = series
        .iter()
        .flat_map(|s| s.points.iter().copied())
        .collect();
    let fold = |pick: fn(&[f64; 2]) -> f64, min: bool| {
        all.iter().map(pick).fold(
            if min {
                f64::INFINITY
            } else {
                f64::NEG_INFINITY
            },
            |a, b| {
                if min { a.min(b) } else { a.max(b) }
            },
        )
    };
    let (mut x0, mut x1) = (fold(|p| p[0], true), fold(|p| p[0], false));
    let (mut y0, mut y1) = (fold(|p| p[1], true), fold(|p| p[1], false));
    if !x0.is_finite() {
        (x0, x1, y0, y1) = (0.0, 1.0, 0.0, 1.0);
    }
    if (x1 - x0).abs() < 1e-12 {
        x1 = x0 + 1.0;
    }
    if (y1 - y0).abs() < 1e-12 {
        y0 -= 0.5;
        y1 += 0.5;
    }
    let pad = (y1 - y0) * 0.06;
    let (y0, y1) = (y0 - pad, y1 + pad);
    let plot_w = width - left - right;
    let plot_h = height - top - bottom;
    let sx = |x: f64| left + (x - x0) / (x1 - x0) * plot_w;
    let sy = |y: f64| top + (1.0 - (y - y0) / (y1 - y0)) * plot_h;
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {width} {height}\" width=\"{width}\" height=\"{height}\" font-family=\"system-ui, sans-serif\" font-size=\"12\">\n<rect width=\"100%\" height=\"100%\" fill=\"{SURFACE}\"/>\n<text x=\"{left}\" y=\"20\" fill=\"{INK}\" font-size=\"14\" font-weight=\"600\">{}</text>\n",
        escape(title)
    );
    for (axis, (a, b)) in [("x", (x0, x1)), ("y", (y0, y1))] {
        let step = nice_step(b - a);
        let mut tick = (a / step).ceil() * step;
        while tick <= b + 1e-9 {
            let label = if step >= 1.0 {
                format!("{tick:.0}")
            } else {
                format!("{tick:.*}", (-step.log10().floor()) as usize)
            };
            if axis == "x" {
                let x = sx(tick);
                svg += &format!(
                    "<line x1=\"{x:.1}\" y1=\"{top}\" x2=\"{x:.1}\" y2=\"{:.1}\" stroke=\"{GRID}\"/>\n<text x=\"{x:.1}\" y=\"{:.1}\" fill=\"{MUTED}\" text-anchor=\"middle\">{label}</text>\n",
                    top + plot_h,
                    top + plot_h + 16.0
                );
            } else {
                let y = sy(tick);
                svg += &format!(
                    "<line x1=\"{left}\" y1=\"{y:.1}\" x2=\"{:.1}\" y2=\"{y:.1}\" stroke=\"{GRID}\"/>\n<text x=\"{:.1}\" y=\"{:.1}\" fill=\"{MUTED}\" text-anchor=\"end\">{label}</text>\n",
                    left + plot_w,
                    left - 6.0,
                    y + 4.0
                );
            }
            tick += step;
        }
    }
    svg += &format!(
        "<text x=\"{:.1}\" y=\"{:.1}\" fill=\"{MUTED}\" text-anchor=\"middle\">{}</text>\n<text transform=\"translate(14 {:.1}) rotate(-90)\" fill=\"{MUTED}\" text-anchor=\"middle\">{}</text>\n",
        left + plot_w / 2.0,
        height - 8.0,
        escape(x_label),
        top + plot_h / 2.0,
        escape(y_label)
    );
    let mut legend_x = left;
    for s in series {
        let swatch = match s.mark {
            Mark::Points => format!(
                "<circle cx=\"{:.1}\" cy=\"40\" r=\"4\" fill=\"{}\"/>",
                legend_x + 8.0,
                s.colour
            ),
            Mark::Line => format!(
                "<line x1=\"{legend_x:.1}\" y1=\"40\" x2=\"{:.1}\" y2=\"40\" stroke=\"{}\" stroke-width=\"2\"/>",
                legend_x + 16.0,
                s.colour
            ),
            Mark::Dashed => format!(
                "<line x1=\"{legend_x:.1}\" y1=\"40\" x2=\"{:.1}\" y2=\"40\" stroke=\"{}\" stroke-width=\"2\" stroke-dasharray=\"5 3\"/>",
                legend_x + 16.0,
                s.colour
            ),
        };
        svg += &format!(
            "{swatch}<text x=\"{:.1}\" y=\"44\" fill=\"{INK}\">{}</text>\n",
            legend_x + 22.0,
            escape(&s.name)
        );
        legend_x += 30.0 + 7.0 * s.name.chars().count() as f64;
    }
    for s in series {
        let mut points = s.points.clone();
        points.sort_by(|a, b| a[0].total_cmp(&b[0]));
        if s.mark != Mark::Points && points.len() > 1 {
            let path: Vec<String> = points
                .iter()
                .map(|p| format!("{:.1},{:.1}", sx(p[0]), sy(p[1])))
                .collect();
            let dash = if s.mark == Mark::Dashed {
                " stroke-dasharray=\"5 3\""
            } else {
                ""
            };
            svg += &format!(
                "<polyline points=\"{}\" fill=\"none\" stroke=\"{}\" stroke-width=\"2\"{dash}/>\n",
                path.join(" "),
                s.colour
            );
        }
        for p in &points {
            match s.mark {
                Mark::Points => {
                    svg += &format!(
                        "<circle cx=\"{:.1}\" cy=\"{:.1}\" r=\"4.5\" fill=\"{}\" stroke=\"{SURFACE}\" stroke-width=\"2\"><title>{}: {:.4}, {:.4}</title></circle>\n",
                        sx(p[0]),
                        sy(p[1]),
                        s.colour,
                        escape(&s.name),
                        p[0],
                        p[1]
                    );
                }
                Mark::Dashed if points.len() <= 24 => {
                    svg += &format!(
                        "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"8\" height=\"8\" rx=\"1\" fill=\"{}\" stroke=\"{SURFACE}\" stroke-width=\"2\"><title>{}: {:.4}, {:.4}</title></rect>\n",
                        sx(p[0]) - 4.0,
                        sy(p[1]) - 4.0,
                        s.colour,
                        escape(&s.name),
                        p[0],
                        p[1]
                    );
                }
                _ => {}
            }
        }
    }
    svg + "</svg>\n"
}

/// A contact sheet: one row per label, one column per frame (Luxforge, Luxforge calibrated,
/// Lightroom), each cell its thumbnail or mid grey when there is none, with a 2-pixel gap; the
/// row labels in a `.txt` beside it.
pub fn contact_sheet(
    path: &Path,
    columns: &[&str],
    rows: &[(String, Vec<Option<image::RgbImage>>)],
) -> Result {
    let cell_w = rows
        .iter()
        .flat_map(|(_, cells)| cells.iter().flatten().map(|t| t.width()))
        .max()
        .unwrap_or(16);
    let cell_h = rows
        .iter()
        .flat_map(|(_, cells)| cells.iter().flatten().map(|t| t.height()))
        .max()
        .unwrap_or(16);
    let gap = 2;
    let width = columns.len() as u32 * (cell_w + gap) + gap;
    let height = rows.len().max(1) as u32 * (cell_h + gap) + gap;
    let mut sheet = image::RgbImage::from_pixel(width, height, image::Rgb([252, 252, 251]));
    for (row, (_, cells)) in rows.iter().enumerate() {
        for (column, cell) in cells.iter().enumerate() {
            let (x0, y0) = (
                gap + column as u32 * (cell_w + gap),
                gap + row as u32 * (cell_h + gap),
            );
            match cell {
                Some(thumb) => {
                    for (x, y, pixel) in thumb.enumerate_pixels() {
                        sheet.put_pixel(x0 + x, y0 + y, *pixel);
                    }
                }
                None => {
                    for y in 0..cell_h {
                        for x in 0..cell_w {
                            sheet.put_pixel(x0 + x, y0 + y, image::Rgb([128, 128, 128]));
                        }
                    }
                }
            }
        }
    }
    sheet.save(path)?;
    let mut text = format!(
        "columns, left to right: {}\nrows, top to bottom:\n",
        columns.join(" | ")
    );
    for (label, _) in rows {
        text += label;
        text.push('\n');
    }
    fs::write(path.with_extension("txt"), text)?;
    Ok(())
}

/// A number for a table: four significant places, `–` for none.
pub fn number(value: Option<f64>) -> String {
    match value {
        Some(v) if v.is_finite() => {
            if v != 0.0 && (v.abs() >= 1e4 || v.abs() < 1e-3) {
                format!("{v:.3e}")
            } else {
                format!("{v:.3}")
            }
        }
        _ => "–".into(),
    }
}

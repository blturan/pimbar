//! The menu bar icon, drawn here as vector shapes rather than shipped as images.
//!
//! A lock, then what needs a glance: a blue spinner with the tasks in flight,
//! a green ✓ with the roles active, an amber ● with what's waiting on
//! someone (approvals for you, and your requests or stops waiting), a red !
//! when something failed. The marks carry their own colour; the
//! lock is drawn black or white to match the menu bar, as macOS would tint a
//! template.
//!
//! Drawn on the pixel grid at 2x: straight edges sit on whole pixels and
//! strokes are whole pixels wide, so nothing is smeared across two pixels.
//! Shapes are signed distance functions, which anti-alias the curves.

use std::f32::consts::PI;

/// 18pt at 2x: macOS scales status item images to the menu bar's height.
const H: usize = 36;
/// The centre line everything is aligned on.
const MID: f32 = 18.0;
/// Digits: 8 × 15 with 3px strokes centred on pixel centres, so their edges are whole pixels.
const DIGIT_TOP: f32 = 10.5;
const DIGIT_HALF: f32 = 1.5;

pub struct Badge {
    /// Tasks still running (a spinner), and what's waiting on an approver (a
    /// dot, with `pending`: approvals waiting for you).
    pub running: usize,
    pub waiting: usize,
    pub active: usize,
    pub pending: usize,
    pub error: bool,
    pub dark: bool,
    /// Animation frame, for the spinner.
    pub frame: u32,
}

type P = (f32, f32);
type Rgb = [f32; 3];

fn tone(dark: bool, light: u32, dark_hex: u32) -> Rgb {
    let c = if dark { dark_hex } else { light };
    [(c >> 16 & 0xff) as f32, (c >> 8 & 0xff) as f32, (c & 0xff) as f32]
}

struct Layer {
    color: Rgb,
    alpha: f32,
    shape: Box<dyn Fn(P) -> f32>,
}

/// Draws the icon; returns straight RGBA and its width and height.
pub fn render(b: &Badge) -> (Vec<u8>, u32, u32) {
    // The panel's tones (ui/style.css), light and dark.
    let ink = tone(b.dark, 0x000000, 0xffffff);
    let accent = tone(b.dark, 0x1d6fe0, 0x5aa2ff);
    let pass = tone(b.dark, 0x12a150, 0x3ccf7c);
    let warn = tone(b.dark, 0xc98a00, 0xf5b82e);
    let fail = tone(b.dark, 0xd92d20, 0xff6b5e);

    let mut layers = vec![Layer { color: ink, alpha: if b.dark { 1.0 } else { 0.9 }, shape: Box::new(lock) }];
    let mut x = 25.0;

    if b.running > 0 {
        let turn = b.frame as f32 * 2.0 * PI / 12.0;
        x = mark(&mut layers, x, accent, 15.0, b.running, move |p| spinner(p, turn));
    }
    if b.active > 0 {
        x = mark(&mut layers, x, pass, 15.0, b.active, check);
    }
    if b.pending + b.waiting > 0 {
        x = mark(&mut layers, x, warn, 15.0, b.pending + b.waiting, dot);
    }
    if b.error {
        x += 6.0;
        let at = x;
        layers.push(Layer { color: fail, alpha: 1.0, shape: Box::new(move |p| bang((p.0 - at, p.1))) });
        x += 3.0;
    }

    let w = x.ceil() as usize + 1;
    let mut px = vec![[0.0f32; 4]; w * H];
    for layer in &layers {
        for y in 0..H {
            for x in 0..w {
                let cover = (0.5 - (layer.shape)((x as f32 + 0.5, y as f32 + 0.5))).clamp(0.0, 1.0);
                if cover > 0.0 {
                    over(&mut px[y * w + x], layer.color, cover * layer.alpha);
                }
            }
        }
    }
    let rgba = px.iter().flat_map(|p| p.map(|c| c.round() as u8)).collect();
    (rgba, w as u32, H as u32)
}

/// Adds a glyph and its count after `x`; returns where they end.
fn mark(layers: &mut Vec<Layer>, x: f32, color: Rgb, width: f32, n: usize, glyph: impl Fn(P) -> f32 + 'static) -> f32 {
    let gx = x + 7.0;
    layers.push(Layer { color, alpha: 1.0, shape: Box::new(move |p| glyph((p.0 - gx, p.1))) });
    let mut dx = gx + width + 3.0;
    for d in n.min(99).to_string().bytes().map(|d| d - b'0') {
        // Stems on pixel centres, so 3px strokes have whole-pixel edges.
        let at = dx.floor() + 1.5 - if d == 1 { 2.0 } else { 0.0 };
        layers.push(Layer { color, alpha: 1.0, shape: Box::new(move |p| digit(p, at, d)) });
        dx += if d == 1 { 8.0 } else { 13.0 };
    }
    dx - 2.0
}

/// The lock: a 21 × 17 body with a keyhole, and a 4px shackle on short
/// straight legs, 28px tall, centred on `MID`. Straight edges sit on whole
/// pixels (the body and the legs' sides), so nothing is smeared across two.
fn lock(p: P) -> f32 {
    let body = rect(p, (2.0, 15.0), (23.0, 32.0), 4.0);
    let keyhole = circle(p, (12.5, 21.8), 2.6).min(rect(p, (11.0, 21.8), (14.0, 27.4), 1.2));
    let shackle = arc(p, (12.5, 11.5), 5.5, PI, 2.0 * PI).min(line(p, (7.0, 11.5), (7.0, 16.0))).min(line(p, (18.0, 11.5), (18.0, 16.0))) - 2.0;
    body.max(-keyhole).min(shackle)
}

fn check(p: P) -> f32 {
    line(p, (1.5, MID + 0.5), (5.5, MID + 4.5)).min(line(p, (5.5, MID + 4.5), (13.5, MID - 5.0))) - 1.6
}

/// A filled dot: something waiting on someone.
fn dot(p: P) -> f32 {
    circle(p, (7.5, MID), 6.5)
}

/// A three-quarter arc, turned by `turn`.
fn spinner(p: P, turn: f32) -> f32 {
    let c = (7.5, MID);
    arc(p, c, 6.0, turn, turn + 1.5 * PI) - 1.5
}

fn bang(p: P) -> f32 {
    rect(p, (0.0, 9.0), (3.0, 22.0), 1.5).min(circle(p, (1.5, 26.0), 1.8))
}

/// A rounded monoline digit in an 8 × 15 box, its left stem at `x`.
fn digit(p: P, x: f32, d: u8) -> f32 {
    let q = (p.0 - x, p.1 - DIGIT_TOP);
    let deg = |a: f32| a * PI / 180.0;
    let l = |a: P, b: P| line(q, a, b);
    let arc = |c: P, r: f32, from: f32, to: f32| arc(q, c, r, deg(from), deg(to));
    let dist = match d {
        0 => arc((4.0, 4.0), 4.0, 180.0, 360.0).min(arc((4.0, 11.0), 4.0, 0.0, 180.0)).min(l((0.0, 4.0), (0.0, 11.0))).min(l((8.0, 4.0), (8.0, 11.0))),
        1 => l((5.0, 0.0), (5.0, 15.0)).min(l((1.6, 2.8), (5.0, 0.0))),
        2 => arc((4.0, 4.0), 4.0, 195.0, 395.0).min(l((7.28, 6.29), (0.0, 15.0))).min(l((0.0, 15.0), (8.0, 15.0))),
        3 => arc((4.0, 3.75), 3.75, 205.0, 450.0).min(arc((4.0, 11.0), 4.0, 270.0, 515.0)),
        4 => l((6.0, 0.0), (6.0, 15.0)).min(l((6.0, 0.0), (0.0, 11.0))).min(l((0.0, 11.0), (8.0, 11.0))),
        5 => l((7.5, 0.0), (1.0, 0.0)).min(l((1.0, 0.0), (0.7, 7.0))).min(arc((3.9, 10.5), 4.5, 222.0, 510.0)),
        6 => circle(q, (4.0, 11.0), 4.0).abs().min(l((6.4, 0.0), (0.4, 9.6))),
        7 => l((0.0, 0.0), (8.0, 0.0)).min(l((8.0, 0.0), (2.6, 15.0))),
        8 => circle(q, (4.0, 3.6), 3.6).abs().min(circle(q, (4.0, 10.9), 4.1).abs()),
        _ => circle(q, (4.0, 4.0), 4.0).abs().min(l((7.6, 5.4), (1.6, 15.0))),
    };
    dist - DIGIT_HALF
}

/// Paints `color` at `alpha` over a straight-alpha pixel.
fn over(dst: &mut [f32; 4], color: Rgb, alpha: f32) {
    let da = dst[3] / 255.0;
    let out = alpha + da * (1.0 - alpha);
    for i in 0..3 {
        dst[i] = (color[i] * alpha + dst[i] * da * (1.0 - alpha)) / out;
    }
    dst[3] = out * 255.0;
}

fn len(x: f32, y: f32) -> f32 {
    (x * x + y * y).sqrt()
}

fn circle(p: P, c: P, r: f32) -> f32 {
    len(p.0 - c.0, p.1 - c.1) - r
}

/// Distance to the segment a–b (zero-width; subtract a radius for a stroke).
fn line(p: P, a: P, b: P) -> f32 {
    let (pa, ba) = ((p.0 - a.0, p.1 - a.1), (b.0 - a.0, b.1 - a.1));
    let t = ((pa.0 * ba.0 + pa.1 * ba.1) / (ba.0 * ba.0 + ba.1 * ba.1)).clamp(0.0, 1.0);
    len(pa.0 - ba.0 * t, pa.1 - ba.1 * t)
}

/// Distance to an arc of radius `r` from angle `from` to `to` (radians, clockwise
/// on screen, 0 pointing right; `to` may pass 2π), with round ends.
fn arc(p: P, c: P, r: f32, from: f32, to: f32) -> f32 {
    let (dx, dy) = (p.0 - c.0, p.1 - c.1);
    let t = from + (dy.atan2(dx) - from).rem_euclid(2.0 * PI);
    if t <= to {
        return (len(dx, dy) - r).abs();
    }
    let end = |a: f32| len(p.0 - (c.0 + r * a.cos()), p.1 - (c.1 + r * a.sin()));
    end(from).min(end(to))
}

fn rect(p: P, min: P, max: P, r: f32) -> f32 {
    let c = ((min.0 + max.0) / 2.0, (min.1 + max.1) / 2.0);
    let half = ((max.0 - min.0) / 2.0 - r, (max.1 - min.1) / 2.0 - r);
    let q = ((p.0 - c.0).abs() - half.0, (p.1 - c.1).abs() - half.1);
    len(q.0.max(0.0), q.1.max(0.0)) + q.0.max(q.1).min(0.0) - r
}

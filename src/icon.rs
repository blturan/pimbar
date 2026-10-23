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
//! Shapes are signed distance functions, which anti-alias the curves. The
//! counts are text, in the menu bar's own font (SF), drawn by Core Text.

use std::f32::consts::PI;

/// 18pt at 2x: macOS scales status item images to the menu bar's height.
const H: usize = 36;
/// The centre line everything is aligned on.
const MID: f32 = 18.0;
/// The counts' font size, in pixels at 2x: 11pt, as tall as the marks beside them.
const COUNT_SIZE: f64 = 22.0;

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
    // On a whole pixel, so the text lands where Core Text drew it.
    let at = (gx + width + 4.0).round();
    let count = Text::new(&n.min(99).to_string());
    let end = at + count.width as f32;
    layers.push(Layer { color, alpha: 1.0, shape: Box::new(move |p| count.shape((p.0 - at, p.1))) });
    end
}

/// A count in the system font (SF, semibold, with digits all one width), drawn
/// by Core Text: how much of each pixel it covers, `width` wide and `H` high.
struct Text {
    width: usize,
    cover: Vec<u8>,
}

impl Text {
    fn new(s: &str) -> Text {
        use objc2_app_kit::{NSFont, NSFontWeightSemibold};
        use objc2_core_foundation::{CFAttributedString, CFDictionary, CFString, CFType, kCFBooleanTrue};
        use objc2_core_graphics::{CGBitmapContextCreate, CGBitmapContextGetData, CGContext, CGImageAlphaInfo};
        use objc2_core_text::{CTFont, CTLine, kCTFontAttributeName, kCTForegroundColorFromContextAttributeName};

        // SAFETY: plain Core Foundation, Core Text and Core Graphics calls, with
        // live arguments; the bitmap is ours, and read only after drawing.
        unsafe {
            let font = NSFont::monospacedDigitSystemFontOfSize_weight(COUNT_SIZE, NSFontWeightSemibold);
            // NSFont and CTFont are the same object ("toll-free bridged").
            let ct = &*(objc2::rc::Retained::as_ptr(&font).cast::<CTFont>());
            let keys: [&CFString; 2] = [kCTFontAttributeName, kCTForegroundColorFromContextAttributeName];
            let values: [&CFType; 2] = [ct.as_ref(), kCFBooleanTrue.expect("kCFBooleanTrue").as_ref()];
            let attributes = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
            let text = CFAttributedString::new(None, Some(&CFString::from_str(s)), Some(attributes.as_opaque())).expect("attributed string");
            let line = CTLine::with_attributed_string(&text);
            let advance = line.typographic_bounds(std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
            let width = advance.ceil() as usize + 1;

            // Coverage only: an alpha-only bitmap, one byte a pixel, rows from the top.
            let Some(context) = CGBitmapContextCreate(std::ptr::null_mut(), width, H, 8, width, None, CGImageAlphaInfo::Only.0) else {
                return Text { width, cover: vec![0; width * H] };
            };
            CGContext::set_gray_fill_color(Some(&context), 1.0, 1.0);
            // Centred on the marks' middle (Core Graphics counts from the bottom),
            // the baseline on a whole pixel so the digits' feet are crisp.
            let baseline = (H as f64 - MID as f64 - font.capHeight() / 2.0).round();
            CGContext::set_text_position(Some(&context), 0.0, baseline);
            line.draw(&context);
            let data = CGBitmapContextGetData(Some(&context)).cast::<u8>();
            Text { width, cover: std::slice::from_raw_parts(data, width * H).to_vec() }
        }
    }

    /// As a shape, for the layers: the point's pixel, as covered as Core Text left it.
    fn shape(&self, p: P) -> f32 {
        let (x, y) = (p.0.floor(), p.1.floor());
        if x < 0.0 || y < 0.0 || x as usize >= self.width || y as usize >= H {
            return 1.0;
        }
        0.5 - f32::from(self.cover[y as usize * self.width + x as usize]) / 255.0
    }
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

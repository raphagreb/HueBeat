//! Album-artwork handling: dominant colour, square cover, JPEG data URL.

use base64::Engine;
use image::imageops::FilterType;
use image::{DynamicImage, Rgb, RgbImage};

// ------------------------------- colour ------------------------------------

fn rgb_to_hsv(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let maxc = r.max(g).max(b);
    let minc = r.min(g).min(b);
    let v = maxc;
    let delta = maxc - minc;
    let s = if maxc <= 0.0 { 0.0 } else { delta / maxc };
    let mut h = 0.0;
    if delta > 0.0 {
        if maxc == r {
            h = ((g - b) / delta) % 6.0;
        } else if maxc == g {
            h = (b - r) / delta + 2.0;
        } else {
            h = (r - g) / delta + 4.0;
        }
        h /= 6.0;
        if h < 0.0 {
            h += 1.0;
        }
    }
    (h, s, v)
}

fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (f64, f64, f64) {
    if s <= 0.0 {
        return (v, v, v);
    }
    let h = (h.fract() + 1.0).fract() * 6.0;
    let i = h.floor() as i64;
    let f = h - i as f64;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match i.rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

/// Pick the most vivid colour from the cover, then saturate it.
pub fn dominant_color(img: &DynamicImage, sat_min: f32, realce_sat: f32) -> [u8; 3] {
    let small = image::imageops::resize(&img.to_rgb8(), 48, 48, FilterType::CatmullRom);
    let mut buckets = [[0.0f64; 4]; 12];
    for pixel in small.pixels() {
        let [r, g, b] = pixel.0;
        let (h, s, v) = rgb_to_hsv(r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
        if v < 0.2 || s < 0.15 {
            continue;
        }
        let weight = s * s * v;
        let bucket = ((h * 12.0) as i64).rem_euclid(12) as usize;
        buckets[bucket][0] += weight;
        buckets[bucket][1] += r as f64 * weight;
        buckets[bucket][2] += g as f64 * weight;
        buckets[bucket][3] += b as f64 * weight;
    }

    let best = buckets
        .iter()
        .max_by(|a, b| a[0].total_cmp(&b[0]))
        .copied()
        .unwrap_or([0.0; 4]);
    if best[0] < 3.0 {
        return [255, 255, 255];
    }

    let r = best[1] / best[0];
    let g = best[2] / best[0];
    let b = best[3] / best[0];
    let (h, s, _) = rgb_to_hsv(r / 255.0, g / 255.0, b / 255.0);
    let s = 1.0f64.min((s * realce_sat as f64).max(sat_min as f64));
    let (r, g, b) = hsv_to_rgb(h, s, 1.0);
    [
        (r * 255.0) as u8,
        (g * 255.0) as u8,
        (b * 255.0) as u8,
    ]
}

// ------------------------------- cover -------------------------------------

fn dark_row(img: &RgbImage, y: u32, step: u32) -> bool {
    (0..img.width()).step_by(step.max(1) as usize).all(|x| {
        let p = img.get_pixel(x, y).0;
        p[0].max(p[1]).max(p[2]) <= 26
    })
}

fn dark_column(img: &RgbImage, x: u32, step: u32) -> bool {
    (0..img.height()).step_by(step.max(1) as usize).all(|y| {
        let p = img.get_pixel(x, y).0;
        p[0].max(p[1]).max(p[2]) <= 26
    })
}

/// Remove uniform black letterbox / pillarbox bars.
fn crop_bars(img: &RgbImage) -> RgbImage {
    let (w, h) = (img.width(), img.height());
    if w < 8 || h < 8 {
        return img.clone();
    }
    let step_x = (w / 80).max(1);
    let step_y = (h / 80).max(1);

    let mut top = 0u32;
    while top < h && dark_row(img, top, step_x) {
        top += 1;
    }
    let mut bottom = h.saturating_sub(1) as i64;
    while bottom >= 0 && dark_row(img, bottom as u32, step_x) {
        bottom -= 1;
    }
    let mut left = 0u32;
    while left < w && dark_column(img, left, step_y) {
        left += 1;
    }
    let mut right = w.saturating_sub(1) as i64;
    while right >= 0 && dark_column(img, right as u32, step_y) {
        right -= 1;
    }

    if top > h / 4 {
        top = 0;
    }
    if (h as i64 - 1 - bottom) > (h / 4) as i64 {
        bottom = h as i64 - 1;
    }
    if left > w / 4 {
        left = 0;
    }
    if (w as i64 - 1 - right) > (w / 4) as i64 {
        right = w as i64 - 1;
    }
    if bottom < top as i64 || right < left as i64 {
        return img.clone();
    }
    let cw = (right - left as i64 + 1) as u32;
    let ch = (bottom - top as i64 + 1) as u32;
    if cw < 32 || ch < 32 {
        return img.clone();
    }
    if top == 0 && bottom == h as i64 - 1 && left == 0 && right == w as i64 - 1 {
        return img.clone();
    }
    image::imageops::crop_imm(img, left, top, cw, ch).to_image()
}

/// Scale to fill and center-crop (Pillow's ``ImageOps.fit``).
fn fill_square(img: &RgbImage, side: u32) -> RgbImage {
    let (w, h) = (img.width().max(1), img.height().max(1));
    let scale = (side as f64 / w as f64).max(side as f64 / h as f64);
    let nw = ((w as f64 * scale).round() as u32).max(1);
    let nh = ((h as f64 * scale).round() as u32).max(1);
    let resized = image::imageops::resize(img, nw, nh, FilterType::Lanczos3);
    let x = (nw - side) / 2;
    let y = (nh - side) / 2;
    image::imageops::crop_imm(&resized, x, y, side, side).to_image()
}

fn scale_to_fit(img: &RgbImage, side: u32) -> RgbImage {
    let (w, h) = (img.width().max(1), img.height().max(1));
    let scale = (side as f64 / w as f64).min(side as f64 / h as f64);
    let nw = ((w as f64 * scale).round() as u32).max(1);
    let nh = ((h as f64 * scale).round() as u32).max(1);
    image::imageops::resize(img, nw, nh, FilterType::Lanczos3)
}

fn brighten(img: &RgbImage, factor: f32) -> RgbImage {
    let mut out = img.clone();
    for pixel in out.pixels_mut() {
        for channel in pixel.0.iter_mut() {
            *channel = ((*channel as f32 * factor).round() as i32).clamp(0, 255) as u8;
        }
    }
    out
}

fn saturate(img: &RgbImage, factor: f32) -> RgbImage {
    let mut out = img.clone();
    for pixel in out.pixels_mut() {
        let [r, g, b] = pixel.0;
        let (h, s, v) = rgb_to_hsv(r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
        let (r, g, b) = hsv_to_rgb(h, (s * factor as f64).clamp(0.0, 1.0), v);
        pixel.0 = [
            (r * 255.0).round() as u8,
            (g * 255.0).round() as u8,
            (b * 255.0).round() as u8,
        ];
    }
    out
}

/// Square cover: cropped when already square-ish, blurred fill otherwise.
fn square_cover(img: &RgbImage, side: u32) -> RgbImage {
    let cropped = crop_bars(img);
    let (w, h) = (cropped.width().max(1), cropped.height().max(1));
    let ratio = w as f64 / h as f64;
    if (ratio - 1.0).abs() < 0.06 {
        return fill_square(&cropped, side);
    }

    let background = fill_square(&cropped, side);
    let background = image::imageops::blur(&background, 22.0);
    let background = brighten(&background, 0.52);
    let mut background = saturate(&background, 0.85);

    let foreground = scale_to_fit(&cropped, side);
    let (fw, fh) = (foreground.width(), foreground.height());
    image::imageops::overlay(
        &mut background,
        &foreground,
        ((side - fw) / 2) as i64,
        ((side - fh) / 2) as i64,
    );
    image::imageops::unsharpen(&background, 1.2, 3)
}

/// Encode the square cover as a data URL for the webview.
pub fn data_url(img: &DynamicImage) -> Option<String> {
    let square = square_cover(&img.to_rgb8(), 640);
    let mut buffer: Vec<u8> = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buffer, 88);
    encoder.encode_image(&square).ok()?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(buffer);
    Some(format!("data:image/jpeg;base64,{encoded}"))
}

/// Decode cover bytes coming from the media session.
pub fn decode(bytes: &[u8]) -> Option<DynamicImage> {
    image::load_from_memory(bytes).ok()
}

/// Small helper used by tests / callers that already have RGB data.
#[allow(dead_code)]
pub fn to_rgb(img: &DynamicImage) -> RgbImage {
    img.to_rgb8()
}

/// Uniform colour used when an image cannot be read.
#[allow(dead_code)]
pub fn solid(color: [u8; 3]) -> RgbImage {
    RgbImage::from_pixel(48, 48, Rgb(color))
}

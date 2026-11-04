use std::{error::Error, fs, path::Path};

use image::{DynamicImage, ImageReader, Rgb, RgbImage, Rgba, RgbaImage, imageops};

const ICON_PATH: &str = "foxy.ico";
const OUT_DIR: &str = "installer/windows/art";

const SIDE_BASE: (u32, u32) = (202, 386);
const SMALL_BASE: u32 = 58;
const BACK_SIZE: (u32, u32) = (1192, 864);
const SCALES: [u32; 3] = [100, 150, 200];

/// Matches `PRIMARY_ACCENT` in `src/ui/palette.rs`.
const ACCENT: Rgb<u8> = Rgb([200, 100, 15]);

struct Theme {
    name: &'static str,
    side_top: Rgb<u8>,
    side_bottom: Rgb<u8>,
    glow_strength: f32,
    back_top: Rgb<u8>,
    back_bottom: Rgb<u8>,
    watermark_opacity: f32,
}

const THEMES: [Theme; 2] = [
    Theme {
        name: "dark",
        side_top: Rgb([45, 45, 45]),
        side_bottom: Rgb([30, 30, 30]),
        glow_strength: 0.38,
        back_top: Rgb([38, 38, 38]),
        back_bottom: Rgb([28, 28, 28]),
        watermark_opacity: 0.05,
    },
    Theme {
        name: "light",
        side_top: Rgb([255, 247, 239]),
        side_bottom: Rgb([244, 240, 236]),
        glow_strength: 0.22,
        back_top: Rgb([251, 250, 248]),
        back_bottom: Rgb([243, 239, 235]),
        watermark_opacity: 0.06,
    },
];

fn main() -> Result<(), Box<dyn Error>> {
    let logo = ImageReader::open(ICON_PATH)?.decode()?;
    let out_dir = Path::new(OUT_DIR);
    fs::create_dir_all(out_dir)?;

    for theme in &THEMES {
        for scale in SCALES {
            let side = side_panel(
                theme,
                &logo,
                scaled(SIDE_BASE.0, scale),
                scaled(SIDE_BASE.1, scale),
            );
            side.save(out_dir.join(format!("side-{}-{scale}.png", theme.name)))?;
        }
        back_image(theme, &logo).save(out_dir.join(format!("back-{}.png", theme.name)))?;
    }
    for scale in SCALES {
        let size = scaled(SMALL_BASE, scale);
        fit_logo(&logo, size).save(out_dir.join(format!("small-{scale}.png")))?;
    }
    Ok(())
}

fn scaled(base: u32, percent: u32) -> u32 {
    base * percent / 100
}

fn lerp(from: Rgb<u8>, to: Rgb<u8>, amount: f32) -> Rgb<u8> {
    let amount = amount.clamp(0.0, 1.0);
    Rgb(std::array::from_fn(|channel| {
        let (from, to) = (f32::from(from.0[channel]), f32::from(to.0[channel]));
        (from + (to - from) * amount).round() as u8
    }))
}

/// Brightest at `center`, falling to nothing `radius` pixels away.
fn glow_amount(x: u32, y: u32, center: (f32, f32), radius: f32, strength: f32) -> f32 {
    let distance = (x as f32 - center.0).hypot(y as f32 - center.1);
    (1.0 - distance / radius).max(0.0).powi(2) * strength
}

fn fit_logo(logo: &DynamicImage, size: u32) -> RgbaImage {
    imageops::resize(logo, size, size, imageops::FilterType::Lanczos3)
}

fn opaque(image: &RgbImage) -> RgbaImage {
    RgbaImage::from_fn(image.width(), image.height(), |x, y| {
        let Rgb([red, green, blue]) = *image.get_pixel(x, y);
        Rgba([red, green, blue, u8::MAX])
    })
}

fn side_panel(theme: &Theme, logo: &DynamicImage, width: u32, height: u32) -> RgbaImage {
    let logo_size = width * 7 / 10;
    let logo_x = (width - logo_size) / 2;
    let logo_y = height * 34 / 100 - logo_size / 2;
    let center = (width as f32 / 2.0, height as f32 * 0.34);
    let glow_radius = width as f32 * 0.95;
    let stripe_start = width - (width / 100).max(2);

    let background = RgbImage::from_fn(width, height, |x, y| {
        if x >= stripe_start {
            return ACCENT;
        }
        let base = lerp(
            theme.side_top,
            theme.side_bottom,
            y as f32 / (height - 1) as f32,
        );
        lerp(
            base,
            ACCENT,
            glow_amount(x, y, center, glow_radius, theme.glow_strength),
        )
    });

    let mut panel = opaque(&background);
    imageops::overlay(
        &mut panel,
        &fit_logo(logo, logo_size),
        i64::from(logo_x),
        i64::from(logo_y),
    );
    panel
}

fn back_image(theme: &Theme, logo: &DynamicImage) -> RgbaImage {
    let (width, height) = BACK_SIZE;
    let background = RgbImage::from_fn(width, height, |_, y| {
        lerp(
            theme.back_top,
            theme.back_bottom,
            y as f32 / (height - 1) as f32,
        )
    });

    let mut watermark = fit_logo(logo, height * 3 / 4);
    for pixel in watermark.pixels_mut() {
        pixel.0[3] = (f32::from(pixel.0[3]) * theme.watermark_opacity).round() as u8;
    }

    let mut back = opaque(&background);
    let x = i64::from(width) - i64::from(watermark.width()) * 4 / 5;
    let y = i64::from(height) - i64::from(watermark.height()) * 4 / 5;
    imageops::overlay(&mut back, &watermark, x, y);
    back
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_logo() -> DynamicImage {
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(64, 64, Rgba([255, 255, 255, 255])))
    }

    #[test]
    fn scaled_applies_percent() {
        assert_eq!(scaled(202, 150), 303);
        assert_eq!(scaled(386, 200), 772);
    }

    #[test]
    fn lerp_hits_endpoints_and_clamps() {
        let (from, to) = (Rgb([0, 100, 200]), Rgb([100, 200, 0]));
        assert_eq!(lerp(from, to, 0.0), from);
        assert_eq!(lerp(from, to, 1.0), to);
        assert_eq!(lerp(from, to, 2.0), to);
        assert_eq!(lerp(from, to, 0.5), Rgb([50, 150, 100]));
    }

    #[test]
    fn glow_fades_to_nothing_at_radius() {
        assert_eq!(glow_amount(0, 0, (0.0, 0.0), 10.0, 0.5), 0.5);
        assert_eq!(glow_amount(10, 0, (0.0, 0.0), 10.0, 0.5), 0.0);
        assert_eq!(glow_amount(30, 0, (0.0, 0.0), 10.0, 0.5), 0.0);
    }

    #[test]
    fn side_panel_is_opaque_with_accent_edge() {
        let panel = side_panel(&THEMES[0], &test_logo(), 202, 386);
        assert_eq!(panel.dimensions(), (202, 386));
        assert!(panel.pixels().all(|pixel| pixel.0[3] == u8::MAX));
        let edge = panel.get_pixel(201, 0);
        assert_eq!(edge.0[..3], ACCENT.0);
    }

    #[test]
    fn back_image_watermark_stays_faint() {
        for theme in &THEMES {
            let back = back_image(theme, &test_logo());
            assert_eq!(back.dimensions(), BACK_SIZE);
            let corner = back.get_pixel(BACK_SIZE.0 - 1, BACK_SIZE.1 - 1);
            let plain = theme.back_bottom;
            for channel in 0..3 {
                let shift = corner.0[channel].abs_diff(plain.0[channel]);
                assert!(
                    u32::from(shift) <= 255 / 16,
                    "{} shifted by {shift}",
                    theme.name
                );
            }
        }
    }
}

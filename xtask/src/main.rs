/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Dev-only asset generation: `cargo xtask assets` writes `assets/icons/{lucide,brand,app}/`. Outputs are committed.

use std::fs;
use std::path::Path;

use image::imageops::{self, FilterType};
use image::{Rgba, RgbaImage};

const LUCIDE_VERSION: &str = "1.72.0";

/// Lucide icon and the stroke width Android tuned for the size it is drawn at (from its `lucide_*.xml`); the
/// desktop-only icons take the width of the Android icon used in the same place.
const LUCIDE_ICONS: [(&str, &str); 26] = [
    ("arrow-down", "1.5"),
    ("arrow-right", "2.2"),
    ("audio-lines", "1.64"),
    ("check", "1.5"),
    ("chevron-down", "1.5"),
    ("chevron-left", "1.5"),
    ("cloud", "1.5"),
    ("database-backup", "1.5"),
    ("ear", "1.5"),
    ("eraser", "1.5"),
    ("eye", "1.5"),
    ("eye-off", "1.5"),
    ("gauge", "1.5"),
    ("mic", "1.64"),
    ("plus", "1.5"),
    ("radio", "1.5"),
    ("refresh-cw", "1.6"),
    ("save-check", "1.7"),
    ("sparkles", "1.25"),
    ("spell-check", "1.5"),
    ("trash", "1.5"),
    ("keyboard", "1.5"),
    ("monitor", "1.7"),
    ("clipboard-paste", "1.64"),
    ("info", "1.5"),
    ("x", "1.5"),
];

const APP_ICON_SIZES: [u32; 10] = [16, 20, 24, 32, 40, 48, 64, 128, 256, 512];
const ICO_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("assets") => {
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
            // The source icons live next to the repository (`../iris_icons`).
            let icons = root.parent().unwrap().join("iris_icons");
            lucide(&root.join("assets/icons/lucide"));
            brand(&icons, &root.join("assets/icons/brand"));
            app_icon(&icons.join("iris-app-icons"), &root.join("assets/icons/app"));
        }
        _ => {
            eprintln!("Usage: cargo xtask assets");
            std::process::exit(2);
        }
    }
}

/// Downloads the Lucide SVGs, drawn white (tinted with `colorize` in Slint) at Android's stroke widths.
fn lucide(out: &Path) {
    fs::create_dir_all(out).unwrap();
    for (name, stroke) in LUCIDE_ICONS {
        let url = format!("https://raw.githubusercontent.com/lucide-icons/lucide/{LUCIDE_VERSION}/icons/{name}.svg");
        let svg = ureq::get(&url).call().unwrap().body_mut().read_to_string().unwrap();
        assert!(svg.contains("stroke=\"currentColor\"") && svg.contains("stroke-width=\"2\""), "{name}");
        let svg = svg
            .replace("stroke=\"currentColor\"", "stroke=\"#FFFFFF\"")
            .replace("stroke-width=\"2\"", &format!("stroke-width=\"{stroke}\""));
        let header =
            format!("<!-- Lucide \"{name}\" {LUCIDE_VERSION}, ISC/MIT: assets/licenses/lucide_LICENSE.txt. -->\n");
        fs::write(out.join(format!("{name}.svg")), header + &svg).unwrap();
    }
}

/// The logo in both themes and the provider/maker marks.
fn brand(icons: &Path, out: &Path) {
    fs::create_dir_all(out).unwrap();
    let logo = fs::read_to_string(icons.join("iris_logo.svg")).unwrap();
    assert!(logo.contains("#3f3f42") && logo.contains("#2b2b2d"));
    fs::write(out.join("iris_logo_light.svg"), &logo).unwrap();
    let dark = logo.replace("#3f3f42", "#d9dbdc").replace("#2b2b2d", "#ecedee");
    fs::write(out.join("iris_logo_dark.svg"), dark).unwrap();

    let providers = icons.join("iris-provider-icons");
    for name in ["deepgram", "openai", "vercel", "soniox", "openrouter-light", "openrouter-dark"] {
        fs::copy(providers.join(format!("{name}.svg")), out.join(format!("{name}.svg"))).unwrap();
    }
    // As Android's provider_nvidia.xml: NVIDIA green, cropped to the mark (x 2..24, y 4.5..19.5).
    let nvidia = fs::read_to_string(icons.join("iris_local_model_icons/nvidia.svg")).unwrap();
    let nvidia = nvidia.replacen("viewBox=\"0 0 24 24\"", "viewBox=\"2 4.5 22 15\" fill=\"#74B71B\"", 1);
    assert!(nvidia.contains("#74B71B"));
    fs::write(out.join("nvidia.svg"), nvidia).unwrap();
}

/// Composites the adaptive-icon layers, crops to the visible 72/108 area, rounds the corners (22 %) and writes the
/// PNG sizes plus `iris.ico`.
fn app_icon(layers: &Path, out: &Path) {
    fs::create_dir_all(out).unwrap();
    let mut icon = image::open(layers.join("background.png")).unwrap().to_rgba8();
    let foreground = image::open(layers.join("foreground.png")).unwrap().to_rgba8();
    imageops::overlay(&mut icon, &foreground, 0, 0);
    let size = icon.width();
    let visible = size * 72 / 108;
    let offset = (size - visible) / 2;
    let cropped = imageops::crop_imm(&icon, offset, offset, visible, visible).to_image();
    let mut icon = imageops::resize(&cropped, 512, 512, FilterType::Lanczos3);
    round_corners(&mut icon, 0.22);

    let mut ico = ico::IconDir::new(ico::ResourceType::Icon);
    for px in APP_ICON_SIZES {
        let scaled = imageops::resize(&icon, px, px, FilterType::Lanczos3);
        scaled.save(out.join(format!("iris-{px}.png"))).unwrap();
        if ICO_SIZES.contains(&px) {
            let entry = ico::IconImage::from_rgba_data(px, px, scaled.into_raw());
            ico.add_entry(ico::IconDirEntry::encode(&entry).unwrap());
        }
    }
    ico.write(fs::File::create(out.join("iris.ico")).unwrap()).unwrap();
}

/// Multiplies alpha by the coverage of a rounded square (4×4 supersampled edges).
fn round_corners(image: &mut RgbaImage, radius_fraction: f32) {
    let size = image.width() as f32;
    let radius = size * radius_fraction;
    let inside = |x: f32, y: f32| {
        let cx = x.clamp(radius, size - radius);
        let cy = y.clamp(radius, size - radius);
        (x - cx).powi(2) + (y - cy).powi(2) <= radius * radius
    };
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let mut hits = 0;
        for sy in 0..4 {
            for sx in 0..4 {
                if inside(x as f32 + (sx as f32 + 0.5) / 4.0, y as f32 + (sy as f32 + 0.5) / 4.0) {
                    hits += 1;
                }
            }
        }
        let Rgba([r, g, b, a]) = *pixel;
        *pixel = Rgba([r, g, b, (a as u32 * hits / 16) as u8]);
    }
}

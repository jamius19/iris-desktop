/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Dev-only asset generation: `cargo xtask assets` writes `assets/icons/{lucide,brand,app}/`, `cargo xtask tray`
//! `assets/icons/tray/`, `cargo xtask sounds` `assets/sounds/`, `cargo xtask licenses`
//! `assets/licenses/THIRD_PARTY_LICENSES.txt`. Outputs are committed.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use image::imageops::{self, FilterType};
use image::{Rgba, RgbaImage};
use serde_json::Value;

const LUCIDE_VERSION: &str = "1.72.0";
/// The commit of Lucide's `1.72.0` tag (a lightweight tag), so the download can't change under the same version.
const LUCIDE_COMMIT: &str = "03eae6069edfc373673bd05d723491f05ba936e3";

/// The shipped binaries' packages per target: `iris` everywhere, the overlay helper on Linux.
const LICENSE_TARGETS: [(&str, &[&str]); 2] =
    [("x86_64-pc-windows-msvc", &["iris"]), ("x86_64-unknown-linux-gnu", &["iris", "iris-overlay-helper"])];
const LICENSE_FILE_PREFIXES: [&str; 5] = ["LICENSE", "LICENCE", "COPYING", "NOTICE", "COPYRIGHT"];

/// Lucide icon and the stroke width Android tuned for the size it is drawn at (from its `lucide_*.xml`); the
/// desktop-only icons take the width of the Android icon used in the same place.
const LUCIDE_ICONS: [(&str, &str); 28] = [
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
    ("copy", "1.5"),
    ("rotate-ccw-clock", "1.5"),
];

const APP_ICON_SIZES: [u32; 10] = [16, 20, 24, 32, 40, 48, 64, 128, 256, 512];
const ICO_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    match std::env::args().nth(1).as_deref() {
        Some("assets") => {
            // The source icons live next to the repository (`../iris_icons`).
            let icons = root.parent().unwrap().join("iris_icons");
            lucide(&root.join("assets/icons/lucide"));
            brand(&icons, &root.join("assets/icons/brand"));
            app_icon(&icons.join("iris-app-icons"), &root.join("assets/icons/app"));
        }
        Some("tray") => tray(&root.join("assets/icons/tray")),
        Some("sounds") => sounds(&root.join("assets/sounds")),
        Some("licenses") => licenses(&root),
        _ => {
            eprintln!("Usage: cargo xtask assets|tray|sounds|licenses");
            std::process::exit(2);
        }
    }
}

/// Downloads the Lucide SVGs, drawn white (tinted with `colorize` in Slint) at Android's stroke widths.
fn lucide(out: &Path) {
    fs::create_dir_all(out).unwrap();
    for (name, stroke) in LUCIDE_ICONS {
        let url = format!("https://raw.githubusercontent.com/lucide-icons/lucide/{LUCIDE_COMMIT}/icons/{name}.svg");
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

/// The logo's waveform (`iris_logo.svg`, stroke 21, round caps), 180 × 153.13 units with its stroke.
const WAVEFORM: &str = "M10.5,83.21c8.78,0,15.9-5.88,15.9-13.13v-26.27c0-7.25,7.12-13.13,15.9-13.13\
s15.9,5.88,15.9,13.13v85.36c0,7.25,7.12,13.13,15.9,13.13s15.9-5.88,15.9-13.13V24.12c0-7.25,7.12-13.13,15.9-13.13\
s15.9,5.88,15.9,13.13v85.36c0,7.25,7.12,13.13,15.9,13.13s15.9-5.88,15.9-13.13v-26.27c0-7.25,7.12-13.13,15.9-13.13";

/// Tray icon sizes: Windows draws 16 px per 96 DPI (100 % to 300 % scaling), the Linux tray gets 32.
const TRAY_SIZES: [u32; 6] = [16, 20, 24, 32, 40, 48];

/// The tray icons on a 100-unit canvas: idle = the waveform, recording = white waveform on a red disc, working = the
/// waveform at 50 % opacity inside a 270° arc. `_light` is for a light taskbar/panel (black), `_dark` for a dark one
/// (white); recording is the same on both.
fn tray(out: &Path) {
    fs::create_dir_all(out).unwrap();
    // The waveform scaled to `width` units and centred at (50, 50).
    let waveform = |width: f32, color: &str, opacity: f32| {
        let scale = width / 180.0;
        let (x, y) = (50.0 - width / 2.0, 50.0 - 153.13 * scale / 2.0);
        format!(
            "<path d=\"{WAVEFORM}\" transform=\"translate({x} {y}) scale({scale})\" fill=\"none\" stroke=\"{color}\" \
             stroke-opacity=\"{opacity}\" stroke-width=\"21\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>"
        )
    };
    // 270° clockwise from 12 o'clock to 9 o'clock, radius 44, open at the top left.
    let arc = |color: &str| {
        format!(
            "<path d=\"M50,6A44,44 0 1 1 6,50\" fill=\"none\" stroke=\"{color}\" stroke-width=\"10\" \
             stroke-linecap=\"round\"/>"
        )
    };
    let mut icons = vec![(
        "recording".to_string(),
        format!("<circle cx=\"50\" cy=\"50\" r=\"50\" fill=\"#D32F2F\"/>{}", waveform(60.0, "#FFFFFF", 1.0)),
    )];
    for (theme, color) in [("light", "#000000"), ("dark", "#FFFFFF")] {
        icons.push((format!("idle_{theme}"), waveform(96.0, color, 1.0)));
        icons.push((format!("working_{theme}"), format!("{}{}", arc(color), waveform(52.0, color, 0.5))));
    }
    for (name, body) in icons {
        let svg = format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 100 100\">{body}</svg>");
        let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).unwrap();
        for px in TRAY_SIZES {
            let mut pixmap = resvg::tiny_skia::Pixmap::new(px, px).unwrap();
            let scale = px as f32 / 100.0;
            resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
            pixmap.save_png(out.join(format!("{name}-{px}.png"))).unwrap();
        }
    }
}

/// Original start/stop sounds (Handy's pop sounds carry no provenance): a 120 ms sine with a 3 ms attack and an
/// exponential decay, gliding up (start) or down (stop) between 587 and 880 Hz. 44.1 kHz mono 16-bit.
fn sounds(out: &Path) {
    fs::create_dir_all(out).unwrap();
    const RATE: u32 = 44_100;
    let spec =
        hound::WavSpec { channels: 1, sample_rate: RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    for (name, from, to) in [("start", 587.0_f32, 880.0_f32), ("stop", 880.0, 587.0)] {
        let mut writer = hound::WavWriter::create(out.join(format!("{name}.wav")), spec).unwrap();
        let samples = RATE * 120 / 1000;
        let mut phase = 0.0_f32;
        for i in 0..samples {
            let t = i as f32 / samples as f32;
            phase += std::f32::consts::TAU * (from + (to - from) * t) / RATE as f32;
            let envelope = (i as f32 / (RATE as f32 * 0.003)).min(1.0) * (-5.0 * t).exp();
            writer.write_sample((phase.sin() * envelope * 0.5 * i16::MAX as f32) as i16).unwrap();
        }
        writer.finalize().unwrap();
    }
}

/// A third-party crate: licence expression, repository and the licence/notice texts shipped in its package.
struct Crate {
    license: String,
    repository: String,
    texts: Vec<String>,
}

/// Writes the About page's "Rust libraries" text: every crate the shipped binaries link and its licence texts, each
/// distinct text printed once. The crate set comes from `cargo tree` (normal edges, real feature resolution; the
/// resolve in `cargo metadata` unifies features with build/dev deps and xtask, adding crates that aren't linked, e.g.
/// Skia), the package details from `cargo metadata`.
fn licenses(root: &Path) {
    let metadata: Value = serde_json::from_slice(&cargo(root, &["metadata", "--format-version", "1"])).unwrap();
    let members: HashSet<&str> = metadata["workspace_members"].as_array().unwrap().iter().map(as_str).collect();
    // "name vX.Y.Z" (as `cargo tree` prints it) -> package, `None` for workspace crates.
    let packages: HashMap<String, Option<&Value>> = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|package| {
            let name = format!("{} v{}", as_str(&package["name"]), as_str(&package["version"]));
            (name, Some(package).filter(|package| !members.contains(as_str(&package["id"]))))
        })
        .collect();
    let mut crates = BTreeMap::new();
    for (target, binaries) in LICENSE_TARGETS {
        let mut args = vec!["tree", "-e", "normal", "--target", target, "--prefix", "none", "--format", "{p}"];
        args.extend(binaries.iter().flat_map(|binary| ["-p", binary]));
        // One tree per binary, separated by a blank line.
        for line in String::from_utf8(cargo(root, &args)).unwrap().lines().filter(|line| !line.is_empty()) {
            let name = line.split(' ').take(2).collect::<Vec<_>>().join(" ");
            let Some(package) = packages.get(&name).unwrap_or_else(|| panic!("not in cargo metadata: {line}")) else {
                continue;
            };
            crates.entry(name).or_insert_with(|| Crate {
                license: package["license"].as_str().unwrap_or("no licence field").to_owned(),
                repository: package["repository"].as_str().unwrap_or("no repository field").to_owned(),
                texts: license_texts(package),
            });
        }
    }

    // Whitespace-normalised text -> (text as printed, crates shipping it).
    let mut groups: BTreeMap<String, (&str, BTreeSet<&str>)> = BTreeMap::new();
    for (name, krate) in &crates {
        for text in &krate.texts {
            let key = text.split_whitespace().collect::<Vec<_>>().join(" ");
            groups.entry(key).or_insert((text.as_str(), BTreeSet::new())).1.insert(name);
        }
    }
    let mut groups: Vec<_> = groups.into_values().collect();
    groups.sort_by_key(|(text, names)| (names.first().copied(), *text));
    let without: Vec<_> = crates.iter().filter(|(_, krate)| krate.texts.is_empty()).collect();

    let mut out = String::new();
    writeln!(out, "Third-party Rust libraries in Iris (Windows and Linux builds) and their licence texts.").unwrap();
    writeln!(
        out,
        "Generated by `cargo xtask licenses` (`cargo tree` + `cargo metadata`); re-run it after dependency changes.\n"
    )
    .unwrap();
    writeln!(out, "== Libraries ({}) ==\n", crates.len()).unwrap();
    for (name, krate) in &crates {
        writeln!(out, "{name}: {}, {}", krate.license, krate.repository).unwrap();
    }
    writeln!(out, "\n== Licence texts ({}) ==", groups.len()).unwrap();
    for (text, names) in &groups {
        let names = wrap(&names.iter().copied().collect::<Vec<_>>(), 100);
        writeln!(out, "\n{}\nUsed by: {names}\n\n{text}", "-".repeat(100)).unwrap();
    }
    writeln!(out, "\n== Libraries without a licence file in their package ({}) ==\n", without.len()).unwrap();
    for (name, krate) in &without {
        writeln!(out, "{name}: {}, {}", krate.license, krate.repository).unwrap();
    }
    let path = root.join("assets/licenses/THIRD_PARTY_LICENSES.txt");
    fs::write(&path, &out).unwrap();
    println!(
        "{}: {} crates, {} distinct texts, {} crates without a licence file, {} bytes",
        path.display(),
        crates.len(),
        groups.len(),
        without.len(),
        out.len()
    );
}

fn as_str(value: &Value) -> &str {
    value.as_str().unwrap()
}

/// Runs cargo in the workspace root and returns its stdout.
fn cargo(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new(env!("CARGO")).current_dir(root).args(args).output().unwrap();
    assert!(output.status.success(), "cargo {}: {}", args.join(" "), String::from_utf8_lossy(&output.stderr));
    output.stdout
}

/// The package's LICENSE/LICENCE/COPYING/NOTICE/COPYRIGHT files plus its `license_file`, line endings and trailing
/// spaces cleaned up.
fn license_texts(package: &Value) -> Vec<String> {
    let dir = Path::new(as_str(&package["manifest_path"])).parent().unwrap();
    let mut paths: BTreeSet<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            let name = path.file_name().unwrap().to_string_lossy().to_uppercase();
            path.is_file() && LICENSE_FILE_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
        })
        .collect();
    if let Some(file) = package["license_file"].as_str() {
        paths.insert(dir.join(file));
    }
    let texts = paths.iter().map(|path| {
        let text = String::from_utf8_lossy(&fs::read(path).unwrap()).replace('\u{feff}', "");
        text.lines().map(str::trim_end).collect::<Vec<_>>().join("\n").trim().to_owned()
    });
    texts.filter(|text| !text.is_empty()).collect()
}

/// Joins `items` with ", " into lines of about `width` columns.
fn wrap(items: &[&str], width: usize) -> String {
    let mut out = String::new();
    let mut line = 0;
    for (i, item) in items.iter().enumerate() {
        let item = if i + 1 < items.len() { format!("{item},") } else { item.to_string() };
        if line > 0 && line + 1 + item.len() > width {
            out.push('\n');
            line = 0;
        } else if line > 0 {
            out.push(' ');
            line += 1;
        }
        out.push_str(&item);
        line += item.len();
    }
    out
}

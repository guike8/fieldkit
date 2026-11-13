//! Native-window smoke driver: captures only this application's rendered pixels.
//! Use disposable cases and local navigation clicks, never collection/provider controls.
use anyhow::{Context, Result, ensure};
use eframe::egui::{self, Event, PointerButton, Pos2, ViewportCommand};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

struct Smoke {
    app: nightjar::Nightjar,
    output: Option<File>,
    started: Instant,
    click: Option<Pos2>,
    pressed: bool,
    released: bool,
    requested: bool,
}

impl eframe::App for Smoke {
    fn raw_input_hook(&mut self, _: &egui::Context, input: &mut egui::RawInput) {
        if self.started.elapsed() >= Duration::from_secs(2)
            && let Some(pos) = self.click
        {
            input.focused = true;
            input.events.push(Event::PointerMoved(pos));
            if !self.pressed {
                input.events.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                });
                self.pressed = true;
            } else if !self.released {
                input.events.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                });
                self.released = true;
            }
        }
    }

    fn update(&mut self, context: &egui::Context, frame: &mut eframe::Frame) {
        self.app.update(context, frame);
        let screenshot = context.input(|input| {
            input.events.iter().find_map(|event| {
                if let Event::Screenshot { image, .. } = event {
                    Some(image.clone())
                } else {
                    None
                }
            })
        });
        if let Some(pixels) = screenshot {
            let rgba: Vec<u8> = pixels
                .pixels
                .iter()
                .flat_map(|color| color.to_array())
                .collect();
            let image =
                image::RgbaImage::from_raw(pixels.width() as u32, pixels.height() as u32, rgba)
                    .expect("RGBA screenshot dimensions");
            let mut output = BufWriter::new(self.output.take().expect("one screenshot"));
            image::DynamicImage::ImageRgba8(image)
                .write_to(&mut output, image::ImageFormat::Png)
                .expect("write exclusive diagnostic screenshot");
            output.flush().expect("flush diagnostic screenshot");
            context.send_viewport_cmd(ViewportCommand::Close);
        } else if !self.requested && self.started.elapsed() >= Duration::from_secs(4) {
            context.send_viewport_cmd(ViewportCommand::Screenshot(Default::default()));
            self.requested = true;
        }
        if self.started.elapsed() > Duration::from_secs(15) {
            eprintln!("Native screenshot deadline exceeded");
            context.send_viewport_cmd(ViewportCommand::Close);
        }
        context.request_repaint_after(Duration::from_millis(50));
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 4 || args.len() == 6,
        "Usage: visual_smoke CASE NEW_PNG WIDTH HEIGHT [LOCAL_CLICK_X LOCAL_CLICK_Y]"
    );
    let case = PathBuf::from(&args[0])
        .canonicalize()
        .context("Existing disposable case")?;
    ensure!(
        case.join("case.sqlite3").is_file(),
        "Expected an existing case ledger"
    );
    let output = File::options()
        .write(true)
        .create_new(true)
        .open(&args[1])
        .context("Screenshot path must be new")?;
    let width: f32 = args[2].parse()?;
    let height: f32 = args[3].parse()?;
    ensure!(
        width.is_finite() && height.is_finite() && width >= 800.0 && height >= 600.0,
        "Invalid window size"
    );
    let click = if args.len() == 6 {
        Some(Pos2::new(args[4].parse()?, args[5].parse()?))
    } else {
        None
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Nightjar visual smoke")
            .with_inner_size([width, height]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "Nightjar visual smoke",
        options,
        Box::new(move |creation| {
            Ok(Box::new(Smoke {
                app: nightjar::Nightjar::new(creation, Some(case)),
                output: Some(output),
                started: Instant::now(),
                click,
                pressed: false,
                released: false,
                requested: false,
            }))
        }),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))?;
    image::open(&args[1]).context("Native screenshot was not produced as a valid PNG")?;
    Ok(())
}

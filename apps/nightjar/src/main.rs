use std::path::PathBuf;

#[cfg(target_os = "macos")]
mod macos_menu;

struct DesktopApp {
    inner: nightjar::Nightjar,
    #[cfg(target_os = "macos")]
    menu_bar: macos_menu::MenuBar,
}

impl eframe::App for DesktopApp {
    fn update(&mut self, context: &eframe::egui::Context, frame: &mut eframe::Frame) {
        #[cfg(target_os = "macos")]
        self.menu_bar.update(context, frame);
        eframe::App::update(&mut self.inner, context, frame);
    }

    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        eframe::App::on_exit(&mut self.inner, gl);
    }
}

fn main() -> eframe::Result {
    let initial_case = std::env::args_os().nth(1).map(PathBuf::from);
    let artwork = nightjar_brand::app_icon(512);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Nightjar")
            .with_icon(eframe::egui::IconData {
                rgba: artwork.rgba,
                width: artwork.size,
                height: artwork.size,
            })
            .with_inner_size([1480.0, 1000.0])
            .with_min_inner_size([1100.0, 740.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "Nightjar",
        options,
        Box::new(move |context| {
            #[cfg(target_os = "macos")]
            let menu_bar = macos_menu::MenuBar::new(&context.egui_ctx)?;
            Ok(Box::new(DesktopApp {
                inner: nightjar::Nightjar::new(context, initial_case),
                #[cfg(target_os = "macos")]
                menu_bar,
            }))
        }),
    )
}

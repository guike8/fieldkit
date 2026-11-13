//! Native macOS status item. Owned by the eframe app, never a background agent.

use std::{
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, ensure};
use eframe::egui::{self, ViewportCommand};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSAccessibility, NSApplication, NSView};
use objc2_foundation::NSString;
use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Open,
    Quit,
}

pub struct MenuBar {
    // Dropping the last TrayIcon removes the NSStatusItem from the system bar.
    icon: TrayIcon,
    _open: MenuItem,
    _quit: MenuItem,
    actions: Receiver<Action>,
    verification_deadline: Instant,
    verified: bool,
}

impl MenuBar {
    /// Called from eframe's creation callback, after the Cocoa event loop starts.
    pub fn new(context: &egui::Context) -> Result<Self> {
        let main_thread =
            MainThreadMarker::new().context("Menu bar requires the Cocoa main thread")?;
        let open = MenuItem::new("Open Nightjar", true, None);
        let quit = MenuItem::new("Quit Nightjar", true, None);
        let menu = Menu::with_items(&[&open, &PredefinedMenuItem::separator(), &quit])?;
        let artwork = nightjar_brand::template_icon(36); // 18 pt at Retina resolution
        let icon = TrayIconBuilder::new()
            .with_id("dev.nightjar.app.status")
            .with_icon(Icon::from_rgba(artwork.rgba, artwork.size, artwork.size)?)
            .with_icon_as_template(true)
            .with_tooltip("Nightjar — investigation workspace")
            .with_menu(Box::new(menu))
            .build()?;
        let native = icon
            .ns_status_item()
            .context("Cocoa did not install the status item")?;
        // The tooltip is not a replacement for the button's VoiceOver name.
        let button = native
            .button(main_thread)
            .context("Status item has no button")?;
        button.setAccessibilityLabel(Some(&NSString::from_str("Nightjar")));
        ensure!(native.isVisible(), "Cocoa status item is hidden");
        ensure!(
            button.image().is_some_and(|image| image.isTemplate()),
            "Status image is not a template"
        );

        let (sender, actions) = mpsc::channel();
        let open_id = open.id().clone();
        let quit_id = quit.id().clone();
        let wake = context.clone();
        // A handler replaces muda's default receiver. Queue before waking egui,
        // so an otherwise idle window processes the action on its next frame.
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let action = if event.id == open_id {
                Some(Action::Open)
            } else if event.id == quit_id {
                Some(Action::Quit)
            } else {
                None
            };
            if let Some(action) = action {
                let _ = sender.send(action);
                wake.request_repaint();
            }
        }));
        // Do not let the library's unused, unbounded hover-event queue grow.
        TrayIconEvent::set_event_handler(Some(|_: TrayIconEvent| {}));
        context.request_repaint();
        Ok(Self {
            icon,
            _open: open,
            _quit: quit,
            actions,
            verification_deadline: Instant::now() + Duration::from_secs(3),
            verified: false,
        })
    }

    pub fn update(&mut self, context: &egui::Context, frame: &eframe::Frame) {
        while let Ok(action) = self.actions.try_recv() {
            match action {
                Action::Open => {
                    restore_window(frame);
                    context.send_viewport_cmd(ViewportCommand::Minimized(false));
                    context.send_viewport_cmd(ViewportCommand::Visible(true));
                    context.send_viewport_cmd(ViewportCommand::Focus);
                }
                // Use the normal close path: Nightjar's existing worker Drop
                // cancellation still runs; never terminate the process directly.
                Action::Quit => context.send_viewport_cmd(ViewportCommand::Close),
            }
        }
        self.verify_native_layout(context);
    }

    fn verify_native_layout(&mut self, context: &egui::Context) {
        if self.verified {
            return;
        }
        if let Some(rect) = self.icon.rect().filter(|rect| {
            rect.size.width > 0
                && rect.size.height > 0
                && rect.position.x.is_finite()
                && rect.position.y.is_finite()
        }) {
            // A native layout check, not a screenshot claim. macOS may still
            // occlude a valid item when the menu bar is crowded or auto-hidden.
            eprintln!("Nightjar menu bar: installed=true visible=true template=true rect={rect:?}");
            self.verified = true;
        } else if Instant::now() < self.verification_deadline {
            context.request_repaint_after(Duration::from_millis(100));
        } else {
            eprintln!(
                "Nightjar menu bar: installed=true; Cocoa has not provided a visible status-item rectangle"
            );
            self.verified = true;
        }
    }
}

fn restore_window(frame: &eframe::Frame) {
    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(main_thread);
    app.unhide(None);
    // Explicit menu interaction warrants activation, including on macOS 12/13.
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    if let Ok(handle) = frame.window_handle()
        && let RawWindowHandle::AppKit(handle) = handle.as_raw()
    {
        // SAFETY: eframe lends its live root NSView for this frame. This code
        // runs on the Cocoa main thread and neither stores nor transfers it.
        let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
        if let Some(window) = view.window() {
            window.deminiaturize(None);
            window.makeKeyAndOrderFront(None);
        }
    }
}

/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Layer-shell mode (S4): a custom Slint platform (`MinimalSoftwareWindow`) renders `OverlayWindow` into `wl_shm`
//! buffers of an SCTK layer surface on the Overlay layer. The surface spans the output's width at the top or bottom
//! edge, takes no keyboard focus and only takes input on the card. Animations run from `wl_surface.frame` callbacks;
//! pointer events go to Slint with `dispatch_event`. The surface exists only while the overlay shows. Integer buffer
//! scale (no fractional scale).

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use iris_overlay::geometry::{BOTTOM_OFFSET, TOP_OFFSET, max_card_width, overlay_size};
use iris_overlay::ipc::{FromHelper, Mode, ToHelper};
use iris_overlay::{HIDE_DELAY, OverlayConfig, OverlayState, OverlayWindow, Position, apply, set_levels};
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType, TargetPixel,
};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize, Timer};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, Region};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::EventLoop;
use smithay_client_toolkit::reexports::calloop::channel::{self, Event};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry, delegate_seat,
    delegate_shm, registry_handlers,
};

use crate::{fail, read_stdin, send};

const BTN_LEFT: u32 = 0x110;

/// `wl_shm` ARGB8888: a little-endian u32 0xAARRGGBB, premultiplied.
#[repr(transparent)]
#[derive(Clone, Copy)]
struct Argb(u32);

impl TargetPixel for Argb {
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        let inverse = 255 - color.alpha as u32;
        let channel = |shift: u32, source: u8| ((((self.0 >> shift) & 0xFF) * inverse / 255) + source as u32).min(255);
        let (a, r, g, b) =
            (channel(24, color.alpha), channel(16, color.red), channel(8, color.green), channel(0, color.blue));
        self.0 = (a << 24) | (r << 16) | (g << 8) | b;
    }

    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Argb(0xFF00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32)
    }

    fn background() -> Self {
        Argb(0)
    }
}

struct HelperPlatform {
    window: Rc<MinimalSoftwareWindow>,
    start: Instant,
}

impl Platform for HelperPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        self.start.elapsed()
    }
}

struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    shm: Shm,
    layer_shell: LayerShell,
    qh: QueueHandle<App>,
    pool: SlotPool,
    surface: Option<LayerSurface>,
    configured: bool,
    frame_pending: bool,
    /// Logical surface size from the last configure.
    size: (u32, u32),
    scale: i32,
    pointer: Option<wl_pointer::WlPointer>,
    slint_window: Rc<MinimalSoftwareWindow>,
    ui: OverlayWindow,
    config: OverlayConfig,
    state: Option<OverlayState>,
    /// Bumped per show, so a late hide never hides a newer show.
    shows: Rc<Cell<u64>>,
    /// Set by the hide timer: destroy the surface.
    unmap: Rc<Cell<bool>>,
    input_region: (i32, i32, i32, i32),
    quit: bool,
}

/// Runs layer-shell mode until stdin closes; false (having done nothing) if there's no layer shell.
pub fn run() -> bool {
    let Ok(connection) = Connection::connect_to_env() else { return false };
    let Ok((globals, event_queue)) = registry_queue_init::<App>(&connection) else { return false };
    let qh = event_queue.handle();
    let Ok(layer_shell) = LayerShell::bind(&globals, &qh) else { return false };

    let (Ok(compositor), Ok(shm)) = (CompositorState::bind(&globals, &qh), Shm::bind(&globals, &qh)) else {
        fail("the compositor lacks wl_compositor or wl_shm".into());
        return true;
    };
    let pool = match SlotPool::new(1 << 20, &shm) {
        Ok(pool) => pool,
        Err(e) => {
            fail(format!("couldn't create the shm pool: {e}"));
            return true;
        }
    };
    let slint_window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let platform = HelperPlatform { window: slint_window.clone(), start: Instant::now() };
    if let Err(e) = slint::platform::set_platform(Box::new(platform)) {
        fail(format!("couldn't set the Slint platform: {e}"));
        return true;
    }
    let ui = match OverlayWindow::new().and_then(|ui| ui.show().map(|_| ui)) {
        Ok(ui) => ui,
        Err(e) => {
            fail(format!("couldn't create the overlay: {e}"));
            return true;
        }
    };
    ui.on_cancel_clicked(|| send(FromHelper::CancelClicked));
    ui.on_notice_clicked(|| send(FromHelper::NoticeClicked));

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        shm,
        layer_shell,
        qh,
        pool,
        surface: None,
        configured: false,
        frame_pending: false,
        size: (0, 0),
        scale: 1,
        pointer: None,
        slint_window,
        ui,
        config: OverlayConfig::default(),
        state: None,
        shows: Rc::new(Cell::new(0)),
        unmap: Rc::new(Cell::new(false)),
        input_region: (0, 0, 0, 0),
        quit: false,
    };

    let mut event_loop: EventLoop<App> = match EventLoop::try_new() {
        Ok(event_loop) => event_loop,
        Err(e) => {
            fail(format!("couldn't create the event loop: {e}"));
            return true;
        }
    };
    let (sender, messages) = channel::channel::<ToHelper>();
    let inserted = WaylandSource::new(connection, event_queue).insert(event_loop.handle()).map_err(|e| e.to_string());
    let inserted = inserted.and_then(|_| {
        event_loop
            .handle()
            .insert_source(messages, |event, _, app: &mut App| match event {
                Event::Msg(message) => app.handle(message),
                Event::Closed => app.quit = true,
            })
            .map(|_| ())
            .map_err(|e| e.to_string())
    });
    if let Err(e) = inserted {
        fail(format!("couldn't start the event loop: {e}"));
        return true;
    }
    // Dropping the sender at EOF closes the channel.
    let sender = std::sync::Mutex::new(Some(sender));
    read_stdin(move |message| {
        let mut sender = sender.lock().unwrap();
        match message {
            Some(message) => {
                if let Some(sender) = sender.as_ref() {
                    let _ = sender.send(message);
                }
            }
            None => *sender = None,
        }
    });

    send(FromHelper::Ready { mode: Mode::LayerShell });
    while !app.quit {
        let timeout = slint::platform::duration_until_next_timer_update();
        if let Err(e) = event_loop.dispatch(timeout, &mut app) {
            fail(format!("the event loop failed: {e}"));
            break;
        }
        slint::platform::update_timers_and_animations();
        if app.unmap.take() {
            app.destroy_surface();
        }
        if app.configured && !app.frame_pending {
            app.draw();
        }
    }
    true
}

impl App {
    fn handle(&mut self, message: ToHelper) {
        match message {
            ToHelper::Config(config) => {
                self.config = config;
                if let Some(state) = self.state.clone().filter(|_| self.ui.get_shown()) {
                    apply(&self.ui, &state, &self.config);
                }
                if self.size.0 > 0 {
                    self.ui.set_max_card_width(max_card_width(&self.config, self.size.0 as f64) as f32);
                }
                if let Some(surface) = &self.surface {
                    configure_layer(surface, &self.config);
                    surface.commit();
                }
            }
            ToHelper::State(state) => {
                self.shows.set(self.shows.get() + 1);
                self.unmap.set(false);
                apply(&self.ui, &state, &self.config);
                self.state = Some(state);
                if self.surface.is_none() {
                    self.create_surface();
                }
            }
            ToHelper::Levels(levels) => set_levels(&self.ui, &levels),
            ToHelper::Hide => {
                self.ui.set_shown(false);
                let (shows, at_hide, unmap) = (self.shows.clone(), self.shows.get(), self.unmap.clone());
                Timer::single_shot(HIDE_DELAY, move || {
                    if shows.get() == at_hide {
                        unmap.set(true);
                    }
                });
            }
        }
    }

    fn create_surface(&mut self) {
        let surface = self.compositor.create_surface(&self.qh);
        let layer =
            self.layer_shell.create_layer_surface(&self.qh, surface, Layer::Overlay, Some("iris-overlay"), None);
        configure_layer(&layer, &self.config);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        // No input until the first draw sets the card's region.
        let empty = Region::new(&self.compositor).ok();
        layer.wl_surface().set_input_region(empty.as_ref().map(|region| region.wl_region()));
        layer.commit();
        self.surface = Some(layer);
        self.configured = false;
        self.frame_pending = false;
        self.input_region = (0, 0, 0, 0);
    }

    fn destroy_surface(&mut self) {
        // Dropping it destroys the layer surface and its wl_surface.
        self.surface = None;
        self.configured = false;
        self.frame_pending = false;
    }

    fn resize(&mut self) {
        let scale = self.scale.max(1) as u32;
        self.slint_window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale as f32 });
        self.slint_window.set_size(PhysicalSize::new(self.size.0 * scale, self.size.1 * scale));
        self.ui.set_max_card_width(max_card_width(&self.config, self.size.0 as f64) as f32);
        self.slint_window.request_redraw();
    }

    fn draw(&mut self) {
        let Some(surface) = self.surface.as_ref().map(|layer| layer.wl_surface().clone()) else { return };
        slint::platform::update_timers_and_animations();
        let scale = self.scale.max(1) as u32;
        let (width, height) = (self.size.0 * scale, self.size.1 * scale);
        if width == 0 || height == 0 {
            return;
        }
        let pool = &mut self.pool;
        let mut drawn = None;
        self.slint_window.draw_if_needed(|renderer| {
            let created = pool.create_buffer(width as i32, height as i32, width as i32 * 4, wl_shm::Format::Argb8888);
            match created {
                Ok((buffer, canvas)) => {
                    // SAFETY: the canvas is 4-byte aligned and exactly width * height * 4 bytes.
                    let pixels: &mut [Argb] = unsafe {
                        std::slice::from_raw_parts_mut(canvas.as_mut_ptr() as *mut Argb, (width * height) as usize)
                    };
                    renderer.render(pixels, width as usize);
                    drawn = Some(buffer);
                }
                Err(e) => eprintln!("iris-overlay: couldn't create a buffer: {e}"),
            }
        });
        let Some(buffer) = drawn else { return };

        let region = (
            self.ui.get_card_x().floor() as i32,
            self.ui.get_card_y().floor() as i32,
            self.ui.get_card_width().ceil() as i32 + 1,
            self.ui.get_card_height().ceil() as i32 + 1,
        );
        if region != self.input_region
            && let Ok(input) = Region::new(&self.compositor)
        {
            input.add(region.0, region.1, region.2, region.3);
            surface.set_input_region(Some(input.wl_region()));
            self.input_region = region;
        }
        surface.set_buffer_scale(scale as i32);
        surface.damage_buffer(0, 0, width as i32, height as i32);
        if self.slint_window.has_active_animations() {
            surface.frame(&self.qh, surface.clone());
            self.frame_pending = true;
        }
        if let Err(e) = buffer.attach_to(&surface) {
            eprintln!("iris-overlay: couldn't attach a buffer: {e}");
        }
        surface.commit();
    }
}

/// Spans the output's width at the configured edge (Handy's offsets), as tall as the tallest card.
fn configure_layer(layer: &LayerSurface, config: &OverlayConfig) {
    let (edge, top, bottom) = match config.position {
        Position::Top => (Anchor::TOP, TOP_OFFSET as i32, 0),
        Position::Bottom => (Anchor::BOTTOM, 0, BOTTOM_OFFSET as i32),
    };
    layer.set_anchor(edge | Anchor::LEFT | Anchor::RIGHT);
    layer.set_margin(top, 0, bottom, 0);
    layer.set_size(0, overlay_size(config, 0.0).1.ceil() as u32);
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, factor: i32) {
        if factor != self.scale {
            self.scale = factor;
            self.resize();
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
        self.draw();
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        // The compositor removed it (e.g. its output went away); the next show makes a new one.
        self.destroy_surface();
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let height = overlay_size(&self.config, 0.0).1.ceil() as u32;
        let size = (configure.new_size.0, if configure.new_size.1 == 0 { height } else { configure.new_size.1 });
        if size != self.size {
            self.size = size;
            self.resize();
        }
        self.configured = true;
        self.slint_window.request_redraw();
        if !self.frame_pending {
            self.draw();
        }
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer
            && let Some(pointer) = self.pointer.take()
        {
            pointer.release();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        let Some(surface) = self.surface.as_ref().map(|layer| layer.wl_surface().clone()) else { return };
        for event in events.iter().filter(|event| event.surface == surface) {
            let position = LogicalPosition::new(event.position.0 as f32, event.position.1 as f32);
            let window_event = match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    WindowEvent::PointerMoved { position }
                }
                PointerEventKind::Leave { .. } => WindowEvent::PointerExited,
                PointerEventKind::Press { button: BTN_LEFT, .. } => {
                    WindowEvent::PointerPressed { position, button: PointerEventButton::Left }
                }
                PointerEventKind::Release { button: BTN_LEFT, .. } => {
                    WindowEvent::PointerReleased { position, button: PointerEventButton::Left }
                }
                _ => continue,
            };
            self.slint_window.dispatch_event(window_event);
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_seat!(App);
delegate_pointer!(App);
delegate_layer!(App);
delegate_registry!(App);

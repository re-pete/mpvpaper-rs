use std::ffi::c_void;

use calloop_wayland_source::WaylandSource;
use clap::Parser;
use khronos_egl as egl;
use libmpv2::{
    Mpv,
    render::{OpenGLInitParams, RenderContext, RenderParam, RenderParamApiType},
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry,
    output::{OutputHandler, OutputInfo, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
};
use wayland_client::{
    Connection, Proxy, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_output, wl_surface},
};
use wayland_egl::WlEglSurface;

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "mpvpaper-rs", about = "Video wallpaper using mpv on Wayland")]
struct Args {
    /// Output name to use (e.g. DP-1, HDMI-A-1, ALL)
    output: String,

    /// Video file or URL to play (omit when using --show-outputs)
    #[arg(required_unless_present = "show_outputs")]
    video: Option<String>,

    /// Forward options to mpv (quote the whole string: -o "loop no-audio")
    #[arg(short = 'o', long)]
    mpv_options: Option<String>,

    /// Shell layer to use
    #[arg(short = 'l', long, default_value = "background")]
    layer: String,

    /// List available outputs and exit
    #[arg(short = 'd', long)]
    show_outputs: bool,

    /// Enable verbose output
    #[arg(short = 'v', long)]
    verbose: bool,
}

// ── EGL ───────────────────────────────────────────────────────────────────────

struct EglState {
    display: egl::Display,
    context: egl::Context,
    config: egl::Config,
}

fn egl_get_proc_address(_: &(), name: &str) -> *mut c_void {
    egl::API
        .get_proc_address(name)
        .map(|f| unsafe { std::mem::transmute::<unsafe extern "system" fn(), *mut c_void>(f) })
        .unwrap_or(std::ptr::null_mut())
}

fn init_egl(display_ptr: *mut c_void) -> EglState {
    let egl_display = unsafe { egl::API.get_display(display_ptr) }
        .expect("Failed to get EGL display");

    egl::API.initialize(egl_display).expect("Failed to initialize EGL");
    egl::API.bind_api(egl::OPENGL_API).expect("Failed to bind OpenGL API");

    let attribs = [
        egl::SURFACE_TYPE,    egl::WINDOW_BIT,
        egl::RENDERABLE_TYPE, egl::OPENGL_BIT,
        egl::RED_SIZE,   8,
        egl::GREEN_SIZE, 8,
        egl::BLUE_SIZE,  8,
        egl::ALPHA_SIZE, 8,
        egl::NONE,
    ];
    let config = egl::API
        .choose_first_config(egl_display, &attribs)
        .expect("EGL config query failed")
        .expect("No suitable EGL config found");

    // Try GL versions from newest to oldest
    let gl_versions = [
        (4, 6), (4, 5), (4, 4), (4, 3), (4, 2), (4, 1), (4, 0),
        (3, 3), (3, 2), (3, 1), (3, 0),
    ];
    let context = gl_versions
        .iter()
        .find_map(|&(major, minor)| {
            let ctx_attribs = [
                egl::CONTEXT_MAJOR_VERSION, major,
                egl::CONTEXT_MINOR_VERSION, minor,
                egl::NONE,
            ];
            egl::API.create_context(egl_display, config, None, &ctx_attribs).ok()
        })
        .expect("Failed to create any EGL context");

    // Make context current with no surface so mpv can load GL functions
    egl::API
        .make_current(egl_display, None, None, Some(context))
        .expect("Failed to make EGL context current");

    EglState { display: egl_display, context, config }
}

// ── Per-output state ──────────────────────────────────────────────────────────

struct Output {
    wl_output: wl_output::WlOutput,
    layer: LayerSurface,
    egl_window: Option<WlEglSurface>,
    egl_surface: Option<egl::Surface>,
    size: (u32, u32),
    scale: i32,
    frame_pending: bool,
    redraw_needed: bool,
}

// ── App state ─────────────────────────────────────────────────────────────────

#[allow(dead_code)]
struct AppState {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,

    outputs: Vec<Output>,
    monitor: String,
    surface_layer: Layer,
    show_outputs: bool,
    verbose: bool,

    egl: EglState,
    mpv: &'static Mpv,
    render_ctx: Option<RenderContext<'static>>,

    qh: QueueHandle<AppState>,
}

impl AppState {
    fn render_output(&mut self, qh: &QueueHandle<AppState>, idx: usize) {
        let Some(egl_surface) = self.outputs[idx].egl_surface else { return };
        let (w, h) = self.outputs[idx].size;
        let scale = self.outputs[idx].scale;
        let pw = (w * scale as u32) as i32;
        let ph = (h * scale as u32) as i32;
        let egl_display = self.egl.display;
        let egl_ctx = self.egl.context;

        // Clone wl_surface before the render_ctx borrow to avoid conflicts
        let wl_surface = self.outputs[idx].layer.wl_surface().clone();

        egl::API
            .make_current(egl_display, Some(egl_surface), Some(egl_surface), Some(egl_ctx))
            .expect("make_current failed");

        if let Some(rc) = &self.render_ctx {
            if rc.render::<()>(0, pw, ph, true).is_err() {
                return;
            }
        } else {
            return;
        }

        // Request frame callback so the compositor tells us when to draw next
        wl_surface.frame(qh, wl_surface.clone());
        self.outputs[idx].frame_pending = true;

        egl::API.swap_buffers(egl_display, egl_surface).expect("swap_buffers failed");
        self.outputs[idx].redraw_needed = false;
    }

    fn output_info_matches(&self, info: &OutputInfo) -> bool {
        let monitor = self.monitor.as_str();
        if monitor.eq_ignore_ascii_case("all") || monitor == "*" {
            return true;
        }
        let name_match = info.name.as_deref().map_or(false, |n| n == monitor);
        let desc_match = info
            .description
            .as_deref()
            .map_or(false, |d| d.contains(monitor));
        name_match || desc_match
    }

    fn maybe_create_output(&mut self, qh: &QueueHandle<AppState>, output: wl_output::WlOutput) {
        if self.show_outputs {
            return;
        }
        let Some(info) = self.output_state.info(&output) else { return };
        if !self.output_info_matches(&info) {
            return;
        }

        let name = info.name.as_deref().unwrap_or("?");
        if self.verbose {
            eprintln!("[mpvpaper] Selected output: {name}");
        }

        let surface = self.compositor.create_surface(qh);
        let layer = self.layer_shell.create_layer_surface(
            qh,
            surface,
            self.surface_layer,
            Some("mpvpaper"),
            Some(&output),
        );
        layer.set_anchor(
            Anchor::TOP | Anchor::RIGHT | Anchor::BOTTOM | Anchor::LEFT,
        );
        layer.set_exclusive_zone(-1);
        layer.set_size(0, 0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();

        let scale = info.scale_factor;
        self.outputs.push(Output {
            wl_output: output,
            layer,
            egl_window: None,
            egl_surface: None,
            size: (0, 0),
            scale,
            frame_pending: false,
            redraw_needed: false,
        });
    }
}

// ── sctk trait impls ──────────────────────────────────────────────────────────

impl CompositorHandler for AppState {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        for i in 0..self.outputs.len() {
            if self.outputs[i].layer.wl_surface() == surface {
                self.outputs[i].scale = new_factor;
                if let Some(ew) = &self.outputs[i].egl_window {
                    let (w, h) = self.outputs[i].size;
                    ew.resize(w as i32 * new_factor, h as i32 * new_factor, 0, 0);
                }
                if !self.outputs[i].frame_pending && self.outputs[i].egl_surface.is_some() {
                    self.render_output(qh, i);
                } else {
                    self.outputs[i].redraw_needed = true;
                }
                break;
            }
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

    fn frame(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        for i in 0..self.outputs.len() {
            if self.outputs[i].layer.wl_surface() == surface {
                self.outputs[i].frame_pending = false;
                if self.outputs[i].redraw_needed {
                    self.render_output(qh, i);
                }
                break;
            }
        }
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

impl OutputHandler for AppState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.maybe_create_output(qh, output);
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.outputs.retain(|o| o.wl_output != output);
    }
}

impl LayerShellHandler for AppState {
    fn closed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        layer: &LayerSurface,
    ) {
        self.outputs.retain(|o| &o.layer != layer);
        if self.outputs.is_empty() {
            std::process::exit(0);
        }
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let Some(idx) = self.outputs.iter().position(|o| &o.layer == layer) else {
            return;
        };

        let w = configure.new_size.0.max(1);
        let h = configure.new_size.1.max(1);
        self.outputs[idx].size = (w, h);

        if self.outputs[idx].egl_window.is_none() {
            // First configure: create EGL window + surface
            let wl_surface = layer.wl_surface();
            let scale = self.outputs[idx].scale;
            let pw = w as i32 * scale;
            let ph = h as i32 * scale;

            let egl_window = WlEglSurface::new(wl_surface.id(), pw, ph)
                .expect("Failed to create WlEglSurface");

            let egl_surface = unsafe {
                egl::API
                    .create_window_surface(
                        self.egl.display,
                        self.egl.config,
                        egl_window.ptr() as egl::NativeWindowType,
                        None,
                    )
                    .expect("Failed to create EGL surface")
            };

            egl::API
                .make_current(
                    self.egl.display,
                    Some(egl_surface),
                    Some(egl_surface),
                    Some(self.egl.context),
                )
                .expect("make_current failed");
            // Disable vsync blocking — we pace via the frame callback instead
            egl::API.swap_interval(self.egl.display, 0).ok();

            self.outputs[idx].egl_window = Some(egl_window);
            self.outputs[idx].egl_surface = Some(egl_surface);

            self.render_output(qh, idx);
        } else {
            // Resize
            let scale = self.outputs[idx].scale;
            let pw = w as i32 * scale;
            let ph = h as i32 * scale;
            if let Some(ew) = &self.outputs[idx].egl_window {
                ew.resize(pw, ph, 0, 0);
            }
            if !self.outputs[idx].frame_pending {
                self.render_output(qh, idx);
            } else {
                self.outputs[idx].redraw_needed = true;
            }
        }
    }
}

impl ProvidesRegistryState for AppState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

delegate_compositor!(AppState);
delegate_output!(AppState);
delegate_layer!(AppState);
delegate_registry!(AppState);

// ── main ─────────────────────────────────────────────────────────────────────

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // ── Wayland connection ────────────────────────────────────────────────────
    let conn = Connection::connect_to_env()
        .expect("Failed to connect to Wayland compositor");
    let (globals, event_queue) = registry_queue_init(&conn)
        .expect("Failed to enumerate Wayland globals");
    let qh: QueueHandle<AppState> = event_queue.handle();

    // ── Bind Wayland globals ──────────────────────────────────────────────────
    let compositor = CompositorState::bind(&globals, &qh)
        .expect("wl_compositor not available");
    let layer_shell = LayerShell::bind(&globals, &qh)
        .expect("zwlr_layer_shell_v1 not available — is your compositor wlroots-based?");
    let output_state = OutputState::new(&globals, &qh);
    let registry_state = RegistryState::new(&globals);

    // ── EGL ───────────────────────────────────────────────────────────────────
    // Wayland display pointer is needed for EGL and for the mpv render context
    let display_ptr = conn.display().id().as_ptr() as *mut c_void;
    let egl = init_egl(display_ptr);

    // ── mpv ───────────────────────────────────────────────────────────────────
    // Box::leak gives us &'static Mpv so RenderContext<'static> can live in AppState
    let mpv_options = args.mpv_options.clone();
    let mpv: &'static Mpv = Box::leak(Box::new(
        Mpv::with_initializer(|init| {
            init.set_option("input-default-bindings", "yes")?;
            init.set_option("input-terminal", "yes")?;
            init.set_option("terminal", "yes")?;
            init.set_option("config", "yes")?;
            init.set_option("background-color", "#00000000")?;
            init.set_option("vo", "libmpv")?;
            // Extra mpv options forwarded via temp config file
            if let Some(opts) = &mpv_options {
                let path = format!("/tmp/mpvpaper-rs-{}.conf", std::process::id());
                std::fs::write(&path, opts.replace(' ', "\n"))
                    .expect("Failed to write mpv temp config");
                init.load_config(&path)?;
                std::fs::remove_file(&path).ok();
            }
            Ok(())
        })
        .expect("Failed to create mpv instance"),
    ));

    // Create the OpenGL render context
    let render_ctx: RenderContext<'static> = mpv
        .create_render_context(vec![
            RenderParam::WaylandDisplay(display_ptr as *const c_void),
            RenderParam::ApiType(RenderParamApiType::OpenGl),
            RenderParam::InitParams(OpenGLInitParams {
                get_proc_address: egl_get_proc_address,
                ctx: (),
            }),
        ])
        .expect("Failed to create mpv render context");

    // ── Surface layer ─────────────────────────────────────────────────────────
    let surface_layer = match args.layer.to_lowercase().as_str() {
        "top"        => Layer::Top,
        "bottom"     => Layer::Bottom,
        "overlay"    => Layer::Overlay,
        "background" => Layer::Background,
        other => {
            eprintln!("Unknown layer '{other}', defaulting to background");
            Layer::Background
        }
    };

    // ── Build app state ───────────────────────────────────────────────────────
    let mut state = AppState {
        registry_state,
        output_state,
        compositor,
        layer_shell,
        outputs: Vec::new(),
        monitor: args.output.clone(),
        surface_layer,
        show_outputs: args.show_outputs,
        verbose: args.verbose,
        egl,
        mpv,
        render_ctx: None, // set below after ping setup
        qh: qh.clone(),
    };

    // Two roundtrips: discover globals, then collect output info / create surfaces
    // (OutputHandler::new_output is called here for each output)
    let mut eq = event_queue;
    eq.roundtrip(&mut state)?;
    eq.roundtrip(&mut state)?;

    // ── --show-outputs ────────────────────────────────────────────────────────
    if args.show_outputs {
        for output in state.output_state.outputs() {
            if let Some(info) = state.output_state.info(&output) {
                println!(
                    "Output: {}  Identifier: {}",
                    info.name.as_deref().unwrap_or("?"),
                    info.description.as_deref().unwrap_or("?"),
                );
            }
        }
        return Ok(());
    }

    if state.outputs.is_empty() {
        eprintln!("No matching output found for '{}'", args.output);
        std::process::exit(1);
    }

    // ── calloop event loop ────────────────────────────────────────────────────
    let mut event_loop: calloop::EventLoop<AppState> = calloop::EventLoop::try_new()?;
    let loop_handle = event_loop.handle();

    // Wire up mpv render-update → ping → calloop
    let (ping_sender, ping_source) = calloop::ping::make_ping()?;
    let ping_sender2 = ping_sender.clone();
    let mut render_ctx = render_ctx;
    render_ctx.set_update_callback(move || ping_sender2.ping());
    state.render_ctx = Some(render_ctx);

    loop_handle
        .insert_source(ping_source, |_, _, state| {
            if let Some(rc) = &state.render_ctx {
                let _ = rc.update();
            }
            let qh = state.qh.clone();
            for i in 0..state.outputs.len() {
                if state.outputs[i].egl_surface.is_none() {
                    continue;
                }
                if !state.outputs[i].frame_pending {
                    state.render_output(&qh, i);
                } else {
                    state.outputs[i].redraw_needed = true;
                }
            }
        })
        .expect("Failed to insert ping source");

    WaylandSource::new(conn, eq)
        .insert(loop_handle)
        .map_err(|e| e.error)?;

    // Load media
    let video = args.video.as_deref().unwrap();
    if let Some(playlist) = video.strip_prefix("--playlist=") {
        mpv.command("loadlist", &[playlist, "replace"])?;
    } else {
        mpv.command("loadfile", &[video, "replace"])?;
    }

    // Run until the compositor closes all surfaces or mpv shuts down
    loop {
        event_loop.dispatch(None, &mut state)?;
    }
}

// Needed by anyhow::Result in main
extern crate anyhow;

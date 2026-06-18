# mpvpaper-rs Design

Rust rewrite of [mpvpaper](https://github.com/GhostNaN/mpvpaper), a Wayland video wallpaper player.

## Motivation

The C version has unsynchronised shared state across pthreads (`is_paused`, `frame_ready`,
`stop_render_loop` are plain ints accessed from multiple threads without atomics), which
causes race conditions. Several GitHub issues report memory leaks. Rust's type system prevents
these classes of bugs.

## Architecture

**One process per monitor** — same as the C version. Launch multiple instances to play different
videos on different outputs. A single instance still accepts `ALL` to mirror across every output.

### Stack

| Concern | Crate |
|---------|-------|
| Wayland client | `wayland-client 0.31` (with `system` feature for C libwayland backend) |
| Layer shell | `smithay-client-toolkit 0.20` (`LayerShell`, `LayerShellHandler`) |
| Event loop | `calloop 0.14` + `calloop-wayland-source 0.4` |
| EGL window | `wayland-egl 0.32` |
| EGL context | `khronos-egl 6` (with `static` feature → `egl::API` global, `Surface: Copy`) |
| mpv render | `libmpv2 6` (with `render` feature) |
| CLI | `clap 4` |

### Key design decisions

**`Box::leak` for `&'static Mpv`**
`RenderContext<'a>` borrows `&'a Mpv` and can't be stored alongside its owner in the same
struct. Leaking the `Mpv` allocation gives a `&'static Mpv`, so `RenderContext<'static>`
can live in `AppState`. Acceptable for a long-running wallpaper process.

**calloop ping instead of eventfd**
mpv's wakeup callback uses `calloop::ping::make_ping()` rather than raw `libc::eventfd`.
The `PingSender` is `Send + Clone` and integrates cleanly into the calloop event loop.

**Frame-paced rendering**
After each `swap_buffers`, a `wl_surface.frame()` callback is registered. The next render
only happens when the compositor signals readiness (via `CompositorHandler::frame`), or
when mpv signals a new frame via the ping source. This avoids busy-looping and respects
compositor vsync.

**EGL `Surface: Copy`**
`khronos_egl::Surface` is `Copy`, so it can be extracted from `self` before a mutable
borrow, sidestepping borrow-checker conflicts in `render_output`.

## Data model

```
AppState
├── registry_state: RegistryState      (sctk global registry)
├── output_state:   OutputState        (tracks wl_output globals)
├── compositor:     CompositorState    (wl_compositor)
├── layer_shell:    LayerShell         (zwlr_layer_shell_v1)
├── outputs:        Vec<Output>        (one per matched monitor)
├── egl:            EglState           (display, context, config — all Copy)
├── mpv:            &'static Mpv       (kept alive; render_ctx borrows it)
└── render_ctx:     Option<RenderContext<'static>>

Output
├── wl_output:      WlOutput
├── layer:          LayerSurface       (zwlr_layer_surface_v1)
├── egl_window:     Option<WlEglSurface>
├── egl_surface:    Option<egl::Surface>
├── size:           (u32, u32)         (logical pixels, from configure)
├── scale:          i32                (HiDPI factor)
├── frame_pending:  bool               (waiting for compositor frame callback)
└── redraw_needed:  bool               (mpv produced a frame while we were waiting)
```

## Event flow

```
Wayland compositor
  └─ configure (size)   →  create EGL window+surface, first render
  └─ frame callback     →  CompositorHandler::frame → render if redraw_needed

mpv thread
  └─ update callback    →  PingSender::ping()

calloop event loop
  └─ PingSource fires   →  render_ctx.update(), then render all outputs
                           (or set redraw_needed if frame_pending)
```

## MVP scope

Included:
- Play a video/URL on a named output or `ALL`
- `-o "mpv options"` forwarding via temp config file
- `-l layer` (background / bottom / top / overlay)
- `-v` verbose
- `-d` list available outputs

Excluded (possible future work):
- `mpvpaper-holder` companion binary (auto-stop on occlusion)
- `--auto-pause` / `--auto-stop`
- `--slideshow`
- `--fork`

## Building

```bash
cargo build --release
./target/release/mpvpaper-rs --help
./target/release/mpvpaper-rs -d                       # list outputs
./target/release/mpvpaper-rs DP-2 video.mp4
./target/release/mpvpaper-rs -o "loop" ALL video.mp4
```

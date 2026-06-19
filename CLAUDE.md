# mpvpaper-rs — Developer Notes

Single-binary Rust rewrite of mpvpaper (video and image wallpaper player). Everything lives in `src/main.rs`.
See `DESIGN.md` for the full architecture rationale.

## Key crate facts (verified against source)

- **`libmpv2 6` + `render` feature**: `Mpv::with_initializer(|init| ...)` for setup;
  `mpv.create_render_context(vec![...])` for the GL render context.
  `load_config` is on `MpvInitializer` (inside the closure), NOT on `Mpv`.

- **`&'static Mpv` pattern**: `RenderContext<'a>` borrows `&'a Mpv`. To store both in
  `AppState`, use `Box::leak(Box::new(mpv))` → `&'static Mpv` →
  `RenderContext<'static>`. This is intentional; do not try to add a lifetime to AppState.

- **`khronos-egl 6` with `static` feature**: exposes `pub static API: Instance<Static>`.
  `egl::Surface`, `egl::Display`, `egl::Context`, `egl::Config` are all `Copy`.
  `API.make_current(...)` and `API.swap_buffers(...)` are NOT unsafe.
  `API.create_window_surface(...)` IS unsafe (takes a raw pointer).

- **`wayland-client 0.31` with `system` feature**: required for `ObjectId::as_ptr()` to
  obtain the raw `*mut wl_proxy` needed for the EGL display pointer.
  Import `wayland_client::Proxy` to call `.id()` on `WlSurface` / `WlDisplay`.

- **`wayland-egl 0.32`**: `WlEglSurface::new(surface.id(), w, h)` takes `ObjectId`.
  `.ptr() -> *const c_void` for passing to EGL.

- **`smithay-client-toolkit 0.20`**: sctk handles `Dispatch<WlCallback, WlSurface>`
  automatically when `delegate_compositor!` is used — no manual impl needed.
  `LayerSurface: PartialEq` (safe to compare with `==`).
  `WaylandSurface` trait must be in scope for `.wl_surface()`.

- **calloop `PingSource`**: `type Ret = ()` — the `insert_source` callback returns `()`
  not `Result<PostAction, _>`.

## Render flow

1. `configure` (first time) → create `WlEglSurface` + `egl::Surface` → `render_output()`
2. `render_output()` → `make_current` → `rc.render()` → `wl_surface.frame(qh, ...)` →
   `swap_buffers` → set `frame_pending = true`
3. mpv update → `PingSender::ping()` → calloop fires → `rc.update()` → `render_output()`
   (or set `redraw_needed = true` if `frame_pending`)
4. Compositor frame callback → `CompositorHandler::frame` → if `redraw_needed` → `render_output()`

## Display pointer

```rust
let display_ptr = conn.display().id().as_ptr() as *mut c_void;
// Used for both EGL init and RenderParam::WaylandDisplay
```

## EGL proc address for mpv

```rust
fn egl_get_proc_address(_: &(), name: &str) -> *mut c_void {
    egl::API.get_proc_address(name)
        .map(|f| unsafe { std::mem::transmute::<unsafe extern "system" fn(), *mut c_void>(f) })
        .unwrap_or(std::ptr::null_mut())
}
```

`transmute` is required — Rust does not allow direct cast from function pointer to `*mut c_void`.

## MVP scope (not yet implemented)

- `mpvpaper-holder` companion binary (auto-stop on occlusion/minimise)
- `--auto-pause` / `--auto-stop`
- `--slideshow`
- `--fork` (daemonise)

# mpvpaper-rs

Rust rewrite of [mpvpaper](../README.md) — a video wallpaper player for wlroots-based Wayland
compositors (Sway, Hyprland, etc.). Uses mpv for playback, EGL for rendering, and
`zwlr_layer_shell_v1` for the wallpaper layer surface.

See [DESIGN.md](DESIGN.md) for architecture decisions.

## Dependencies

- `libmpv` (runtime)
- `libEGL` (runtime)
- `libwayland-client` (runtime)
- Rust toolchain (build)

## Build

```bash
cargo build --release
```

The binary lands at `target/release/mpvpaper-rs`.

## Usage

```
mpvpaper-rs [OPTIONS] <OUTPUT> <VIDEO>
```

**Arguments:**

| Argument | Description |
|----------|-------------|
| `<OUTPUT>` | Wayland output name (`DP-2`, `HDMI-A-1`, `ALL`) |
| `<VIDEO>` | Video file, URL, or `--playlist=/path/to/playlist` |

**Options:**

| Flag | Description |
|------|-------------|
| `-o, --mpv-options <OPTS>` | Options forwarded to mpv (quoted string) |
| `-l, --layer <LAYER>` | Shell layer: `background` (default), `bottom`, `top`, `overlay` |
| `-d, --show-outputs` | List available outputs and exit |
| `-v, --verbose` | Print selected output name on startup |
| `-h, --help` | Show help |

## Examples

```bash
# Play on a specific output
mpvpaper-rs DP-2 /path/to/video.mp4

# Mirror on all outputs
mpvpaper-rs ALL /path/to/video.mp4

# Loop with no audio
mpvpaper-rs -o "loop no-audio" DP-2 /path/to/video.mp4

# IPC socket for external control (e.g. toggle pause)
mpvpaper-rs -o "input-ipc-server=/tmp/mpv-socket" DP-1 /path/to/video.mp4
echo 'cycle pause' | socat - /tmp/mpv-socket

# List available outputs
mpvpaper-rs -d

# Multiple independent monitors — launch two instances
mpvpaper-rs DP-1 video1.mp4 &
mpvpaper-rs HDMI-A-1 video2.mp4 &
```

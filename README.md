# Quick Video Editor

Quick Video Editor cuts out a fragment of a video given the start and end timestamps. The video is never re-encoded, so the process is very fast and does not reduce the video quality.

This is a fork of [Video Trimmer](https://gitlab.gnome.org/YaLTeR/video-trimmer) by Ivan Molodetskikh. Differences from upstream so far:

- Drop an audio file (MP3, OGG, FLAC…) onto the window to lay music over the trim. Drag its waveform to line it up with the video, set its volume with the knob next to it, and optionally fade it out during the last second. The music is cut where the video ends. The video is still copied without re-encoding; only the audio is encoded. With **Remove Audio** enabled, the music replaces the original audio, otherwise the two are mixed. From the command line: `--music PATH`.
- A **speed knob** speeds the trimmed video up or slows it down (10%–1000%); drag it up or down or scroll, double-click to reset. The preview plays at the chosen speed. A speed change re-encodes the video (x264, or VP9 for WebM) and time-stretches the audio without changing its pitch.
- The preview runs on GStreamer Editing Services instead of GtkMediaFile: scrubbing uses keyframe seeks and lands on the exact frame on release, and playback no longer gets stuck after seeking. NVDEC hardware decoders are avoided, since they intermittently hang on seek; set `GST_PLUGIN_FEATURE_RANK` yourself to override this.

![Screenshot of the window.](https://gitlab.gnome.org/-/project/11135/uploads/f6e5a36b50822816e1aa191b63bab3b5/Screenshot_from_2025-03-28_15-32-23.png)

## Installing

Download a package from the [latest release](https://github.com/jjolmo/QuickVideoEditor/releases/latest):

- **Flatpak** (any distribution; bundles GStreamer Editing Services and codecs):
  `flatpak install --user QuickVideoEditor.flatpak`
- **Ubuntu 26.04 / Debian**: `sudo apt install ./quick-video-editor_*_amd64.deb`
- **Fedora**: `sudo dnf install ./quick-video-editor-*.rpm`. Fedora's own ffmpeg and GStreamer lack H.264 encoding; install the RPM Fusion versions to speed up or export MP4 videos.
- **Arch Linux**: `sudo pacman -U quick-video-editor-*.pkg.tar.zst`, or build `packaging/arch/PKGBUILD` with `makepkg -si`.

The native packages need GTK 4.16, libadwaita 1.8 and GStreamer Editing Services 1.24 or newer; on older distributions use the Flatpak.

## Trimmer and Editor

Quick Video Editor has two modes, switched from the header bar:

- **Trimmer**, for a single video: set a start and an end and trim without re-encoding, as in Video Trimmer. A speed knob changes the speed of the trimmed video.
- **Editor**, for one or more videos: the timeline shows segments that you can cut at the playhead (**S**), delete (**Delete**), move by dragging, trim by dragging their edges, and speed up or slow down (10%–1000%) by **Ctrl**-dragging an edge (a spring shows the change). Overlapping two segments crossfades them for as long as they overlap. **Fade Out at End** fades the last second to black and silence. A thin track under the timeline shows the videos' own audio, with a knob for its volume. **Ctrl**+scroll zooms the timeline. **Ctrl+Z** undoes an edit and **Ctrl+Shift+Z** or **Ctrl+Y** redoes it.

The options menu sets the frame rate and resolution of the export: by default both match the (first) source video, or pick 24–60 fps or any custom rate, and 1080p, 720p or a custom size. **Snap to 0:59** makes the trim and the Editor's edges snap to the one-minute limit of many social networks. Other aspect ratios are letterboxed, never stretched.

Dropping or opening a second video adds it at the end of the edit and switches to the Editor. The Editor exports with GStreamer Editing Services, the same engine as the preview, so the export matches what plays: H.264/AAC for `.mp4` and `.mkv`, VP9/Opus for `.webm`.

## Command-line arguments

You can pass the input video path and the default output video path as command-line arguments:

```
$ quick-video-editor --output trimmed.mp4 input_video.mp4
```

Several videos open in the Editor, one after another. With `--export`, the edit is rendered without opening the window:

```
$ quick-video-editor first.mp4 second.mp4 --export joined.mp4
```

The Flatpak version needs a special `--file-forwarding` flag and `@@` marker to pass the input video through the sandbox:

```
$ flatpak run --file-forwarding io.github.jjolmo.QuickVideoEditor -o trimmed.mp4 @@ input_video.mp4
```

Other options like start and end timestamp can also be set through command-line arguments. See `quick-video-editor --help`.

## Format support

For trimming Quick Video Editor uses the `ffmpeg` binary, thus the non-Flatpak version depends on the muxers and demuxers available in your system's `ffmpeg`. The Flatpak package uses the `ffmpeg` of the GNOME Platform runtime.

The video preview goes through GTK 4 which usually relies on GStreamer, and therefore your system's or Flatpak GNOME Platform's installed GStreamer plugins.

The optional re-encoding also uses `ffmpeg` and thus needs the respective decoders and encoders to work. For `.mp4` output extension Quick Video Editor sets the encoder to `libvpx-vp9` for better Flatpak support, for other output extensions it leaves the decision `ffmpeg`.

## Translations

Translations are inherited from upstream Video Trimmer. To help translate the original app: https://l10n.gnome.org/module/video-trimmer/.

## Building

Dependencies: Rust (see `rust-toolchain.toml`), GTK 4, libadwaita, blueprint-compiler, meson and ninja. On Debian/Ubuntu:
```
sudo apt install libgtk-4-dev libadwaita-1-dev blueprint-compiler meson gettext desktop-file-utils appstream ffmpeg
```

Build and run from the build directory:
```
meson setup build
ninja -C build
meson devenv -C build ./src/quick-video-editor
```

## License

GPL-3.0-or-later, same as upstream. See `COPYING`.

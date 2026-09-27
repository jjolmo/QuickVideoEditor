# Quick Video Editor

Quick Video Editor cuts out a fragment of a video given the start and end timestamps. The video is never re-encoded, so the process is very fast and does not reduce the video quality.

This is a fork of [Video Trimmer](https://gitlab.gnome.org/YaLTeR/video-trimmer) by Ivan Molodetskikh. Differences from upstream so far:

- The video preview avoids NVDEC hardware decoders, which intermittently hang the playback pipeline on seek. Set `GST_PLUGIN_FEATURE_RANK` yourself to override this.

![Screenshot of the window.](https://gitlab.gnome.org/-/project/11135/uploads/f6e5a36b50822816e1aa191b63bab3b5/Screenshot_from_2025-03-28_15-32-23.png)

## Command-line arguments

You can pass the input video path and the default output video path as command-line arguments:

```
$ quick-video-editor --output trimmed.mp4 input_video.mp4
```

The Flatpak version needs a special `--file-forwarding` flag and `@@` marker to pass the input video through the sandbox:

```
$ flatpak run --file-forwarding io.github.jjolmo.QuickVideoEditor -o trimmed.mp4 @@ input_video.mp4
```

Other options like start and end timestamp can also be set through command-line arguments. See `quick-video-editor --help`.

## Format support

For trimming Quick Video Editor uses the `ffmpeg` binary, thus the non-Flatpak version depends on the muxers and demuxers available in your system's `ffmpeg`. The Flatpak package uses the `org.freedesktop.Platform.ffmpeg-full` extension.

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

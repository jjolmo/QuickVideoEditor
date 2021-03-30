# Video Trimmer

Video Trimmer cuts out a fragment of a video given the start and end timestamps. The video is never re-encoded, so the process is very fast and does not reduce the video quality.

<a href='https://flathub.org/apps/details/org.gnome.gitlab.YaLTeR.VideoTrimmer'><img width='240' alt='Download on Flathub' src='https://flathub.org/assets/badges/flathub-badge-en.png'/></a>

![Screenshot of the window.](/uploads/0596c5d3be7de3204c4dc40baa23a1a0/image.png)

## Command-line arguments

You can pass the input video path and the default output video path as command-line arguments:

```
$ video-trimmer --output trimmed.mp4 input_video.mp4
```

The Flatpak version needs a special `--file-forwarding` flag and `@@` marker to pass the input video through the sandbox:

```
$ flatpak run --file-forwarding org.gnome.gitlab.YaLTeR.VideoTrimmer -o trimmed.mp4 @@ input_video.mp4
```

## Format support

For trimming Video Trimmer uses the `ffmpeg` binary, thus the non-Flatpak version depends on the muxers and demuxers available in your system's `ffmpeg`. The Flatpak package contains `ffmpeg` built with `--enable-gpl` muxers and demuxers which should support everything imaginable.

The video preview goes through GTK 4 which usually relies on GStreamer, and therefore your system's or Flatpak GNOME Platform's installed GStreamer plugins.

## Contributing translations

You can help translate Video Trimmer on POEditor: https://poeditor.com/join/project?hash=5bXw9CXAAh. Any help is appreciated!

## Building

The easiest way is to clone the repository with GNOME Builder and press the Build button.

Alternatively, you can build it manually:
```
meson -Dprofile=development -Dprefix=$PWD/install build
ninja -C build install
```

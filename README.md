# Video Trimmer

Video Trimmer cuts out a fragment of a video given the start and end timestamps. The video is never re-encoded, so the process is very fast and does not reduce the video quality.

<a href='https://flathub.org/apps/details/org.gnome.gitlab.YaLTeR.VideoTrimmer'><img width='240' alt='Download on Flathub' src='https://flathub.org/assets/badges/flathub-badge-en.png'/></a>

![Screenshot of the window.](/uploads/717fa281218d3649a9d72e46e3ab4e08/image.png)

## Format support

For trimming the `ffmpeg` binary is used and thus the non-Flatpak version depends on the muxers and demuxers available in your system's `ffmpeg`. The Flatpak package contains `ffmpeg` built with `--enable-gpl` muxers and demuxers which should support everything imaginable.

The video preview relies on GStreamer, and therefore your system's or Flatpak GNOME Platform's installed GStreamer plugins.

## Building

The easiest way is to clone the repository with GNOME Builder and press the Build button.

Alternatively, you can build it manually:
```
meson -Dprofile=development -Dprefix=$PWD/install build
ninja -C build install
```

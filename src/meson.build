pkgdatadir = join_paths(get_option('prefix'), get_option('datadir'), meson.project_name())
gnome = import('gnome')

blps = [
  'window.blp',
  'video_preview.blp',
  'timeline.blp',
  'shortcuts.blp',
]

uis = []
foreach blp : blps
  uis += blp.replace('.blp', '.ui')
endforeach

blueprint_compiler = find_program('blueprint-compiler', version: '>= 0.10.0')

blueprints = custom_target('blueprints',
  input: blps,
  output: uis,
  command: [blueprint_compiler, 'batch-compile', '@OUTDIR@', '@CURRENT_SOURCE_DIR@', '@INPUT@'],
)

gnome.compile_resources('video-trimmer',
  configure_file(
    input: 'video_trimmer.gresource.xml.in',
    output: '@BASENAME@',
    configuration: { 'appdata-file': appstream_file.full_path() },
  ),
  gresource_bundle: true,
  install: true,
  install_dir: pkgdatadir,
  dependencies: [blueprints, appstream_file],
)

conf = configuration_data()
conf.set_quoted('LOCALEDIR', join_paths(get_option('prefix'), get_option('localedir')))
conf.set_quoted('PKGDATADIR', pkgdatadir)
conf.set_quoted('VERSION', meson.project_version() + version_suffix)
conf.set_quoted('NAME_SUFFIX', name_suffix)
conf.set_quoted('APP_ID', app_id)
conf.set_quoted('G_LOG_DOMAIN', 'VideoTrimmer')
conf.set_quoted('PROFILE', profile)

configure_file(
    input: 'config.rs.in',
    output: 'config.rs',
    configuration: conf
)

# Copy the config.rs output to the source directory.
run_command(
  'cp',
  join_paths(meson.project_build_root(), 'src', 'config.rs'),
  join_paths(meson.project_source_root(), 'src', 'config.rs'),
  check: true
)

sources += files(
  'application.rs',
  'config.rs',
  'main.rs',
  'parse.rs',
  'timeline.rs',
  'video_preview.rs',
  'util.rs',
  'window.rs',
)

cargo_script = find_program(join_paths(meson.project_source_root(), 'build-aux/cargo.sh'))
cargo_release = custom_target(
  'cargo-build',
  build_by_default: true,
  input: sources,
  output: meson.project_name(),
  console: true,
  install: true,
  install_dir: get_option('bindir'),
  command: [
    cargo_script,
    meson.project_build_root(),
    meson.project_source_root(),
    '@OUTPUT@',
    app_id_suffix,
    meson.project_name(),
  ]
)

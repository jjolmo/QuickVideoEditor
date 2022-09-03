project('video-trimmer',
        'rust',
          version: '0.7.1',
    meson_version: '>= 0.59.0',
          license: 'GPL3+',
)

dependency('gtk4', version: '>= 4.0.0')
dependency('libadwaita-1')

i18n = import('i18n')

if get_option('profile') == 'development'
  profile = 'Devel'
  app_id_suffix = '.Devel'
  name_suffix = ' (Devel)'
  vcs_tag = run_command('git', 'rev-parse', '--short', 'HEAD').stdout().strip()
  if vcs_tag == ''
    version_suffix = '-devel'
  else
    version_suffix = '-' + vcs_tag
  endif
else
  profile = ''
  app_id_suffix = ''
  name_suffix = ''
  version_suffix = ''
endif

base_id = 'org.gnome.gitlab.YaLTeR.VideoTrimmer'
app_id = base_id + app_id_suffix

sources = files(
  'Cargo.lock',
  'Cargo.toml',
)

subdir('data')
subdir('src')
subdir('po')

gnome = import('gnome')
gnome.post_install(
  glib_compile_schemas: true,
  gtk_update_icon_cache: true,
  update_desktop_database: true,
)

meson.add_dist_script(
  'build-aux/dist-vendor.sh',
  meson.project_build_root() / 'meson-dist' / meson.project_name() + '-' + meson.project_version(),
  meson.project_source_root()
)

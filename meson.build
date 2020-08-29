project('video-trimmer',  
        'rust',
          version: '0.3.0',
    meson_version: '>= 0.50.0',
          license: 'GPL3+',
)

i18n = import('i18n')

if get_option('profile') == 'development'
  app_id_suffix = 'Devel'
  name_suffix = ' (Devel)'
  vcs_tag = run_command('git', 'rev-parse', '--short', 'HEAD').stdout().strip()
  if vcs_tag == ''
    version_suffix = '-devel'
  else
    version_suffix = '-' + vcs_tag
  endif
else
  app_id_suffix = ''
  name_suffix = ''
  version_suffix = ''
endif

base_id = 'org.gnome.gitlab.YaLTeR.VideoTrimmer'
app_id = base_id + app_id_suffix

subdir('data')
subdir('src')
subdir('po')

meson.add_install_script('build-aux/meson/postinstall.py')

meson.add_dist_script(
  'build-aux/dist-vendor.sh',
  meson.build_root() / 'meson-dist' / meson.project_name() + '-' + meson.project_version(),
  meson.source_root()
)

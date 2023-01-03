subdir('icons')

data_conf = configuration_data()
data_conf.set('app_id', app_id)

desktop_file = i18n.merge_file(
  input: configure_file(
    input: base_id + '.desktop.in.in',
    output: '@BASENAME@',
    configuration: data_conf
  ),
  output: app_id + '.desktop',
  type: 'desktop',
  po_dir: '../po',
  install: true,
  install_dir: get_option('datadir') / 'applications'
)

desktop_utils = find_program('desktop-file-validate', required: false)
if desktop_utils.found()
  test('Validate desktop file', desktop_utils,
    args: [desktop_file]
  )
endif

appstream_file = i18n.merge_file(
  input: configure_file(
    input: base_id + '.metainfo.xml.in.in',
    output: '@BASENAME@',
    configuration: data_conf
  ),
  output: app_id + '.metainfo.xml',
  po_dir: '../po',
  install: true,
  install_dir: get_option('datadir') / 'metainfo'
)

appstream_util = find_program('appstream-util', required: false)
if appstream_util.found()
  test('Validate appstream file', appstream_util,
    args: ['validate-relax', appstream_file]
  )
endif

install_data('org.gnome.gitlab.YaLTeR.VideoTrimmer.gschema.xml',
  install_dir: get_option('datadir') / 'glib-2.0' / 'schemas'
)
gnome.compile_schemas()

compile_schemas = find_program('glib-compile-schemas', required: false)
if compile_schemas.found()
  test('Validate schema file', compile_schemas,
    args: ['--strict', '--dry-run', meson.current_source_dir()]
  )
endif

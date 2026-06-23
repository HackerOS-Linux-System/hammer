project('hammer-store', 'vala', 'c',
  version: '0.4.0',
  license: 'Apache-2.0',
  meson_version: '>= 0.59',
  default_options: ['warning_level=2']
)

gnome  = import('gnome')
i18n   = import('i18n')

deps = [
  dependency('gtk4',           version: '>= 4.10'),
  dependency('libadwaita-1',   version: '>= 1.4'),
  dependency('glib-2.0',       version: '>= 2.76'),
  dependency('json-glib-1.0',  version: '>= 1.6'),
]

# GResources (icons, UI files)
resources = gnome.compile_resources(
  'hammer-store-resources',
  'data/org.hackerOS.HammerStore.gresource.xml',
  source_dir: 'data',
  c_name: 'hammer_store',
)

sources = files(
  'src/Application.vala',
  'src/MainWindow.vala',
  'src/Models.vala',
  'src/Views.vala',
  'src/Widgets.vala',
)

executable('hammer-store',
  [sources, resources],
  dependencies: deps,
  install: true,
  install_dir: get_option('bindir'),
)

install_data('data/org.hackerOS.HammerStore.desktop',
  install_dir: join_paths(get_option('datadir'), 'applications')
)

install_data('data/icons/hammer-store.svg',
  install_dir: join_paths(get_option('datadir'), 'icons', 'hicolor', 'scalable', 'apps')
)

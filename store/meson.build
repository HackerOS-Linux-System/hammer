project('hammer-store', 'vala', 'c',
    version:  '0.0.1',
    license:  'Apache-2.0',
    meson_version: '>= 0.60',
    default_options: [
        'c_std=c11',
        'warning_level=2',
        'werror=false',
    ],
)

# ─────────────────────────────────────────────────────────────
#  Dependencies
# ─────────────────────────────────────────────────────────────

gtk4_dep   = dependency('gtk4',    version: '>= 4.6',  required: true)
glib_dep   = dependency('glib-2.0',version: '>= 2.72', required: true)
gio_dep    = dependency('gio-2.0', version: '>= 2.72', required: true)

# ─────────────────────────────────────────────────────────────
#  Vala compiler flags
# ─────────────────────────────────────────────────────────────

vala_args = [
    '--target-glib=2.72',
    '--enable-experimental',
]

# ─────────────────────────────────────────────────────────────
#  Sources
# ─────────────────────────────────────────────────────────────

sources = files(
    'src/main.vala',
)

# ─────────────────────────────────────────────────────────────
#  Executable
# ─────────────────────────────────────────────────────────────

hammer_store_exe = executable(
    'store',
    sources,
    vala_args:    vala_args,
    dependencies: [gtk4_dep, glib_dep, gio_dep],
    install:      true,
    # Install to /usr/share/hammer/ — launched by `hammer store`
    install_dir:  get_option('datadir') / 'hammer',
)

# ─────────────────────────────────────────────────────────────
#  Desktop entry (optional — for app launchers)
# ─────────────────────────────────────────────────────────────

desktop_data = configuration_data()
desktop_data.set('VERSION',     meson.project_version())
desktop_data.set('STORE_PATH',  get_option('prefix') / get_option('datadir') / 'hammer' / 'store')

configure_file(
    input:         'data/org.hackerOS.HammerStore.desktop.in',
    output:        'org.hackerOS.HammerStore.desktop',
    configuration: desktop_data,
    install:       true,
    install_dir:   get_option('datadir') / 'applications',
)

# ─────────────────────────────────────────────────────────────
#  AppStream metainfo (optional)
# ─────────────────────────────────────────────────────────────

install_data(
    'data/org.hackerOS.HammerStore.metainfo.xml',
    install_dir: get_option('datadir') / 'metainfo',
)

# ─────────────────────────────────────────────────────────────
#  Icon
# ─────────────────────────────────────────────────────────────

install_data(
    'data/org.hackerOS.HammerStore.svg',
    install_dir: get_option('datadir') / 'icons' / 'hicolor' / 'scalable' / 'apps',
)

# ─────────────────────────────────────────────────────────────
#  Summary
# ─────────────────────────────────────────────────────────────

summary({
    'prefix':     get_option('prefix'),
    'datadir':    get_option('datadir'),
    'Store bin':  get_option('prefix') / get_option('datadir') / 'hammer' / 'store',
}, section: 'Hammer Store')

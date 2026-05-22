project('hammer-store', 'vala', 'c',
    version:  '0.0.1',
    license:  'Apache-2.0',
    meson_version: '>= 0.60',
    default_options: [
        'c_std=c11',
        'warning_level=1',
        'werror=false',
    ],
)

# ─────────────────────────────────────────────────────────────
#  Dependencies
#  Note: we deliberately do NOT add gio-unix-2.0 — the Vala
#  code uses only pure GLib IOChannel which is in glib-2.0.
# ─────────────────────────────────────────────────────────────

gtk4_dep  = dependency('gtk4',     version: '>= 4.6',  required: true)
glib_dep  = dependency('glib-2.0', version: '>= 2.72', required: true)
gio_dep   = dependency('gio-2.0',  version: '>= 2.72', required: true)

# ─────────────────────────────────────────────────────────────
#  Detect GTK version for AlertDialog conditional compilation
# ─────────────────────────────────────────────────────────────

gtk_version = gtk4_dep.version()
gtk_major   = gtk_version.split('.')[0].to_int()
gtk_minor   = gtk_version.split('.')[1].to_int()

# AlertDialog available since GTK 4.10
have_alert_dialog = gtk_major > 4 or (gtk_major == 4 and gtk_minor >= 10)

vala_args = [
    '--target-glib=2.72',
    '--enable-experimental',
]

if have_alert_dialog
    vala_args += ['--define=GTK_4_10']
    message('GTK >= 4.10 detected — using AlertDialog')
else
    message('GTK < 4.10 — using fallback Dialog for errors')
endif

# ─────────────────────────────────────────────────────────────
#  Executable
# ─────────────────────────────────────────────────────────────

executable(
    'store',
    'src/main.vala',
    vala_args:    vala_args,
    dependencies: [gtk4_dep, glib_dep, gio_dep],
    install:      true,
    install_dir:  get_option('datadir') / 'hammer',
)

# ─────────────────────────────────────────────────────────────
#  Data files
# ─────────────────────────────────────────────────────────────

desktop_conf = configuration_data()
desktop_conf.set('STORE_PATH',
    get_option('prefix') / get_option('datadir') / 'hammer' / 'store')

configure_file(
    input:         'data/org.hackerOS.HammerStore.desktop.in',
    output:        'org.hackerOS.HammerStore.desktop',
    configuration: desktop_conf,
    install:       true,
    install_dir:   get_option('datadir') / 'applications',
)

install_data(
    'data/org.hackerOS.HammerStore.metainfo.xml',
    install_dir: get_option('datadir') / 'metainfo',
)

install_data(
    'data/org.hackerOS.HammerStore.svg',
    install_dir: get_option('datadir') / 'icons' / 'hicolor' / 'scalable' / 'apps',
)

summary({
    'GTK version':    gtk_version,
    'AlertDialog':    have_alert_dialog,
    'Install prefix': get_option('prefix'),
    'Store binary':   get_option('prefix') / get_option('datadir') / 'hammer' / 'store',
}, section: 'Hammer Store')

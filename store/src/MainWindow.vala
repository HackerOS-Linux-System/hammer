using Gtk;
using Adw;

namespace HammerStore {

    // MainWindow — no [GtkTemplate]: UI is built entirely in code.
    // Using Adw.NavigationSplitView (replaces deprecated Adw.Leaflet ≥ 1.4)
    public class MainWindow : Adw.ApplicationWindow {

        // ── Widgets ─────────────────────────────────────────────
        private Gtk.SearchEntry   global_search;
        private Gtk.ToggleButton  dark_mode_btn;
        private Gtk.Stack         main_stack;

        // ── Pages (views) ───────────────────────────────────────
        private FeaturedView   featured_view;
        private CategoryView   category_view;
        private InstalledView  installed_view;
        private UpdatesView    updates_view;
        private SearchView     search_view;
        private PackageDetails detail_view;
        // 0.5: Historia transakcji
        private HistoryView    history_view;
        // 0.6: Statystyki i ustawienia
        private StatsView      stats_view;
        private SettingsView   settings_view;

        // ── State ───────────────────────────────────────────────
        private PackageStore    pkg_store;
        private Adw.ToastOverlay toast_overlay;

        public MainWindow (Gtk.Application app) {
            Object (application: app, title: "Hammer Store");
        }

        construct {
            pkg_store     = new PackageStore ();
            toast_overlay = new Adw.ToastOverlay ();

            set_default_size (1100, 720);
            build_ui ();
            setup_actions ();
            setup_css ();
            setup_keyboard_shortcuts ();
            setup_network_monitor ();

            pkg_store.refresh_async.begin (() => {
                featured_view.refresh (pkg_store);
                category_view.load (pkg_store);
                updates_view.refresh (pkg_store);
                installed_view.refresh (pkg_store);
                history_view.refresh (pkg_store);
                stats_view.refresh (pkg_store);
            });

            // Przeładuj historię gdy paczka się zmieni
            pkg_store.history_loaded.connect (() => {
                history_view.refresh (pkg_store);
            });
        }

        // ── UI construction ─────────────────────────────────────

        private void build_ui () {
            // ── Content header bar (right pane) ─────────────────
            var content_header = new Adw.HeaderBar ();

            global_search = new Gtk.SearchEntry ();
            global_search.placeholder_text = "Search packages…";
            global_search.hexpand = true;
            global_search.search_changed.connect (on_search_changed);
            global_search.activate.connect (on_search_activate);
            content_header.set_title_widget (global_search);

            dark_mode_btn = new Gtk.ToggleButton ();
            dark_mode_btn.icon_name    = "weather-clear-night-symbolic";
            dark_mode_btn.tooltip_text = "Toggle dark mode";
            dark_mode_btn.toggled.connect (on_dark_mode_toggled);
            content_header.pack_end (dark_mode_btn);

            var refresh_btn = new Gtk.Button.from_icon_name ("view-refresh-symbolic");
            refresh_btn.tooltip_text = "Refresh package lists";
            refresh_btn.clicked.connect (() => {
                pkg_store.refresh_async.begin (() => {
                    featured_view.refresh (pkg_store);
                    updates_view.refresh (pkg_store);
                    installed_view.refresh (pkg_store);
                    history_view.refresh (pkg_store);
                    stats_view.refresh (pkg_store);
                    show_toast ("Package lists refreshed");
                });
            });
            content_header.pack_end (refresh_btn);

            // ── Main content stack ───────────────────────────────
            main_stack = new Gtk.Stack ();
            main_stack.transition_type = Gtk.StackTransitionType.SLIDE_LEFT_RIGHT;
            main_stack.hexpand = true;
            main_stack.vexpand = true;

            featured_view  = new FeaturedView ();
            category_view  = new CategoryView ();
            installed_view = new InstalledView ();
            updates_view   = new UpdatesView ();
            search_view    = new SearchView ();
            detail_view    = new PackageDetails ();
            history_view   = new HistoryView ();  // 0.5
            stats_view     = new StatsView ();    // 0.6
            settings_view  = new SettingsView (); // 0.6

            featured_view.package_selected.connect  (show_package_detail);
            featured_view.install_requested.connect ((pkg) => {
                show_progress ("Installing %s…".printf (pkg.name));
                pkg_store.install_package_async.begin (pkg, () => {
                    hide_progress ();
                    featured_view.refresh (pkg_store);
                    installed_view.refresh (pkg_store);
                    stats_view.refresh (pkg_store);
                    show_toast ("%s installed.".printf (pkg.name));
                });
            });
            category_view.package_selected.connect  (show_package_detail);
            installed_view.package_selected.connect (show_package_detail);
            search_view.package_selected.connect    (show_package_detail);
            updates_view.package_selected.connect   (show_package_detail);

            // Po cofnięciu wrócij do Discover
            history_view.undo_requested.connect (() => {
                main_stack.visible_child_name = "featured";
                show_toast ("Operacja cofnięta. Zmiany wejdą po restarcie.");
            });

            main_stack.add_named (featured_view,  "featured");
            main_stack.add_named (category_view,  "categories");
            main_stack.add_named (installed_view, "installed");
            main_stack.add_named (updates_view,   "updates");
            main_stack.add_named (history_view,   "history");   // 0.5
            main_stack.add_named (stats_view,     "stats");     // 0.6
            main_stack.add_named (settings_view,  "settings");  // 0.6
            main_stack.add_named (search_view,    "search");
            main_stack.add_named (detail_view,    "detail");

            // Po zmianie źródeł odśwież paczki
            settings_view.sources_changed.connect (() => {
                pkg_store.refresh_async.begin (() => {
                    featured_view.refresh (pkg_store);
                    updates_view.refresh (pkg_store);
                    installed_view.refresh (pkg_store);
                    stats_view.refresh (pkg_store);
                    show_toast ("Package lists refreshed");
                });
            });

            toast_overlay.child = main_stack;

            // ── Content area (header + stack) ────────────────────
            var content_box = new Gtk.Box (Gtk.Orientation.VERTICAL, 0);
            content_box.append (content_header);
            content_box.append (toast_overlay);

            // ── Sidebar ──────────────────────────────────────────
            var sidebar_header = new Adw.HeaderBar ();
            sidebar_header.show_end_title_buttons = false;
            var title_lbl = new Gtk.Label ("<b>Hammer Store</b>");
            title_lbl.use_markup = true;
            sidebar_header.set_title_widget (title_lbl);

            var nav_list = build_nav_list ();

            var sidebar_box = new Gtk.Box (Gtk.Orientation.VERTICAL, 0);
            sidebar_box.width_request = 220;
            sidebar_box.append (sidebar_header);
            sidebar_box.append (nav_list);

            // ── Adw.NavigationSplitView ──────────────────────────
            var sidebar_page = new Adw.NavigationPage (sidebar_box, "sidebar");
            var content_page = new Adw.NavigationPage (content_box, "content");

            var split_view = new Adw.NavigationSplitView ();
            split_view.sidebar      = sidebar_page;
            split_view.content      = content_page;
            split_view.min_sidebar_width = 180;
            split_view.max_sidebar_width = 260;

            set_content (split_view);
        }

        private Gtk.Widget build_nav_list () {
            var list = new Gtk.ListBox ();
            list.css_classes = { "navigation-sidebar" };
            list.vexpand     = true;

            // 7 pozycji — 0.5: History, 0.6: Statistics + Settings
            string[,] items = {
                { "featured",   "Discover",    "starred-symbolic"                    },
                { "categories", "Categories",  "view-grid-symbolic"                  },
                { "installed",  "Installed",   "emblem-default-symbolic"             },
                { "updates",    "Updates",     "software-update-available-symbolic"  },
                { "history",    "History",     "document-open-recent-symbolic"       },
                { "stats",      "Statistics",  "utilities-system-monitor-symbolic"   },
                { "settings",   "Settings",    "preferences-system-symbolic"         },
            };

            for (int i = 0; i < 7; i++) {
                string key   = items[i, 0];
                string label = items[i, 1];
                string icon  = items[i, 2];

                var row  = new Adw.ActionRow ();
                row.title       = label;
                var row_icon = new Gtk.Image.from_icon_name (icon);
                row_icon.pixel_size = 16;
                row.add_prefix (row_icon);
                row.activatable = true;

                string capture_key = key;
                row.activated.connect (() => {
                    _last_view = capture_key;
                    main_stack.visible_child_name = capture_key;
                    switch (capture_key) {
                        case "history":
                            history_view.refresh (pkg_store);
                            break;
                        case "stats":
                            stats_view.refresh (pkg_store);
                            break;
                        default:
                            break;
                    }
                });
                list.append (row);
            }

            list.select_row (list.get_row_at_index (0));
            return list;
        }

        private void setup_actions () {
            var focus_action = new SimpleAction ("focus-search", null);
            focus_action.activate.connect (() => { global_search.grab_focus (); });
            add_action (focus_action);

            var trigger  = Gtk.ShortcutTrigger.parse_string ("<Control>f");
            var action   = Gtk.ShortcutAction.parse_string  ("action(win.focus-search)");

            var shortcut = new Gtk.Shortcut (trigger, action);
            var ctrl     = new Gtk.ShortcutController ();
            ctrl.scope   = Gtk.ShortcutScope.MANAGED;
            ctrl.add_shortcut (shortcut);
            add_controller (ctrl);
        }

        private void setup_css () {
            var css = new Gtk.CssProvider ();
            css.load_from_string (STORE_CSS);
            Gtk.StyleContext.add_provider_for_display (
                Gdk.Display.get_default (),
                css,
                Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
            );
        }

        // ── Signal handlers ─────────────────────────────────────

        private void on_search_changed () {
            var text = global_search.text.strip ();
            if (text.length == 0) {
                main_stack.visible_child_name = "featured";
            } else {
                main_stack.visible_child_name = "search";
                search_view.search (text, pkg_store);
            }
        }

        private void on_search_activate () {
            var text = global_search.text.strip ();
            if (text.length > 0) {
                main_stack.visible_child_name = "search";
                search_view.search (text, pkg_store);
            }
        }

        private void on_dark_mode_toggled () {
            var mgr = Adw.StyleManager.get_default ();
            mgr.color_scheme = dark_mode_btn.active
                ? Adw.ColorScheme.FORCE_DARK
                : Adw.ColorScheme.PREFER_LIGHT;
        }

        private void show_package_detail (PackageInfo pkg) {
            detail_view.load (pkg, pkg_store);
            detail_view.back_clicked.connect (() => {
                main_stack.visible_child_name = "featured";
            });
            main_stack.visible_child_name = "detail";
        }

        public void show_toast (string msg) {
            var toast = new Adw.Toast (msg);
            toast.timeout = 3;
            toast_overlay.add_toast (toast);
        }

        private void setup_keyboard_shortcuts () {
            // Ctrl+F → focus search
            var search_action = new GLib.SimpleAction ("focus-search", null);
            search_action.activate.connect (() => {
                global_search.grab_focus ();
                global_search.select_region (0, -1);
            });
            add_action (search_action);
            application.set_accels_for_action ("win.focus-search", { "<Control>f", "slash" });

            // Ctrl+R → refresh
            var refresh_action = new GLib.SimpleAction ("refresh", null);
            refresh_action.activate.connect (() => {
                pkg_store.refresh_async.begin (() => {
                    featured_view.refresh (pkg_store);
                    updates_view.refresh (pkg_store);
                    installed_view.refresh (pkg_store);
                    stats_view.refresh (pkg_store);
                    show_toast ("Refreshed");
                });
            });
            add_action (refresh_action);
            application.set_accels_for_action ("win.refresh", { "<Control>r" });

            // Alt+Left → back (from detail)
            var back_action = new GLib.SimpleAction ("go-back", null);
            back_action.activate.connect (() => {
                if (main_stack.visible_child_name == "detail") {
                    main_stack.visible_child_name = _last_view;
                }
            });
            add_action (back_action);
            application.set_accels_for_action ("win.go-back", { "alt+Left", "<Alt>Left" });

            // Ctrl+1..7 → navigate to views
            string[] view_names = {
                "featured", "categories", "installed",
                "updates", "history", "stats", "settings"
            };
            for (int i = 0; i < view_names.length; i++) {
                string view_name = view_names[i];
                string accel = "<Control>%d".printf (i + 1);
                var nav_action = new GLib.SimpleAction ("nav-%s".printf (view_name), null);
                nav_action.activate.connect (() => {
                    main_stack.visible_child_name = view_name;
                });
                add_action (nav_action);
                application.set_accels_for_action (
                    "win.nav-%s".printf (view_name), { accel });
            }

            // Escape → clear search / close detail
            var ctrl = new Gtk.EventControllerKey ();
            ctrl.key_pressed.connect ((keyval, _keycode, _state) => {
                if (keyval == Gdk.Key.Escape) {
                    if (main_stack.visible_child_name == "detail") {
                        main_stack.visible_child_name = _last_view;
                        return true;
                    }
                    if (global_search.text != "") {
                        global_search.text = "";
                        main_stack.visible_child_name = "featured";
                        return true;
                    }
                }
                return false;
            });
            add_controller (ctrl);
        }

        private string _last_view = "featured";

        private void setup_network_monitor () {
            var monitor = GLib.NetworkMonitor.get_default ();
            monitor.network_changed.connect ((available) => {
                if (!available) {
                    show_toast ("⚠ Offline — showing cached data");
                } else {
                    // Back online — silently refresh in background
                    pkg_store.refresh_async.begin (() => {
                        featured_view.refresh (pkg_store);
                        updates_view.refresh (pkg_store);
                    });
                }
            });
        }

        // ── Progress banner (shown during install/remove) ──────────

        private Adw.Banner? _progress_banner = null;

        public void show_progress (string msg) {
            if (_progress_banner == null) {
                _progress_banner = new Adw.Banner (msg);
                _progress_banner.revealed = true;
                // Insert before the content — below header
                // We add it to the toast_overlay's parent
                if (toast_overlay.parent is Gtk.Box) {
                    ((Gtk.Box) toast_overlay.parent).prepend (_progress_banner);
                }
            } else {
                _progress_banner.title   = msg;
                _progress_banner.revealed = true;
            }
        }

        public void hide_progress () {
            if (_progress_banner != null) {
                _progress_banner.revealed = false;
            }
        }

        // ── CSS ─────────────────────────────────────────────────

        private const string STORE_CSS = """
            /* ── Card ──────────────────────────────────────────── */
            .store-card {
                border-radius: 14px;
                padding: 14px;
                background: alpha(@card_bg_color, 0.8);
                border: 1px solid alpha(@borders, 0.25);
                transition: all 180ms ease;
                box-shadow: 0 1px 3px alpha(#000, 0.07);
            }
            .store-card:hover {
                background: alpha(@accent_bg_color, 0.09);
                border-color: @accent_color;
                box-shadow: 0 2px 8px alpha(@accent_color, 0.15);
            }

            /* ── Package icon ───────────────────────────────────── */
            .pkg-icon-frame {
                border-radius: 18px;
                border: none;
                box-shadow: 0 2px 8px alpha(#000, 0.15);
                padding: 0;
            }
            .pkg-icon-lg { min-width: 80px; min-height: 80px; }
            .pkg-icon-sm { min-width: 32px; min-height: 32px; }

            /* ── Screenshots ────────────────────────────────────── */
            .screenshot-frame {
                border-radius: 10px;
                border: 1px solid alpha(@borders, 0.3);
                overflow: hidden;
                box-shadow: 0 2px 6px alpha(#000, 0.12);
            }

            /* ── Badges ─────────────────────────────────────────── */
            .badge-installed {
                background: @success_color;
                color: @success_fg_color;
                border-radius: 999px;
                padding: 2px 10px;
                font-size: 0.75em;
                font-weight: bold;
            }
            .badge-update {
                background: @warning_color;
                color: @warning_fg_color;
                border-radius: 999px;
                padding: 2px 10px;
                font-size: 0.75em;
                font-weight: bold;
            }
            .badge-size {
                background: alpha(@accent_bg_color, 0.15);
                color: @accent_color;
                border-radius: 999px;
                padding: 2px 8px;
                font-size: 0.72em;
            }
            .badge-new {
                background: alpha(@purple_3, 0.85);
                color: white;
                border-radius: 999px;
                padding: 2px 8px;
                font-size: 0.70em;
                font-weight: bold;
            }

            /* ── Category chip ──────────────────────────────────── */
            .category-chip {
                border-radius: 6px;
                padding: 4px 10px;
                background: alpha(@accent_bg_color, 0.15);
                font-size: 0.8em;
            }

            /* ── Detail page header ─────────────────────────────── */
            .detail-header {
                background: alpha(@headerbar_bg_color, 0.55);
                border-bottom: 1px solid alpha(@borders, 0.25);
            }

            /* ── Stat card ──────────────────────────────────────── */
            .stat-card {
                border-radius: 14px;
                padding: 18px 14px;
                background: alpha(@card_bg_color, 0.8);
                border: 1px solid alpha(@borders, 0.2);
                box-shadow: 0 1px 4px alpha(#000, 0.06);
            }

            /* ── Stars ──────────────────────────────────────────── */
            .rating-star { color: @warning_color; }

            /* ── History rows ───────────────────────────────────── */
            .history-row-install { color: @success_color; }
            .history-row-remove  { color: @error_color; }
            .history-row-upgrade { color: @warning_color; }

            /* ── Featured hero banner ────────────────────────────── */
            .hero-card {
                border-radius: 18px;
                padding: 28px;
                min-height: 160px;
                background: linear-gradient(135deg,
                    alpha(@accent_bg_color, 0.25) 0%,
                    alpha(@card_bg_color, 0.6) 100%);
                border: 1px solid alpha(@accent_color, 0.2);
            }
        """;
    }
}

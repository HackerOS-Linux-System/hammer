using Gtk;
using Adw;

namespace HammerStore {

    [GtkTemplate (ui = "/org/hackerOS/HammerStore/ui/main_window.ui")]
    public class MainWindow : Adw.ApplicationWindow {

        // ── Sidebar navigation ──────────────────────────────────
        [GtkChild] private unowned Gtk.StackSidebar sidebar;
        [GtkChild] private unowned Gtk.Stack        main_stack;
        [GtkChild] private unowned Gtk.SearchEntry  global_search;
        [GtkChild] private unowned Gtk.ToggleButton dark_mode_btn;

        // ── Pages (views) ───────────────────────────────────────
        private FeaturedView   featured_view;
        private CategoryView   category_view;
        private InstalledView  installed_view;
        private UpdatesView    updates_view;
        private SearchView     search_view;
        private PackageDetails detail_view;

        // ── State ───────────────────────────────────────────────
        private PackageStore   pkg_store;
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

            pkg_store.refresh_async.begin (() => {
                featured_view.refresh (pkg_store);
                category_view.load (pkg_store);
                updates_view.refresh (pkg_store);
                installed_view.refresh (pkg_store);
            });
        }

        // ── UI construction ─────────────────────────────────────

        private void build_ui () {
            var header = new Adw.HeaderBar ();

            // Search entry in header
            global_search = new Gtk.SearchEntry ();
            global_search.placeholder_text = "Search packages…";
            global_search.hexpand = true;
            global_search.search_changed.connect (on_search_changed);
            global_search.activate.connect (on_search_activate);
            header.set_title_widget (global_search);

            // Dark mode toggle
            dark_mode_btn = new Gtk.ToggleButton ();
            dark_mode_btn.icon_name  = "weather-clear-night-symbolic";
            dark_mode_btn.tooltip_text = "Toggle dark mode";
            dark_mode_btn.toggled.connect (on_dark_mode_toggled);
            header.pack_end (dark_mode_btn);

            // Refresh button
            var refresh_btn = new Gtk.Button.from_icon_name ("view-refresh-symbolic");
            refresh_btn.tooltip_text = "Refresh package lists";
            refresh_btn.clicked.connect (() => {
                pkg_store.refresh_async.begin (() => {
                    featured_view.refresh (pkg_store);
                    updates_view.refresh (pkg_store);
                    installed_view.refresh (pkg_store);
                    show_toast ("Package lists refreshed");
                });
            });
            header.pack_end (refresh_btn);

            // Leaflet for responsive layout
            var leaflet = new Adw.Leaflet ();
            leaflet.can_swipe_back = true;

            // Sidebar
            var sidebar_box = new Gtk.Box (Gtk.Orientation.VERTICAL, 0);
            sidebar_box.width_request = 220;

            var sidebar_header = new Adw.HeaderBar ();
            sidebar_header.show_end_title_buttons = false;
            var title_lbl = new Gtk.Label ("<b>Hammer Store</b>");
            title_lbl.use_markup = true;
            sidebar_header.set_title_widget (title_lbl);
            sidebar_box.append (sidebar_header);

            // Nav list
            var nav_list = build_nav_list ();
            sidebar_box.append (nav_list);

            // Main content stack
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

            featured_view.package_selected.connect  (show_package_detail);
            category_view.package_selected.connect  (show_package_detail);
            installed_view.package_selected.connect (show_package_detail);
            search_view.package_selected.connect    (show_package_detail);

            main_stack.add_named (featured_view,  "featured");
            main_stack.add_named (category_view,  "categories");
            main_stack.add_named (installed_view, "installed");
            main_stack.add_named (updates_view,   "updates");
            main_stack.add_named (search_view,    "search");
            main_stack.add_named (detail_view,    "detail");

            // Wrap in toast overlay
            toast_overlay.child = main_stack;

            leaflet.append (sidebar_box);
            leaflet.append (toast_overlay);

            var root_box = new Gtk.Box (Gtk.Orientation.VERTICAL, 0);
            root_box.append (header);
            root_box.append (leaflet);

            set_content (root_box);
        }

        private Gtk.Widget build_nav_list () {
            var list = new Gtk.ListBox ();
            list.css_classes = { "navigation-sidebar" };
            list.vexpand     = true;

            string[,] items = {
                { "featured",   "Discover",    "starred-symbolic" },
                { "categories", "Categories",  "view-grid-symbolic" },
                { "installed",  "Installed",   "emblem-default-symbolic" },
                { "updates",    "Updates",     "software-update-available-symbolic" },
            };

            for (int i = 0; i < 4; i++) {
                string key   = items[i, 0];
                string label = items[i, 1];
                string icon  = items[i, 2];

                var row  = new Adw.ActionRow ();
                row.title       = label;
                row.icon_name   = icon;
                row.activatable = true;

                var capture_key = key;
                row.activated.connect (() => {
                    main_stack.visible_child_name = capture_key;
                });
                list.append (row);
            }

            // Select first item
            list.select_row (list.get_row_at_index (0));
            return list;
        }

        private void setup_actions () {
            // Keyboard shortcut: Ctrl+F → focus search
            var key_ctrl = new Gtk.EventControllerKey ();
            key_ctrl.key_pressed.connect ((keyval, _keycode, state) => {
                if ((state & Gdk.ModifierType.CONTROL_MASK) != 0 && keyval == Gdk.Key.f) {
                    global_search.grab_focus ();
                    return true;
                }
                return false;
            });
            add_controller (key_ctrl);
        }

        private void setup_css () {
            var css = new Gtk.CssProvider ();
            css.load_from_data (STORE_CSS.data);
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
            detail_view.back_clicked.connect (() => main_stack.navigate (Gtk.StackTransitionType.SLIDE_RIGHT));
            main_stack.visible_child_name = "detail";
        }

        public void show_toast (string msg) {
            var toast = new Adw.Toast (msg);
            toast.timeout = 3;
            toast_overlay.add_toast (toast);
        }

        // ── CSS string ──────────────────────────────────────────

        private const string STORE_CSS = """
            .store-card {
                border-radius: 12px;
                padding: 12px;
                background: alpha(@card_bg_color, 0.7);
                border: 1px solid alpha(@borders, 0.3);
                transition: all 200ms ease;
            }
            .store-card:hover {
                background: alpha(@accent_bg_color, 0.08);
                border-color: @accent_color;
            }
            .pkg-icon-lg {
                min-width: 64px;
                min-height: 64px;
            }
            .pkg-icon-sm {
                min-width: 32px;
                min-height: 32px;
            }
            .badge-installed {
                background: @success_color;
                color: @success_fg_color;
                border-radius: 999px;
                padding: 2px 8px;
                font-size: 0.75em;
                font-weight: bold;
            }
            .badge-update {
                background: @warning_color;
                color: @warning_fg_color;
                border-radius: 999px;
                padding: 2px 8px;
                font-size: 0.75em;
                font-weight: bold;
            }
            .category-chip {
                border-radius: 6px;
                padding: 4px 10px;
                background: alpha(@accent_bg_color, 0.15);
                font-size: 0.8em;
            }
            .detail-header {
                padding: 24px;
                background: alpha(@headerbar_bg_color, 0.5);
                border-bottom: 1px solid alpha(@borders, 0.3);
            }
            .rating-star { color: @warning_color; }
        """;
    }
}

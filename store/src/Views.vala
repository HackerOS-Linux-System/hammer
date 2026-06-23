using Gtk;
using Adw;

namespace HammerStore {

    // ────────────────────────────────────────────────────────────
    //  FeaturedView — Discover page
    // ────────────────────────────────────────────────────────────

    public class FeaturedView : Gtk.Box {

        public signal void package_selected (PackageInfo pkg);

        private Gtk.FlowBox grid;
        private Gtk.Label   empty_lbl;

        public FeaturedView () {
            Object (orientation: Gtk.Orientation.VERTICAL, spacing: 0);

            var scroll = new Gtk.ScrolledWindow ();
            scroll.hscrollbar_policy  = Gtk.PolicyType.NEVER;
            scroll.vscrollbar_policy  = Gtk.PolicyType.AUTOMATIC;
            scroll.vexpand = true;

            var inner = new Gtk.Box (Gtk.Orientation.VERTICAL, 16);
            inner.margin_start  = 24;
            inner.margin_end    = 24;
            inner.margin_top    = 20;
            inner.margin_bottom = 20;

            // Banner
            var banner = new Adw.Banner ("Hammer Store — discover packages for HackerOS");
            banner.button_label   = "Learn more";
            inner.append (banner);

            // Section header
            var section = new Gtk.Label ("<b>Featured Applications</b>");
            section.use_markup   = true;
            section.halign       = Gtk.Align.START;
            section.margin_top   = 8;
            inner.append (section);

            grid = new Gtk.FlowBox ();
            grid.max_children_per_line = 4;
            grid.min_children_per_line = 1;
            grid.column_spacing = 12;
            grid.row_spacing    = 12;
            grid.homogeneous    = false;
            grid.selection_mode = Gtk.SelectionMode.NONE;
            inner.append (grid);

            empty_lbl = new Gtk.Label ("No featured packages found.");
            empty_lbl.visible = false;
            inner.append (empty_lbl);

            scroll.child = inner;
            append (scroll);
        }

        public void refresh (PackageStore store) {
            // Remove old
            while (true) {
                var ch = grid.get_first_child ();
                if (ch == null) break;
                grid.remove (ch);
            }

            var pkgs = store.get_featured ();
            if (pkgs.length () == 0) { empty_lbl.visible = true; return; }
            empty_lbl.visible = false;

            pkgs.@foreach ((pkg) => {
                var card = new PackageCard (pkg, true);
                card.clicked.connect (() => package_selected (pkg));
                grid.append (card);
            });
        }
    }

    // ────────────────────────────────────────────────────────────
    //  CategoryView
    // ────────────────────────────────────────────────────────────

    public class CategoryView : Gtk.Box {

        public signal void package_selected (PackageInfo pkg);

        private Gtk.Box     chips_row;
        private Gtk.FlowBox grid;
        private PackageStore? _store;
        private string current_cat = "";

        public CategoryView () {
            Object (orientation: Gtk.Orientation.VERTICAL, spacing: 0);

            var scroll = new Gtk.ScrolledWindow ();
            scroll.hscrollbar_policy = Gtk.PolicyType.NEVER;
            scroll.vexpand = true;

            var inner = new Gtk.Box (Gtk.Orientation.VERTICAL, 12);
            inner.margin_start  = 24;
            inner.margin_end    = 24;
            inner.margin_top    = 20;
            inner.margin_bottom = 20;

            chips_row = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 8);
            chips_row.hexpand = true;
            inner.append (chips_row);

            grid = new Gtk.FlowBox ();
            grid.max_children_per_line = 4;
            grid.column_spacing = 12;
            grid.row_spacing    = 12;
            grid.selection_mode = Gtk.SelectionMode.NONE;
            inner.append (grid);

            scroll.child = inner;
            append (scroll);
        }

        public void load (PackageStore store) {
            _store = store;
            // Clear chips
            while (true) {
                var ch = chips_row.get_first_child ();
                if (ch == null) break;
                chips_row.remove (ch);
            }
            string[] cats = store.categories ();
            if (cats.length == 0) return;
            current_cat = cats[0];

            // All chip
            var all_btn = new Gtk.Button.with_label ("All");
            all_btn.css_classes = { "category-chip" };
            all_btn.clicked.connect (() => { current_cat = ""; show_category (""); });
            chips_row.append (all_btn);

            foreach (var cat in cats) {
                string cap = "%s%s".printf (cat.substring (0, 1).up (), cat.substring (1));
                var btn = new Gtk.Button.with_label (cap);
                btn.css_classes = { "category-chip" };
                string c = cat;
                btn.clicked.connect (() => { current_cat = c; show_category (c); });
                chips_row.append (btn);
            }
            show_category ("");
        }

        private const int PAGE_SIZE = 80;
        private int _current_offset = 0;

        private void show_category (string cat) {
            if (_store == null) return;
            _current_offset = 0;
            _current_cat    = cat;
            while (true) {
                var ch = grid.get_first_child ();
                if (ch == null) break;
                grid.remove (ch);
            }
            load_page (cat, 0);
        }

        private void load_page (string cat, int offset) {
            if (_store == null) return;
            List<PackageInfo> all_pkgs = cat.length == 0
                ? _store.search ("")
                : _store.get_by_category (cat);

            // Paginate: show PAGE_SIZE items at a time
            int shown = 0;
            int idx   = 0;
            all_pkgs.@foreach ((pkg) => {
                if (idx < offset) { idx++; return; }
                if (shown >= PAGE_SIZE) return;
                var card = new PackageCard (pkg, false);
                card.clicked.connect (() => package_selected (pkg));
                grid.append (card);
                shown++;
                idx++;
            });

            // Append "Load more" button if there are more results
            if (offset + shown < (int)all_pkgs.length ()) {
                var more_btn = new Gtk.Button.with_label (
                    "Load more (%u remaining)…".printf (
                        (uint)(all_pkgs.length () - offset - shown)));
                more_btn.margin_top    = 12;
                more_btn.margin_bottom = 12;
                int next_offset = offset + shown;
                more_btn.clicked.connect (() => {
                    grid.remove (more_btn);
                    load_page (cat, next_offset);
                });
                grid.append (more_btn);
            }
        }
    }

    // ────────────────────────────────────────────────────────────
    //  InstalledView
    // ────────────────────────────────────────────────────────────

    public class InstalledView : Gtk.Box {

        public signal void package_selected (PackageInfo pkg);

        private Gtk.ListBox list;

        public InstalledView () {
            Object (orientation: Gtk.Orientation.VERTICAL, spacing: 0);

            var header = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 8);
            header.margin_start  = 20;
            header.margin_end    = 20;
            header.margin_top    = 16;
            header.margin_bottom = 8;

            var lbl = new Gtk.Label ("<b>Installed Packages</b>");
            lbl.use_markup = true;
            lbl.hexpand    = true;
            lbl.halign     = Gtk.Align.START;
            header.append (lbl);
            append (header);

            var scroll = new Gtk.ScrolledWindow ();
            scroll.hscrollbar_policy = Gtk.PolicyType.NEVER;
            scroll.vexpand = true;

            list = new Gtk.ListBox ();
            list.css_classes = { "boxed-list" };
            list.margin_start  = 20;
            list.margin_end    = 20;
            list.margin_bottom = 20;
            list.row_activated.connect ((row) => {
                var pkg = row.get_data<PackageInfo> ("pkg");
                if (pkg != null) package_selected (pkg);
            });

            scroll.child = list;
            append (scroll);
        }

        public void refresh (PackageStore store) {
            while (true) {
                var ch = list.get_first_child ();
                if (ch == null) break;
                list.remove (ch);
            }
            var pkgs = store.get_installed ();
            if (pkgs.length () == 0) {
                var row = new Gtk.ListBoxRow ();
                row.selectable = false;
                var lbl = new Gtk.Label ("No packages installed yet.");
                lbl.margin_top = lbl.margin_bottom = 16;
                row.child = lbl;
                list.append (row);
                return;
            }
            pkgs.@foreach ((pkg) => {
                var row  = build_installed_row (pkg);
                row.set_data ("pkg", pkg);
                list.append (row);
            });
        }

        private Gtk.ListBoxRow build_installed_row (PackageInfo pkg) {
            var row = new Gtk.ListBoxRow ();
            var box = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 12);
            box.margin_start  = 12;
            box.margin_end    = 12;
            box.margin_top    = 10;
            box.margin_bottom = 10;

            var icon = new Gtk.Image.from_icon_name (pkg.icon_name);
            icon.pixel_size = 32;
            icon.css_classes = { "pkg-icon-sm" };
            box.append (icon);

            var labels = new Gtk.Box (Gtk.Orientation.VERTICAL, 2);
            labels.hexpand = true;
            var name_lbl = new Gtk.Label ("<b>%s</b>".printf (pkg.name));
            name_lbl.use_markup = true;
            name_lbl.halign     = Gtk.Align.START;
            labels.append (name_lbl);
            var sum_lbl = new Gtk.Label (pkg.summary);
            sum_lbl.halign = Gtk.Align.START;
            sum_lbl.add_css_class ("dim-label");
            labels.append (sum_lbl);
            box.append (labels);

            var ver_lbl = new Gtk.Label (pkg.version);
            ver_lbl.add_css_class ("dim-label");
            ver_lbl.halign = Gtk.Align.END;
            box.append (ver_lbl);

            if (pkg.status == PackageStatus.UPDATE_AVAILABLE) {
                var badge = new Gtk.Label ("Update");
                badge.css_classes = { "badge-update" };
                box.append (badge);
            }

            row.child = box;
            return row;
        }
    }

    // ────────────────────────────────────────────────────────────
    //  UpdatesView
    // ────────────────────────────────────────────────────────────

    public class UpdatesView : Gtk.Box {

        public signal void package_selected (PackageInfo pkg);

        private Gtk.ListBox list;
        private Gtk.Button  upgrade_all_btn;
        private PackageStore? _store;

        public UpdatesView () {
            Object (orientation: Gtk.Orientation.VERTICAL, spacing: 0);

            var header = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 8);
            header.margin_start  = 20;
            header.margin_end    = 20;
            header.margin_top    = 16;
            header.margin_bottom = 8;

            var lbl = new Gtk.Label ("<b>Available Updates</b>");
            lbl.use_markup = true;
            lbl.hexpand    = true;
            lbl.halign     = Gtk.Align.START;
            header.append (lbl);

            upgrade_all_btn = new Gtk.Button.with_label ("Upgrade All");
            upgrade_all_btn.css_classes = { "suggested-action" };
            upgrade_all_btn.clicked.connect (on_upgrade_all);
            header.append (upgrade_all_btn);
            append (header);

            var scroll = new Gtk.ScrolledWindow ();
            scroll.hscrollbar_policy = Gtk.PolicyType.NEVER;
            scroll.vexpand = true;

            list = new Gtk.ListBox ();
            list.css_classes = { "boxed-list" };
            list.margin_start  = 20;
            list.margin_end    = 20;
            list.margin_bottom = 20;
            list.row_activated.connect ((row) => {
                var pkg = row.get_data<PackageInfo> ("pkg");
                if (pkg != null) package_selected (pkg);
            });

            scroll.child = list;
            append (scroll);
        }

        public void refresh (PackageStore store) {
            _store = store;
            while (true) {
                var ch = list.get_first_child ();
                if (ch == null) break;
                list.remove (ch);
            }
            var pkgs = store.get_updates ();
            upgrade_all_btn.sensitive = pkgs.length () > 0;

            if (pkgs.length () == 0) {
                var row = new Gtk.ListBoxRow ();
                row.selectable = false;
                var lbl = new Gtk.Label ("System is up to date.");
                lbl.margin_top = lbl.margin_bottom = 24;
                row.child = lbl;
                list.append (row);
                return;
            }
            pkgs.@foreach ((pkg) => {
                var row = build_update_row (pkg);
                row.set_data ("pkg", pkg);
                list.append (row);
            });
        }

        private Gtk.ListBoxRow build_update_row (PackageInfo pkg) {
            var row = new Gtk.ListBoxRow ();
            var box = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 12);
            box.margin_start  = 12;
            box.margin_end    = 12;
            box.margin_top    = 10;
            box.margin_bottom = 10;

            var icon = new Gtk.Image.from_icon_name (pkg.icon_name);
            icon.pixel_size = 32;
            box.append (icon);

            var labels = new Gtk.Box (Gtk.Orientation.VERTICAL, 2);
            labels.hexpand = true;
            var name_lbl = new Gtk.Label ("<b>%s</b>".printf (pkg.name));
            name_lbl.use_markup = true;
            name_lbl.halign = Gtk.Align.START;
            labels.append (name_lbl);
            var ver_lbl = new Gtk.Label ("%s → %s".printf (pkg.installed_ver, pkg.version));
            ver_lbl.halign = Gtk.Align.START;
            ver_lbl.add_css_class ("dim-label");
            labels.append (ver_lbl);
            box.append (labels);

            var upd_btn = new Gtk.Button.with_label ("Update");
            upd_btn.css_classes = { "suggested-action" };
            upd_btn.valign = Gtk.Align.CENTER;
            upd_btn.clicked.connect (() => {
                if (_store != null) _store.install_package_async.begin (pkg, () => {});
            });
            box.append (upd_btn);

            row.child = box;
            return row;
        }

        private void on_upgrade_all () {
            if (_store == null) return;
            upgrade_all_btn.sensitive = false;
            upgrade_all_btn.label     = "Upgrading…";
            _store.upgrade_all_async.begin (() => {
                upgrade_all_btn.label     = "Upgrade All";
                upgrade_all_btn.sensitive = true;
                refresh (_store);
            });
        }
    }

    // ────────────────────────────────────────────────────────────
    //  SearchView
    // ────────────────────────────────────────────────────────────

    public class SearchView : Gtk.Box {

        public signal void package_selected (PackageInfo pkg);

        private Gtk.FlowBox grid;
        private Gtk.Label   results_lbl;
        private Gtk.Spinner spinner;

        public SearchView () {
            Object (orientation: Gtk.Orientation.VERTICAL, spacing: 0);

            var top = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 8);
            top.margin_start  = 20;
            top.margin_end    = 20;
            top.margin_top    = 16;
            top.margin_bottom = 8;

            results_lbl = new Gtk.Label ("");
            results_lbl.halign  = Gtk.Align.START;
            results_lbl.hexpand = true;
            top.append (results_lbl);

            spinner = new Gtk.Spinner ();
            top.append (spinner);
            append (top);

            var scroll = new Gtk.ScrolledWindow ();
            scroll.hscrollbar_policy = Gtk.PolicyType.NEVER;
            scroll.vexpand = true;

            grid = new Gtk.FlowBox ();
            grid.max_children_per_line = 4;
            grid.column_spacing = 12;
            grid.row_spacing    = 12;
            grid.selection_mode = Gtk.SelectionMode.NONE;
            grid.margin_start   = 20;
            grid.margin_end     = 20;
            grid.margin_bottom  = 20;

            scroll.child = grid;
            append (scroll);
        }

        public void search (string query, PackageStore store) {
            // Clear
            while (true) {
                var ch = grid.get_first_child ();
                if (ch == null) break;
                grid.remove (ch);
            }
            spinner.spinning = true;
            var pkgs = store.search (query);
            spinner.spinning = false;
            results_lbl.label = "<b>%u</b> result(s) for \"%s\"".printf (pkgs.length (), query);
            results_lbl.use_markup = true;

            pkgs.@foreach ((pkg) => {
                var card = new PackageCard (pkg, false);
                card.clicked.connect (() => package_selected (pkg));
                grid.append (card);
            });
        }
    }
}

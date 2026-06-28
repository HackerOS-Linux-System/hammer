using Gtk;
using Adw;

namespace HammerStore {

    // ────────────────────────────────────────────────────────────
    //  FeaturedView — Discover page
    // ────────────────────────────────────────────────────────────

    public class FeaturedView : Gtk.Box {

        public signal void package_selected (PackageInfo pkg);
        public signal void install_requested (PackageInfo pkg);

        private Gtk.Box     hero_area;
        private Gtk.FlowBox grid;
        private Gtk.Label   empty_lbl;
        private Gtk.Label   section_lbl;

        public FeaturedView () {
            Object (orientation: Gtk.Orientation.VERTICAL, spacing: 0);

            var scroll = new Gtk.ScrolledWindow ();
            scroll.hscrollbar_policy = Gtk.PolicyType.NEVER;
            scroll.vscrollbar_policy = Gtk.PolicyType.AUTOMATIC;
            scroll.vexpand = true;

            var inner = new Gtk.Box (Gtk.Orientation.VERTICAL, 0);
            inner.margin_bottom = 20;

            // ── Hero area (top featured picks) ────────────────────
            hero_area = new Gtk.Box (Gtk.Orientation.VERTICAL, 8);
            hero_area.margin_top    = 16;
            hero_area.margin_bottom = 8;
            inner.append (hero_area);

            // ── Section header ────────────────────────────────────
            var sep = new Gtk.Separator (Gtk.Orientation.HORIZONTAL);
            sep.margin_start  = 20;
            sep.margin_end    = 20;
            sep.margin_top    = 4;
            sep.margin_bottom = 12;
            inner.append (sep);

            section_lbl = new Gtk.Label ("<b>All Featured</b>");
            section_lbl.use_markup   = true;
            section_lbl.halign       = Gtk.Align.START;
            section_lbl.margin_start = 20;
            section_lbl.margin_bottom = 8;
            inner.append (section_lbl);

            // ── Card grid ─────────────────────────────────────────
            grid = new Gtk.FlowBox ();
            grid.max_children_per_line = 4;
            grid.min_children_per_line = 2;
            grid.column_spacing = 12;
            grid.row_spacing    = 12;
            grid.homogeneous    = true;
            grid.selection_mode = Gtk.SelectionMode.NONE;
            grid.margin_start   = 20;
            grid.margin_end     = 20;
            inner.append (grid);

            empty_lbl = new Gtk.Label ("No featured packages found.");
            empty_lbl.add_css_class ("dim-label");
            empty_lbl.margin_top = 48;
            empty_lbl.halign     = Gtk.Align.CENTER;
            empty_lbl.visible    = false;
            inner.append (empty_lbl);

            scroll.child = inner;
            append (scroll);
        }

        public void refresh (PackageStore store) {
            // Clear hero area
            while (true) {
                var ch = hero_area.get_first_child ();
                if (ch == null) break;
                hero_area.remove (ch);
            }
            // Clear grid
            while (true) {
                var ch = grid.get_first_child ();
                if (ch == null) break;
                grid.remove (ch);
            }

            var pkgs = store.get_featured ();
            if (pkgs.length () == 0) {
                empty_lbl.visible = true;
                return;
            }
            empty_lbl.visible = false;

            // First 2 packages → hero banners
            uint hero_count = 0;
            pkgs.@foreach ((pkg) => {
                if (hero_count < 2) {
                    var banner = new HeroBanner (pkg);
                    banner.details_clicked.connect ((p) => package_selected (p));
                    banner.install_clicked.connect ((p) => install_requested (p));
                    hero_area.append (banner);
                    hero_count++;
                } else {
                    var card = new PackageCard (pkg, true);
                    card.clicked.connect (() => package_selected (pkg));
                    grid.append (card);
                }
            });

            section_lbl.visible = pkgs.length () > 2;
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
            current_cat     = cat;
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
        private Gtk.Label   count_lbl;
        private Gtk.DropDown sort_dd;
        private string _sort = "Name A–Z";

        public InstalledView () {
            Object (orientation: Gtk.Orientation.VERTICAL, spacing: 0);

            // ── Header with sort ──────────────────────────────────
            var header = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 8);
            header.margin_start  = 20;
            header.margin_end    = 20;
            header.margin_top    = 16;
            header.margin_bottom = 4;

            var title_box = new Gtk.Box (Gtk.Orientation.VERTICAL, 2);
            title_box.hexpand = true;
            var lbl = new Gtk.Label ("<b>Installed Packages</b>");
            lbl.use_markup = true; lbl.halign = Gtk.Align.START;
            title_box.append (lbl);
            count_lbl = new Gtk.Label ("");
            count_lbl.halign = Gtk.Align.START;
            count_lbl.add_css_class ("dim-label");
            title_box.append (count_lbl);
            header.append (title_box);

            string[] sorts = { "Name A–Z", "Name Z–A", "Size ↓", "Size ↑",
                               "Category", "Newest installed" };
            sort_dd = new Gtk.DropDown.from_strings (sorts);
            sort_dd.notify["selected"].connect (() => {
                uint idx = sort_dd.selected;
                _sort = idx < sorts.length ? sorts[idx] : "Name A–Z";
                if (_store != null) rerender ();
            });
            header.append (sort_dd);
            append (header);

            var scroll = new Gtk.ScrolledWindow ();
            scroll.hscrollbar_policy = Gtk.PolicyType.NEVER;
            scroll.vexpand = true;

            list = new Gtk.ListBox ();
            list.css_classes   = { "boxed-list" };
            list.margin_start  = 20;
            list.margin_end    = 20;
            list.margin_top    = 8;
            list.margin_bottom = 20;
            list.row_activated.connect ((row) => {
                var pkg = row.get_data<PackageInfo> ("pkg");
                if (pkg != null) package_selected (pkg);
            });
            scroll.child = list;
            append (scroll);
        }

        private PackageStore? _store;

        public void refresh (PackageStore store) {
            _store = store;
            rerender ();
        }

        private void rerender () {
            while (true) {
                var ch = list.get_first_child ();
                if (ch == null) break;
                list.remove (ch);
            }
            if (_store == null) return;

            var pkgs = _store.get_installed ();
            uint n = pkgs.length ();
            count_lbl.label = "%u package%s".printf (n, n == 1 ? "" : "s");

            if (n == 0) {
                var row = new Gtk.ListBoxRow ();
                row.selectable = false;
                var empty_box = new Gtk.Box (Gtk.Orientation.VERTICAL, 8);
                empty_box.margin_top = empty_box.margin_bottom = 32;
                empty_box.halign = Gtk.Align.CENTER;
                var icon = new Gtk.Image.from_icon_name ("package-x-generic-symbolic");
                icon.pixel_size = 48; icon.add_css_class ("dim-label");
                empty_box.append (icon);
                var el = new Gtk.Label ("No packages installed yet.");
                el.add_css_class ("dim-label");
                empty_box.append (el);
                row.child = empty_box;
                list.append (row);
                return;
            }

            // Sort
            pkgs.sort ((a, b) => {
                switch (_sort) {
                    case "Name Z–A":
                        return b.name.collate (a.name);
                    case "Size ↓":
                        return (int)(b.installed_size - a.installed_size);
                    case "Size ↑":
                        return (int)(a.installed_size - b.installed_size);
                    case "Category":
                        int cv = a.category.collate (b.category);
                        return cv != 0 ? cv : a.name.collate (b.name);
                    case "Newest installed":
                        // Reverse name as proxy (no timestamp in PackageInfo)
                        return b.name.collate (a.name);
                    default: // Name A–Z
                        return a.name.collate (b.name);
                }
            });

            // Group by category when sorting by Category
            string last_cat = "";
            pkgs.@foreach ((pkg) => {
                if (_sort == "Category" && pkg.category != last_cat) {
                    last_cat = pkg.category;
                    string cap = last_cat != "" ?
                        "%s%s".printf (last_cat.substring (0,1).up (), last_cat.substring (1)) :
                        "Other";
                    var hdr_row = new Gtk.ListBoxRow ();
                    hdr_row.selectable = false;
                    var hdr_lbl = new Gtk.Label ("<b>%s</b>".printf (
                        GLib.Markup.escape_text (cap)));
                    hdr_lbl.use_markup   = true;
                    hdr_lbl.halign       = Gtk.Align.START;
                    hdr_lbl.margin_start = 12;
                    hdr_lbl.margin_top   = 12;
                    hdr_lbl.margin_bottom = 4;
                    hdr_row.child = hdr_lbl;
                    list.append (hdr_row);
                }
                var row = build_installed_row (pkg);
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

            var icon = new Gtk.Image.from_icon_name (
                pkg.icon_name != "" ? pkg.icon_name : "package-x-generic-symbolic");

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
        private Gtk.Label   count_lbl;
        private PackageStore? _store;

        public UpdatesView () {
            Object (orientation: Gtk.Orientation.VERTICAL, spacing: 0);

            // ── Header ────────────────────────────────────────────
            var header = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 8);
            header.margin_start  = 20;
            header.margin_end    = 20;
            header.margin_top    = 16;
            header.margin_bottom = 8;

            var title_box = new Gtk.Box (Gtk.Orientation.VERTICAL, 2);
            title_box.hexpand = true;

            var lbl = new Gtk.Label ("<b>Available Updates</b>");
            lbl.use_markup = true;
            lbl.halign     = Gtk.Align.START;
            title_box.append (lbl);

            count_lbl = new Gtk.Label ("");
            count_lbl.halign = Gtk.Align.START;
            count_lbl.add_css_class ("dim-label");
            title_box.append (count_lbl);
            header.append (title_box);

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
            uint n = pkgs.length ();
            count_lbl.label = n == 0 ? "System is up to date" :
                              "%u package%s available".printf (n, n == 1 ? "" : "s");

            if (n == 0) {
                var row = new Gtk.ListBoxRow ();
                row.selectable = false;
                var inner = new Gtk.Box (Gtk.Orientation.VERTICAL, 8);
                inner.margin_top = inner.margin_bottom = 32;
                inner.halign = Gtk.Align.CENTER;
                var icon = new Gtk.Image.from_icon_name ("emblem-default-symbolic");
                icon.pixel_size = 64;
                icon.add_css_class ("dim-label");
                inner.append (icon);
                var l = new Gtk.Label ("System is up to date");
                l.add_css_class ("title-2");
                inner.append (l);
                row.child = inner;
                list.append (row);
                return;
            }
            pkgs.@foreach ((pkg) => {
                var row = build_update_row (pkg);
                row.set_data ("pkg", pkg);
                list.append (row);
            });
        }

        private Adw.ExpanderRow build_update_row (PackageInfo pkg) {
            var row = new Adw.ExpanderRow ();
            row.title    = pkg.name;
            row.subtitle = "%s → %s".printf (
                pkg.installed_ver != "" ? pkg.installed_ver : "—",
                pkg.version);

            // Package icon (load from icon_url if available)
            if (pkg.icon_url != "" || pkg.icon_name != "package-x-generic") {
                var icon = new Gtk.Image.from_icon_name (
                    pkg.icon_name != "" ? pkg.icon_name : "package-x-generic-symbolic");
                icon.pixel_size = 32;
                row.add_prefix (icon);
            }

            // Changelog sub-row
            var cl_content = pkg.changelog != "" ? pkg.changelog :
                "No changelog available.\nRun  hammer changelog %s  for details.".printf (pkg.name);

            var cl_row = new Adw.ActionRow ();
            cl_row.title = "What's new";

            var cl_box = new Gtk.Box (Gtk.Orientation.VERTICAL, 4);
            cl_box.margin_start  = 12;
            cl_box.margin_end    = 12;
            cl_box.margin_top    = 8;
            cl_box.margin_bottom = 8;

            var cl_lbl = new Gtk.Label (cl_content);
            cl_lbl.halign      = Gtk.Align.START;
            cl_lbl.wrap        = true;
            cl_lbl.selectable  = true;
            cl_lbl.add_css_class ("monospace");
            cl_box.append (cl_lbl);
            cl_row.child = cl_box;

            // Update button row
            var btn_row = new Adw.ActionRow ();
            btn_row.title = "Size: %s".printf (pkg.formatted_download_size ());

            var upd_btn = new Gtk.Button.with_label ("Update");
            upd_btn.css_classes = { "suggested-action" };
            upd_btn.valign = Gtk.Align.CENTER;
            upd_btn.clicked.connect (() => {
                if (_store != null) {
                    upd_btn.label     = "Updating…";
                    upd_btn.sensitive = false;
                    _store.install_package_async.begin (pkg, () => {
                        upd_btn.label     = "Update";
                        upd_btn.sensitive = true;
                    });
                }
            });
            btn_row.add_suffix (upd_btn);

            row.add_row (cl_row);
            row.add_row (btn_row);

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

        private Gtk.FlowBox       grid;
        private Gtk.Label         results_lbl;
        private Gtk.Spinner       spinner;
        private Gtk.DropDown      category_filter;
        private Gtk.DropDown      status_filter;
        private Gtk.DropDown      sort_filter;
        private PackageStore?     _store;
        private string            _last_query = "";

        // Filter state
        private string _category = "All";
        private string _status   = "All";
        private string _sort     = "Relevance";

        public SearchView () {
            Object (orientation: Gtk.Orientation.VERTICAL, spacing: 0);

            // ── Results header ────────────────────────────────────
            var top = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 8);
            top.margin_start  = 20;
            top.margin_end    = 20;
            top.margin_top    = 14;
            top.margin_bottom = 4;

            results_lbl = new Gtk.Label ("");
            results_lbl.halign  = Gtk.Align.START;
            results_lbl.hexpand = true;
            top.append (results_lbl);

            spinner = new Gtk.Spinner ();
            top.append (spinner);
            append (top);

            // ── Filter bar ────────────────────────────────────────
            var filter_bar = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 8);
            filter_bar.margin_start  = 20;
            filter_bar.margin_end    = 20;
            filter_bar.margin_top    = 4;
            filter_bar.margin_bottom = 8;

            var filter_lbl = new Gtk.Label ("Filter:");
            filter_lbl.add_css_class ("dim-label");
            filter_bar.append (filter_lbl);

            // Category filter
            string[] categories = {
                "All", "Applications", "Libraries", "Development",
                "System", "Utilities", "Games", "Graphics", "Network",
                "Science", "Other"
            };
            category_filter = new Gtk.DropDown.from_strings (categories);
            category_filter.notify["selected"].connect (() => {
                uint idx = category_filter.selected;
                _category = idx < categories.length ? categories[idx] : "All";
                refilter ();
            });
            filter_bar.append (category_filter);

            // Status filter
            string[] statuses = { "All", "Installed", "Available", "Updates" };
            status_filter = new Gtk.DropDown.from_strings (statuses);
            status_filter.notify["selected"].connect (() => {
                uint idx = status_filter.selected;
                _status = idx < statuses.length ? statuses[idx] : "All";
                refilter ();
            });
            filter_bar.append (status_filter);

            // Sort
            var sort_lbl = new Gtk.Label ("Sort:");
            sort_lbl.add_css_class ("dim-label");
            sort_lbl.margin_start = 8;
            filter_bar.append (sort_lbl);

            string[] sorts = { "Relevance", "Name A–Z", "Name Z–A", "Size ↑", "Size ↓" };
            sort_filter = new Gtk.DropDown.from_strings (sorts);
            sort_filter.notify["selected"].connect (() => {
                uint idx = sort_filter.selected;
                _sort = idx < sorts.length ? sorts[idx] : "Relevance";
                refilter ();
            });
            filter_bar.append (sort_filter);

            append (filter_bar);

            // ── Separator ─────────────────────────────────────────
            var sep = new Gtk.Separator (Gtk.Orientation.HORIZONTAL);
            sep.margin_start = sep.margin_end = 20;
            append (sep);

            // ── Results grid ──────────────────────────────────────
            var scroll = new Gtk.ScrolledWindow ();
            scroll.hscrollbar_policy = Gtk.PolicyType.NEVER;
            scroll.vexpand = true;

            grid = new Gtk.FlowBox ();
            grid.max_children_per_line = 4;
            grid.min_children_per_line = 1;
            grid.column_spacing = 12;
            grid.row_spacing    = 12;
            grid.selection_mode = Gtk.SelectionMode.NONE;
            grid.margin_start   = 20;
            grid.margin_end     = 20;
            grid.margin_top     = 12;
            grid.margin_bottom  = 20;

            scroll.child = grid;
            append (scroll);
        }

        public void search (string query, PackageStore store) {
            _store      = store;
            _last_query = query;

            // Reset filters to All on new query
            category_filter.selected = 0;
            status_filter.selected   = 0;
            sort_filter.selected     = 0;
            _category = "All"; _status = "All"; _sort = "Relevance";

            refilter ();
        }

        private void refilter () {
            if (_store == null) return;
            // Clear grid
            while (true) {
                var ch = grid.get_first_child ();
                if (ch == null) break;
                grid.remove (ch);
            }
            spinner.spinning = true;

            var pkgs = _store.search (_last_query);

            // Apply category filter
            if (_category != "All") {
                var filtered = new List<PackageInfo> ();
                pkgs.@foreach ((p) => {
                    if (p.category.down () == _category.down ()) filtered.append (p);
                });
                pkgs = (owned) filtered;
            }

            // Apply status filter
            if (_status == "Installed") {
                var filtered = new List<PackageInfo> ();
                pkgs.@foreach ((p) => { if (p.is_installed ()) filtered.append (p); });
                pkgs = (owned) filtered;
            } else if (_status == "Available") {
                var filtered = new List<PackageInfo> ();
                pkgs.@foreach ((p) => { if (!p.is_installed ()) filtered.append (p); });
                pkgs = (owned) filtered;
            } else if (_status == "Updates") {
                var filtered = new List<PackageInfo> ();
                pkgs.@foreach ((p) => {
                    if (p.status == PackageStatus.UPDATE_AVAILABLE) filtered.append (p);
                });
                pkgs = (owned) filtered;
            }

            // Sort
            pkgs.sort ((a, b) => {
                switch (_sort) {
                    case "Name A–Z": return a.name.collate (b.name);
                    case "Name Z–A": return b.name.collate (a.name);
                    case "Size ↑":   return (int)(a.installed_size - b.installed_size);
                    case "Size ↓":   return (int)(b.installed_size - a.installed_size);
                    default:         return 0; // relevance = original order
                }
            });

            spinner.spinning = false;
            uint count = pkgs.length ();
            results_lbl.label      = _last_query == "" ?
                "<b>%u</b> package(s)".printf (count) :
                "<b>%u</b> result(s) for \"%s\"".printf (count, GLib.Markup.escape_text (_last_query));
            results_lbl.use_markup = true;

            if (count == 0) {
                var empty = new Gtk.Label ("No packages match your filters.");
                empty.add_css_class ("dim-label");
                empty.margin_top = 48;
                grid.append (empty);
                return;
            }

            pkgs.@foreach ((pkg) => {
                var card = new PackageCard (pkg, false);
                card.clicked.connect (() => package_selected (pkg));
                grid.append (card);
            });
        }
    }

    // ────────────────────────────────────────────────────────────
    //  HistoryView — Historia transakcji (nowość 0.5)
    // ────────────────────────────────────────────────────────────

    public class HistoryView : Gtk.Box {

        public signal void undo_requested ();

        private Gtk.ListBox   list;
        private Gtk.Button    undo_btn;
        private Gtk.Spinner   spinner;
        private PackageStore? _store;
        private Gtk.Label     status_lbl;

        public HistoryView () {
            Object (orientation: Gtk.Orientation.VERTICAL, spacing: 0);

            // ── Header ──────────────────────────────────────────
            var header = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 8);
            header.margin_start  = 20;
            header.margin_end    = 20;
            header.margin_top    = 16;
            header.margin_bottom = 8;

            var title_lbl = new Gtk.Label ("<b>Transaction History</b>");
            title_lbl.use_markup = true;
            title_lbl.hexpand    = true;
            title_lbl.halign     = Gtk.Align.START;
            header.append (title_lbl);

            spinner = new Gtk.Spinner ();
            header.append (spinner);

            undo_btn = new Gtk.Button.with_label ("Undo Last");
            undo_btn.css_classes = { "destructive-action" };
            undo_btn.tooltip_text = "Cofnij ostatnią operację (hammer undo)";
            undo_btn.clicked.connect (on_undo_clicked);
            header.append (undo_btn);
            append (header);

            // ── Status bar ──────────────────────────────────────
            status_lbl = new Gtk.Label ("");
            status_lbl.margin_start  = 20;
            status_lbl.margin_bottom = 4;
            status_lbl.halign        = Gtk.Align.START;
            status_lbl.visible       = false;
            append (status_lbl);

            // ── List ─────────────────────────────────────────────
            var scroll = new Gtk.ScrolledWindow ();
            scroll.hscrollbar_policy = Gtk.PolicyType.NEVER;
            scroll.vexpand = true;

            list = new Gtk.ListBox ();
            list.css_classes   = { "boxed-list" };
            list.margin_start  = 20;
            list.margin_end    = 20;
            list.margin_bottom = 20;
            list.selection_mode = Gtk.SelectionMode.NONE;

            scroll.child = list;
            append (scroll);
        }

        public void refresh (PackageStore store) {
            _store = store;
            spinner.spinning = true;
            while (true) {
                var ch = list.get_first_child ();
                if (ch == null) break;
                list.remove (ch);
            }

            var entries = store.get_history ();
            spinner.spinning = false;
            undo_btn.sensitive = entries.length () > 0;

            if (entries.length () == 0) {
                var row = new Gtk.ListBoxRow ();
                row.selectable = false;
                var lbl = new Gtk.Label ("No transaction history yet.");
                lbl.margin_top = lbl.margin_bottom = 24;
                row.child = lbl;
                list.append (row);
                return;
            }

            entries.@foreach ((entry) => {
                list.append (build_history_row (entry));
            });
        }

        private Gtk.ListBoxRow build_history_row (HistoryEntry entry) {
            var row = new Gtk.ListBoxRow ();
            row.selectable = false;

            var box = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 12);
            box.margin_start  = 12;
            box.margin_end    = 12;
            box.margin_top    = 10;
            box.margin_bottom = 10;

            // Ikona akcji
            var icon = new Gtk.Image.from_icon_name (entry.action_icon ());
            icon.pixel_size = 20;
            box.append (icon);

            // Paczka + akcja
            var info_box = new Gtk.Box (Gtk.Orientation.VERTICAL, 2);
            info_box.hexpand = true;

            var pkg_lbl = new Gtk.Label ("<b>%s</b>".printf (entry.package));
            pkg_lbl.use_markup = true;
            pkg_lbl.halign     = Gtk.Align.START;
            info_box.append (pkg_lbl);

            var detail = "%s  %s".printf (entry.action_label (), entry.version_display ());
            var det_lbl = new Gtk.Label (detail);
            det_lbl.halign = Gtk.Align.START;
            det_lbl.add_css_class ("dim-label");
            info_box.append (det_lbl);
            box.append (info_box);

            // Generacja
            var gen_lbl = new Gtk.Label ("gen-%d".printf (entry.generation));
            gen_lbl.add_css_class ("dim-label");
            gen_lbl.halign = Gtk.Align.END;
            box.append (gen_lbl);

            // Timestamp
            var ts_lbl = new Gtk.Label (entry.timestamp);
            ts_lbl.add_css_class ("dim-label");
            ts_lbl.halign = Gtk.Align.END;
            box.append (ts_lbl);

            row.child = box;
            return row;
        }

        private void on_undo_clicked () {
            if (_store == null) return;

            undo_btn.sensitive = false;
            undo_btn.label     = "Undoing…";
            status_lbl.label   = "Cofanie ostatniej operacji…";
            status_lbl.visible = true;

            _store.undo_last_async.begin ((obj, res) => {
                bool ok = _store.undo_last_async.end (res);
                undo_btn.label     = "Undo Last";
                undo_btn.sensitive = true;
                if (ok) {
                    status_lbl.label   = "✔ Cofnięto pomyślnie. Zmiany wejdą po restarcie.";
                    refresh (_store);
                    undo_requested ();
                } else {
                    status_lbl.label = "✗ Nie udało się cofnąć. Sprawdź uprawnienia root.";
                }
                // Ukryj status po 5 sekundach
                GLib.Timeout.add (5000, () => {
                    status_lbl.visible = false;
                    return GLib.Source.REMOVE;
                });
            });
        }
    }

    // ────────────────────────────────────────────────────────────
    //  PackageDetails — szczegóły paczki (rozbudowa 0.5)
    // ────────────────────────────────────────────────────────────

    public class PackageDetails : Gtk.Box {

        public signal void back_clicked ();

        // ── Header widgets ────────────────────────────────────────
        private Gtk.Image  pkg_icon;
        private Gtk.Label  name_lbl;
        private Gtk.Label  summary_lbl;
        private Gtk.Label  size_lbl;
        private Gtk.Label  dl_size_lbl;
        private Gtk.Label  status_badge;
        private Gtk.Button install_btn;
        private Gtk.Button remove_btn;

        // ── Tab content ──────────────────────────────────────────
        private Gtk.Label  desc_lbl;
        private Gtk.Label  maintainer_lbl;
        private Gtk.Label  homepage_lbl;
        private Gtk.Label  license_lbl;
        private Gtk.Label  deps_lbl;
        private Gtk.Label  conflicts_lbl;
        private Gtk.Box    screenshots_box;
        private Gtk.Label  changelog_lbl;
        private Gtk.ListBox files_list;

        private PackageInfo?  _pkg;
        private PackageStore? _store;

        public PackageDetails () {
            Object (orientation: Gtk.Orientation.VERTICAL, spacing: 0);

            // ── Header (icon + title + actions) ───────────────────
            var header = new Gtk.Box (Gtk.Orientation.VERTICAL, 0);
            header.css_classes = { "detail-header" };

            var nav_bar = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 0);
            var back_btn = new Gtk.Button.from_icon_name ("go-previous-symbolic");
            back_btn.tooltip_text = "Back";
            back_btn.css_classes  = { "flat" };
            back_btn.margin_start = 8;
            back_btn.margin_top   = 8;
            back_btn.margin_bottom = 4;
            back_btn.clicked.connect (() => back_clicked ());
            nav_bar.append (back_btn);
            header.append (nav_bar);

            var hero = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 20);
            hero.margin_start  = 24;
            hero.margin_end    = 24;
            hero.margin_top    = 8;
            hero.margin_bottom = 20;

            // App icon with rounded frame
            var icon_frame = new Gtk.Frame (null);
            icon_frame.css_classes = { "pkg-icon-frame" };
            pkg_icon = new Gtk.Image.from_icon_name ("package-x-generic");
            pkg_icon.pixel_size = 80;
            icon_frame.child = pkg_icon;
            hero.append (icon_frame);

            var meta = new Gtk.Box (Gtk.Orientation.VERTICAL, 4);
            meta.hexpand = true;

            name_lbl = new Gtk.Label ("");
            name_lbl.use_markup = true;
            name_lbl.halign     = Gtk.Align.START;
            name_lbl.wrap       = false;
            meta.append (name_lbl);

            summary_lbl = new Gtk.Label ("");
            summary_lbl.halign = Gtk.Align.START;
            summary_lbl.wrap   = true;
            summary_lbl.add_css_class ("dim-label");
            meta.append (summary_lbl);

            status_badge = new Gtk.Label ("");
            status_badge.halign     = Gtk.Align.START;
            status_badge.margin_top = 4;
            meta.append (status_badge);

            var sizes_row = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 20);
            sizes_row.margin_top = 6;

            size_lbl = new Gtk.Label ("");
            size_lbl.halign = Gtk.Align.START;
            size_lbl.add_css_class ("dim-label");
            sizes_row.append (size_lbl);

            dl_size_lbl = new Gtk.Label ("");
            dl_size_lbl.halign = Gtk.Align.START;
            dl_size_lbl.add_css_class ("dim-label");
            sizes_row.append (dl_size_lbl);
            meta.append (sizes_row);
            hero.append (meta);

            // Actions column
            var actions = new Gtk.Box (Gtk.Orientation.VERTICAL, 8);
            actions.valign = Gtk.Align.CENTER;

            install_btn = new Gtk.Button.with_label ("Install");
            install_btn.css_classes = { "suggested-action" };
            install_btn.width_request = 110;
            install_btn.clicked.connect (on_install);
            actions.append (install_btn);

            remove_btn = new Gtk.Button.with_label ("Remove");
            remove_btn.css_classes = { "destructive-action" };
            remove_btn.width_request = 110;
            remove_btn.clicked.connect (on_remove);
            actions.append (remove_btn);
            hero.append (actions);
            header.append (hero);
            append (header);

            // ── Tabs (Adw.ViewSwitcher) ───────────────────────────
            var stack = new Adw.ViewStack ();

            // Tab 1: Description + Screenshots + Meta
            var desc_scroll = new Gtk.ScrolledWindow ();
            desc_scroll.hscrollbar_policy = Gtk.PolicyType.NEVER;

            var desc_inner = new Gtk.Box (Gtk.Orientation.VERTICAL, 0);
            desc_inner.margin_start  = 24;
            desc_inner.margin_end    = 24;
            desc_inner.margin_top    = 16;
            desc_inner.margin_bottom = 24;

            // Screenshots carousel
            screenshots_box = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 12);
            screenshots_box.margin_bottom = 16;
            screenshots_box.visible = false;
            desc_inner.append (screenshots_box);

            // Description
            var desc_header = new Gtk.Label ("<b>Description</b>");
            desc_header.use_markup   = true;
            desc_header.halign       = Gtk.Align.START;
            desc_header.margin_bottom = 6;
            desc_inner.append (desc_header);

            desc_lbl = new Gtk.Label ("");
            desc_lbl.wrap      = true;
            desc_lbl.halign    = Gtk.Align.START;
            desc_lbl.xalign    = 0;
            desc_lbl.selectable = true;
            desc_inner.append (desc_lbl);

            // Metadata list
            var meta_sep = new Gtk.Separator (Gtk.Orientation.HORIZONTAL);
            meta_sep.margin_top    = 16;
            meta_sep.margin_bottom = 12;
            desc_inner.append (meta_sep);

            var meta_list = new Gtk.ListBox ();
            meta_list.css_classes   = { "boxed-list" };
            meta_list.selection_mode = Gtk.SelectionMode.NONE;

            maintainer_lbl = new Gtk.Label ("");
            license_lbl    = new Gtk.Label ("");
            homepage_lbl   = new Gtk.Label ("");
            homepage_lbl.selectable = true;

            meta_list.append (make_meta_row ("Maintainer", maintainer_lbl));
            meta_list.append (make_meta_row ("License",    license_lbl));
            meta_list.append (make_meta_row ("Homepage",   homepage_lbl));
            desc_inner.append (meta_list);

            desc_scroll.child = desc_inner;
            stack.add_titled_with_icon (desc_scroll, "description",
                "Description", "document-edit-symbolic");

            // Tab 2: Dependencies & Conflicts
            var deps_scroll = new Gtk.ScrolledWindow ();
            deps_scroll.hscrollbar_policy = Gtk.PolicyType.NEVER;

            var deps_inner = new Gtk.Box (Gtk.Orientation.VERTICAL, 12);
            deps_inner.margin_start  = 24;
            deps_inner.margin_end    = 24;
            deps_inner.margin_top    = 16;
            deps_inner.margin_bottom = 24;

            var dep_hdr = new Gtk.Label ("<b>Dependencies</b>");
            dep_hdr.use_markup = true; dep_hdr.halign = Gtk.Align.START;
            deps_inner.append (dep_hdr);
            deps_lbl = new Gtk.Label ("");
            deps_lbl.wrap    = true;
            deps_lbl.halign  = Gtk.Align.START;
            deps_lbl.selectable = true;
            deps_inner.append (deps_lbl);

            var conf_hdr = new Gtk.Label ("<b>Conflicts</b>");
            conf_hdr.use_markup = true;
            conf_hdr.halign     = Gtk.Align.START;
            conf_hdr.margin_top = 8;
            deps_inner.append (conf_hdr);
            conflicts_lbl = new Gtk.Label ("");
            conflicts_lbl.wrap    = true;
            conflicts_lbl.halign  = Gtk.Align.START;
            conflicts_lbl.selectable = true;
            deps_inner.append (conflicts_lbl);

            deps_scroll.child = deps_inner;
            stack.add_titled_with_icon (deps_scroll, "deps",
                "Dependencies", "emblem-default-symbolic");

            // Tab 3: Changelog
            var cl_scroll = new Gtk.ScrolledWindow ();
            cl_scroll.hscrollbar_policy = Gtk.PolicyType.NEVER;

            var cl_inner = new Gtk.Box (Gtk.Orientation.VERTICAL, 8);
            cl_inner.margin_start  = 24;
            cl_inner.margin_end    = 24;
            cl_inner.margin_top    = 16;
            cl_inner.margin_bottom = 24;

            var cl_hdr = new Gtk.Label ("<b>Changelog</b>");
            cl_hdr.use_markup = true; cl_hdr.halign = Gtk.Align.START;
            cl_inner.append (cl_hdr);

            changelog_lbl = new Gtk.Label ("");
            changelog_lbl.wrap       = true;
            changelog_lbl.halign     = Gtk.Align.START;
            changelog_lbl.xalign     = 0;
            changelog_lbl.selectable = true;
            changelog_lbl.add_css_class ("monospace");
            cl_inner.append (changelog_lbl);

            cl_scroll.child = cl_inner;
            stack.add_titled_with_icon (cl_scroll, "changelog",
                "Changelog", "document-open-recent-symbolic");

            // Tab 4: Installed Files
            var files_scroll = new Gtk.ScrolledWindow ();
            files_scroll.hscrollbar_policy = Gtk.PolicyType.NEVER;
            files_scroll.vexpand = true;

            files_list = new Gtk.ListBox ();
            files_list.css_classes   = { "boxed-list" };
            files_list.selection_mode = Gtk.SelectionMode.NONE;
            files_list.margin_start   = 24;
            files_list.margin_end     = 24;
            files_list.margin_top     = 16;
            files_list.margin_bottom  = 24;
            files_scroll.child = files_list;

            stack.add_titled_with_icon (files_scroll, "files",
                "Files", "folder-symbolic");

            // ViewSwitcherBar at bottom
            var switcher_bar = new Adw.ViewSwitcherBar ();
            switcher_bar.stack = stack;
            switcher_bar.reveal = true;

            var content = new Gtk.Box (Gtk.Orientation.VERTICAL, 0);
            content.vexpand = true;
            content.append (stack);

            append (content);
            append (switcher_bar);
        }

        private Adw.ActionRow make_meta_row (string title, Gtk.Label value_lbl) {
            var row = new Adw.ActionRow ();
            row.title = title;
            value_lbl.halign = Gtk.Align.END;
            value_lbl.hexpand = true;
            row.add_suffix (value_lbl);
            return row;
        }

        public void load (PackageInfo pkg, PackageStore store) {
            _pkg   = pkg;
            _store = store;

            // ── Header ────────────────────────────────────────────
            name_lbl.label = "<span size='xx-large' weight='bold'>%s</span>  <span size='large' foreground='gray'>%s</span>".printf (
                GLib.Markup.escape_text (pkg.name),
                GLib.Markup.escape_text (pkg.version)
            );
            name_lbl.use_markup = true;
            summary_lbl.label = pkg.summary;

            // Status badge
            switch (pkg.status) {
                case PackageStatus.INSTALLED:
                    status_badge.label      = "● Installed";
                    status_badge.css_classes = { "success" };
                    break;
                case PackageStatus.UPDATE_AVAILABLE:
                    status_badge.label      = "↑ Update available";
                    status_badge.css_classes = { "warning" };
                    break;
                case PackageStatus.INSTALLING:
                    status_badge.label      = "⟳ Installing…";
                    status_badge.css_classes = {};
                    break;
                case PackageStatus.REMOVING:
                    status_badge.label      = "✘ Removing…";
                    status_badge.css_classes = {};
                    break;
                default:
                    status_badge.label      = "";
                    status_badge.css_classes = {};
                    break;
            }

            size_lbl.label    = pkg.installed_size > 0 ?
                "Installed: %s".printf (pkg.formatted_installed_size ()) : "";
            dl_size_lbl.label = pkg.download_size > 0 ?
                "Download: %s".printf (pkg.formatted_download_size ()) : "";

            install_btn.visible = !pkg.is_installed () &&
                pkg.status != PackageStatus.INSTALLING;
            remove_btn.visible  =  pkg.is_installed () &&
                pkg.status != PackageStatus.REMOVING;

            // ── Load icon ─────────────────────────────────────────
            pkg_icon.set_from_icon_name (
                pkg.icon_name != "" ? pkg.icon_name : "package-x-generic");

            // If icon_url is set, load it asynchronously
            if (pkg.icon_url != "") {
                load_icon_url_async.begin (pkg.icon_url);
            }

            // ── Screenshots ───────────────────────────────────────
            while (true) {
                var ch = screenshots_box.get_first_child ();
                if (ch == null) break;
                screenshots_box.remove (ch);
            }
            if (pkg.screenshot_urls.length > 0) {
                screenshots_box.visible = true;
                foreach (var url in pkg.screenshot_urls) {
                    load_screenshot_async.begin (url);
                }
            } else {
                screenshots_box.visible = false;
            }

            // ── Description tab ───────────────────────────────────
            desc_lbl.label    = pkg.description != "" ? pkg.description :
                "No description available.";
            maintainer_lbl.label = pkg.maintainer != "" ? pkg.maintainer : "—";
            license_lbl.label    = pkg.license    != "" ? pkg.license    : "—";
            if (pkg.homepage != "") {
                homepage_lbl.label = "<a href=\"%s\">%s</a>".printf (
                    GLib.Markup.escape_text (pkg.homepage),
                    GLib.Markup.escape_text (pkg.homepage));
                homepage_lbl.use_markup = true;
            } else {
                homepage_lbl.label = "—";
            }

            // ── Deps tab ──────────────────────────────────────────
            deps_lbl.label      = pkg.dependencies.length > 0 ?
                string.joinv ("\n", pkg.dependencies) : "None";
            conflicts_lbl.label = pkg.conflicts.length > 0 ?
                string.joinv ("\n", pkg.conflicts) : "None";

            // ── Changelog tab ─────────────────────────────────────
            changelog_lbl.label = pkg.changelog != "" ? pkg.changelog :
                "No changelog available.\n\nRun  hammer changelog %s  in a terminal for details.".printf (pkg.name);

            // ── Fetch installed files asynchronously ──────────────
            if (pkg.is_installed () && pkg.installed_files.length == 0) {
                load_files_async.begin (pkg);
            } else {
                populate_files_list (pkg);
            }
        }

        private async void load_files_async (PackageInfo pkg) {
            // Run `hammer files <pkg> --json` to get file list
            try {
                var sub = new GLib.Subprocess.newv (
                    { "hammer", "files", pkg.name, "--json" },
                    GLib.SubprocessFlags.STDOUT_PIPE | GLib.SubprocessFlags.STDERR_SILENCE
                );
                string stdout_data = "";
                yield sub.communicate_utf8_async (null, null, out stdout_data, null);

                // Parse JSON array of file paths
                if (stdout_data.strip () != "") {
                    try {
                        var parser = new Json.Parser ();
                        parser.load_from_data (stdout_data);
                        var root = parser.get_root ();
                        if (root != null && root.get_node_type () == Json.NodeType.ARRAY) {
                            string[] files = {};
                            root.get_array ().foreach_element ((arr, _i, node) => {
                                string path = node.get_string ();
                                if (path != "") files += path;
                            });
                            pkg.installed_files = files;
                        }
                    } catch (Error parse_e) {
                        // Fallback: parse as plain text (one file per line)
                        string[] files = {};
                        foreach (var line in stdout_data.split ("\n")) {
                            string l = line.strip ();
                            if (l.has_prefix ("/") || l.has_prefix ("d ") || l.has_prefix ("- ")) {
                                // Strip "d " / "- " / "l " prefixes from human output
                                if (l.length > 2 && l[1] == ' ') l = l.substring (2);
                                files += l.strip ();
                            }
                        }
                        pkg.installed_files = files;
                    }
                }
            } catch (Error e) {
                // hammer not available or package not installed
            }
            populate_files_list (pkg);
        }

        private void populate_files_list (PackageInfo pkg) {
            // Clear
            while (true) {
                var ch = files_list.get_first_child ();
                if (ch == null) break;
                files_list.remove (ch);
            }

            if (pkg.installed_files.length > 0) {
                foreach (var f in pkg.installed_files) {
                    var row = new Adw.ActionRow ();
                    row.title      = f;
                    row.css_classes = { "monospace" };
                    string icon_nm = f.has_suffix ("/") ? "folder-symbolic" :
                        f.has_suffix (".so") || f.has_suffix (".so.1") ? "application-x-sharedlib-symbolic" :
                        f.has_prefix ("/usr/bin") || f.has_prefix ("/usr/sbin") ? "application-x-executable-symbolic" :
                        "text-x-generic-symbolic";
                    row.add_prefix (new Gtk.Image.from_icon_name (icon_nm));
                    files_list.append (row);
                }
            } else {
                var row = new Adw.ActionRow ();
                row.title = pkg.is_installed () ?
                    "Loading file list…" :
                    "Install package to view files";
                files_list.append (row);
            }
        }

        private async void load_icon_url_async (string url) {
            try {
                var session = new Soup.Session ();
                var msg     = new Soup.Message ("GET", url);
                var stream  = yield session.send_async (msg, GLib.Priority.DEFAULT, null);
                if (msg.status_code != 200) return;

                var loader  = new Gdk.PixbufLoader ();
                uint8[] buf = new uint8[65536];
                while (true) {
                    var n = yield stream.read_async (buf, GLib.Priority.DEFAULT, null);
                    if (n == 0) break;
                    loader.write (buf[0:n]);
                }
                loader.close ();
                var pixbuf = loader.get_pixbuf ();
                if (pixbuf != null) {
                    pixbuf = pixbuf.scale_simple (80, 80, Gdk.InterpType.BILINEAR);
                    pkg_icon.set_from_pixbuf (pixbuf);
                }
            } catch (Error e) {
                // Fallback icon already shown
            }
        }

        private async void load_screenshot_async (string url) {
            try {
                var session = new Soup.Session ();
                var msg     = new Soup.Message ("GET", url);
                var stream  = yield session.send_async (msg, GLib.Priority.DEFAULT, null);
                if (msg.status_code != 200) return;

                var loader = new Gdk.PixbufLoader ();
                uint8[] buf = new uint8[65536];
                while (true) {
                    var n = yield stream.read_async (buf, GLib.Priority.DEFAULT, null);
                    if (n == 0) break;
                    loader.write (buf[0:n]);
                }
                loader.close ();
                var pixbuf = loader.get_pixbuf ();
                if (pixbuf == null) return;

                // Scale to max 320×200
                int w = pixbuf.get_width ();
                int h = pixbuf.get_height ();
                if (w > 320) {
                    h = (int)((double)h / w * 320);
                    w = 320;
                    pixbuf = pixbuf.scale_simple (w, h, Gdk.InterpType.BILINEAR);
                }

                var frame = new Gtk.Frame (null);
                frame.css_classes = { "screenshot-frame" };
                var img = new Gtk.Image.from_pixbuf (pixbuf);
                img.pixel_size = -1;
                frame.child = img;

                screenshots_box.append (frame);
            } catch (Error e) {
                // Skip failed screenshots silently
            }
        }

        private void on_install () {
            if (_pkg == null || _store == null) return;
            install_btn.sensitive = false;
            install_btn.label     = "Installing…";
            _store.install_package_async.begin (_pkg, () => {
                install_btn.label     = "Install";
                install_btn.sensitive = true;
                if (_pkg != null) load (_pkg, _store);
            });
        }

        private void on_remove () {
            if (_pkg == null || _store == null) return;
            remove_btn.sensitive = false;
            remove_btn.label     = "Removing…";
            _store.remove_package_async.begin (_pkg, () => {
                remove_btn.label     = "Remove";
                remove_btn.sensitive = true;
                if (_pkg != null) load (_pkg, _store);
            });
        }
    }

    // ────────────────────────────────────────────────────────────
    //  StatsView — statystyki dysku i paczek (nowość 0.6)
    // ────────────────────────────────────────────────────────────
    public class StatsView : Gtk.Box {

        private Gtk.Label  total_size_lbl;
        private Gtk.Label  pkg_count_lbl;
        private Gtk.Label  store_size_lbl;
        private Gtk.ListBox cat_list;
        private PackageStore? _store;

        public StatsView () {
            Object (orientation: Gtk.Orientation.VERTICAL, spacing: 0);

            var header = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 8);
            header.margin_start  = 20;
            header.margin_end    = 20;
            header.margin_top    = 16;
            header.margin_bottom = 8;

            var title = new Gtk.Label ("<b>Disk &amp; Package Statistics</b>");
            title.use_markup = true;
            title.halign     = Gtk.Align.START;
            title.hexpand    = true;
            header.append (title);

            var refresh_btn = new Gtk.Button.from_icon_name ("view-refresh-symbolic");
            refresh_btn.tooltip_text = "Recalculate";
            refresh_btn.clicked.connect (() => { if (_store != null) refresh (_store); });
            header.append (refresh_btn);
            append (header);

            // ── Summary cards ────────────────────────────────────
            var cards_row = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 16);
            cards_row.margin_start  = 20;
            cards_row.margin_end    = 20;
            cards_row.margin_bottom = 16;
            cards_row.homogeneous   = true;

            pkg_count_lbl  = new Gtk.Label ("—");
            store_size_lbl = new Gtk.Label ("—");
            total_size_lbl = new Gtk.Label ("—");

            cards_row.append (make_stat_card ("Installed packages", pkg_count_lbl, "package-x-generic-symbolic"));
            cards_row.append (make_stat_card ("Total installed size", total_size_lbl, "drive-harddisk-symbolic"));
            cards_row.append (make_stat_card ("Store cache", store_size_lbl, "folder-download-symbolic"));
            append (cards_row);

            // ── Per-category list ────────────────────────────────
            var sep = new Gtk.Separator (Gtk.Orientation.HORIZONTAL);
            sep.margin_start = sep.margin_end = 20;
            append (sep);

            var cat_lbl = new Gtk.Label ("<b>By Category</b>");
            cat_lbl.use_markup   = true;
            cat_lbl.halign       = Gtk.Align.START;
            cat_lbl.margin_start = 20;
            cat_lbl.margin_top   = 16;
            cat_lbl.margin_bottom = 8;
            append (cat_lbl);

            var scroll = new Gtk.ScrolledWindow ();
            scroll.hscrollbar_policy = Gtk.PolicyType.NEVER;
            scroll.vexpand = true;

            cat_list = new Gtk.ListBox ();
            cat_list.css_classes   = { "boxed-list" };
            cat_list.margin_start  = 20;
            cat_list.margin_end    = 20;
            cat_list.margin_bottom = 20;
            cat_list.selection_mode = Gtk.SelectionMode.NONE;
            scroll.child = cat_list;
            append (scroll);
        }

        private Gtk.Box make_stat_card (string title, Gtk.Label value_lbl, string icon_name) {
            var card = new Gtk.Box (Gtk.Orientation.VERTICAL, 6);
            card.css_classes = { "store-card" };
            card.hexpand     = true;

            var icon = new Gtk.Image.from_icon_name (icon_name);
            icon.pixel_size = 32;
            icon.halign     = Gtk.Align.CENTER;
            card.append (icon);

            value_lbl.css_classes = { "title-2" };
            value_lbl.halign      = Gtk.Align.CENTER;
            card.append (value_lbl);

            var lbl = new Gtk.Label (title);
            lbl.add_css_class ("dim-label");
            lbl.halign  = Gtk.Align.CENTER;
            lbl.wrap    = true;
            lbl.justify = Gtk.Justification.CENTER;
            card.append (lbl);

            return card;
        }

        public void refresh (PackageStore store) {
            _store = store;

            var installed = store.get_installed ();
            uint count    = installed.length ();
            pkg_count_lbl.label = count.to_string ();

            // Total installed size
            int64 total = 0;
            installed.@foreach ((p) => total += p.installed_size);
            total_size_lbl.label = format_bytes (total);

            // Store cache size (async via hammer)
            fetch_store_size_async.begin ();

            // Per-category breakdown
            while (true) {
                var ch = cat_list.get_first_child ();
                if (ch == null) break;
                cat_list.remove (ch);
            }

            var cat_totals = new HashTable<string, int64?> (str_hash, str_equal);
            var cat_counts = new HashTable<string, uint?> (str_hash, str_equal);
            installed.@foreach ((p) => {
                var cat = p.category == "" ? "other" : p.category;
                int64 prev_size  = (int64)(cat_totals.get (cat) ?? 0);
                uint  prev_count = (uint)(cat_counts.get (cat) ?? 0);
                cat_totals.set (cat, prev_size + p.installed_size);
                cat_counts.set (cat, prev_count + 1);
            });

            string[] cats = {};
            cat_totals.@foreach ((k, _v) => cats += k);
            // Sort by size desc
            for (int i = 0; i < cats.length; i++) {
                for (int j = i + 1; j < cats.length; j++) {
                    if ((int64)(cat_totals.get (cats[j]) ?? 0) >
                        (int64)(cat_totals.get (cats[i]) ?? 0)) {
                        string tmp = cats[i]; cats[i] = cats[j]; cats[j] = tmp;
                    }
                }
            }

            if (cats.length == 0) {
                var row = new Gtk.ListBoxRow ();
                row.selectable = false;
                var lbl = new Gtk.Label ("No installed packages.");
                lbl.margin_top = lbl.margin_bottom = 16;
                row.child = lbl;
                cat_list.append (row);
                return;
            }

            foreach (var cat in cats) {
                var row = new Gtk.ListBoxRow ();
                row.selectable = false;
                var box = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 12);
                box.margin_start  = 12;
                box.margin_end    = 12;
                box.margin_top    = 10;
                box.margin_bottom = 10;

                string cap = "%s%s".printf (cat.substring (0, 1).up (), cat.substring (1));
                var name_lbl = new Gtk.Label ("<b>%s</b>".printf (cap));
                name_lbl.use_markup = true;
                name_lbl.halign     = Gtk.Align.START;
                name_lbl.hexpand    = true;
                box.append (name_lbl);

                uint c = (uint)(cat_counts.get (cat) ?? 0);
                var cnt_lbl = new Gtk.Label ("%u pkg".printf (c));
                cnt_lbl.add_css_class ("dim-label");
                box.append (cnt_lbl);

                var sz_lbl = new Gtk.Label (format_bytes ((int64)(cat_totals.get (cat) ?? 0)));
                sz_lbl.add_css_class ("dim-label");
                box.append (sz_lbl);

                row.child = box;
                cat_list.append (row);
            }
        }

        private async void fetch_store_size_async () {
            string out_data = "";
            try {
                var sub = new GLib.Subprocess.newv (
                    { "du", "-sh", "/hammer/store" },
                    GLib.SubprocessFlags.STDOUT_PIPE | GLib.SubprocessFlags.STDERR_SILENCE
                );
                yield sub.communicate_utf8_async (null, null, out out_data, null);
                var parts = out_data.strip ().split ("\t");
                if (parts.length > 0 && parts[0] != "") {
                    store_size_lbl.label = parts[0];
                    return;
                }
            } catch (Error e) {}
            store_size_lbl.label = "—";
        }

        private string format_bytes (int64 bytes) {
            if (bytes <= 0)               return "—";
            if (bytes < 1024)             return "%lld B".printf (bytes);
            if (bytes < 1024 * 1024)      return "%.1f KiB".printf (bytes / 1024.0);
            if (bytes < 1024*1024*1024)   return "%.1f MiB".printf (bytes / (1024.0 * 1024));
            return "%.2f GiB".printf (bytes / (1024.0 * 1024 * 1024));
        }
    }

    // ────────────────────────────────────────────────────────────
    //  SettingsView — zarządzanie źródłami i ustawieniami (0.6)
    // ────────────────────────────────────────────────────────────

    public class SettingsView : Gtk.Box {

        public signal void sources_changed ();

        private Gtk.ListBox sources_list;
        private Gtk.Switch  auto_update_sw;
        private Gtk.Switch  notifications_sw;
        private Gtk.Switch  check_signatures_sw;

        // Loaded source entries
        private SourceEntry[] _sources = {};

        // Settings persistence via GLib.KeyFile
        private static string settings_path () {
            return Path.build_filename (
                Environment.get_user_config_dir (), "hammer", "hammer-store.conf");
        }

        private GLib.KeyFile _kf = new GLib.KeyFile ();
        private bool         _kf_loaded = false;

        private void load_settings () {
            if (_kf_loaded) return;
            try {
                _kf.load_from_file (settings_path (),
                    GLib.KeyFileFlags.KEEP_COMMENTS);
            } catch (Error e) { /* first run */ }
            _kf_loaded = true;
        }

        private void save_settings () {
            string path = settings_path ();
            string dir  = Path.get_dirname (path);
            DirUtils.create_with_parents (dir, 0755);
            try {
                string data = _kf.to_data ();
                FileUtils.set_contents (path, data);
            } catch (Error e) {
                warning ("Cannot save settings: %s", e.message);
            }
        }

        private bool get_bool (string key, bool fallback) {
            load_settings ();
            try { return _kf.get_boolean ("preferences", key); }
            catch (Error e) { return fallback; }
        }

        private void set_bool (string key, bool val) {
            _kf.set_boolean ("preferences", key, val);
            save_settings ();
        }

        public SettingsView () {
            Object (orientation: Gtk.Orientation.VERTICAL, spacing: 0);

            var scroll = new Gtk.ScrolledWindow ();
            scroll.hscrollbar_policy = Gtk.PolicyType.NEVER;
            scroll.vexpand = true;

            var inner = new Gtk.Box (Gtk.Orientation.VERTICAL, 0);
            inner.margin_start  = 24;
            inner.margin_end    = 24;
            inner.margin_top    = 20;
            inner.margin_bottom = 20;

            // ── Sources section ───────────────────────────────────
            inner.append (make_section_header ("Package Sources",
                "Repositories used by hammer to find packages"));

            var src_header = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 8);
            src_header.margin_bottom = 8;

            var src_lbl = new Gtk.Label ("Active sources:");
            src_lbl.halign  = Gtk.Align.START;
            src_lbl.hexpand = true;
            src_lbl.add_css_class ("dim-label");
            src_header.append (src_lbl);

            var add_src_btn = new Gtk.Button.with_label ("Add source…");
            add_src_btn.css_classes = { "suggested-action" };
            add_src_btn.clicked.connect (on_add_source);
            src_header.append (add_src_btn);
            inner.append (src_header);

            sources_list = new Gtk.ListBox ();
            sources_list.css_classes   = { "boxed-list" };
            sources_list.selection_mode = Gtk.SelectionMode.NONE;
            sources_list.margin_bottom = 20;
            inner.append (sources_list);

            // ── Preferences section ───────────────────────────────
            inner.append (make_section_header ("Preferences", "General hammer behaviour"));

            var prefs_list = new Gtk.ListBox ();
            prefs_list.css_classes   = { "boxed-list" };
            prefs_list.selection_mode = Gtk.SelectionMode.NONE;
            prefs_list.margin_bottom = 20;

            auto_update_sw       = new Gtk.Switch ();
            notifications_sw     = new Gtk.Switch ();
            check_signatures_sw  = new Gtk.Switch ();

            // Load persisted values
            auto_update_sw.active      = get_bool ("auto_update",      false);
            notifications_sw.active    = get_bool ("notifications",    false);
            check_signatures_sw.active = get_bool ("check_signatures", true);

            // Save on change
            auto_update_sw.notify["active"].connect (() =>
                set_bool ("auto_update", auto_update_sw.active));
            notifications_sw.notify["active"].connect (() =>
                set_bool ("notifications", notifications_sw.active));
            check_signatures_sw.notify["active"].connect (() =>
                set_bool ("check_signatures", check_signatures_sw.active));

            prefs_list.append (make_switch_row (
                "Automatic update checks",
                "Check for updates in the background every 24 h",
                auto_update_sw));
            prefs_list.append (make_switch_row (
                "Desktop notifications",
                "Show a notification when updates are available",
                notifications_sw));
            prefs_list.append (make_switch_row (
                "Verify GPG signatures",
                "Always check package integrity before installing",
                check_signatures_sw));
            inner.append (prefs_list);

            // ── Danger zone ────────────────────────────────────────
            inner.append (make_section_header ("Maintenance", "Advanced operations"));

            var maint_list = new Gtk.ListBox ();
            maint_list.css_classes   = { "boxed-list" };
            maint_list.selection_mode = Gtk.SelectionMode.NONE;

            maint_list.append (make_action_row (
                "Clear download cache",
                "Free disk space used by cached .deb packages",
                "edit-clear-symbolic"));
            maint_list.append (make_action_row (
                "Rebuild file index",
                "Regenerate the file-to-package mapping database",
                "media-playlist-repeat-symbolic"));
            maint_list.append (make_action_row (
                "Sync package lists",
                "Same as running  hammer sync  in the terminal",
                "network-transmit-receive-symbolic"));

            // Connect activated signals
            ((Adw.ActionRow) maint_list.get_row_at_index (0)).activated.connect (on_clear_cache);
            ((Adw.ActionRow) maint_list.get_row_at_index (1)).activated.connect (on_rebuild_index);
            ((Adw.ActionRow) maint_list.get_row_at_index (2)).activated.connect (on_sync);
            inner.append (maint_list);

            scroll.child = inner;
            append (scroll);

            load_sources ();
        }

        private Gtk.Box make_section_header (string title, string subtitle) {
            var box = new Gtk.Box (Gtk.Orientation.VERTICAL, 2);
            box.margin_top    = 12;
            box.margin_bottom = 8;

            var t = new Gtk.Label ("<b>%s</b>".printf (title));
            t.use_markup = true;
            t.halign     = Gtk.Align.START;
            box.append (t);

            var s = new Gtk.Label (subtitle);
            s.halign = Gtk.Align.START;
            s.add_css_class ("dim-label");
            box.append (s);

            return box;
        }

        private Adw.ActionRow make_switch_row (string title, string subtitle, Gtk.Switch sw) {
            var row = new Adw.ActionRow ();
            row.title    = title;
            row.subtitle = subtitle;
            sw.valign    = Gtk.Align.CENTER;
            row.add_suffix (sw);
            row.activatable_widget = sw;
            return row;
        }

        private Adw.ActionRow make_action_row (string title, string subtitle,
                                               string icon) {
            var row = new Adw.ActionRow ();
            row.title       = title;
            row.subtitle    = subtitle;
            row.activatable = true;
            var arrow = new Gtk.Image.from_icon_name (icon);
            row.add_suffix (arrow);
            return row;
        }

        // ── Sources loading ─────────────────────────────────────

        private void load_sources () {
            while (true) {
                var ch = sources_list.get_first_child ();
                if (ch == null) break;
                sources_list.remove (ch);
            }
            _sources = {};
            load_sources_from_file ("/hammer/db/sources-list.hk");
            // Fallback stubs if empty
            if (_sources.length == 0) {
                _sources += SourceEntry () { name = "debian-trixie",
                    baseurl = "http://deb.debian.org/debian",
                    suite = "trixie", enabled = true };
                _sources += SourceEntry () { name = "debian-trixie-security",
                    baseurl = "http://security.debian.org/debian-security",
                    suite = "trixie-security", enabled = true };
            }
            populate_sources_list ();
        }

        private void load_sources_from_file (string path) {
            try {
                string content;
                FileUtils.get_contents (path, out content);
                string? current_name = null;
                SourceEntry entry = SourceEntry ();

                foreach (var raw in content.split ("\n")) {
                    var line = raw.strip ();
                    if (line.has_prefix ("!") || line == "") continue;
                    if (line.has_prefix ("[") && line.has_suffix ("]")) {
                        if (current_name != null) _sources += entry;
                        current_name   = line[1:line.length - 1];
                        entry          = SourceEntry ();
                        entry.name     = current_name;
                        entry.enabled  = true;
                        continue;
                    }
                    if (!line.contains ("=>")) continue;
                    var parts = line.split ("=>", 2);
                    if (parts.length < 2) continue;
                    var key = parts[0].strip ();
                    if (key.has_prefix ("->")) key = key.substring (2).strip ();
                    var val = parts[1].strip ();
                    switch (key) {
                        case "baseurl": entry.baseurl = val; break;
                        case "suite":   entry.suite   = val; break;
                        case "enabled": entry.enabled = val == "true"; break;
                        case "gpgkey":  entry.gpgkey  = val; break;
                        default: break;
                    }
                }
                if (current_name != null) _sources += entry;
            } catch (Error e) {} // Plik nie istnieje — użyj stubów
        }

        private void populate_sources_list () {
            foreach (var src in _sources) {
                var row = new Adw.ExpanderRow ();
                row.title    = src.name;
                row.subtitle = src.suite;

                var sw = new Gtk.Switch ();
                sw.active = src.enabled;
                sw.valign = Gtk.Align.CENTER;
                row.add_action (sw);

                // URL row
                var url_row = new Adw.ActionRow ();
                url_row.title    = "URL";
                url_row.subtitle = src.baseurl;
                row.add_row (url_row);

                // GPG row
                if (src.gpgkey != null && src.gpgkey != "") {
                    var gpg_row = new Adw.ActionRow ();
                    gpg_row.title    = "GPG key";
                    gpg_row.subtitle = src.gpgkey;
                    row.add_row (gpg_row);
                }

                // Remove button
                var rm_row = new Adw.ActionRow ();
                rm_row.title = "Remove this source";
                var rm_btn = new Gtk.Button.with_label ("Remove");
                rm_btn.css_classes = { "destructive-action" };
                rm_btn.valign = Gtk.Align.CENTER;
                rm_row.add_suffix (rm_btn);
                string src_name = src.name;
                rm_btn.clicked.connect (() => {
                    remove_source (src_name);
                });
                row.add_row (rm_row);

                sources_list.append (row);
            }

            if (_sources.length == 0) {
                var row = new Gtk.ListBoxRow ();
                row.selectable = false;
                var lbl = new Gtk.Label ("No sources configured.");
                lbl.margin_top = lbl.margin_bottom = 16;
                row.child = lbl;
                sources_list.append (row);
            }
        }

        // ── Source operations ────────────────────────────────────

        private void remove_source (string name) {
            SourceEntry[] updated = {};
            foreach (var s in _sources) {
                if (s.name != name) updated += s;
            }
            _sources = updated;
            populate_sources_list ();
            sources_changed ();
        }

        private void on_add_source () {
            // Simple dialog — ask for baseurl + suite
            var dialog = new Adw.MessageDialog (get_root () as Gtk.Window,
                "Add Package Source", null);
            dialog.add_response ("cancel", "Cancel");
            dialog.add_response ("add",    "Add");
            dialog.set_response_appearance ("add", Adw.ResponseAppearance.SUGGESTED);

            var box = new Gtk.Box (Gtk.Orientation.VERTICAL, 8);
            box.margin_top = 12;

            var url_entry = new Adw.EntryRow ();
            url_entry.title = "Repository URL";
            url_entry.text  = "http://deb.debian.org/debian";
            box.append (url_entry);

            var suite_entry = new Adw.EntryRow ();
            suite_entry.title = "Suite (e.g. trixie)";
            suite_entry.text  = "trixie";
            box.append (suite_entry);

            var name_entry = new Adw.EntryRow ();
            name_entry.title = "Source name";
            name_entry.text  = "my-repo";
            box.append (name_entry);

            dialog.extra_child = box;

            dialog.response.connect ((resp) => {
                if (resp == "add") {
                    var entry = SourceEntry ();
                    entry.name    = name_entry.text.strip ();
                    entry.baseurl = url_entry.text.strip ();
                    entry.suite   = suite_entry.text.strip ();
                    entry.enabled = true;
                    if (entry.name != "" && entry.baseurl != "") {
                        _sources += entry;
                        load_sources ();
                        sources_changed ();
                    }
                }
                dialog.destroy ();
            });
            dialog.present ();
        }

        private void on_clear_cache () {
            var dialog = new Adw.MessageDialog (get_root () as Gtk.Window,
                "Clear Download Cache?",
                "This will delete all cached .deb files from /hammer/cache. " +
                "Packages will be re-downloaded when needed.");
            dialog.add_response ("cancel", "Cancel");
            dialog.add_response ("clear",  "Clear cache");
            dialog.set_response_appearance ("clear", Adw.ResponseAppearance.DESTRUCTIVE);
            dialog.response.connect ((resp) => {
                if (resp == "clear") {
                    try {
                        var _sub = new GLib.Subprocess.newv (
                            { "pkexec", "hammer", "cache", "--clear" },
                            GLib.SubprocessFlags.NONE
                        );
                    } catch (Error e) {
                        warning ("Failed to clear cache: %s", e.message);
                    }
                }
                dialog.destroy ();
            });
            dialog.present ();
        }

        private void on_rebuild_index () {
            try {
                var _sub = new GLib.Subprocess.newv (
                    { "pkexec", "hammer", "index", "--rebuild" },
                    GLib.SubprocessFlags.NONE
                );
            } catch (Error e) {
                warning ("Failed to rebuild index: %s", e.message);
            }
        }

        private void on_sync () {
            try {
                var _sub = new GLib.Subprocess.newv (
                    { "pkexec", "hammer", "sync" },
                    GLib.SubprocessFlags.NONE
                );
            } catch (Error e) {
                warning ("hammer sync failed: %s", e.message);
            }
            sources_changed ();
        }
    }

} // end namespace HammerStore

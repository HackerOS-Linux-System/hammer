using Gtk;
using Adw;

namespace HammerStore {

    // ────────────────────────────────────────────────────────────
    //  PackageCard — used in grids
    // ────────────────────────────────────────────────────────────

    public class PackageCard : Gtk.Box {

        public signal void clicked ();

        private Gtk.Button btn;

        public PackageCard (PackageInfo pkg, bool large) {
            Object (orientation: Gtk.Orientation.VERTICAL, spacing: 0);

            btn = new Gtk.Button ();
            btn.css_classes = { "store-card" };
            btn.has_frame   = false;
            btn.hexpand     = true;
            btn.clicked.connect (() => this.clicked ());

            var inner = new Gtk.Box (Gtk.Orientation.VERTICAL, 8);
            inner.margin_start  = 4;
            inner.margin_end    = 4;
            inner.margin_top    = 4;
            inner.margin_bottom = 4;

            // Icon
            var icon = new Gtk.Image.from_icon_name (pkg.icon_name);
            icon.pixel_size = large ? 64 : 48;
            icon.halign     = Gtk.Align.CENTER;
            inner.append (icon);

            // Name
            var name_lbl = new Gtk.Label ("<b>%s</b>".printf (pkg.name));
            name_lbl.use_markup      = true;
            name_lbl.halign          = Gtk.Align.CENTER;
            name_lbl.ellipsize       = Pango.EllipsizeMode.END;
            name_lbl.max_width_chars = 20;
            inner.append (name_lbl);

            // Summary
            var sum_lbl = new Gtk.Label (pkg.summary);
            sum_lbl.halign          = Gtk.Align.CENTER;
            sum_lbl.ellipsize       = Pango.EllipsizeMode.END;
            sum_lbl.max_width_chars = 24;
            sum_lbl.add_css_class ("dim-label");
            inner.append (sum_lbl);

            if (large) {
                // Rating stars
                var rating_box = build_rating (pkg.rating);
                rating_box.halign = Gtk.Align.CENTER;
                inner.append (rating_box);
            }

            // Status badge
            if (pkg.is_installed ()) {
                var badge = new Gtk.Label (pkg.status_label ());
                badge.css_classes = pkg.status == PackageStatus.UPDATE_AVAILABLE
                    ? new string[] { "badge-update" }
                    : new string[] { "badge-installed" };
                badge.halign = Gtk.Align.CENTER;
                inner.append (badge);
            }

            btn.child = inner;
            append (btn);
        }

        private Gtk.Box build_rating (double r) {
            var box = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 2);
            for (int i = 1; i <= 5; i++) {
                var star = new Gtk.Label (i <= (int)(r / 20) ? "★" : "☆");
                star.css_classes = { "rating-star" };
                box.append (star);
            }
            return box;
        }
    }

    // ────────────────────────────────────────────────────────────
    //  PackageDetails — full detail page
    // ────────────────────────────────────────────────────────────

    public class PackageDetails : Gtk.Box {

        public signal void back_clicked ();

        private Gtk.Label  name_lbl;
        private Gtk.Label  version_lbl;
        private Gtk.Label  summary_lbl;
        private Gtk.Label  description_lbl;
        private Gtk.Label  size_dl_lbl;
        private Gtk.Label  size_inst_lbl;
        private Gtk.Label  maintainer_lbl;
        private Gtk.Label  license_lbl;
        private Gtk.Label  homepage_lbl;
        private Gtk.Image  icon_img;
        private Gtk.Button action_btn;
        private Gtk.Button back_btn;
        private Gtk.ProgressBar progress;

        private PackageInfo?   _pkg;
        private PackageStore?  _store;

        public PackageDetails () {
            Object (orientation: Gtk.Orientation.VERTICAL, spacing: 0);
            build_ui ();
        }

        private void build_ui () {
            var scroll = new Gtk.ScrolledWindow ();
            scroll.hscrollbar_policy = Gtk.PolicyType.NEVER;
            scroll.vexpand = true;

            var root = new Gtk.Box (Gtk.Orientation.VERTICAL, 0);

            // ── Header ──────────────────────────────────────────
            var header_box = new Gtk.Box (Gtk.Orientation.VERTICAL, 12);
            header_box.css_classes  = { "detail-header" };
            header_box.margin_start = 24;
            header_box.margin_end   = 24;
            header_box.margin_top   = 16;
            header_box.margin_bottom= 16;

            // Back button
            back_btn = new Gtk.Button.with_label ("← Back");
            back_btn.has_frame = false;
            back_btn.halign    = Gtk.Align.START;
            back_btn.clicked.connect (() => back_clicked ());
            header_box.append (back_btn);

            var top_row = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 20);
            icon_img = new Gtk.Image.from_icon_name ("package-x-generic");
            icon_img.pixel_size = 80;
            icon_img.css_classes = { "pkg-icon-lg" };
            top_row.append (icon_img);

            var info_col = new Gtk.Box (Gtk.Orientation.VERTICAL, 6);
            info_col.vexpand = true;

            name_lbl = new Gtk.Label ("");
            name_lbl.add_css_class ("title-1");
            name_lbl.halign = Gtk.Align.START;
            info_col.append (name_lbl);

            version_lbl = new Gtk.Label ("");
            version_lbl.add_css_class ("dim-label");
            version_lbl.halign = Gtk.Align.START;
            info_col.append (version_lbl);

            summary_lbl = new Gtk.Label ("");
            summary_lbl.halign = Gtk.Align.START;
            info_col.append (summary_lbl);

            // Action button
            action_btn = new Gtk.Button ();
            action_btn.halign = Gtk.Align.START;
            action_btn.valign = Gtk.Align.CENTER;
            action_btn.clicked.connect (on_action_clicked);
            info_col.append (action_btn);

            // Progress bar (hidden by default)
            progress = new Gtk.ProgressBar ();
            progress.pulse_step = 0.05;
            progress.visible    = false;
            info_col.append (progress);

            top_row.append (info_col);
            header_box.append (top_row);
            root.append (header_box);

            // ── Details grid ─────────────────────────────────────
            var details = new Gtk.Grid ();
            details.row_spacing    = 8;
            details.column_spacing = 16;
            details.margin_start   = 24;
            details.margin_end     = 24;
            details.margin_top     = 20;
            details.margin_bottom  = 20;

            string[] row_labels = {
                "Description:", "Download size:", "Installed size:",
                "Maintainer:", "License:", "Homepage:"
            };

            for (int i = 0; i < row_labels.length; i++) {
                var lbl = new Gtk.Label (row_labels[i]);
                lbl.add_css_class ("dim-label");
                lbl.halign = Gtk.Align.END;
                details.attach (lbl, 0, i);
            }

            description_lbl = make_val_label (); details.attach (description_lbl, 1, 0);
            size_dl_lbl     = make_val_label (); details.attach (size_dl_lbl,     1, 1);
            size_inst_lbl   = make_val_label (); details.attach (size_inst_lbl,   1, 2);
            maintainer_lbl  = make_val_label (); details.attach (maintainer_lbl,  1, 3);
            license_lbl     = make_val_label (); details.attach (license_lbl,     1, 4);
            homepage_lbl    = make_val_label (); details.attach (homepage_lbl,    1, 5);

            root.append (details);

            scroll.child = root;
            append (scroll);
        }

        private Gtk.Label make_val_label () {
            var l = new Gtk.Label ("");
            l.halign   = Gtk.Align.START;
            l.hexpand  = true;
            l.wrap     = true;
            l.selectable = true;
            return l;
        }

        public void load (PackageInfo pkg, PackageStore store) {
            _pkg   = pkg;
            _store = store;

            icon_img.icon_name   = pkg.icon_name;
            name_lbl.label       = pkg.name;
            version_lbl.label    = "Version %s".printf (pkg.version);
            summary_lbl.label    = pkg.summary;
            description_lbl.label = pkg.description.length > 0 ? pkg.description : pkg.summary;
            size_dl_lbl.label    = pkg.formatted_download_size ();
            size_inst_lbl.label  = pkg.formatted_installed_size ();
            maintainer_lbl.label = pkg.maintainer.length > 0 ? pkg.maintainer : "—";
            license_lbl.label    = pkg.license.length   > 0 ? pkg.license    : "—";
            homepage_lbl.label   = pkg.homepage.length  > 0 ? pkg.homepage   : "—";

            update_action_button ();

            store.package_changed.connect ((changed) => {
                if (changed.name == pkg.name) update_action_button ();
            });
        }

        private void update_action_button () {
            if (_pkg == null) return;
            switch (_pkg.status) {
                case PackageStatus.AVAILABLE:
                    action_btn.label       = "Install";
                    action_btn.css_classes = { "suggested-action" };
                    action_btn.sensitive   = true;
                    progress.visible       = false;
                    break;
                case PackageStatus.INSTALLED:
                    action_btn.label       = "Remove";
                    action_btn.css_classes = { "destructive-action" };
                    action_btn.sensitive   = true;
                    progress.visible       = false;
                    break;
                case PackageStatus.UPDATE_AVAILABLE:
                    action_btn.label       = "Update";
                    action_btn.css_classes = { "suggested-action" };
                    action_btn.sensitive   = true;
                    progress.visible       = false;
                    break;
                case PackageStatus.INSTALLING:
                case PackageStatus.REMOVING:
                    action_btn.label       = _pkg.status == PackageStatus.INSTALLING ? "Installing…" : "Removing…";
                    action_btn.sensitive   = false;
                    progress.visible       = true;
                    GLib.Timeout.add (80, () => { progress.pulse (); return _pkg.status == PackageStatus.INSTALLING || _pkg.status == PackageStatus.REMOVING; });
                    break;
                default:
                    action_btn.label     = "Broken";
                    action_btn.sensitive = false;
                    break;
            }
        }

        private void on_action_clicked () {
            if (_pkg == null || _store == null) return;
            if (_pkg.status == PackageStatus.AVAILABLE || _pkg.status == PackageStatus.UPDATE_AVAILABLE) {
                _store.install_package_async.begin (_pkg, () => {});
            } else if (_pkg.status == PackageStatus.INSTALLED) {
                _store.remove_package_async.begin (_pkg, () => {});
            }
        }
    }
}

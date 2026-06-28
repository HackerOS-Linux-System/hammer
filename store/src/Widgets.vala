using Gtk;
using Adw;

namespace HammerStore {

    // ────────────────────────────────────────────────────────────
    //  PackageCard — used in grids (rozbudowa 0.5: rozmiar badge)
    // ────────────────────────────────────────────────────────────

    public class PackageCard : Gtk.Box {

        public signal void clicked ();

        private Gtk.Button  btn;
        private Gtk.Image   icon_img;
        private Gtk.Overlay img_overlay;

        public PackageCard (PackageInfo pkg, bool large) {
            Object (orientation: Gtk.Orientation.VERTICAL, spacing: 0);

            btn = new Gtk.Button ();
            btn.css_classes = { "store-card" };
            btn.has_frame   = false;
            btn.hexpand     = true;
            btn.clicked.connect (() => this.clicked ());

            var outer = new Gtk.Box (Gtk.Orientation.VERTICAL, 0);

            // ── Icon area with overlay for badges ────────────────
            img_overlay = new Gtk.Overlay ();

            icon_img = new Gtk.Image.from_icon_name (
                pkg.icon_name != "" ? pkg.icon_name : "package-x-generic");
            icon_img.pixel_size = large ? 72 : 48;
            icon_img.halign     = Gtk.Align.CENTER;
            icon_img.valign     = Gtk.Align.CENTER;
            icon_img.margin_top = large ? 16 : 10;
            icon_img.margin_bottom = 0;

            // Rounded frame around icon
            var icon_frame_box = new Gtk.Box (Gtk.Orientation.VERTICAL, 0);
            icon_frame_box.css_classes = { "pkg-icon-frame" };
            icon_frame_box.halign      = Gtk.Align.CENTER;
            icon_frame_box.valign      = Gtk.Align.CENTER;
            icon_frame_box.margin_top  = large ? 12 : 8;
            icon_frame_box.append (icon_img);

            img_overlay.child = icon_frame_box;

            // Top-right badge for "New" featured packages
            if (pkg.featured && !pkg.is_installed ()) {
                var new_badge = new Gtk.Label ("NEW");
                new_badge.css_classes = { "badge-new" };
                new_badge.halign      = Gtk.Align.END;
                new_badge.valign      = Gtk.Align.START;
                new_badge.margin_end  = 4;
                new_badge.margin_top  = 4;
                img_overlay.add_overlay (new_badge);
            }
            outer.append (img_overlay);

            // ── Text content ──────────────────────────────────────
            var inner = new Gtk.Box (Gtk.Orientation.VERTICAL, 4);
            inner.margin_start  = 8;
            inner.margin_end    = 8;
            inner.margin_top    = 8;
            inner.margin_bottom = 10;

            var name_lbl = new Gtk.Label ("<b>%s</b>".printf (
                GLib.Markup.escape_text (pkg.name)));
            name_lbl.use_markup      = true;
            name_lbl.halign          = Gtk.Align.CENTER;
            name_lbl.ellipsize       = Pango.EllipsizeMode.END;
            name_lbl.max_width_chars = 18;
            inner.append (name_lbl);

            if (pkg.summary != "") {
                var sum_lbl = new Gtk.Label (pkg.summary);
                sum_lbl.halign          = Gtk.Align.CENTER;
                sum_lbl.ellipsize       = Pango.EllipsizeMode.END;
                sum_lbl.max_width_chars = 22;
                sum_lbl.add_css_class ("dim-label");
                sum_lbl.wrap           = false;
                inner.append (sum_lbl);
            }

            // ── Badges row ────────────────────────────────────────
            var badges = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 4);
            badges.halign = Gtk.Align.CENTER;
            badges.margin_top = 4;

            if (pkg.is_installed ()) {
                var badge = new Gtk.Label (
                    pkg.status == PackageStatus.UPDATE_AVAILABLE ? "↑ Update" : "✔ Installed");
                badge.css_classes = pkg.status == PackageStatus.UPDATE_AVAILABLE
                    ? new string[] { "badge-update" }
                    : new string[] { "badge-installed" };
                badges.append (badge);
            }

            if (large && pkg.installed_size > 0) {
                var size_badge = new Gtk.Label (pkg.formatted_installed_size ());
                size_badge.css_classes = { "badge-size" };
                badges.append (size_badge);
            }

            inner.append (badges);

            if (large) {
                var rating_box = build_rating (pkg.rating);
                rating_box.halign    = Gtk.Align.CENTER;
                rating_box.margin_top = 2;
                inner.append (rating_box);
            }

            outer.append (inner);
            btn.child = outer;
            append (btn);

            // Load remote icon asynchronously if icon_url is set
            if (pkg.icon_url != "") {
                load_icon_async.begin (pkg.icon_url, large ? 72 : 48);
            }
        }

        private async void load_icon_async (string url, int size) {
            try {
                var session = new Soup.Session ();
                var msg     = new Soup.Message ("GET", url);
                var stream  = yield session.send_async (msg, GLib.Priority.LOW, null);
                if (msg.status_code != 200) return;

                var loader = new Gdk.PixbufLoader ();
                uint8[] buf = new uint8[32768];
                while (true) {
                    var n = yield stream.read_async (buf, GLib.Priority.LOW, null);
                    if (n == 0) break;
                    loader.write (buf[0:n]);
                }
                loader.close ();
                var pixbuf = loader.get_pixbuf ();
                if (pixbuf == null) return;
                pixbuf = pixbuf.scale_simple (size, size, Gdk.InterpType.BILINEAR);
                icon_img.set_from_pixbuf (pixbuf);
            } catch (Error e) {}
        }

        private Gtk.Box build_rating (double r) {
            var box  = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 1);
            int stars = (int)Math.round (r / 20.0);
            for (int i = 1; i <= 5; i++) {
                var star = new Gtk.Label (i <= stars ? "★" : "☆");
                star.css_classes = { "rating-star" };
                box.append (star);
            }
            return box;
        }
    }

    // ────────────────────────────────────────────────────────────
    //  SizeBar — prosty pasek rozmiaru (0.5)
    //  Używany w InstalledView do wizualizacji rozmiaru paczki
    //  względem największej zainstalowanej paczki.
    // ────────────────────────────────────────────────────────────

    public class SizeBar : Gtk.Box {

        private Gtk.ProgressBar bar;
        private Gtk.Label       label;

        public SizeBar (int64 size_bytes, int64 max_bytes) {
            Object (orientation: Gtk.Orientation.HORIZONTAL, spacing: 6);

            bar = new Gtk.ProgressBar ();
            bar.valign  = Gtk.Align.CENTER;
            bar.hexpand = true;
            bar.fraction = max_bytes > 0 ? (double)size_bytes / (double)max_bytes : 0.0;
            bar.add_css_class ("size-bar");
            append (bar);

            label = new Gtk.Label (format_bytes (size_bytes));
            label.add_css_class ("dim-label");
            label.width_chars = 8;
            label.halign      = Gtk.Align.END;
            append (label);
        }

        private string format_bytes (int64 b) {
            if (b <= 0)                 return "—";
            if (b < 1024)              return "%lld B".printf (b);
            if (b < 1024 * 1024)       return "%.0f K".printf (b / 1024.0);
            if (b < 1024 * 1024 * 1024) return "%.1f M".printf (b / (1024.0 * 1024));
            return "%.1f G".printf (b / (1024.0 * 1024 * 1024));
        }
    }

    // ────────────────────────────────────────────────────────────
    //  PackageDetails — full detail page (rozbudowa 0.5)
    // ────────────────────────────────────────────────────────────

    public class OldPackageDetails : Gtk.Box {

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

        public OldPackageDetails () {
            Object (orientation: Gtk.Orientation.VERTICAL, spacing: 0);
            build_ui ();
        }

        private void build_ui () {
            var scroll = new Gtk.ScrolledWindow ();
            scroll.hscrollbar_policy = Gtk.PolicyType.NEVER;
            scroll.vexpand = true;

            var root = new Gtk.Box (Gtk.Orientation.VERTICAL, 0);

            var header_box = new Gtk.Box (Gtk.Orientation.VERTICAL, 12);
            header_box.css_classes  = { "detail-header" };
            header_box.margin_start = 24;
            header_box.margin_end   = 24;
            header_box.margin_top   = 16;
            header_box.margin_bottom= 16;

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

            action_btn = new Gtk.Button ();
            action_btn.halign = Gtk.Align.START;
            action_btn.valign = Gtk.Align.CENTER;
            action_btn.clicked.connect (on_action_clicked);
            info_col.append (action_btn);

            progress = new Gtk.ProgressBar ();
            progress.pulse_step = 0.05;
            progress.visible    = false;
            info_col.append (progress);

            top_row.append (info_col);
            header_box.append (top_row);
            root.append (header_box);

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
                    action_btn.label     = _pkg.status == PackageStatus.INSTALLING ? "Installing…" : "Removing…";
                    action_btn.sensitive = false;
                    progress.visible     = true;
                    GLib.Timeout.add (80, () => {
                        progress.pulse ();
                        return _pkg.status == PackageStatus.INSTALLING
                            || _pkg.status == PackageStatus.REMOVING;
                    });
                    break;
                default:
                    action_btn.label     = "Broken";
                    action_btn.sensitive = false;
                    break;
            }
        }

        private void on_action_clicked () {
    // ────────────────────────────────────────────────────────────
    //  ProgressTerminal — scrolling log output during install (0.6)
    //  Shows real-time output from `hammer install` subprocess.
    // ────────────────────────────────────────────────────────────

    public class ProgressTerminal : Gtk.Box {

        private Gtk.TextView  text_view;
        private Gtk.TextBuffer buffer;
        private Gtk.ProgressBar progress_bar;
        private Gtk.Label   status_lbl;
        private bool        _active = false;

        public signal void operation_finished (bool success);

        public ProgressTerminal () {
            Object (orientation: Gtk.Orientation.VERTICAL, spacing: 0);

            // Status bar
            var status_bar = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 8);
            status_bar.margin_start  = 12;
            status_bar.margin_end    = 12;
            status_bar.margin_top    = 8;
            status_bar.margin_bottom = 4;

            status_lbl = new Gtk.Label ("Ready");
            status_lbl.halign  = Gtk.Align.START;
            status_lbl.hexpand = true;
            status_bar.append (status_lbl);
            append (status_bar);

            // Progress bar
            progress_bar = new Gtk.ProgressBar ();
            progress_bar.margin_start  = 12;
            progress_bar.margin_end    = 12;
            progress_bar.margin_bottom = 4;
            progress_bar.visible       = false;
            append (progress_bar);

            // Terminal text view
            var scroll = new Gtk.ScrolledWindow ();
            scroll.hscrollbar_policy = Gtk.PolicyType.AUTOMATIC;
            scroll.vexpand = true;
            scroll.min_content_height = 200;

            buffer    = new Gtk.TextBuffer (null);
            text_view = new Gtk.TextView.with_buffer (buffer);
            text_view.editable       = false;
            text_view.cursor_visible = false;
            text_view.monospace      = true;
            text_view.add_css_class ("terminal-view");
            text_view.left_margin    = 10;
            text_view.right_margin   = 10;
            text_view.top_margin     = 6;
            text_view.bottom_margin  = 6;

            scroll.child = text_view;
            append (scroll);
        }

        public bool active { get { return _active; } }

        public void start (string operation) {
            _active = true;
            buffer.set_text ("");
            status_lbl.label    = operation;
            progress_bar.visible = true;
            progress_bar.pulse_step = 0.05;
            // Pulse every 150ms while active
            GLib.Timeout.add (150, () => {
                if (!_active) return false;
                progress_bar.pulse ();
                return true;
            });
        }

        public void append_line (string line) {
            var end = buffer.get_end_iter ();
            buffer.insert (ref end, line + "\n", -1);
            // Scroll to bottom
            var adj = text_view.get_vadjustment ();
            if (adj != null) {
                GLib.Idle.add (() => {
                    adj.value = adj.upper - adj.page_size;
                    return false;
                });
            }
        }

        public void finish (bool success, string msg) {
            _active = false;
            progress_bar.visible  = false;
            progress_bar.fraction = success ? 1.0 : 0.0;
            status_lbl.label = msg;
            operation_finished (success);
        }

        /// Run `pkexec hammer <args>` as a subprocess and stream output here.
        public async void run_hammer (string[] args) {
            start ("Running: hammer %s".printf (string.joinv (" ", args)));
            try {
                // Prepend pkexec so install/remove have root rights
                string[] argv = new string[args.length + 2];
                argv[0] = "pkexec";
                argv[1] = "hammer";
                for (int i = 0; i < args.length; i++) argv[i + 2] = args[i];

                var sub = new GLib.Subprocess.newv (
                    argv,
                    GLib.SubprocessFlags.STDOUT_PIPE |
                    GLib.SubprocessFlags.STDERR_MERGE
                );

                var stream = new GLib.DataInputStream (sub.get_stdout_pipe ());
                while (true) {
                    var line = yield stream.read_line_async (GLib.Priority.DEFAULT, null);
                    if (line == null) break;
                    append_line (line);
                }
                yield sub.wait_async (null);
                bool ok = sub.get_exit_status () == 0;
                finish (ok, ok ? "Completed successfully." : "Failed — see output above.");
            } catch (Error e) {
                append_line ("Error: %s".printf (e.message));
                finish (false, "Error launching hammer: %s".printf (e.message));
            }
        }
    }

    // ────────────────────────────────────────────────────────────
    //  OperationQueue — serialises concurrent install/remove ops (0.5)
    //  Prevents multiple pkexec prompts and race conditions when
    //  the user clicks multiple Install/Remove buttons rapidly.
    // ────────────────────────────────────────────────────────────

    public class OperationQueue : GLib.Object {

        public signal void started  (string description);
        public signal void finished (string description, bool success);
        public signal void progress (string line);

        private struct Operation {
            string description;
            string[] hammer_args;
        }

        private Queue<Operation?> _queue    = new Queue<Operation?> ();
        private bool              _running  = false;
        private ProgressTerminal? _terminal = null;

        public OperationQueue (ProgressTerminal? terminal = null) {
            _terminal = terminal;
        }

        /// Enqueue an operation. Returns false if an identical op is already queued.
        public bool enqueue (string description, string[] hammer_args) {
            // Dedup: skip if same args already pending
            var head = _queue.peek_head ();
            if (head != null && head.description == description) return false;

            Operation op = { description, hammer_args };
            _queue.push_tail (op);

            if (!_running) {
                process_next.begin ();
            }
            return true;
        }

        private async void process_next () {
            _running = true;
            while (!_queue.is_empty ()) {
                Operation? op = _queue.pop_head ();
                if (op == null) break;

                started (op.description);

                if (_terminal != null) {
                    _terminal.run_hammer.begin (op.hammer_args, (obj, res) => {
                        _terminal.run_hammer.end (res);
                        finished (op.description, true); // result handled by terminal signals
                        Idle.add (() => { process_next.begin (); return false; });
                    });
                    // Wait for terminal to finish via its signal
                    bool done = false;
                    ulong sid = _terminal.operation_finished.connect ((success) => {
                        done = true;
                        finished (op.description, success);
                    });
                    // Spin until done
                    while (!done) {
                        Idle.add (() => false);
                        yield;
                    }
                    _terminal.disconnect (sid);
                } else {
                    // No terminal — run silently
                    try {
                        string[] argv = new string[op.hammer_args.length + 2];
                        argv[0] = "pkexec"; argv[1] = "hammer";
                        for (int i = 0; i < op.hammer_args.length; i++)
                            argv[i+2] = op.hammer_args[i];
                        var sub = new GLib.Subprocess.newv (
                            argv,
                            GLib.SubprocessFlags.STDOUT_PIPE |
                            GLib.SubprocessFlags.STDERR_MERGE);
                        yield sub.wait_async (null);
                        bool ok = sub.get_exit_status () == 0;
                        finished (op.description, ok);
                    } catch (Error e) {
                        finished (op.description, false);
                    }
                }
            }
            _running = false;
        }

        public bool is_busy   { get { return _running; } }
        public uint queue_len { get { return _queue.get_length (); } }
    }

    // ────────────────────────────────────────────────────────────
    //  HeroBanner — featured/hero card for FeaturedView (0.6)
    //  Displays a large gradient banner with screenshot + CTA.
    // ────────────────────────────────────────────────────────────

    public class HeroBanner : Gtk.Box {

        public signal void install_clicked (PackageInfo pkg);
        public signal void details_clicked (PackageInfo pkg);

        public HeroBanner (PackageInfo pkg) {
            Object (orientation: Gtk.Orientation.HORIZONTAL, spacing: 0);
            css_classes = { "hero-card" };
            margin_start  = 20;
            margin_end    = 20;
            margin_top    = 12;
            margin_bottom = 8;

            // Left: icon + text
            var left = new Gtk.Box (Gtk.Orientation.HORIZONTAL, 20);
            left.hexpand = true;
            left.valign  = Gtk.Align.CENTER;

            var icon = new Gtk.Image.from_icon_name (
                pkg.icon_name != "" ? pkg.icon_name : "package-x-generic");
            icon.pixel_size = 88;
            icon.css_classes = { "pkg-icon-lg" };
            left.append (icon);

            var text_col = new Gtk.Box (Gtk.Orientation.VERTICAL, 6);
            text_col.valign = Gtk.Align.CENTER;

            var name_lbl = new Gtk.Label (
                "<span size='x-large' weight='bold'>%s</span>".printf (
                    GLib.Markup.escape_text (pkg.name)));
            name_lbl.use_markup = true;
            name_lbl.halign     = Gtk.Align.START;
            text_col.append (name_lbl);

            if (pkg.summary != "") {
                var sum_lbl = new Gtk.Label (pkg.summary);
                sum_lbl.halign = Gtk.Align.START;
                sum_lbl.wrap   = true;
                sum_lbl.add_css_class ("dim-label");
                text_col.append (sum_lbl);
            }

            // Category chip
            if (pkg.category != "") {
                var cat_lbl = new Gtk.Label (pkg.category);
                cat_lbl.css_classes = { "category-chip" };
                cat_lbl.halign      = Gtk.Align.START;
                cat_lbl.margin_top  = 4;
                text_col.append (cat_lbl);
            }

            left.append (text_col);
            append (left);

            // Right: buttons
            var btn_col = new Gtk.Box (Gtk.Orientation.VERTICAL, 8);
            btn_col.valign = Gtk.Align.CENTER;
            btn_col.halign = Gtk.Align.END;

            var install_btn = new Gtk.Button.with_label (
                pkg.is_installed () ? "✔ Installed" : "Install");
            install_btn.css_classes   = pkg.is_installed ()
                ? new string[]{ "suggested-action" }
                : new string[]{ "suggested-action" };
            install_btn.sensitive     = !pkg.is_installed ();
            install_btn.width_request = 120;
            install_btn.clicked.connect (() => install_clicked (pkg));
            btn_col.append (install_btn);

            var details_btn = new Gtk.Button.with_label ("Details");
            details_btn.css_classes   = { "flat" };
            details_btn.width_request = 120;
            details_btn.clicked.connect (() => details_clicked (pkg));
            btn_col.append (details_btn);

            append (btn_col);
        }
    }

} // end namespace HammerStore

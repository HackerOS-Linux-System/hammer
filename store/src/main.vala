using Gtk;
using GLib;

// ─────────────────────────────────────────────────────────────
//  Package model
// ─────────────────────────────────────────────────────────────

public class HammerPackage : Object {
    public string name         { get; set; default = ""; }
    public string version      { get; set; default = ""; }
    public string architecture { get; set; default = ""; }
    public string summary      { get; set; default = ""; }
    public string section      { get; set; default = ""; }
    public int64  size_bytes   { get; set; default = 0;  }
    public bool   installed    { get; set; default = false; }
    public string repo         { get; set; default = ""; }

    public string size_human {
        owned get {
            if (size_bytes >= 1073741824)
                return "%.1f GiB".printf ((double) size_bytes / 1073741824.0);
            else if (size_bytes >= 1048576)
                return "%.1f MiB".printf ((double) size_bytes / 1048576.0);
            else if (size_bytes >= 1024)
                return "%.0f KiB".printf ((double) size_bytes / 1024.0);
            return "%lld B".printf (size_bytes);
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  PackageLoader
// ─────────────────────────────────────────────────────────────

public class PackageLoader : Object {
    public signal void packages_loaded (GLib.ListStore store);
    public signal void load_error      (string message);

    public void load_async () {
        new Thread<void> ("pkg-loader", () => {
            var store = new GLib.ListStore (typeof (HammerPackage));
            load_from_hammer (store);
            Idle.add (() => {
                packages_loaded (store);
                return Source.REMOVE;
            });
        });
    }

    private void load_from_hammer (GLib.ListStore store) {
        var map = new HashTable<string, HammerPackage> (str_hash, str_equal);
        run_hammer_list ("--installed", map, true);
        run_hammer_list ("",            map, false);

        var keys = new GLib.List<string> ();
        map.foreach ((k, _) => { keys.prepend (k); });
        keys.sort (strcmp);
        foreach (var k in keys) {
            store.append (map.get (k));
        }
    }

    private void run_hammer_list (string extra_flag,
                                   HashTable<string, HammerPackage> map,
                                   bool is_installed_pass)
    {
        string[] argv = extra_flag.length > 0
            ? new string[] { "/usr/bin/hammer", "list", extra_flag }
            : new string[] { "/usr/bin/hammer", "list" };

        try {
            string stdout_data, stderr_data;
            int    exit_status;
            Process.spawn_sync (null, argv, null, SpawnFlags.SEARCH_PATH,
                                null, out stdout_data, out stderr_data, out exit_status);
            parse_list_output (stdout_data, map, is_installed_pass);
        } catch (SpawnError e) {
            warning ("hammer list failed: %s", e.message);
        }
    }

    private void parse_list_output (string output,
                                     HashTable<string, HammerPackage> map,
                                     bool is_installed_pass)
    {
        foreach (var raw_line in output.split ("\n")) {
            var line = raw_line.strip ();
            if (line.length == 0) continue;

            bool inst = line.contains ("\xe2\x9c\x94")
                     || line.has_prefix ("  \xe2\x9c\x94")
                     || line.has_prefix ("\xe2\x9c\x94");

            if (is_installed_pass && !inst) continue;
            if (!is_installed_pass && inst) continue;

            // Find first alphabetic character
            int start = 0;
            while (start < (int) line.length) {
                unichar c;
                int old = start;
                line.get_next_char (ref start, out c);
                if (c.isalpha ()) { start = old; break; }
            }
            var rest  = line.substring (start).strip ();
            var parts = rest.split_set (" \t", 4);
            if (parts.length < 1) continue;

            var na       = parts[0].split (".", 2);
            var pkg_name = na[0];
            var arch_s   = na.length > 1 ? na[1] : "";
            var version_s= parts.length > 1 ? parts[1].strip () : "";
            var repo_s   = parts.length > 2 ? parts[2].strip () : "";

            if (pkg_name.length == 0) continue;
            if (map.contains (pkg_name) && !is_installed_pass) continue;

            var pkg = new HammerPackage ();
            pkg.name         = pkg_name;
            pkg.version      = version_s;
            pkg.architecture = arch_s;
            pkg.repo         = repo_s;
            pkg.installed    = inst;
            map.set (pkg_name, pkg);
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  PackageRow
// ─────────────────────────────────────────────────────────────

public class PackageRow : ListBoxRow {
    private HammerPackage _pkg;

    public PackageRow (HammerPackage p) {
        _pkg = p;
        margin_start = 8; margin_end = 8;
        margin_top   = 4; margin_bottom = 4;

        var grid = new Grid () { column_spacing = 12, row_spacing = 2 };

        var name_lbl = new Label (p.name) {
            xalign = 0f, halign = Align.START, hexpand = true };
        name_lbl.add_css_class ("heading");
        if (p.installed) name_lbl.add_css_class ("accent");

        var ver_lbl = new Label (p.version) { xalign = 0f, halign = Align.START };
        ver_lbl.add_css_class ("dim-label");
        ver_lbl.add_css_class ("monospace");

        var repo_lbl = new Label (p.repo) { xalign = 1f, halign = Align.END, hexpand = true };
        repo_lbl.add_css_class ("dim-label");

        var badge = new Label (p.installed ? "installed" : "") {
            xalign = 1f, halign = Align.END };
        if (p.installed) {
            badge.add_css_class ("success");
            badge.add_css_class ("caption");
        }

        grid.attach (name_lbl, 0, 0, 1, 1);
        grid.attach (ver_lbl,  0, 1, 1, 1);
        grid.attach (repo_lbl, 1, 0, 1, 1);
        grid.attach (badge,    1, 1, 1, 1);
        set_child (grid);
    }

    public HammerPackage get_package () { return _pkg; }
}

// ─────────────────────────────────────────────────────────────
//  PackageDetail
// ─────────────────────────────────────────────────────────────

public class PackageDetail : Box {
    private Label  name_label;
    private Label  version_label;
    private Label  arch_label;
    private Label  repo_label;
    private Label  summary_label;
    private Label  installed_label;
    private Button action_button;
    private HammerPackage? _current = null;

    public signal void action_requested (HammerPackage pkg, bool do_install);

    public PackageDetail () {
        orientation = Orientation.VERTICAL; spacing = 16;
        margin_start = 24; margin_end = 24;
        margin_top   = 24; margin_bottom = 24;
        halign = Align.FILL; valign = Align.START;

        var icon = new Image.from_icon_name ("system-software-install") {
            pixel_size = 64, halign = Align.CENTER };
        append (icon);

        name_label = new Label ("") {
            halign = Align.CENTER, wrap = true,
            justify = Justification.CENTER };
        name_label.add_css_class ("title-1");
        append (name_label);

        var info_box = new Box (Orientation.VERTICAL, 4) { halign = Align.CENTER };
        version_label   = make_dim ("");
        arch_label      = make_dim ("");
        repo_label      = make_dim ("");
        installed_label = make_dim ("");
        info_box.append (version_label);
        info_box.append (arch_label);
        info_box.append (repo_label);
        info_box.append (installed_label);
        append (info_box);

        summary_label = new Label ("") {
            halign = Align.CENTER, wrap = true,
            justify = Justification.CENTER, max_width_chars = 38 };
        summary_label.add_css_class ("body");
        append (summary_label);

        action_button = new Button () {
            halign = Align.CENTER, width_request = 200, sensitive = false };
        action_button.add_css_class ("pill");
        action_button.clicked.connect (on_action);
        append (action_button);
    }

    private Label make_dim (string text) {
        var l = new Label (text) { halign = Align.CENTER };
        l.add_css_class ("dim-label");
        l.add_css_class ("caption");
        return l;
    }

    public void show_package (HammerPackage pkg) {
        _current = pkg;
        name_label.set_label (pkg.name);
        version_label.set_label   ("Version: " + pkg.version);
        arch_label.set_label      ("Arch: "    + pkg.architecture);
        repo_label.set_label      ("Repo: "    + pkg.repo);
        installed_label.set_label (pkg.installed
            ? "\xe2\x97\x8f Installed"
            : "\xe2\x97\x8b Not installed");

        installed_label.remove_css_class ("success");
        installed_label.remove_css_class ("dim-label");
        if (pkg.installed) installed_label.add_css_class ("success");
        else               installed_label.add_css_class ("dim-label");

        summary_label.set_label (
            pkg.summary.length > 0 ? pkg.summary : "(no description)");

        action_button.sensitive = true;
        if (pkg.installed) {
            action_button.set_label ("Remove");
            action_button.remove_css_class ("suggested-action");
            action_button.add_css_class    ("destructive-action");
        } else {
            action_button.set_label ("Install");
            action_button.remove_css_class ("destructive-action");
            action_button.add_css_class    ("suggested-action");
        }
    }

    public void clear () {
        _current = null;
        name_label.set_label ("");      version_label.set_label ("");
        arch_label.set_label ("");      repo_label.set_label ("");
        summary_label.set_label ("");   installed_label.set_label ("");
        action_button.set_label ("Select a package");
        action_button.sensitive = false;
        action_button.remove_css_class ("suggested-action");
        action_button.remove_css_class ("destructive-action");
    }

    private void on_action () {
        if (_current == null) return;
        action_requested (_current, !_current.installed);
    }
}

// ─────────────────────────────────────────────────────────────
//  TerminalDialog
// ─────────────────────────────────────────────────────────────

public class TerminalDialog : Dialog {
    private TextBuffer buf;
    private TextView   text_view;
    private Button     close_btn;

    public TerminalDialog (Window parent, string title_str) {
        set_transient_for (parent);
        set_modal (true);
        set_title (title_str);
        set_default_size (680, 420);

        buf       = new TextBuffer (null);
        text_view = new TextView.with_buffer (buf) {
            editable       = false,
            cursor_visible = false,
            monospace      = true,
            wrap_mode      = WrapMode.CHAR,
            margin_start   = 10, margin_end   = 10,
            margin_top     = 10, margin_bottom = 10
        };

        var scroll = new ScrolledWindow () {
            vexpand           = true,
            hexpand           = true,
            hscrollbar_policy = PolicyType.NEVER,
            child             = text_view
        };

        close_btn = new Button.with_label ("Close") {
            sensitive  = false,
            halign     = Align.END,
            margin_top = 8
        };
        close_btn.add_css_class ("suggested-action");
        close_btn.add_css_class ("pill");
        close_btn.clicked.connect (() => close ());

        var box = get_content_area ();
        box.orientation   = Orientation.VERTICAL;
        box.spacing       = 0;
        box.margin_start  = 12; box.margin_end   = 12;
        box.margin_top    = 12; box.margin_bottom = 12;
        box.append (scroll);
        box.append (close_btn);
    }

    public void append_text (string text) {
        Idle.add (() => {
            TextIter end;
            buf.get_end_iter (out end);
            buf.insert (ref end, text, -1);
            TextIter iter;
            buf.get_end_iter (out iter);
            text_view.scroll_to_iter (iter, 0.0, false, 0.0, 0.0);
            return Source.REMOVE;
        });
    }

    public void set_done () {
        Idle.add (() => {
            close_btn.sensitive = true;
            return Source.REMOVE;
        });
    }
}

// ─────────────────────────────────────────────────────────────
//  HammerStoreWindow
// ─────────────────────────────────────────────────────────────

public class HammerStoreWindow : ApplicationWindow {
    private SearchEntry    search_entry;
    private ListBox        pkg_list;
    private PackageDetail  detail_panel;
    private Spinner        spinner;
    private Stack          main_stack;
    private Label          status_label;
    private GLib.ListStore _all_packages;
    private string         _filter = "";

    public HammerStoreWindow (Gtk.Application app) {
        Object (application: app, title: "Hammer Store",
                default_width: 980, default_height: 660);
        build_ui ();
        load_packages ();
    }

    // ── UI ────────────────────────────────────────────────────

    private void build_ui () {
        var header = new HeaderBar ();

        search_entry = new SearchEntry () {
            placeholder_text = "Search packages\xe2\x80\xa6",
            width_chars = 30
        };
        search_entry.search_changed.connect (() => {
            _filter = search_entry.get_text ().down ();
            pkg_list.invalidate_filter ();
            update_status ();
        });
        header.set_title_widget (search_entry);

        var sync_btn = new Button.with_label ("Sync") {
            tooltip_text = "Run hammer sync"
        };
        sync_btn.add_css_class ("suggested-action");
        sync_btn.add_css_class ("pill");
        sync_btn.clicked.connect (run_hammer_sync);
        header.pack_start (sync_btn);

        var refresh_btn = new Button.from_icon_name ("view-refresh-symbolic") {
            tooltip_text = "Reload package list"
        };
        refresh_btn.clicked.connect (load_packages);
        header.pack_end (refresh_btn);
        set_titlebar (header);

        // Loading view
        spinner = new Spinner () { spinning = true };
        var loading_box = new Box (Orientation.VERTICAL, 12) {
            halign = Align.CENTER, valign = Align.CENTER };
        loading_box.append (spinner);
        loading_box.append (new Label ("Loading packages\xe2\x80\xa6"));

        // Package list
        pkg_list = new ListBox () {
            selection_mode  = SelectionMode.SINGLE,
            show_separators = true
        };
        pkg_list.set_filter_func (filter_func);
        pkg_list.row_selected.connect (on_row_selected);
        var list_scroll = new ScrolledWindow () {
            vexpand           = true,
            hscrollbar_policy = PolicyType.NEVER,
            child             = pkg_list,
            width_request     = 420
        };

        // Detail panel
        detail_panel = new PackageDetail ();
        detail_panel.action_requested.connect (on_action_requested);
        detail_panel.clear ();
        var detail_scroll = new ScrolledWindow () {
            vexpand           = true,
            hscrollbar_policy = PolicyType.NEVER,
            child             = detail_panel,
            width_request     = 320
        };

        // Status bar
        status_label = new Label ("") {
            xalign        = 0f,
            margin_start  = 8,
            margin_top    = 4,
            margin_bottom = 4
        };
        status_label.add_css_class ("dim-label");
        status_label.add_css_class ("caption");

        var paned = new Paned (Orientation.HORIZONTAL) {
            start_child = list_scroll,
            end_child   = detail_scroll,
            position    = 480,
            wide_handle = true
        };

        var content_box = new Box (Orientation.VERTICAL, 0);
        content_box.append (paned);
        content_box.append (status_label);

        main_stack = new Stack ();
        main_stack.add_named (loading_box, "loading");
        main_stack.add_named (content_box, "content");
        main_stack.set_visible_child_name ("loading");
        set_child (main_stack);
    }

    // ── Load packages ─────────────────────────────────────────

    private void load_packages () {
        main_stack.set_visible_child_name ("loading");
        spinner.spinning = true;

        var loader = new PackageLoader ();
        loader.packages_loaded.connect ((store) => {
            _all_packages = store;
            repopulate_list ();
            spinner.spinning = false;
            main_stack.set_visible_child_name ("content");
            update_status ();
        });
        loader.load_error.connect ((msg) => {
            spinner.spinning = false;
            show_error ("Failed to load packages", msg);
        });
        loader.load_async ();
    }

    private void repopulate_list () {
        ListBoxRow? row = pkg_list.get_row_at_index (0);
        while (row != null) {
            pkg_list.remove (row);
            row = pkg_list.get_row_at_index (0);
        }
        uint n = _all_packages.get_n_items ();
        for (uint i = 0; i < n; i++) {
            var pkg = (HammerPackage) _all_packages.get_item (i);
            pkg_list.append (new PackageRow (pkg));
        }
        pkg_list.invalidate_filter ();
        detail_panel.clear ();
    }

    private void update_status () {
        if (_all_packages == null) return;
        uint total   = _all_packages.get_n_items ();
        uint visible = 0;
        for (int i = 0; ; i++) {
            var r = pkg_list.get_row_at_index (i);
            if (r == null) break;
            if (r.visible) visible++;
        }
        status_label.set_label (_filter.length > 0
            ? "%u of %u packages".printf (visible, total)
            : "%u packages".printf (total));
    }

    // ── Filter ────────────────────────────────────────────────

    private bool filter_func (ListBoxRow list_row) {
        if (_filter.length == 0) return true;
        var pkg = ((PackageRow) list_row).get_package ();
        return pkg.name.down ().contains (_filter)
            || pkg.summary.down ().contains (_filter)
            || pkg.repo.down ().contains (_filter);
    }

    // ── Row selection ─────────────────────────────────────────

    private void on_row_selected (ListBoxRow? list_row) {
        if (list_row == null) { detail_panel.clear (); return; }
        detail_panel.show_package (((PackageRow) list_row).get_package ());
    }

    // ── Install / Remove ──────────────────────────────────────

    private void on_action_requested (HammerPackage pkg, bool do_install) {
        string op    = do_install ? "install" : "remove";
        string title = do_install
            ? "Installing %s".printf (pkg.name)
            : "Removing %s".printf (pkg.name);
        var dlg = new TerminalDialog (this, title);
        dlg.present ();
        run_hammer_command ({ "/usr/bin/hammer", op, "-y", pkg.name }, dlg);
    }

    // ── Sync ─────────────────────────────────────────────────

    private void run_hammer_sync () {
        var dlg = new TerminalDialog (this, "hammer sync");
        dlg.present ();
        run_hammer_command ({ "/usr/bin/hammer", "sync" }, dlg);
    }

    // ── Command runner ────────────────────────────────────────
    //
    //  FIX: replaced UnixInputStream (needs gio-unix-2.0) and
    //  Posix.waitpid (needs posix pkg) with pure GLib IOChannel
    //  + ChildWatch.add — no extra pkg dependencies needed.

    private void run_hammer_command (string[] argv, TerminalDialog dlg) {
        var argv_copy = argv;

        new Thread<void> ("hammer-cmd", () => {
            try {
                Pid child_pid;
                int stdout_fd;
                int stderr_fd;

                Process.spawn_async_with_pipes (
                    null,
                    argv_copy,
                    null,
                    SpawnFlags.SEARCH_PATH | SpawnFlags.DO_NOT_REAP_CHILD,
                    null,
                    out child_pid,
                    null,
                    out stdout_fd,
                    out stderr_fd
                );

                // Read stdout via IOChannel (pure GLib, no posix/gio-unix needed)
                drain_channel (stdout_fd, dlg);
                drain_channel (stderr_fd, dlg);

                // Reap child using GLib ChildWatch (runs in main loop)
                Pid pid_copy = child_pid;
                Idle.add (() => {
                    ChildWatch.add (pid_copy, (pid, _status) => {
                        Process.close_pid (pid);
                    });
                    return Source.REMOVE;
                });

                dlg.append_text ("\n\xe2\x94\x80\xe2\x94\x80 Done. \xe2\x94\x80\xe2\x94\x80\n");
                dlg.set_done ();

                Idle.add (() => { load_packages (); return Source.REMOVE; });

            } catch (Error e) {
                dlg.append_text ("Error: %s\n".printf (e.message));
                dlg.set_done ();
            }
        });
    }

    // Drain a file descriptor line-by-line into the terminal dialog.
    // Uses GLib.IOChannel — available without any extra .vapi.
    private static void drain_channel (int fd, TerminalDialog dlg) {
        try {
            var chan = new IOChannel.unix_new (fd);
            // Set encoding to null (binary / raw bytes) to avoid encoding errors
            chan.set_encoding (null);
            chan.set_buffered (true);

            string line;
            size_t length;
            IOStatus st;

            // Switch to UTF-8 for line reading
            chan.set_encoding ("UTF-8");

            while (true) {
                st = chan.read_line (out line, out length, null);
                if (st == IOStatus.NORMAL && line != null) {
                    dlg.append_text (line);
                } else {
                    break;
                }
            }
        } catch (Error e) {
            // EOF / broken pipe — normal at process end
        }
    }

    // ── Error dialog ──────────────────────────────────────────
    //
    //  FIX: MessageDialog is deprecated since GTK 4.10.
    //  Replaced with AlertDialog (GTK >= 4.10) with a fallback
    //  to a plain Dialog for older GTK versions.

    private void show_error (string title_str, string msg_str) {
        // AlertDialog is available since GTK 4.10 — use it when possible.
        // We check at compile time via Vala's version conditional.
#if GTK_4_10
        var alert = new AlertDialog (title_str);
        alert.set_detail (msg_str);
        alert.set_buttons ({ "OK" });
        alert.set_default_button (0);
        alert.choose.begin (this, null, (obj, res) => {
            try { alert.choose.end (res); } catch { }
        });
#else
        // Fallback: plain Dialog with a label (no deprecated MessageDialog)
        var err_dlg  = new Dialog ();
        err_dlg.set_transient_for (this);
        err_dlg.set_modal (true);
        err_dlg.set_title (title_str);
        err_dlg.set_default_size (360, 160);

        var lbl = new Label (msg_str) {
            wrap            = true,
            max_width_chars = 48,
            margin_start    = 16, margin_end    = 16,
            margin_top      = 16, margin_bottom = 16
        };
        var ok_btn = new Button.with_label ("OK") {
            halign     = Align.CENTER,
            margin_top = 8,
            margin_bottom = 12
        };
        ok_btn.add_css_class ("suggested-action");
        ok_btn.clicked.connect (() => err_dlg.close ());

        var box = err_dlg.get_content_area ();
        box.append (lbl);
        box.append (ok_btn);
        err_dlg.present ();
#endif
    }
}

// ─────────────────────────────────────────────────────────────
//  Application + entry point
// ─────────────────────────────────────────────────────────────

public class HammerStoreApp : Gtk.Application {
    public HammerStoreApp () {
        Object (application_id: "org.hackerOS.HammerStore",
                flags: ApplicationFlags.FLAGS_NONE);
    }
    protected override void activate () {
        new HammerStoreWindow (this).present ();
    }
}

int main (string[] args) {
    return new HammerStoreApp ().run (args);
}

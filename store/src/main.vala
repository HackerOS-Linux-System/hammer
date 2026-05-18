using Gtk;
using GLib;

// ─────────────────────────────────────────────────────────────
//  Package model
// ─────────────────────────────────────────────────────────────

public class HammerPackage : Object {
    public string  name        { get; set; }
    public string  version     { get; set; }
    public string  architecture{ get; set; }
    public string  summary     { get; set; }
    public string  section     { get; set; }
    public int64   size_bytes  { get; set; }
    public bool    installed   { get; set; }
    public string  repo        { get; set; }

    public string size_human {
        get {
            if (size_bytes >= 1073741824)
                return "%.1f GiB".printf(size_bytes / 1073741824.0);
            else if (size_bytes >= 1048576)
                return "%.1f MiB".printf(size_bytes / 1048576.0);
            else if (size_bytes >= 1024)
                return "%.0f KiB".printf(size_bytes / 1024.0);
            return "%lld B".printf(size_bytes);
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  PackageLoader — runs "hammer list" and parses output
// ─────────────────────────────────────────────────────────────

public class PackageLoader : Object {
    public signal void packages_loaded (ListStore store);
    public signal void load_error      (string message);

    public void load_async () {
        new Thread<void> ("pkg-loader", () => {
            var store = new ListStore (typeof (HammerPackage));
            try {
                string stdout_data, stderr_data;
                int exit_status;
                // hammer list --installed outputs: mark name.arch  version  repo
                Process.spawn_command_line_sync (
                    "/usr/bin/hammer list --installed",
                    out stdout_data, out stderr_data, out exit_status
                );
                parse_hammer_list (stdout_data, store, true);

                // Also load available packages (uninstalled)
                Process.spawn_command_line_sync (
                    "/usr/bin/hammer list",
                    out stdout_data, out stderr_data, out exit_status
                );
                parse_hammer_list (stdout_data, store, false);

            } catch (Error e) {
                Idle.add (() => {
                    load_error (e.message);
                    return Source.REMOVE;
                });
                return;
            }
            Idle.add (() => {
                packages_loaded (store);
                return Source.REMOVE;
            });
        });
    }

    private void parse_hammer_list (string output, ListStore store, bool installed_filter) {
        foreach (var line in output.split ("\n")) {
            line = line.strip ();
            if (line.length == 0) continue;
            // Format: [  ✔] name.arch  version  repo
            bool installed = line.has_prefix ("  ✔") || line.has_prefix ("✔");
            if (installed_filter && !installed) continue;
            if (!installed_filter && installed) continue;

            // Strip leading mark (3 chars + space)
            string rest = line.length > 4 ? line.substring (3).strip () : line;
            // Split on runs of whitespace
            string[] parts = rest.split_set (" \t", 4);
            if (parts.length < 2) continue;

            // parts[0] = name.arch, parts[1] = version, parts[2] = repo (optional)
            string name_arch = parts[0];
            string version   = parts.length > 1 ? parts[1] : "";
            string repo      = parts.length > 2 ? parts[2] : "";

            string[] na = name_arch.split (".", 2);
            string   pkg_name = na[0];
            string   arch     = na.length > 1 ? na[1] : "";

            var pkg = new HammerPackage ();
            pkg.name         = pkg_name;
            pkg.version      = version;
            pkg.architecture = arch;
            pkg.summary      = "";
            pkg.section      = "";
            pkg.size_bytes   = 0;
            pkg.installed    = installed;
            pkg.repo         = repo;
            store.append (pkg);
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  PackageRow — one row in the package list
// ─────────────────────────────────────────────────────────────

[GtkTemplate (ui = "")]
public class PackageRow : ListBoxRow {
    private HammerPackage pkg;

    public PackageRow (HammerPackage p) {
        this.pkg = p;
        set_margin_start (8);
        set_margin_end   (8);
        set_margin_top   (4);
        set_margin_bottom (4);

        var grid = new Grid () {
            column_spacing = 12,
            row_spacing    = 2
        };

        var name_lbl = new Label (p.name) {
            xalign = 0,
            halign = Align.START
        };
        name_lbl.add_css_class ("heading");
        if (p.installed) name_lbl.add_css_class ("accent");

        var ver_lbl = new Label (p.version) {
            xalign = 0,
            halign = Align.START
        };
        ver_lbl.add_css_class ("dim-label");
        ver_lbl.add_css_class ("monospace");

        var repo_lbl = new Label (p.repo) {
            xalign = 1,
            halign = Align.END,
            hexpand = true
        };
        repo_lbl.add_css_class ("dim-label");

        var badge = new Label (p.installed ? "installed" : "") {
            xalign = 1,
            halign = Align.END
        };
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

    public HammerPackage get_package () { return pkg; }
}

// ─────────────────────────────────────────────────────────────
//  PackageDetail — right-hand panel showing package info
// ─────────────────────────────────────────────────────────────

public class PackageDetail : Box {
    private Label  name_label;
    private Label  version_label;
    private Label  arch_label;
    private Label  repo_label;
    private Label  summary_label;
    private Label  installed_label;
    private Button action_button;
    private HammerPackage? current_pkg = null;

    public signal void action_requested (HammerPackage pkg, bool install);

    public PackageDetail () {
        orientation = Orientation.VERTICAL;
        spacing     = 16;
        margin_start  = 24;
        margin_end    = 24;
        margin_top    = 24;
        margin_bottom = 24;
        halign        = Align.FILL;
        valign        = Align.START;

        // Icon placeholder
        var icon = new Image.from_icon_name ("system-software-install") {
            pixel_size = 64,
            halign     = Align.CENTER
        };
        append (icon);

        // Name
        name_label = new Label ("") {
            halign  = Align.CENTER,
            wrap    = true,
            justify = Justification.CENTER
        };
        name_label.add_css_class ("title-1");
        append (name_label);

        // Version / arch / repo in a small grid
        var info_box = new Box (Orientation.VERTICAL, 4) { halign = Align.CENTER };

        version_label   = make_info_label ("");
        arch_label      = make_info_label ("");
        repo_label      = make_info_label ("");
        installed_label = make_info_label ("");

        info_box.append (version_label);
        info_box.append (arch_label);
        info_box.append (repo_label);
        info_box.append (installed_label);
        append (info_box);

        // Summary
        summary_label = new Label ("") {
            halign  = Align.CENTER,
            wrap    = true,
            justify = Justification.CENTER,
            max_width_chars = 38
        };
        summary_label.add_css_class ("body");
        append (summary_label);

        // Action button
        action_button = new Button () {
            halign = Align.CENTER,
            width_request = 200
        };
        action_button.clicked.connect (on_action);
        append (action_button);
    }

    private Label make_info_label (string text) {
        var l = new Label (text) { halign = Align.CENTER };
        l.add_css_class ("dim-label");
        l.add_css_class ("caption");
        return l;
    }

    public void show_package (HammerPackage pkg) {
        current_pkg = pkg;
        name_label.set_label (pkg.name);
        version_label.set_label ("Version: " + pkg.version);
        arch_label.set_label    ("Arch: "    + pkg.architecture);
        repo_label.set_label    ("Repo: "    + pkg.repo);
        installed_label.set_label (pkg.installed ? "● Installed" : "○ Not installed");
        installed_label.remove_css_class ("success");
        installed_label.remove_css_class ("dim-label");
        if (pkg.installed) installed_label.add_css_class ("success");
        else               installed_label.add_css_class ("dim-label");

        summary_label.set_label (pkg.summary.length > 0 ? pkg.summary : "(no description)");

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
        current_pkg = null;
        name_label.set_label ("");
        version_label.set_label ("");
        arch_label.set_label ("");
        repo_label.set_label ("");
        summary_label.set_label ("");
        installed_label.set_label ("");
        action_button.set_label ("Select a package");
        action_button.sensitive = false;
    }

    private void on_action () {
        if (current_pkg == null) return;
        action_requested (current_pkg, !current_pkg.installed);
    }
}

// ─────────────────────────────────────────────────────────────
//  TerminalDialog — shows hammer install/remove output
// ─────────────────────────────────────────────────────────────

public class TerminalDialog : Dialog {
    private TextView  text_view;
    private TextBuffer buf;
    private Button    close_btn;

    public TerminalDialog (Window parent, string title) {
        set_transient_for (parent);
        set_modal (true);
        set_title (title);
        set_default_size (640, 400);

        buf       = new TextBuffer (null);
        text_view = new TextView.with_buffer (buf) {
            editable        = false,
            cursor_visible  = false,
            monospace       = true,
            wrap_mode       = WrapMode.WORD_CHAR,
            margin_start    = 8,
            margin_end      = 8,
            margin_top      = 8,
            margin_bottom   = 8
        };

        var scroll = new ScrolledWindow () {
            vexpand        = true,
            hexpand        = true,
            child          = text_view
        };

        close_btn = new Button.with_label ("Close") {
            sensitive = false,
            halign    = Align.END
        };
        close_btn.add_css_class ("suggested-action");
        close_btn.clicked.connect (() => { close (); });

        var content = get_content_area ();
        content.orientation = Orientation.VERTICAL;
        content.spacing     = 8;
        content.margin_start  = 12;
        content.margin_end    = 12;
        content.margin_top    = 12;
        content.margin_bottom = 12;
        content.append (scroll);
        content.append (close_btn);
    }

    public void append_text (string text) {
        TextIter end_iter;
        buf.get_end_iter (out end_iter);
        buf.insert (ref end_iter, text, -1);
        // Scroll to bottom
        Idle.add (() => {
            TextIter iter;
            buf.get_end_iter (out iter);
            text_view.scroll_to_iter (iter, 0.0, false, 0.0, 0.0);
            return Source.REMOVE;
        });
    }

    public void finish () {
        close_btn.sensitive = true;
    }
}

// ─────────────────────────────────────────────────────────────
//  HammerStoreWindow — main application window
// ─────────────────────────────────────────────────────────────

public class HammerStoreWindow : ApplicationWindow {
    private SearchEntry   search_entry;
    private ListBox       pkg_list;
    private PackageDetail detail_panel;
    private Spinner       spinner;
    private Stack         main_stack;
    private ListStore     all_packages;
    private string        filter_query = "";

    public HammerStoreWindow (Gtk.Application app) {
        Object (application: app,
                title: "Hammer Store",
                default_width: 960,
                default_height: 640);

        build_ui ();
        load_packages ();
    }

    private void build_ui () {
        // ── Header bar ────────────────────────────────────────
        var header = new HeaderBar ();

        search_entry = new SearchEntry () {
            placeholder_text = "Search packages…",
            width_chars      = 28
        };
        search_entry.search_changed.connect (on_search_changed);
        header.set_title_widget (search_entry);

        var refresh_btn = new Button.from_icon_name ("view-refresh-symbolic") {
            tooltip_text = "Refresh package list"
        };
        refresh_btn.clicked.connect (load_packages);
        header.pack_end (refresh_btn);

        var sync_btn = new Button.with_label ("Sync") {
            tooltip_text = "Run hammer sync to refresh index"
        };
        sync_btn.add_css_class ("suggested-action");
        sync_btn.clicked.connect (run_hammer_sync);
        header.pack_end (sync_btn);

        set_titlebar (header);

        // ── Spinner / loading view ────────────────────────────
        var loading_box = new Box (Orientation.VERTICAL, 12) {
            halign = Align.CENTER,
            valign = Align.CENTER
        };
        spinner = new Spinner () { spinning = true };
        var loading_lbl = new Label ("Loading packages…");
        loading_box.append (spinner);
        loading_box.append (loading_lbl);

        // ── Package list ──────────────────────────────────────
        pkg_list = new ListBox () {
            selection_mode = SelectionMode.SINGLE,
            show_separators = true
        };
        pkg_list.set_filter_func (filter_func);
        pkg_list.row_selected.connect (on_row_selected);

        var list_scroll = new ScrolledWindow () {
            vexpand       = true,
            hscrollbar_policy = PolicyType.NEVER,
            child         = pkg_list
        };

        // ── Detail panel ──────────────────────────────────────
        detail_panel = new PackageDetail ();
        detail_panel.action_requested.connect (on_action_requested);
        detail_panel.clear ();

        var detail_scroll = new ScrolledWindow () {
            vexpand       = true,
            hscrollbar_policy = PolicyType.NEVER,
            child         = detail_panel,
            width_request = 300
        };

        // ── Paned layout ──────────────────────────────────────
        var paned = new Paned (Orientation.HORIZONTAL) {
            start_child      = list_scroll,
            end_child        = detail_scroll,
            position         = 460,
            wide_handle      = true
        };

        // ── Stack: loading / content ──────────────────────────
        main_stack = new Stack ();
        main_stack.add_named (loading_box, "loading");
        main_stack.add_named (paned,       "content");
        main_stack.set_visible_child_name ("loading");

        set_child (main_stack);
    }

    // ── Load packages in background thread ────────────────────

    private void load_packages () {
        main_stack.set_visible_child_name ("loading");
        spinner.spinning = true;

        var loader = new PackageLoader ();
        loader.packages_loaded.connect ((store) => {
            all_packages = store;
            repopulate_list ();
            spinner.spinning = false;
            main_stack.set_visible_child_name ("content");
        });
        loader.load_error.connect ((msg) => {
            spinner.spinning = false;
            show_error_dialog ("Failed to load packages", msg);
        });
        loader.load_async ();
    }

    private void repopulate_list () {
        // Remove old rows
        while (true) {
            var row = pkg_list.get_row_at_index (0);
            if (row == null) break;
            pkg_list.remove (row);
        }
        // Add new rows
        uint n = all_packages.get_n_items ();
        for (uint i = 0; i < n; i++) {
            var pkg = (HammerPackage) all_packages.get_item (i);
            pkg_list.append (new PackageRow (pkg));
        }
        pkg_list.invalidate_filter ();
        detail_panel.clear ();
    }

    // ── Search ────────────────────────────────────────────────

    private void on_search_changed () {
        filter_query = search_entry.get_text ().down ();
        pkg_list.invalidate_filter ();
    }

    private bool filter_func (ListBoxRow row) {
        if (filter_query.length == 0) return true;
        var pr = (PackageRow) row;
        var pkg = pr.get_package ();
        return pkg.name.down ().contains (filter_query)
            || pkg.summary.down ().contains (filter_query)
            || pkg.repo.down ().contains (filter_query);
    }

    // ── Row selection ─────────────────────────────────────────

    private void on_row_selected (ListBoxRow? row) {
        if (row == null) { detail_panel.clear (); return; }
        var pr = (PackageRow) row;
        detail_panel.show_package (pr.get_package ());
    }

    // ── Install / Remove ──────────────────────────────────────

    private void on_action_requested (HammerPackage pkg, bool install) {
        string op    = install ? "install" : "remove";
        string title = install ? "Installing " + pkg.name : "Removing " + pkg.name;

        var dlg = new TerminalDialog (this, title);
        dlg.present ();

        new Thread<void> ("hammer-op", () => {
            try {
                string[] argv = { "/usr/bin/hammer", op, "-y", pkg.name };
                Pid child_pid;
                int stdout_fd, stderr_fd;

                Process.spawn_async_with_pipes (
                    null, argv, null,
                    SpawnFlags.SEARCH_PATH | SpawnFlags.DO_NOT_REAP_CHILD,
                    null, out child_pid, null, out stdout_fd, out stderr_fd
                );

                var stdout_chan = new IOChannel.unix_new (stdout_fd);
                var stderr_chan = new IOChannel.unix_new (stderr_fd);

                string line;
                while (stdout_chan.read_line (out line, null, null) == IOStatus.NORMAL) {
                    var l = line; // capture for closure
                    Idle.add (() => { dlg.append_text (l); return Source.REMOVE; });
                }
                while (stderr_chan.read_line (out line, null, null) == IOStatus.NORMAL) {
                    var l = line;
                    Idle.add (() => { dlg.append_text (l); return Source.REMOVE; });
                }

                ChildWatch.add (child_pid, (pid, status) => {
                    Process.close_pid (pid);
                });

                Idle.add (() => {
                    dlg.append_text ("\n── Done. ──\n");
                    dlg.finish ();
                    // Reload package list to reflect new state
                    load_packages ();
                    return Source.REMOVE;
                });

            } catch (Error e) {
                Idle.add (() => {
                    dlg.append_text ("Error: " + e.message + "\n");
                    dlg.finish ();
                    return Source.REMOVE;
                });
            }
        });
    }

    // ── hammer sync ───────────────────────────────────────────

    private void run_hammer_sync () {
        var dlg = new TerminalDialog (this, "hammer sync");
        dlg.present ();

        new Thread<void> ("hammer-sync", () => {
            try {
                string[] argv = { "/usr/bin/hammer", "sync" };
                Pid child_pid;
                int stdout_fd, stderr_fd;

                Process.spawn_async_with_pipes (
                    null, argv, null,
                    SpawnFlags.SEARCH_PATH | SpawnFlags.DO_NOT_REAP_CHILD,
                    null, out child_pid, null, out stdout_fd, out stderr_fd
                );

                var out_chan = new IOChannel.unix_new (stdout_fd);
                var err_chan = new IOChannel.unix_new (stderr_fd);
                string line;
                while (out_chan.read_line (out line, null, null) == IOStatus.NORMAL) {
                    var l = line;
                    Idle.add (() => { dlg.append_text (l); return Source.REMOVE; });
                }
                while (err_chan.read_line (out line, null, null) == IOStatus.NORMAL) {
                    var l = line;
                    Idle.add (() => { dlg.append_text (l); return Source.REMOVE; });
                }

                ChildWatch.add (child_pid, (pid, status) => {
                    Process.close_pid (pid);
                });

                Idle.add (() => {
                    dlg.append_text ("\n── Sync complete. ──\n");
                    dlg.finish ();
                    load_packages ();
                    return Source.REMOVE;
                });
            } catch (Error e) {
                Idle.add (() => {
                    dlg.append_text ("Error: " + e.message + "\n");
                    dlg.finish ();
                    return Source.REMOVE;
                });
            }
        });
    }

    // ── Error dialog ──────────────────────────────────────────

    private void show_error_dialog (string title, string message) {
        var dlg = new MessageDialog (this, DialogFlags.MODAL, MessageType.ERROR,
                                     ButtonsType.OK, "%s", title);
        dlg.secondary_text = message;
        dlg.response.connect (() => dlg.close ());
        dlg.present ();
    }
}

// ─────────────────────────────────────────────────────────────
//  Application entry point
// ─────────────────────────────────────────────────────────────

public class HammerStoreApp : Gtk.Application {
    public HammerStoreApp () {
        Object (
            application_id: "org.hackerOS.HammerStore",
            flags: ApplicationFlags.FLAGS_NONE
        );
    }

    protected override void activate () {
        var win = new HammerStoreWindow (this);
        win.present ();
    }
}

int main (string[] args) {
    var app = new HammerStoreApp ();
    return app.run (args);
}

using GLib;
using Json;

namespace HammerStore {

    public enum PackageStatus {
        AVAILABLE,
        INSTALLED,
        UPDATE_AVAILABLE,
        INSTALLING,
        REMOVING,
        BROKEN,
    }

    public class PackageInfo : GLib.Object {
        public string   name          { get; set; default = ""; }
        public string   version       { get; set; default = ""; }
        public string   installed_ver { get; set; default = ""; }
        public string   summary       { get; set; default = ""; }
        public string   description   { get; set; default = ""; }
        public string   category      { get; set; default = ""; }
        public string   icon_name     { get; set; default = "package-x-generic"; }
        public string   icon_url      { get; set; default = ""; }
        public string   homepage      { get; set; default = ""; }
        public string   maintainer    { get; set; default = ""; }
        public string   license       { get; set; default = ""; }
        public int64    installed_size{ get; set; default = 0; }
        public int64    download_size { get; set; default = 0; }
        public double   rating        { get; set; default = 0.0; }
        public bool     featured      { get; set; default = false; }
        public PackageStatus status   { get; set; default = PackageStatus.AVAILABLE; }

        public string[] dependencies  { get; set; default = {}; }
        public string[] conflicts     { get; set; default = {}; }
        public string[] tags          { get; set; default = {}; }

        // Extended fields (0.6)
        public string[] screenshot_urls { get; set; default = {}; }
        public string   changelog      { get; set; default = ""; }
        public string[] installed_files{ get; set; default = {}; }
        public string   appstream_id   { get; set; default = ""; }
        public string   source_package { get; set; default = ""; }

        public string status_label () {
            switch (status) {
                case PackageStatus.INSTALLED:        return "Installed";
                case PackageStatus.UPDATE_AVAILABLE: return "Update available";
                case PackageStatus.INSTALLING:       return "Installing…";
                case PackageStatus.REMOVING:         return "Removing…";
                case PackageStatus.BROKEN:           return "Broken";
                default:                             return "Available";
            }
        }

        public string formatted_download_size () {
            if (download_size <= 0)                return "—";
            if (download_size < 1024)              return "%lld B".printf (download_size);
            if (download_size < 1024 * 1024)       return "%.1f KiB".printf (download_size / 1024.0);
            if (download_size < 1024 * 1024 * 1024)
                return "%.1f MiB".printf (download_size / (1024.0 * 1024));
            return "%.2f GiB".printf (download_size / (1024.0 * 1024 * 1024));
        }

        public string formatted_installed_size () {
            if (installed_size <= 0)               return "—";
            if (installed_size < 1024)             return "%lld B".printf (installed_size);
            if (installed_size < 1024 * 1024)      return "%.1f KiB".printf (installed_size / 1024.0);
            if (installed_size < 1024 * 1024 * 1024)
                return "%.1f MiB".printf (installed_size / (1024.0 * 1024));
            return "%.2f GiB".printf (installed_size / (1024.0 * 1024 * 1024));
        }

        public bool is_installed () {
            return status == PackageStatus.INSTALLED || status == PackageStatus.UPDATE_AVAILABLE;
        }
    }

    // ── Historia transakcji ───────────────────────────────────────

    public class HistoryEntry : GLib.Object {
        public int64   id         { get; set; default = 0; }
        public string  action     { get; set; default = ""; }
        public string  package    { get; set; default = ""; }
        public string  old_ver    { get; set; default = ""; }
        public string  new_ver    { get; set; default = ""; }
        public int     generation { get; set; default = 0; }
        public string  timestamp  { get; set; default = ""; }

        public string action_icon () {
            switch (action) {
                case "install": return "list-add-symbolic";
                case "remove":  return "list-remove-symbolic";
                case "upgrade": return "software-update-available-symbolic";
                default:        return "document-edit-symbolic";
            }
        }

        public string action_label () {
            switch (action) {
                case "install": return "Installed";
                case "remove":  return "Removed";
                case "upgrade": return "Upgraded";
                default:        return action;
            }
        }

        public string version_display () {
            if (action == "upgrade" && old_ver != "" && new_ver != "") {
                return "%s → %s".printf (old_ver, new_ver);
            } else if (new_ver != "") {
                return new_ver;
            } else if (old_ver != "") {
                return old_ver;
            }
            return "—";
        }
    }

    // ── Source entry (repository) ─────────────────────────────

    public struct SourceEntry {
        public string  name;
        public string  baseurl;
        public string  suite;
        public bool    enabled;
        public string? gpgkey;
    }

    // ── Package store (data backend) ─────────────────────────────

    public class PackageStore : GLib.Object {

        private List<PackageInfo> _packages = new List<PackageInfo> ();
        private HashTable<string, PackageInfo> _by_name =
            new HashTable<string, PackageInfo> (str_hash, str_equal);

        // Historia
        private List<HistoryEntry> _history = new List<HistoryEntry> ();

        public signal void refresh_started ();
        public signal void refresh_finished ();
        public signal void package_changed (PackageInfo pkg);
        public signal void history_loaded ();

        // ── Cached package maps for O(1) lookup ──────────────────
        private HashTable<string, PackageInfo> _installed_map =
            new HashTable<string, PackageInfo> (str_hash, str_equal);
        private HashTable<string, PackageInfo> _available_map =
            new HashTable<string, PackageInfo> (str_hash, str_equal);

        public async void refresh_async () {
            refresh_started ();
            yield load_installed_packages ();
            yield load_available_packages ();
            yield load_appstream_metadata ();
            yield load_history_async ();
            refresh_finished ();
        }

        // ── AppStream metadata loading ─────────────────────────

        private async void load_appstream_metadata () {
            // AppStream XML files live in /usr/share/appdata/ or
            // /usr/share/metainfo/ — one file per application.
            // We try both standard paths and the hammer-specific cache.
            string[] appstream_dirs = {
                "/usr/share/metainfo",
                "/usr/share/appdata",
                "/hammer/db/appstream",
                "/var/lib/hammer/appstream",
            };

            yield load_appstream_from_dirs (appstream_dirs);
        }

        private async void load_appstream_from_dirs (string[] dirs) {
            foreach (var dir_path in dirs) {
                var dir = File.new_for_path (dir_path);
                if (!dir.query_exists ()) continue;

                try {
                    var enumerator = yield dir.enumerate_children_async (
                        "standard::name,standard::type",
                        FileQueryInfoFlags.NONE, Priority.DEFAULT, null);

                    while (true) {
                        var files = yield enumerator.next_files_async (
                            10, Priority.LOW, null);
                        if (files == null || files.length () == 0) break;

                        foreach (var info in files) {
                            string name = info.get_name ();
                            if (!name.has_suffix (".xml") &&
                                !name.has_suffix (".appdata.xml") &&
                                !name.has_suffix (".metainfo.xml")) continue;

                            string path = Path.build_filename (dir_path, name);
                            yield parse_appstream_file (path);
                        }
                    }
                } catch (Error e) {
                    // Directory not readable — skip silently
                }
            }
        }

        private async void parse_appstream_file (string path) {
            try {
                string content;
                FileUtils.get_contents (path, out content);

                var parser = new Json.Parser ();
                // AppStream is XML — parse manually with basic line extraction
                // (full XMLParser would require libxml2 binding)
                string? pkg_id      = extract_xml_tag (content, "id");
                string? name_tag    = extract_xml_tag (content, "name");
                string? summary_tag = extract_xml_tag (content, "summary");
                string? desc_tag    = extract_xml_text (content, "description");
                string? homepage    = extract_xml_attr (content, "url", "type=\"homepage\"");
                string? icon_url_tag = extract_xml_attr (content, "icon", "type=\"remote\"");
                string[]? screenshots = extract_xml_tags (content, "image");

                // pkg_id is like "org.gnome.Gedit" or "firefox"
                // derive package name
                string? pkg_name = pkg_id;
                if (pkg_name == null) return;
                if (pkg_name.contains (".")) {
                    // Reverse DNS: take last component as hint
                    var parts = pkg_name.split (".");
                    pkg_name  = parts[parts.length - 1].down ();
                }

                // Look up in our loaded packages
                var pkg = _installed_map.get (pkg_name);
                if (pkg == null) pkg = _available_map.get (pkg_name);
                if (pkg == null) return;

                // Enrich
                if (name_tag    != null && name_tag.strip ()    != "") pkg.name    = name_tag.strip ();
                if (summary_tag != null && summary_tag.strip ()  != "") pkg.summary = summary_tag.strip ();
                if (desc_tag    != null && desc_tag.strip ()     != "") pkg.description = desc_tag.strip ();
                if (homepage    != null && homepage.strip ()     != "") pkg.homepage    = homepage.strip ();
                if (icon_url_tag != null && icon_url_tag.strip () != "") pkg.icon_url   = icon_url_tag.strip ();
                if (pkg_id      != null) pkg.appstream_id = pkg_id;

                if (screenshots != null) {
                    string[] urls = {};
                    foreach (var s in screenshots) {
                        string ss = s.strip ();
                        if (ss != "") urls += ss;
                    }
                    if (urls.length > 0) pkg.screenshot_urls = urls;
                }
            } catch (Error e) {
                // Parse error — skip file
            }
        }

        // ── Minimal XML helpers ───────────────────────────────────

        private string? extract_xml_tag (string xml, string tag) {
            string open  = "<%s>".printf (tag);
            string close = "</%s>".printf (tag);
            int start = xml.index_of (open);
            if (start < 0) return null;
            start += open.length;
            int end = xml.index_of (close, start);
            if (end < 0) return null;
            return xml.slice (start, end);
        }

        private string[]? extract_xml_tags (string xml, string tag) {
            string[] results = {};
            string search = xml;
            string open   = "<%s".printf (tag);
            string close  = "</%s>".printf (tag);
            int pos = 0;
            while (true) {
                int s = search.index_of (open, pos);
                if (s < 0) break;
                int gt = search.index_of (">", s);
                if (gt < 0) break;
                int e = search.index_of (close, gt);
                if (e < 0) break;
                results += search.slice (gt + 1, e).strip ();
                pos = e + close.length;
            }
            return results.length > 0 ? results : null;
        }

        private string? extract_xml_attr (string xml, string tag, string attr_filter) {
            string open  = "<%s %s>".printf (tag, attr_filter);
            string close = "</%s>".printf (tag);
            int start = xml.index_of (open);
            if (start < 0) {
                // Try without closing >
                open = "<%s %s".printf (tag, attr_filter);
                start = xml.index_of (open);
                if (start < 0) return null;
                int gt = xml.index_of (">", start);
                if (gt < 0) return null;
                start = gt + 1;
            } else {
                start += open.length;
            }
            int end = xml.index_of (close, start);
            if (end < 0) return null;
            return xml.slice (start, end).strip ();
        }

        private string? extract_xml_text (string xml, string tag) {
            // Extract text content stripping child XML tags
            string? raw = extract_xml_tag (xml, tag);
            if (raw == null) return null;
            // Strip XML tags
            var sb = new StringBuilder ();
            bool in_tag = false;
            foreach (var ch in raw.to_utf8 ()) {
                if (ch == '<') { in_tag = true; continue; }
                if (ch == '>') { in_tag = false; sb.append_c (' '); continue; }
                if (!in_tag) sb.append_c ((char)ch);
            }
            return sb.str.strip ();
        }

        private async void load_installed_packages () {
            string stdout_data = "";
            try {
                var sub = new GLib.Subprocess.newv (
                    { "hammer", "list", "--installed", "--json" },
                    GLib.SubprocessFlags.STDOUT_PIPE | GLib.SubprocessFlags.STDERR_SILENCE
                );
                yield sub.communicate_utf8_async (null, null, out stdout_data, null);
                parse_hammer_json (stdout_data, PackageStatus.INSTALLED);
            } catch (Error e) {
                load_stub_data ();
            }
        }

        private async void load_available_packages () {
            string stdout_data = "";
            try {
                var sub = new GLib.Subprocess.newv (
                    { "hammer", "search", "--all", "--json" },
                    GLib.SubprocessFlags.STDOUT_PIPE | GLib.SubprocessFlags.STDERR_SILENCE
                );
                yield sub.communicate_utf8_async (null, null, out stdout_data, null);
                parse_hammer_json (stdout_data, PackageStatus.AVAILABLE);
            } catch (Error e) {
                // ignoruj — stub już załadowany
            }
        }

        // ── Historia transakcji ──────────────────────────────────

        public async void load_history_async () {
            string stdout_data = "";
            try {
                var sub = new GLib.Subprocess.newv (
                    { "hammer", "history", "--json", "--limit=200" },
                    GLib.SubprocessFlags.STDOUT_PIPE | GLib.SubprocessFlags.STDERR_SILENCE
                );
                yield sub.communicate_utf8_async (null, null, out stdout_data, null);
                parse_history_json (stdout_data);
            } catch (Error e) {
                load_stub_history ();
            }
            history_loaded ();
        }

        private void parse_history_json (string json_str) {
            if (json_str.strip ().length == 0) { load_stub_history (); return; }
            _history = new List<HistoryEntry> ();
            try {
                var parser = new Json.Parser ();
                parser.load_from_data (json_str);
                var root = parser.get_root ();
                if (root == null || root.get_node_type () != Json.NodeType.ARRAY) {
                    load_stub_history (); return;
                }
                root.get_array ().foreach_element ((arr, _idx, node) => {
                    var obj = node.get_object ();
                    if (obj == null) return;
                    var e = new HistoryEntry ();
                    e.id         = obj.has_member ("id")         ? obj.get_int_member ("id")             : 0;
                    e.action     = obj.get_string_member_with_default ("action",    "");
                    e.package    = obj.get_string_member_with_default ("package",   "");
                    e.old_ver    = obj.get_string_member_with_default ("old_ver",   "");
                    e.new_ver    = obj.get_string_member_with_default ("new_ver",   "");
                    e.generation = (int)(obj.has_member ("generation") ? obj.get_int_member ("generation") : 0);
                    e.timestamp  = obj.get_string_member_with_default ("timestamp", "");
                    _history.append (e);
                });
            } catch (Error e) {
                warning ("History JSON parse error: %s", e.message);
                load_stub_history ();
            }
        }

        private void load_stub_history () {
            _history = new List<HistoryEntry> ();
            string[,] stubs = {
                { "install", "firefox",     "",        "127.0.2",   "5", "2025-06-20 10:00" },
                { "install", "vlc",         "",        "3.0.20",    "4", "2025-06-18 14:30" },
                { "upgrade", "python3",     "3.11.0",  "3.12.2",    "3", "2025-06-15 09:15" },
                { "remove",  "thunderbird", "115.8.0", "",           "2", "2025-06-10 16:45" },
                { "install", "git",         "",        "2.43.0",    "1", "2025-06-01 08:00" },
            };
            for (int i = 0; i < 5; i++) {
                var e = new HistoryEntry ();
                e.action     = stubs[i,0];
                e.package    = stubs[i,1];
                e.old_ver    = stubs[i,2];
                e.new_ver    = stubs[i,3];
                e.generation = int.parse (stubs[i,4]);
                e.timestamp  = stubs[i,5];
                _history.append (e);
            }
        }

        public owned List<HistoryEntry> get_history () {
            var result = new List<HistoryEntry> ();
            _history.@foreach ((e) => result.append (e));
            return (owned) result;
        }

        // ── Rozmiar paczki przez `hammer size` ───────────────────

        public async string get_size_string_async (string pkg_name) {
            string stdout_data = "";
            try {
                var sub = new GLib.Subprocess.newv (
                    { "hammer", "size", pkg_name },
                    GLib.SubprocessFlags.STDOUT_PIPE | GLib.SubprocessFlags.STDERR_SILENCE
                );
                yield sub.communicate_utf8_async (null, null, out stdout_data, null);
                // Wyciągnij pierwszą linię z rozmiarem (prosta heurystyka)
                foreach (var line in stdout_data.split ("\n")) {
                    var l = line.strip ();
                    if (l.contains (pkg_name) && (l.contains ("MiB") || l.contains ("KiB") || l.contains ("GiB"))) {
                        // Znajdź rozmiar w linii
                        var parts = l.split_set (" \t");
                        foreach (var p in parts) {
                            if (p.contains ("MiB") || p.contains ("KiB") || p.contains ("GiB") || p.contains (" B")) {
                                return p;
                            }
                        }
                    }
                }
            } catch (Error e) {}
            // Fallback na pole installed_size z PackageInfo
            var pkg = _by_name.get (pkg_name);
            if (pkg != null && pkg.installed_size > 0) {
                return pkg.formatted_installed_size ();
            }
            return "—";
        }

        private void parse_hammer_json (string json_str, PackageStatus default_status) {
            if (json_str.strip ().length == 0) return;
            try {
                var parser = new Json.Parser ();
                parser.load_from_data (json_str);
                var root = parser.get_root ();
                if (root == null || root.get_node_type () != Json.NodeType.ARRAY) return;
                root.get_array ().foreach_element ((arr, _idx, node) => {
                    var obj = node.get_object ();
                    if (obj == null) return;
                    var info      = new PackageInfo ();
                    info.name     = obj.get_string_member_with_default ("name",    "");
                    info.version  = obj.get_string_member_with_default ("version", "");
                    info.summary  = obj.get_string_member_with_default ("summary", "");
                    info.description = obj.get_string_member_with_default ("description", "");
                    info.category = obj.get_string_member_with_default ("category","other");
                    info.icon_name = obj.get_string_member_with_default ("icon",   "package-x-generic");
                    info.homepage  = obj.get_string_member_with_default ("homepage","");
                    info.maintainer = obj.get_string_member_with_default ("maintainer","");
                    info.download_size  = obj.has_member ("download_size")  ? (int64)obj.get_int_member ("download_size")  : 0;
                    info.installed_size = obj.has_member ("installed_size") ? (int64)obj.get_int_member ("installed_size") : 0;
                    info.rating    = obj.has_member ("rating") ? obj.get_double_member ("rating") : 0.0;
                    info.featured  = obj.has_member ("featured") ? obj.get_boolean_member ("featured") : false;
                    info.status    = default_status;

                    if (_by_name.contains (info.name)) {
                        var existing = _by_name.get (info.name);
                        if (default_status == PackageStatus.INSTALLED) {
                            existing.status        = PackageStatus.INSTALLED;
                            existing.installed_ver = existing.version;
                            existing.installed_size = info.installed_size;
                            _installed_map.set (existing.name, existing);
                        }
                    } else {
                        _packages.append (info);
                        _by_name.set (info.name, info);
                        if (default_status == PackageStatus.INSTALLED) {
                            _installed_map.set (info.name, info);
                        } else {
                            _available_map.set (info.name, info);
                        }
                    }
                });
            } catch (Error e) {
                warning ("PackageStore JSON parse error: %s", e.message);
            }
        }

        // Stub data dla demo / gdy hammer niedostępny
        private void load_stub_data () {
            string[,] stubs = {
                { "firefox",        "Firefox",        "Web Browser",    "internet",   "127.0.2",   "true",  "85.2",  "89400000",   "120000000" },
                { "thunderbird",    "Thunderbird",    "Email Client",   "internet",   "115.8.0",   "false", "75.4",  "73200000",   "95000000"  },
                { "vlc",            "VLC",            "Media Player",   "multimedia", "3.0.20",    "true",  "92.1",  "52400000",   "68000000"  },
                { "gimp",           "GIMP",           "Image Editor",   "graphics",   "2.10.36",   "false", "88.5",  "261000000",  "310000000" },
                { "libreoffice",    "LibreOffice",    "Office Suite",   "office",     "7.6.4",     "true",  "81.3",  "371000000",  "430000000" },
                { "vim",            "Vim",            "Text Editor",    "devel",      "9.1.0",     "false", "79.9",  "3800000",    "4200000"   },
                { "neovim",         "Neovim",         "Text Editor",    "devel",      "0.9.5",     "false", "83.7",  "8500000",    "9700000"   },
                { "htop",           "htop",           "Process Viewer", "system",     "3.3.0",     "true",  "91.0",  "280000",     "350000"    },
                { "git",            "Git",            "Version Control","devel",      "2.43.0",    "true",  "95.4",  "7800000",    "9200000"   },
                { "code",           "VS Code",        "Code Editor",    "devel",      "1.87.0",    "false", "89.2",  "97200000",   "115000000" },
                { "blender",        "Blender",        "3D Modelling",   "graphics",   "4.0.2",     "false", "93.6",  "210000000",  "285000000" },
                { "obs-studio",     "OBS Studio",     "Screen Recorder","multimedia", "30.0.2",    "false", "87.1",  "67400000",   "78000000"  },
                { "inkscape",       "Inkscape",       "Vector Graphics","graphics",   "1.3.2",     "false", "84.4",  "113000000",  "132000000" },
                { "audacity",       "Audacity",       "Audio Editor",   "multimedia", "3.5.1",     "false", "80.0",  "28700000",   "34000000"  },
                { "steam",          "Steam",          "Game Launcher",  "games",      "1.0.0.78",  "false", "88.9",  "4200000",    "310000000" },
                { "docker",         "Docker",         "Containers",     "system",     "25.0.3",    "false", "86.3",  "131000000",  "145000000" },
                { "python3",        "Python 3",       "Programming",    "devel",      "3.12.2",    "true",  "96.0",  "21500000",   "25000000"  },
                { "nodejs",         "Node.js",        "Runtime",        "devel",      "20.11.1",   "false", "90.5",  "32600000",   "38000000"  },
                { "rust",           "Rust",           "Programming",    "devel",      "1.76.0",    "false", "93.2",  "71800000",   "82000000"  },
                { "golang",         "Go",             "Programming",    "devel",      "1.22.0",    "false", "89.8",  "62400000",   "70000000"  },
            };

            for (int i = 0; i < 20; i++) {
                var info = new PackageInfo ();
                info.name           = stubs[i,0];
                info.summary        = stubs[i,2];
                info.description    = "%s — available via hammer.".printf (stubs[i,2]);
                info.category       = stubs[i,3];
                info.version        = stubs[i,4];
                info.featured       = stubs[i,5] == "true";
                info.rating         = double.parse (stubs[i,6]);
                info.installed_size = int64.parse (stubs[i,7]);
                info.download_size  = int64.parse (stubs[i,8]);
                info.status         = PackageStatus.AVAILABLE;
                _packages.append (info);
                _by_name.set (info.name, info);
            }
        }

        // ── Queries ───────────────────────────────────────────────

        public owned List<PackageInfo> get_featured () {
            var result = new List<PackageInfo> ();
            _packages.@foreach ((p) => { if (p.featured) result.append (p); });
            return (owned) result;
        }

        public owned List<PackageInfo> get_by_category (string cat) {
            var result = new List<PackageInfo> ();
            _packages.@foreach ((p) => { if (p.category == cat) result.append (p); });
            return (owned) result;
        }

        public owned List<PackageInfo> get_installed () {
            var result = new List<PackageInfo> ();
            _packages.@foreach ((p) => { if (p.is_installed ()) result.append (p); });
            return (owned) result;
        }

        public owned List<PackageInfo> get_updates () {
            var result = new List<PackageInfo> ();
            _packages.@foreach ((p) => {
                if (p.status == PackageStatus.UPDATE_AVAILABLE) result.append (p);
            });
            return (owned) result;
        }

        public owned List<PackageInfo> search (string query) {
            string q = query.down ().strip ();
            var result = new List<PackageInfo> ();
            _packages.@foreach ((p) => {
                if (p.name.down ().contains (q) ||
                    p.summary.down ().contains (q) ||
                    p.description.down ().contains (q) ||
                    p.category.down ().contains (q)) {
                    result.append (p);
                }
            });
            return (owned) result;
        }

        public string[] categories () {
            var cats = new HashTable<string, bool> (str_hash, str_equal);
            _packages.@foreach ((p) => cats.set (p.category, true));
            string[] res = {};
            cats.@foreach ((k, _v) => res += k);
            return res;
        }

        public async void install_package_async (PackageInfo pkg) {
            pkg.status = PackageStatus.INSTALLING;
            package_changed (pkg);
            try {
                var sub = new GLib.Subprocess.newv (
                    { "pkexec", "hammer", "install", "-y", pkg.name },
                    GLib.SubprocessFlags.NONE
                );
                yield sub.wait_async ();
                pkg.status = sub.get_exit_status () == 0
                    ? PackageStatus.INSTALLED
                    : PackageStatus.AVAILABLE;
            } catch (Error e) {
                pkg.status = PackageStatus.AVAILABLE;
            }
            package_changed (pkg);
            yield load_history_async ();
        }

        public async void remove_package_async (PackageInfo pkg) {
            pkg.status = PackageStatus.REMOVING;
            package_changed (pkg);
            try {
                var sub = new GLib.Subprocess.newv (
                    { "pkexec", "hammer", "remove", "-y", pkg.name },
                    GLib.SubprocessFlags.NONE
                );
                yield sub.wait_async ();
                pkg.status = sub.get_exit_status () == 0
                    ? PackageStatus.AVAILABLE
                    : PackageStatus.INSTALLED;
            } catch (Error e) {
                pkg.status = PackageStatus.INSTALLED;
            }
            package_changed (pkg);
            yield load_history_async ();
        }

        public async void upgrade_all_async () {
            try {
                var sub = new GLib.Subprocess.newv (
                    { "pkexec", "hammer", "upgrade", "-y" },
                    GLib.SubprocessFlags.NONE
                );
                yield sub.wait_async ();
            } catch (Error e) {}
            yield refresh_async ();
        }

        /// Cofnij ostatnią operację przez `hammer undo --yes`
        public async bool undo_last_async () {
            bool ok = false;
            try {
                var sub = new GLib.Subprocess.newv (
                    { "pkexec", "hammer", "undo", "--yes" },
                    GLib.SubprocessFlags.NONE
                );
                yield sub.wait_async ();
                ok = sub.get_exit_status () == 0;
            } catch (Error e) {}
            if (ok) { yield refresh_async (); }
            return ok;
        }
    }
}


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

    public class PackageInfo : Object {
        public string   name          { get; set; default = ""; }
        public string   version       { get; set; default = ""; }
        public string   installed_ver { get; set; default = ""; }
        public string   summary       { get; set; default = ""; }
        public string   description   { get; set; default = ""; }
        public string   category      { get; set; default = ""; }
        public string   icon_name     { get; set; default = "package-x-generic"; }
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
            if (download_size < 1024)             return "%lld B".printf (download_size);
            if (download_size < 1024 * 1024)      return "%.1f KiB".printf (download_size / 1024.0);
            if (download_size < 1024 * 1024 * 1024)
                return "%.1f MiB".printf (download_size / (1024.0 * 1024));
            return "%.2f GiB".printf (download_size / (1024.0 * 1024 * 1024));
        }

        public string formatted_installed_size () {
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

    // ── Package store (data backend) ─────────────────────────────

    public class PackageStore : Object {

        private List<PackageInfo> _packages = new List<PackageInfo> ();
        private HashTable<string, PackageInfo> _by_name =
            new HashTable<string, PackageInfo> (str_hash, str_equal);

        public signal void refresh_started ();
        public signal void refresh_finished ();
        public signal void package_changed (PackageInfo pkg);

        public async void refresh_async () {
            refresh_started ();
            yield load_installed_packages ();
            yield load_available_packages ();
            refresh_finished ();
        }

        private async void load_installed_packages () {
            // Run: hammer list --installed --json
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
                // ignore — stub already loaded
            }
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
                        }
                    } else {
                        _packages.append (info);
                        _by_name.set (info.name, info);
                    }
                });
            } catch (Error e) {
                warning ("PackageStore JSON parse error: %s", e.message);
            }
        }

        // Stub data for demo / when hammer is not available
        private void load_stub_data () {
            string[,] stubs = {
                { "firefox",        "Firefox",        "Web Browser",  "internet",  "127.0.2",  "true",  "85.2" },
                { "thunderbird",    "Thunderbird",    "Email Client", "internet",  "115.8.0",  "false", "75.4" },
                { "vlc",            "VLC",            "Media Player", "multimedia","3.0.20",   "true",  "92.1" },
                { "gimp",           "GIMP",           "Image Editor", "graphics",  "2.10.36",  "false", "88.5" },
                { "libreoffice",    "LibreOffice",    "Office Suite", "office",    "7.6.4",    "true",  "81.3" },
                { "vim",            "Vim",            "Text Editor",  "devel",     "9.1.0",    "false", "79.9" },
                { "neovim",         "Neovim",         "Text Editor",  "devel",     "0.9.5",    "false", "83.7" },
                { "htop",           "htop",           "Process Viewer","system",   "3.3.0",    "true",  "91.0" },
                { "git",            "Git",            "Version Control","devel",   "2.43.0",   "true",  "95.4" },
                { "code",           "VS Code",        "Code Editor",  "devel",     "1.87.0",   "false", "89.2" },
                { "blender",        "Blender",        "3D Modelling", "graphics",  "4.0.2",    "false", "93.6" },
                { "obs-studio",     "OBS Studio",     "Screen Recorder","multimedia","30.0.2",  "false", "87.1" },
                { "inkscape",       "Inkscape",       "Vector Graphics","graphics", "1.3.2",   "false", "84.4" },
                { "audacity",       "Audacity",       "Audio Editor", "multimedia","3.5.1",    "false", "80.0" },
                { "steam",          "Steam",          "Game Launcher","games",     "1.0.0.78", "false", "88.9" },
                { "docker",         "Docker",         "Containers",   "system",    "25.0.3",   "false", "86.3" },
                { "python3",        "Python 3",       "Programming",  "devel",     "3.12.2",   "true",  "96.0" },
                { "nodejs",         "Node.js",        "Runtime",      "devel",     "20.11.1",  "false", "90.5" },
                { "rust",           "Rust",           "Programming",  "devel",     "1.76.0",   "false", "93.2" },
                { "golang",         "Go",             "Programming",  "devel",     "1.22.0",   "false", "89.8" },
            };

            for (int i = 0; i < 20; i++) {
                var info = new PackageInfo ();
                info.name          = stubs[i,0];
                info.summary       = stubs[i,2];
                info.description   = "%s — available via hammer.".printf (stubs[i,2]);
                info.category      = stubs[i,3];
                info.version       = stubs[i,4];
                info.featured      = stubs[i,5] == "true";
                info.rating        = double.parse (stubs[i,6]);
                info.status        = PackageStatus.AVAILABLE;
                info.download_size = GLib.Random.int_range (1024*500, 1024*1024*200);
                _packages.append (info);
                _by_name.set (info.name, info);
            }
        }

        // ── Queries ───────────────────────────────────────────────

        public List<PackageInfo> get_featured () {
            var result = new List<PackageInfo> ();
            _packages.@foreach ((p) => { if (p.featured) result.append (p); });
            return result;
        }

        public List<PackageInfo> get_by_category (string cat) {
            var result = new List<PackageInfo> ();
            _packages.@foreach ((p) => { if (p.category == cat) result.append (p); });
            return result;
        }

        public List<PackageInfo> get_installed () {
            var result = new List<PackageInfo> ();
            _packages.@foreach ((p) => { if (p.is_installed ()) result.append (p); });
            return result;
        }

        public List<PackageInfo> get_updates () {
            var result = new List<PackageInfo> ();
            _packages.@foreach ((p) => {
                if (p.status == PackageStatus.UPDATE_AVAILABLE) result.append (p);
            });
            return result;
        }

        public List<PackageInfo> search (string query) {
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
            return result;
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
                    { "pkexec", "hammer", "install", pkg.name },
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
        }

        public async void remove_package_async (PackageInfo pkg) {
            pkg.status = PackageStatus.REMOVING;
            package_changed (pkg);
            try {
                var sub = new GLib.Subprocess.newv (
                    { "pkexec", "hammer", "remove", pkg.name },
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
        }

        public async void upgrade_all_async () {
            try {
                var sub = new GLib.Subprocess.newv (
                    { "pkexec", "hammer", "upgrade" },
                    GLib.SubprocessFlags.NONE
                );
                yield sub.wait_async ();
            } catch (Error e) {}
            yield refresh_async ();
        }
    }
}

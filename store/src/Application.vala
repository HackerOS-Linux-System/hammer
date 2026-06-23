using Gtk;
using GLib;

namespace HammerStore {

    public class Application : Gtk.Application {

        public static Application instance { get; private set; }

        public Application () {
            Object (
                application_id: "org.hackerOS.HammerStore",
                flags: ApplicationFlags.FLAGS_NONE
            );
            instance = this;
        }

        protected override void activate () {
            var win = this.active_window;
            if (win == null) {
                win = new MainWindow (this);
            }
            win.present ();
        }

        public static int main (string[] args) {
            // Init GTK
            Gtk.init ();
            var app = new Application ();
            return app.run (args);
        }
    }
}

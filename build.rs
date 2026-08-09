fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_FEATURE_OCI_MODE").is_err() {
        // Not building with --features oci-mode: nothing to do. Do NOT
        // probe pkg-config or run bindgen — that would turn libostree-dev
        // into a hard requirement for every build of this crate, which is
        // exactly what oci-mode's whole cfg-gating design is meant to avoid.
        return;
    }

    #[cfg(feature = "oci-mode")]
    oci_mode::run();
}

#[cfg(feature = "oci-mode")]
mod oci_mode {
    use std::env;
    use std::path::PathBuf;

    pub fn run() {
        println!("cargo:rerun-if-changed=source-code/oci/ffi/wrapper.h");

        let library = pkg_config::Config::new()
            .atleast_version("2020.1") // anything reasonably modern
            .probe("ostree-1")
            .expect(
                "hammer oci (--features oci-mode) requires libostree-dev \
                 (and its transitive glib/gobject/gio dev headers) to be \
                 installed and discoverable via pkg-config. On Debian/Ubuntu: \
                 `apt install libostree-dev pkg-config clang libclang-dev`.",
            );

        let mut builder = bindgen::Builder::default()
            .header("source-code/oci/ffi/wrapper.h")
            // We only want OSTree's own API surface in the output — pulling
            // in every glib/gobject symbol transitively would generate an
            // enormous, mostly-unused bindings file. `allowlist_*` narrows
            // bindgen's output to Ostree* symbols plus the handful of
            // GLib/GObject/GIO types + functions the safe wrapper needs
            // directly (GError, GCancellable, GFile, GPtrArray, GKeyFile,
            // ref-counting).
            .allowlist_type("Ostree.*")
            .allowlist_function("ostree_.*")
            .allowlist_var("OSTREE_.*")
            .allowlist_type("GError")
            .allowlist_type("GCancellable")
            .allowlist_type("GFile")
            .allowlist_type("GPtrArray")
            .allowlist_type("GKeyFile")
            .allowlist_type("GObject")
            .allowlist_type("GVariant")
            .allowlist_type("GHashTable")
            .allowlist_type("GHashTableIter")
            .allowlist_function("g_object_unref")
            .allowlist_function("g_object_ref")
            .allowlist_function("g_error_free")
            .allowlist_function("g_file_new_for_path")
            .allowlist_function("g_ptr_array_.*")
            .allowlist_function("g_key_file_.*")
            .allowlist_function("g_free")
            .allowlist_function("g_cancellable_new")
            .allowlist_function("g_variant_get_child_value")
            .allowlist_function("g_variant_get_string")
            .allowlist_function("g_variant_get_uint64")
            .allowlist_function("g_variant_n_children")
            .allowlist_function("g_variant_unref")
            .allowlist_function("g_variant_ref")
            .allowlist_function("g_hash_table_size")
            .allowlist_function("g_hash_table_unref")
            .allowlist_function("g_hash_table_iter_init")
            .allowlist_function("g_hash_table_iter_next")
            // GLib uses a handful of function-like macros for ref-counting
            // that bindgen can't always expand safely; we call the
            // underlying g_object_(un)ref functions directly instead in
            // the safe wrapper, so no need to fight the macros here.
            .blocklist_function("g_object_unref_inline")
            .derive_default(true)
            .derive_debug(true)
            .generate_comments(true)
            .layout_tests(false)
            .default_enum_style(bindgen::EnumVariation::Rust { non_exhaustive: true });

        for path in &library.include_paths {
            builder = builder.clang_arg(format!("-I{}", path.display()));
        }

        let bindings = builder
            .generate()
            .expect(
                "bindgen failed to generate libostree FFI bindings — this \
                 usually means libostree-dev's headers changed in an \
                 incompatible way, or libclang could not be found \
                 (install `libclang-dev`/`clang`).",
            );

        let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
        bindings
            .write_to_file(out_path.join("ostree_bindings.rs"))
            .expect("Couldn't write ostree_bindings.rs");

        // pkg_config::Config::probe already emits the right
        // cargo:rustc-link-lib / cargo:rustc-link-search directives for
        // ostree-1 and its transitive deps (glib-2.0, gobject-2.0,
        // gio-2.0) — nothing further to link manually.
    }
}

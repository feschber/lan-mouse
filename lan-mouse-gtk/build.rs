fn main() {
    let unix = cfg!(unix);
    let macos = cfg!(target_os = "macos");
    let wayland_window_id = cfg!(feature = "wayland_window_id");
    let wayland_window_id = unix && !macos && wayland_window_id;
    println!("cargo::rustc-check-cfg=cfg(wayland_window_id)");

    if wayland_window_id {
        println!("cargo::rustc-cfg=wayland_window_id");
    }

    // composite_templates
    glib_build_tools::compile_resources(
        &["resources"],
        "resources/resources.gresource.xml",
        "lan-mouse.gresource",
    );
}

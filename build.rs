fn main() {
    // composite_templates
    glib_build_tools::compile_resources(
        &["resources"],
        "resources/resources.gresource.xml",
        "lan-mouse.gresource",
    );
}

fn main() {
    glib_build_tools::compile_resources(&["data"], "data/aureus.gresource.xml", "aureus.gresource");
}

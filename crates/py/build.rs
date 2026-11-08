// A Python extension on macOS leaves the interpreter's symbols for Python to
// resolve at import; without `-undefined dynamic_lookup` it doesn't link.
fn main() {
    pyo3_build_config::add_extension_module_link_args();
}

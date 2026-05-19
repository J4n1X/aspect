fn main() {
    // Auto-detects if we added new tests or modified existing ones, and triggers a rebuild if so.
    println!("cargo:rerun-if-changed=tests/programs");
}

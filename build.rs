fn main() {
    // The `generate_tests!` proc-macro scans this directory at compile time;
    // adding a `.tjlb` file (or editing one's `# expected:` annotation) must
    // re-trigger expansion. Cargo doesn't know `.tjlb` files are inputs, so we
    // list the scan root here — invalidates the test binary on any change.
    println!("cargo:rerun-if-changed=tests/programs");
}

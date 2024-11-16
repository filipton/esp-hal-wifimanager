fn main() {
    // This tells Cargo to rerun build script if WM_CONN env var changes
    println!("cargo:rerun-if-env-changed=WM_CONN");
}

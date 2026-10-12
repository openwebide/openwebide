fn main() {
    println!("cargo::rerun-if-env-changed=OPENWEBIDE_BUILD_COMMIT");
}

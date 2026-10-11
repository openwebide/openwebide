use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=OPENWEBIDE_BUNDLED_ARTIFACTS");
    if env::var_os("CARGO_FEATURE_BUNDLED_DEFAULTS").is_some() {
        assert!(
            env::var_os("OPENWEBIDE_BUNDLED_ARTIFACTS").is_some(),
            "Distribution hosts require OPENWEBIDE_BUNDLED_ARTIFACTS from openwebide-bundle-plugins"
        );
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"))
        .join("bundled-artifacts.json");
    if let Some(input) = env::var_os("OPENWEBIDE_BUNDLED_ARTIFACTS") {
        let input = PathBuf::from(input)
            .canonicalize()
            .expect("Compiled bundle must exist");
        println!("cargo:rerun-if-changed={}", input.display());
        assert!(
            fs::metadata(&input).expect("Bundle metadata").len() <= 128 * 1024 * 1024,
            "Compiled bundle exceeds its limit"
        );
        fs::copy(input, output).expect("Embed compiled host defaults");
    } else {
        // Development hosts can compile source on install. Distribution builds supply this file.
        fs::write(output, br#"{"artifacts":[]}"#).expect("Empty development bundle");
    }
}

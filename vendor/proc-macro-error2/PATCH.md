# Public procedural-macro re-export

Source: crates.io `proc-macro-error2` 2.0.1, MIT OR Apache-2.0
(see LICENSE-MIT and LICENSE-APACHE),
https://github.com/GnomedDev/proc-macro-error-2.
Archive SHA-256: 11ec05c52be0a07b08061f7dd003e7d7092e0472bc731b4af7bb1ef876109802.

The only Rust source change makes `extern crate proc_macro` public. Upstream
already publicly re-exports it through `__export` for generated macros; the
private import triggers E0365 and a future-incompatibility notice on the pinned
Rust toolchain. This applies the compiler's suggested visibility correction
without changing macro behavior or suppressing the diagnostic.

Keep the patch until a released dependency supplies the correction. The upstream
repository is archived. Packaging excludes registry metadata and Cargo.lock.

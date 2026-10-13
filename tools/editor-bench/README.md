# Editor primitive measurements

Run from the repository root, using its shared `target/`. Build before measuring;
run each measurement separately, without other builds or browser test runs.

Input preparation compares editing context with complete highlights/folds using
the same admitted documents, fresh parser caches and 12 ms continuation policy:

```sh
cargo build -p openwebide-editor-bench --release --features input-preparation
./target/release/openwebide-editor-bench --input
./target/release/openwebide-editor-bench --full-input
```

For browser measurements, use Chrome/chromedriver and the workspace's matching
wasm-bindgen runner. Build once, then run each preparation variant in its own browser:

```sh
cargo test -p openwebide-editor-bench --release --features input-preparation --target wasm32-unknown-unknown --no-run
CHROMEDRIVER=/path/to/chromedriver cargo test -p openwebide-editor-bench --release --features input-preparation --target wasm32-unknown-unknown shared_input_preparation_workloads -- --nocapture
CHROMEDRIVER=/path/to/chromedriver cargo test -p openwebide-editor-bench --release --features input-preparation --target wasm32-unknown-unknown shared_full_preparation_workloads -- --nocapture
```

Each run covers HTML code, many HTML bodies, Markdown code and Rust from 64 KiB
through an admitted 2.48 MiB source above the syntax limit. Results separate
document construction (including allocation, admission and indexes), preparation
and edit cost. They record readiness, cancellation, context availability and
actual pairing; they do not assume that a timing budget guarantees readiness.
Insertion, immutable source and undo are checked outside the measured phases.

Fixture generation and selection setup are excluded. These are shared core
primitive costs, excluding frontend state publication, browser layout and paint.
Fresh parser caches do not reset process-wide grammar/JIT warmup. Keep workload
order, profile, clock adapter, hardware, versions and background load with any
results; repeat in fresh processes before making cold-latency claims.

For core edit/undo measurements, build once, then run without concurrent builds:

```sh
cargo build -p openwebide-editor-bench --release
./target/release/openwebide-editor-bench --edits
```

Each of three samples measures 20 insertion/undo or line-split/undo pairs on
near-MiB ASCII/Unicode rows and a near-limit dense-row document. Construction,
selection setup and exact source/native-coordinate checks are outside timing.
The measured phases include core allocation, admission, history and row-index
publication; they exclude browser input, syntax, shaping, layout and paint.
Compare baseline/candidate binaries built with the same compiler, dependencies
and profile, interleaving runs on the same otherwise idle host. Single samples
are not evidence of a whole-editor latency or memory improvement.

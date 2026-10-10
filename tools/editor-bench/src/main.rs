fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let start = std::time::Instant::now();
        let clock = || start.elapsed().as_secs_f64() * 1000.0;
        #[cfg(feature = "input-preparation")]
        if std::env::args().any(|argument| matches!(argument.as_str(), "--input" | "--full-input"))
        {
            let decorations = std::env::args().any(|argument| argument == "--full-input");
            println!(
                "workload,decorations,bytes,status,context,paired,document_ms,preparation_ms,edit_ms"
            );
            for record in openwebide_editor_bench::input::measure(clock, decorations) {
                openwebide_editor_bench::input::print(&record);
            }
            return;
        }
        let records = if std::env::args().any(|argument| argument == "--lexical") {
            openwebide_editor_bench::measure_lexical(clock)
        } else {
            openwebide_editor_bench::measure(clock)
        };
        println!("operation,bytes,total_ms");
        for record in records {
            println!(
                "{},{},{:.3}",
                record.operation, record.bytes, record.milliseconds
            );
        }
    }
}

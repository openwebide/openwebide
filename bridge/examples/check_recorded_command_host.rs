//! Disposable native HTTP host for abrupt-process-loss recovery contracts.
//! Its private-home constructor is available only in the test-support build.

#[cfg(all(
    feature = "test-support",
    any(target_os = "linux", target_os = "macos")
))]
#[tokio::main(worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use openwebide_bridge::{ServerConfig, run_server_until};
    let mut args = std::env::args_os().skip(1);
    let root = args.next().ok_or("Missing fixture workspace")?;
    if root == "--recovery" {
        let home = std::path::PathBuf::from(args.next().ok_or("Missing private fixture home")?);
        let request = args
            .map(|value| {
                value
                    .into_string()
                    .map_err(|_| "Invalid fixture recovery argument")
            })
            .collect::<Result<Vec<_>, _>>()?;
        println!(
            "{}",
            openwebide_bridge::recovery::execute_in_test_home(&home, &request)?
        );
        return Ok(());
    }
    let (root, port) = if root == "--port" {
        let port = args
            .next()
            .ok_or("Missing fixture port")?
            .into_string()
            .map_err(|_| "Invalid fixture port")?
            .parse::<u16>()?;
        (args.next().ok_or("Missing fixture workspace")?, port)
    } else {
        (root, 0)
    };
    let home = args.next().ok_or("Missing private fixture home")?;
    let backend = args
        .next()
        .ok_or("Missing fixture callback")?
        .into_string()
        .map_err(|_| "Invalid fixture callback")?;
    let origins = args
        .map(|value| value.into_string().map_err(|_| "Invalid fixture origin"))
        .collect::<Result<Vec<_>, _>>()?;
    let mut config = ServerConfig::new_for_test(
        root.into(),
        "dummy_secret".into(),
        Some("paired-token".into()),
        home.into(),
    )
    .map_err(std::io::Error::other)?;
    config.backend_url = backend;
    if !origins.is_empty() {
        config.allowed_origins = origins;
    }
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    println!("{}", listener.local_addr()?.port());
    run_server_until(listener, config, async move {
        terminate.recv().await;
    })
    .await;
    Ok(())
}

#[cfg(not(all(
    feature = "test-support",
    any(target_os = "linux", target_os = "macos")
)))]
fn main() {
    eprintln!("This recovery fixture requires test-support on Linux or macOS");
    std::process::exit(1);
}

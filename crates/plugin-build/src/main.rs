//! Small, single-threaded compiler launcher; no app or plugin feature behavior.
use anyhow::Result;
#[cfg(not(target_os = "linux"))]
use anyhow::bail;

#[cfg(target_os = "linux")]
mod linux;

fn main() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        linux::run()
    }
    #[cfg(not(target_os = "linux"))]
    {
        bail!("This compiler launcher is only used by Linux container hosts")
    }
}

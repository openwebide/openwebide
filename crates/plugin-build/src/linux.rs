use super::*;
use anyhow::{Context, bail};
use landlock::{
    ABI, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
    path_beneath_rules,
};
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};
use std::{
    collections::BTreeMap,
    ffi::CString,
    os::unix::{ffi::OsStrExt, process::CommandExt},
    path::{Path, PathBuf},
    process::Command,
};

/// Root container builds run as a fresh, unprivileged identity. Landlock handles
/// data access; different ownership also protects metadata on read-only trees.
pub fn run() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let network = args.next().context("Specify fetch or build")?;
    let network = match network.to_str() {
        Some("fetch") => true,
        Some("build") => false,
        _ => bail!("Specify fetch or build"),
    };
    let source = directory(args.next().context("Source directory")?)?;
    let work = directory(args.next().context("Build directory")?)?;
    let sdk = directory(args.next().context("SDK directory")?)?;
    let toolchain = directory(args.next().context("Toolchain directory")?)?;
    let program = PathBuf::from(args.next().context("Compiler executable")?).canonicalize()?;
    if !program.starts_with(&toolchain)
        || work.starts_with(&source)
        || source.starts_with(&work)
        || sdk.starts_with(&work)
        || toolchain.starts_with(&work)
        || source.parent() != work.parent()
        || sdk.parent() != work.parent()
    {
        bail!("Invalid compiler sandbox paths");
    }
    // SAFETY: geteuid has no pointer arguments or side effects.
    if unsafe { libc::geteuid() } != 0 {
        bail!("Container compiler launcher requires root to create its isolated identity");
    }
    let uid = compiler_identity()?;
    change_owner(&work, uid)?;
    // A private staging root contains only public source/SDK and build outputs.
    // Keep the root-owned source/SDK separate from the writable work directory.
    for path in [&source, &work, &sdk] {
        let parent = path.parent().context("Staging parent")?;
        std::fs::set_permissions(parent, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    }
    // SAFETY: the single-threaded helper drops all supplementary groups and all
    // real/effective/saved IDs before starting any package code.
    unsafe {
        if libc::setgroups(0, std::ptr::null()) != 0
            || libc::setresgid(uid, uid, uid) != 0
            || libc::setresuid(uid, uid, uid) != 0
        {
            return Err(std::io::Error::last_os_error()).context("Drop compiler privileges");
        }
    }
    let abi = ABI::V3;
    let mut read = vec![source, sdk, toolchain];
    for path in [
        "/usr",
        "/bin",
        "/lib",
        "/lib64",
        "/etc/ld.so.cache",
        "/etc/ssl/certs",
        "/etc/ssl/openssl.cnf",
        "/etc/resolv.conf",
        "/etc/hosts",
        "/etc/nsswitch.conf",
        "/etc/gai.conf",
        "/sys/devices/system/cpu/online",
        "/dev/urandom",
        "/dev/random",
    ] {
        if Path::new(path).exists() {
            read.push(PathBuf::from(path));
        }
    }
    let status = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))?
        .create()?
        .add_rules(path_beneath_rules(&read, AccessFs::from_read(abi)))?
        .add_rules(path_beneath_rules(
            [work.as_path(), Path::new("/dev/null")],
            AccessFs::from_all(abi),
        ))?
        .restrict_self()?;
    if status.ruleset != RulesetStatus::FullyEnforced || !status.no_new_privs {
        bail!("Linux kernel must fully enforce Landlock ABI 3 for plugin compilation");
    }
    let mut denied = vec![
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_pidfd_getfd,
        libc::SYS_setsid,
        libc::SYS_setpgid,
    ];
    if !network {
        denied.extend([
            libc::SYS_socket,
            libc::SYS_connect,
            libc::SYS_bind,
            libc::SYS_listen,
            libc::SYS_accept,
            libc::SYS_accept4,
        ]);
    }
    let mut rules = denied
        .into_iter()
        .map(|syscall| (syscall, Vec::new()))
        .collect::<BTreeMap<_, _>>();
    if !network {
        rules.insert(
            libc::SYS_socketpair,
            vec![SeccompRule::new(vec![SeccompCondition::new(
                0,
                SeccompCmpArgLen::Dword,
                SeccompCmpOp::Ne,
                u64::try_from(libc::AF_UNIX)?,
            )?])?],
        );
    }
    let filter: BpfProgram = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(u32::try_from(libc::EPERM)?),
        TargetArch::try_from(std::env::consts::ARCH)?,
    )?
    .try_into()?;
    seccompiler::apply_filter(&filter).context("Restrict compiler system calls")?;
    let error = Command::new(program).args(args).current_dir(work).exec();
    Err(error).context("Start sandboxed compiler")
}

fn directory(value: std::ffi::OsString) -> Result<PathBuf> {
    let path = PathBuf::from(value).canonicalize()?;
    if !path.is_dir() || path == Path::new("/") {
        bail!("Expected a compiler directory");
    }
    Ok(path)
}
fn change_owner(path: &Path, uid: u32) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    let name = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: lchown receives a valid NUL-terminated path and never follows a
    // symlink out of the private build directory.
    if unsafe { libc::lchown(name.as_ptr(), uid, uid) } != 0 {
        return Err(std::io::Error::last_os_error()).context("Set build directory ownership");
    }
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path)? {
            change_owner(&entry?.path(), uid)?;
        }
    }
    Ok(())
}

fn compiler_identity() -> Result<u32> {
    let ranges = |path: &str| -> Result<Vec<(u64, u64)>> {
        std::fs::read_to_string(path)?
            .lines()
            .map(|line| {
                let fields = line
                    .split_whitespace()
                    .map(str::parse::<u64>)
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                if fields.len() != 3 {
                    bail!("Invalid compiler identity mapping");
                }
                Ok((
                    fields[0],
                    fields[0]
                        .checked_add(fields[2])
                        .context("Invalid identity range")?,
                ))
            })
            .collect()
    };
    for (user_start, user_end) in ranges("/proc/self/uid_map")? {
        for (group_start, group_end) in ranges("/proc/self/gid_map")? {
            let start = user_start.max(group_start).max(10_000);
            let end = user_end.min(group_end).min(u64::from(u32::MAX));
            if start < end {
                // Keep normal root builds away from real system accounts, while
                // supporting the narrower mappings used by rootless Docker.
                let start = start.max((end - 1).min(0x4000_0000));
                let identity = start + u64::from(std::process::id()) % (end - start);
                if identity != 65_534 {
                    return Ok(u32::try_from(identity)?);
                }
            }
        }
    }
    bail!("The container must map an unprivileged compiler identity")
}

use super::*;

fn record(id: Id, mask: u32, length: u32) -> Vec<u8> {
    [
        id.to_ne_bytes(),
        mask.to_ne_bytes(),
        0_u32.to_ne_bytes(),
        length.to_ne_bytes(),
    ]
    .concat()
}

#[test]
fn removing_one_hard_link_registration_keeps_the_other_watched() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("a");
    let alias = root.path().join("b");
    std::fs::write(&source, "one").unwrap();
    std::fs::hard_link(&source, &alias).unwrap();
    let mut kernel = Kernel::new(std::sync::Arc::new(tokio::sync::Semaphore::new(16))).unwrap();
    let first = kernel
        .register(&open(&source).unwrap(), false, &source)
        .unwrap();
    let second = kernel
        .register(&open(&alias).unwrap(), false, &alias)
        .unwrap();
    assert_eq!(first, second);
    kernel.remove(first).unwrap();
    assert!(!kernel.poll().unwrap());
    std::fs::write(&alias, "two").unwrap();
    assert!(kernel.poll().unwrap());
    kernel.remove(second).unwrap();
    assert!(!kernel.poll().unwrap());
    assert!(kernel.references.is_empty());
    assert!(kernel.removed.is_empty());
}

#[test]
fn unexpected_kernel_watch_loss_is_not_an_ordinary_removal_receipt() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("a");
    std::fs::write(&source, "one").unwrap();
    let mut kernel = Kernel::new(std::sync::Arc::new(tokio::sync::Semaphore::new(16))).unwrap();
    let id = kernel
        .register(&open(&source).unwrap(), false, &source)
        .unwrap();
    // Simulate a kernel-side removal without the adapter's expected receipt.
    assert_eq!(
        unsafe { libc::inotify_rm_watch(kernel.descriptor.as_raw_fd(), id) },
        0
    );
    assert!(kernel.poll().unwrap_err().contains("watch was lost"));
}

#[test]
fn overflow_truncated_records_and_unknown_registrations_require_a_new_observation() {
    let mut kernel = Kernel::new(std::sync::Arc::new(tokio::sync::Semaphore::new(16))).unwrap();
    assert!(kernel.records(&record(-1, libc::IN_Q_OVERFLOW, 0)).is_err());
    assert!(kernel.records(&[0]).is_err());
    assert!(kernel.records(&record(1, libc::IN_MODIFY, 4)).is_err());
    assert!(kernel.records(&record(1, libc::IN_MODIFY, 0)).is_err());
}

//! Original metadata ACKs outlive folder retirement and browser/host lifetimes.
//! Both adapters use one persisted retry policy and the original callback only.
use super::*;
use crate::workspace_transactions::original_command::OriginalCommandJournal;
impl Host {
    pub(super) fn recover_command_settlements(&self, namespace: Arc<Namespace>) {
        let Ok(pending) = namespace.pending_command_settlements() else {
            return;
        };
        for settlement in pending {
            self.recover_command_settlement(namespace.clone(), settlement);
        }
    }
    fn recover_command_settlement(
        &self,
        namespace: Arc<Namespace>,
        settlement: crate::native_recovery::PendingCommandSettlement,
    ) {
        let key = format!("settlement:{}", settlement.id);
        let Ok(Some(recovery)) = self.inner.command_recoveries.reserve(key) else {
            return;
        };
        let coordinator = self.inner.coordinator.clone();
        let Ok(registration) = coordinator.worker() else {
            return;
        };
        tokio::spawn(async move {
            let _registration = registration;
            let _recovery = recovery;
            let Ok(journal) = OriginalCommandJournal::new(
                settlement.origin.clone(),
                settlement.authority.clone(),
                settlement.worker.clone(),
            ) else {
                return;
            };
            loop {
                match namespace.has_command_settlement(&settlement) {
                    Ok(false) => return,
                    Ok(true) => {
                        if coordinator
                            .prepare(journal.acknowledge_settlement(&settlement.recovery))
                            .await
                            .is_ok_and(|result| result.is_ok())
                            && namespace
                                .acknowledged_command_settlement(&settlement)
                                .is_ok()
                        {
                            return;
                        }
                    }
                    Err(_) => {}
                }
                tokio::select! { biased;
                    ()=coordinator.stopped()=>return,
                    ()=tokio::time::sleep(Duration::from_secs(2))=>{}
                }
            }
        });
    }
}

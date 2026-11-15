//! Queue local lock attempts so fresh readers cannot starve a waiting publisher.
//! The filesystem lock still coordinates processes; this queue owns no file I/O.
use super::*;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Condvar, Mutex, OnceLock, Weak},
    time::Instant,
};

const MAX_QUEUES: usize = 256;
const MAX_WAITERS: usize = 256;
#[derive(Default)]
struct Queue {
    waiting: VecDeque<u64>,
    next: u64,
}
#[derive(Default)]
struct Gate {
    queue: Mutex<Queue>,
    changed: Condvar,
}
pub(super) struct Turn {
    gate: Arc<Gate>,
    id: u64,
}
impl Drop for Turn {
    fn drop(&mut self) {
        if let Ok(mut queue) = self.gate.queue.lock() {
            queue.waiting.retain(|id| *id != self.id);
            self.gate.changed.notify_all();
        }
    }
}
fn gate(file: &File) -> Result<Arc<Gate>, String> {
    type Gates = Mutex<BTreeMap<(u64, u64), Weak<Gate>>>;
    static GATES: OnceLock<Gates> = OnceLock::new();
    let metadata = io(file.metadata())?;
    let key = (metadata.dev(), metadata.ino());
    let mut gates = GATES
        .get_or_init(Gates::default)
        .lock()
        .map_err(|_| INVALID)?;
    if let Some(gate) = gates.get(&key).and_then(Weak::upgrade) {
        return Ok(gate);
    }
    gates.retain(|_, gate| gate.strong_count() != 0);
    if gates.len() >= MAX_QUEUES {
        return Err(BUSY.into());
    }
    let gate = Arc::new(Gate::default());
    gates.insert(key, Arc::downgrade(&gate));
    Ok(gate)
}
pub(super) fn turn(file: &File, deadline: Instant, queued: impl FnOnce()) -> Result<Turn, String> {
    let gate = gate(file)?;
    let mut queue = gate.queue.lock().map_err(|_| INVALID)?;
    if queue.waiting.len() >= MAX_WAITERS {
        return Err(BUSY.into());
    }
    let id = queue.next;
    queue.next = id.checked_add(1).ok_or(BUSY)?;
    queue.waiting.push_back(id);
    let turn = Turn {
        gate: gate.clone(),
        id,
    };
    // Observers may signal another thread, but never run under the queue mutex.
    drop(queue);
    queued();
    let mut queue = gate.queue.lock().map_err(|_| INVALID)?;
    while queue.waiting.front() != Some(&id) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            drop(queue);
            return Err(BUSY.into());
        }
        queue = gate
            .changed
            .wait_timeout(queue, remaining)
            .map_err(|_| INVALID)?
            .0;
    }
    drop(queue);
    if Instant::now() >= deadline {
        return Err(BUSY.into());
    }
    Ok(turn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};

    #[test]
    fn readers_cannot_barge_ahead_of_an_already_waiting_namespace_publisher() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("anchor");
        let first = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        first.lock_shared().unwrap();
        let held = gate(&first).unwrap();
        let (writer_queued, waiting_writer) = mpsc::channel();
        let (writer_acquired, publishing) = mpsc::channel();
        let (release_writer, finish_publishing) = mpsc::channel();
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            let file = File::open(writer_path).unwrap();
            let turn = turn(&file, Instant::now() + Duration::from_secs(5), || {
                writer_queued.send(()).unwrap();
            })
            .unwrap();
            file.lock().unwrap();
            drop(turn);
            writer_acquired.send(()).unwrap();
            finish_publishing
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        });
        waiting_writer.recv_timeout(Duration::from_secs(5)).unwrap();
        let (reader_queued, waiting_reader) = mpsc::channel();
        let (reader_acquired, reading) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let file = File::open(path).unwrap();
            let turn = turn(&file, Instant::now() + Duration::from_secs(5), || {
                reader_queued.send(()).unwrap();
            })
            .unwrap();
            file.lock_shared().unwrap();
            drop(turn);
            reader_acquired.send(()).unwrap();
        });
        waiting_reader.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            matches!(reading.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "A fresh shared reader passed the queued exclusive publisher"
        );
        first.unlock().unwrap();
        publishing.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(reading.try_recv(), Err(mpsc::TryRecvError::Empty)));
        release_writer.send(()).unwrap();
        reading.recv_timeout(Duration::from_secs(5)).unwrap();
        writer.join().unwrap();
        reader.join().unwrap();
        assert!(held.queue.lock().unwrap().waiting.is_empty());
    }
}

use std::collections::VecDeque;

/// In-memory model of the relay's per-(node_id, session) append-only blob log.
///
/// The relay crate owns the on-disk implementation (flat segment files, see
/// WORMHOLE.md §12 open decision 1). This struct is the pure-logic model used
/// for testing resume semantics and as the relay's hot in-memory window.
///
/// Entries are stored as `(seq, encoded_bytes)`. When `total_bytes` exceeds
/// `cap_bytes`, the oldest entries are evicted and the gap range is recorded
/// so the relay can emit a `GapNotice` frame.
pub struct ReplayLog {
    entries: VecDeque<(u64, Vec<u8>)>,
    cap_bytes: usize,
    total_bytes: usize,
    gap: Option<(u64, u64)>, // (oldest_evicted_seq, newest_evicted_seq)
}

impl ReplayLog {
    pub fn new(cap_bytes: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            cap_bytes,
            total_bytes: 0,
            gap: None,
        }
    }

    /// Append `payload` tagged with `seq`. Entries must arrive in monotonically
    /// increasing seq order (the relay enforces this; callers do not need to
    /// check). Returns `true` iff no eviction occurred.
    pub fn push(&mut self, seq: u64, payload: Vec<u8>) -> bool {
        self.total_bytes += payload.len();
        self.entries.push_back((seq, payload));

        if self.total_bytes <= self.cap_bytes {
            return true;
        }

        while self.total_bytes > self.cap_bytes {
            if let Some((evicted_seq, evicted_payload)) = self.entries.pop_front() {
                self.total_bytes -= evicted_payload.len();
                self.gap = Some(match self.gap {
                    None => (evicted_seq, evicted_seq),
                    Some((from, _)) => (from, evicted_seq),
                });
            } else {
                break;
            }
        }
        false
    }

    /// Iterator over entries with `seq >= from_seq`, in order.
    /// Returns an empty iterator if `from_seq` is past the tail.
    pub fn replay_from(&self, from_seq: u64) -> impl Iterator<Item = &(u64, Vec<u8>)> {
        self.entries
            .iter()
            .skip_while(move |(seq, _)| *seq < from_seq)
    }

    /// Returns the evicted range `(from, to)` (inclusive) if entries have been
    /// evicted due to the cap. The relay emits `Kind::GapNotice { from, to }`
    /// when this is non-`None` and the operator's `from_seq` falls inside it.
    pub fn gap(&self) -> Option<(u64, u64)> {
        self.gap
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

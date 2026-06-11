/// Per-session sequence state for one direction of the channel.
///
/// Both ends maintain one `SeqState` for the frames they *send* and one for
/// the frames they *receive*. Inbound acceptance is strict: only the next
/// expected seq is accepted, so gaps and replays are both rejected at this
/// layer (Noise nonces handle per-connection replays; `seq` handles the
/// cross-reconnect monotonicity invariant).
#[derive(Debug, Default, Clone)]
pub struct SeqState {
    next_inbound: u64,
    next_outbound: u64,
}

impl SeqState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Accept an inbound frame seq. Returns `true` iff `seq == next_expected`
    /// and advances the watermark. Returns `false` (and does not advance) for
    /// duplicates, gaps, or rewinds.
    pub fn accept(&mut self, seq: u64) -> bool {
        if seq == self.next_inbound {
            self.next_inbound = seq + 1;
            true
        } else {
            false
        }
    }

    /// Mint the next outbound seq and advance the counter.
    pub fn next_send(&mut self) -> u64 {
        let s = self.next_outbound;
        self.next_outbound += 1;
        s
    }

    /// Highest successfully accepted inbound seq, or `None` if no frame has
    /// been accepted yet.
    pub fn watermark(&self) -> Option<u64> {
        self.next_inbound.checked_sub(1)
    }

    /// Fast-forward the inbound watermark to `upto` (inclusive). Used after a
    /// `Resume` handshake: both sides agree frames `0..=upto` have been
    /// delivered, so the receiver starts expecting `upto + 1`.
    pub fn advance_inbound_to(&mut self, upto: u64) {
        if upto + 1 > self.next_inbound {
            self.next_inbound = upto + 1;
        }
    }
}

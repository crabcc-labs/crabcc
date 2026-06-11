// Allocation profiler for the Noise_IK hot path.
//
// The wormhole-node design claim: zero per-frame heap allocations on the
// encrypt/decrypt round-trip. NoiseBufs are pre-allocated; snow's transport
// mode writes into caller-supplied slices.
//
// divan measures actual allocator calls per iteration, not just throughput.
// A non-zero alloc count here means the zero-heap claim is broken.
//
// Run:
//   cargo bench -p wormhole-node --bench alloc_profile
//   cargo bench -p wormhole-node --bench alloc_profile -- --sample-count 100

use divan::{black_box, AllocProfiler, Bencher};
use snow::Builder;

#[global_allocator]
static ALLOC: AllocProfiler = AllocProfiler::system();

fn main() {
    divan::main();
}

const PARAMS: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
const BUF: usize = 65536 + 128;

struct Transport {
    tx: snow::TransportState,
    rx: snow::TransportState,
    send: Box<[u8; BUF]>,
    recv: Box<[u8; BUF]>,
}

impl Transport {
    fn new() -> Self {
        let params: snow::params::NoiseParams = PARAMS.parse().unwrap();
        let init_kp = Builder::new(params.clone()).generate_keypair().unwrap();
        let resp_kp = Builder::new(params.clone()).generate_keypair().unwrap();

        let mut init = Builder::new(params.clone())
            .local_private_key(&init_kp.private)
            .remote_public_key(&resp_kp.public)
            .build_initiator()
            .unwrap();
        let mut resp = Builder::new(params)
            .local_private_key(&resp_kp.private)
            .build_responder()
            .unwrap();

        let mut buf = vec![0u8; BUF];
        // Separate read buffer so the handshake message (input) and the
        // decrypted payload (output) don't alias the same allocation.
        let mut rbuf = vec![0u8; BUF];
        let n = init.write_message(&[], &mut buf).unwrap();
        resp.read_message(&buf[..n], &mut rbuf).unwrap();
        let n = resp.write_message(&[], &mut buf).unwrap();
        init.read_message(&buf[..n], &mut rbuf).unwrap();

        Self {
            tx: init.into_transport_mode().unwrap(),
            rx: resp.into_transport_mode().unwrap(),
            send: Box::new([0u8; BUF]),
            recv: Box::new([0u8; BUF]),
        }
    }

    #[inline(always)]
    fn roundtrip(&mut self, payload: &[u8]) -> usize {
        let enc_len = self
            .tx
            .write_message(payload, self.send.as_mut_slice())
            .unwrap();
        let cipher = unsafe { self.send.get_unchecked(..enc_len) };
        self.rx
            .read_message(cipher, self.recv.as_mut_slice())
            .unwrap()
    }
}

// Bench function takes ownership of Transport per sample so the allocator
// sees per-iteration setup allocations separately from hot-path allocations.
// The `bencher.bench_local` closure is the hot path — alloc count here must be 0.

#[divan::bench(args = [64, 512, 16384])]
fn roundtrip_alloc(bencher: Bencher, payload_len: usize) {
    let payload = vec![0xABu8; payload_len];
    let mut t = Transport::new();
    bencher.bench_local(|| {
        black_box(t.roundtrip(black_box(&payload)));
    });
}

// Explicitly track allocations for the handshake (setup cost, expected non-zero).
#[divan::bench]
fn handshake_alloc(bencher: Bencher) {
    bencher.bench_local(|| {
        black_box(Transport::new());
    });
}

// Baseline: getrandom syscall path (16 bytes, one syscall per call).
#[divan::bench]
fn session_id_getrandom(bencher: Bencher) {
    bencher.bench_local(|| {
        let mut b = [0u8; 16];
        getrandom::getrandom(black_box(&mut b)).unwrap();
        black_box(u128::from_le_bytes(b))
    });
}

// Optimized: atomic fetch_add (seeded once at startup, no syscall per call).
#[divan::bench]
fn session_id_atomic(bencher: Bencher) {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::OnceLock;
    static CTR: OnceLock<AtomicU64> = OnceLock::new();
    let ctr = CTR.get_or_init(|| {
        let mut seed = [0u8; 8];
        getrandom::getrandom(&mut seed).ok();
        AtomicU64::new(u64::from_le_bytes(seed))
    });
    let fake_node_id = [0xABu8; 32];
    bencher.bench_local(|| {
        let seq = ctr.fetch_add(1, Ordering::Relaxed);
        let hi = u64::from_le_bytes(black_box(&fake_node_id[..8]).try_into().unwrap());
        black_box(((hi as u128) << 64) | (seq as u128))
    });
}

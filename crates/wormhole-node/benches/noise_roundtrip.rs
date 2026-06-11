// Measures the hot-path cost: Noise_IK_25519_ChaChaPoly_BLAKE2s encrypt+decrypt
// round-trip at three payload sizes representative of real traffic:
//   - 64 B  : Ping/Pong, Ack
//   - 512 B : typical Cmd/Event with small stdout
//   - 16 KB : large ExecResult (log output, file content)
//
// Run: cargo bench -p wormhole-node

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use snow::Builder;

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
        // IK: initiator writes msg1, responder reads + writes msg2, initiator reads
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
        // SAFETY: snow guarantees enc_len <= BUF
        let cipher = unsafe { self.send.get_unchecked(..enc_len) };
        let dec_len = self
            .rx
            .read_message(cipher, self.recv.as_mut_slice())
            .unwrap();
        dec_len
    }
}

fn bench_noise(c: &mut Criterion) {
    let sizes: &[usize] = &[64, 512, 16 * 1024];
    let mut group = c.benchmark_group("noise_roundtrip");

    for &sz in sizes {
        let payload = vec![0xABu8; sz];
        group.throughput(Throughput::Bytes(sz as u64));
        group.bench_with_input(BenchmarkId::from_parameter(sz), &payload, |b, p| {
            let mut t = Transport::new();
            b.iter(|| t.roundtrip(black_box(p)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_noise);
criterion_main!(benches);

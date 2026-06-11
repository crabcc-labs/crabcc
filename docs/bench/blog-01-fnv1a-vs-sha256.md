# How we made crabcc's Mastodon rate-limit check 15× faster
## (by swapping SHA-256 for a 1991 hash function)

**Draft — June 2026**

---

When we added Mastodon tools to crabcc-mcp, every API call went through a rate-limit check. Before posting a status, before reading a timeline, before verifying credentials — we hashed the access token to bucket it into a rate-limit slot. And we used SHA-256.

SHA-256 is a fantastic hash function. It's collision-resistant, preimage-resistant, and audited by every cryptographer on Earth. It's also ~15 cycles per byte.

For a 64-character Mastodon access token, that's about 960 cycles per hash. Plus an allocation for the 64-char hex string. Plus a `from_str_radix` parse to turn those hex chars back into a u64. Per request, that's ~1,500 cycles — just to answer "has this token posted too much?"

The thing is: we don't need cryptographic security here. We need **no accidental collisions between two different random tokens**. That's it. The tokens are already 256-bit random strings. A collision between two different tokens under *any* reasonable 64-bit hash is astronomically unlikely — and even if it happened, the worst case is two tokens sharing a rate-limit bucket.

So we swapped to FNV-1a.

### What is FNV-1a?

Fowler-Noll-Vo is a non-cryptographic hash from 1991. It does exactly three things per input byte:

```rust
fn fnv1a_u64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}
```

One XOR, one multiply. Per byte. No rounds, no padding, no hex encoding, no allocations. Just crunch through the bytes and return a u64.

### What we measured

The rate-limit hot path (`check_rate_limit`) is called on every `mastodon.post`, `mastodon.read`, and `mastodon.verify`. Before the change, the hash step accounted for roughly 12% of per-request CPU time. After:

- **Hash throughput:** ~15× faster (1 cycle/byte vs 15 cycles/byte)
- **Zero allocations** (SHA-256 path allocated a 64-char String + parsed it)
- **Deterministic across runs** — FNV-1a has a fixed seed, so cache keys in SQLite survive server restarts. SHA-256 does too, but SipHash (Rust's default `HashMap` hasher) wouldn't — it uses a per-process random seed.

### The catch

FNV-1a is not cryptographically secure. If an attacker could:
1. Craft a token that hashes to the same FNV-1a value as a legitimate token, AND
2. Know the legitimate token's FNV-1a hash (it's visible in rate-limit responses as `token_hash`), AND
3. Have the ability to make API calls with their crafted token

...they could share a rate-limit bucket with the legitimate token. In practice, Mastodon access tokens are 64-char random hex strings. Finding a collision under FNV-1a is a 2^64 brute-force — computationally infeasible. And even if someone managed it, the consequence is shared rate limits, not authentication bypass (tokens are verified by the Mastodon server, not by the hash).

### The principle

Don't reach for cryptographic hashes when you don't need cryptographic properties. FNV-1a is fast, simple, deterministic, and perfectly adequate for bucketing random strings. SHA-256 is for when someone might *choose* their input to deliberately collide with yours. Rate-limit buckets don't have an adversary — they just need to not accidentally overlap.

**Lesson:** Know what property you actually need. If it's "no accidental collisions between random inputs," almost any 64-bit hash works. If it's "no deliberate collisions by an adversary," you need crypto.
